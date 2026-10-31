// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Now Playing's Background: Custom images. The Wrapper's background window
//! picks an image file with Windows' own file picker
//! (`wrapper_pick_background`); the files picked so far are a history in the
//! settings (`background_images`, newest first), which the window lists and
//! can forget one of (`wrapper_forget_background`).
//!
//! The page shows them through the app's `bgimg` image route
//! (`convertFileSrc(path, "bgimg")`), which serves image files only (by
//! their extension), so it can't be used to read anything else off the PC.
//! (Not only the history's: a profile keeps its image after the file drops
//! off the history.) A file that has moved or gone comes back as not found,
//! and the page shows no image (just its color behind).

use tauri::http::{Request, Response};
use tauri::{AppHandle, Emitter, UriSchemeResponder, WebviewWindow};

use crate::settings::{self, AppSettings};

/// How many files the history keeps.
const HISTORY: usize = 12;

/// The kinds of image offered, and served.
const IMAGE_TYPES: [(&str, &str); 8] = [
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("png", "image/png"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
    ("bmp", "image/bmp"),
    ("avif", "image/avif"),
    ("jfif", "image/jpeg"),
];

/// Images bigger than this aren't served (a background has no use for them).
const MAX_BYTES: u64 = 80 * 1024 * 1024;

fn content_type(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    IMAGE_TYPES.iter().find(|(e, _)| *e == ext).map(|(_, t)| *t)
}

/// Windows paths: the same file whatever the letters' case.
fn same_path(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
}

/// The history with `path` first (once), at most `HISTORY` long.
fn with_first(history: &[String], path: &str) -> Vec<String> {
    let mut out = vec![path.to_string()];
    out.extend(history.iter().filter(|p| !same_path(p, path)).cloned());
    out.truncate(HISTORY);
    out
}

/// Changes the history and tells the pages, as a settings change.
fn change_history(app: &AppHandle, change: impl FnOnce(&[String]) -> Vec<String>) -> Result<AppSettings, String> {
    let path = settings::path(app).ok_or("Couldn't find the app data folder.")?;
    let mut current = settings::load(&path);
    current.background_images = change(&current.background_images);
    settings::save(&path, &current)?;
    let _ = app.emit("settings-changed", &current);
    Ok(current)
}

/// Opens Windows' file picker for an image, over the Wrapper; the picked file
/// goes to the top of the history. None when cancelled.
#[tauri::command]
pub async fn wrapper_pick_background(app: AppHandle, window: WebviewWindow) -> Result<Option<String>, String> {
    #[cfg(windows)]
    let owner = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    #[cfg(not(windows))]
    let owner = {
        let _ = &window;
        0isize
    };
    let picked = tauri::async_runtime::spawn_blocking(move || pick_image(owner))
        .await
        .map_err(|e| e.to_string())??;
    if let Some(file) = &picked {
        change_history(&app, |history| with_first(history, file))?;
    }
    Ok(picked)
}

/// Takes a file off the history (the X by it in the list).
#[tauri::command]
pub fn wrapper_forget_background(app: AppHandle, path: String) -> Result<AppSettings, String> {
    change_history(&app, |history| history.iter().filter(|p| !same_path(p, &path)).cloned().collect())
}

/// Serves `bgimg://localhost/<path, URL-encoded>` (on Windows the webview
/// spells it `http://bgimg.localhost/...`): the image, if it's an image file
/// and still there.
pub fn image_request(request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let file = decode(request.uri().path().trim_start_matches('/'));
    tauri::async_runtime::spawn(async move {
        let reply = |status: u16, content_type: &str, body: Vec<u8>| {
            Response::builder()
                .status(status)
                .header("Content-Type", content_type)
                // (the file can change under the same name)
                .header("Cache-Control", "no-cache")
                .header("Access-Control-Allow-Origin", "*")
                .body(body)
                .unwrap()
        };
        let Some(file) = file.filter(|f| std::path::Path::new(f).is_absolute()) else {
            responder.respond(reply(400, "text/plain", b"not a file path".to_vec()));
            return;
        };
        let Some(kind) = content_type(&file) else {
            responder.respond(reply(403, "text/plain", b"not an image".to_vec()));
            return;
        };
        let read = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
            if std::fs::metadata(&file)?.len() > MAX_BYTES {
                return Err(std::io::Error::other("too big"));
            }
            std::fs::read(&file)
        })
        .await;
        match read {
            Ok(Ok(bytes)) => responder.respond(reply(200, kind, bytes)),
            _ => responder.respond(reply(404, "text/plain", b"not found".to_vec())),
        }
    });
}

/// %XX escapes back to the text (UTF-8); None if they don't make text.
fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Windows' file picker, for images; None when cancelled.
#[cfg(windows)]
fn pick_image(owner: isize) -> Result<Option<String>, String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, SIGDN_FILESYSPATH};
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let pattern: String = IMAGE_TYPES.iter().map(|(e, _)| format!("*.{e}")).collect::<Vec<_>>().join(";");
    let (name, spec, title) = (wide("Images"), wide(&pattern), wide("Choose a background image"));
    // SAFETY: COM on this (blocking pool) thread for the dialog's lifetime;
    // the strings outlive the calls that use them; the path string is freed
    // with CoTaskMemFree as the API asks.
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| -> Result<Option<String>, String> {
            let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
            let options = dialog.GetOptions().map_err(|e| e.to_string())?;
            dialog.SetOptions(options | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM).map_err(|e| e.to_string())?;
            let filters = [COMDLG_FILTERSPEC { pszName: PCWSTR(name.as_ptr()), pszSpec: PCWSTR(spec.as_ptr()) }];
            dialog.SetFileTypes(&filters).map_err(|e| e.to_string())?;
            let _ = dialog.SetTitle(PCWSTR(title.as_ptr()));
            let owner = (owner != 0).then(|| HWND(owner as *mut _));
            if dialog.Show(owner).is_err() {
                return Ok(None); // cancelled
            }
            let item = dialog.GetResult().map_err(|e| e.to_string())?;
            let path: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|e| e.to_string())?;
            let text = path.to_string().map_err(|e| e.to_string());
            CoTaskMemFree(Some(path.0 as *const _));
            text.map(Some)
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(windows))]
fn pick_image(_owner: isize) -> Result<Option<String>, String> {
    Err("Choosing an image works on Windows only.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_order() {
        let h = vec!["C:\\a.jpg".to_string(), "C:\\b.png".to_string()];
        assert_eq!(with_first(&h, "c:\\B.PNG"), vec!["c:\\B.PNG", "C:\\a.jpg"]);
        let long: Vec<String> = (0..20).map(|i| format!("C:\\{i}.jpg")).collect();
        assert_eq!(with_first(&long, "C:\\new.jpg").len(), HISTORY);
    }

    #[test]
    fn decoding() {
        assert_eq!(decode("C%3A%5CPictures%5Cmy%20wall.jpg").as_deref(), Some("C:\\Pictures\\my wall.jpg"));
        assert_eq!(decode("%C3%B6.png").as_deref(), Some("ö.png"));
        assert_eq!(decode("%ZZ"), None);
    }

    #[test]
    fn image_types() {
        assert_eq!(content_type("C:\\x\\Wall.JPG"), Some("image/jpeg"));
        assert_eq!(content_type("C:\\x\\notes.txt"), None);
        assert_eq!(content_type("C:\\x\\noext"), None);
    }
}
