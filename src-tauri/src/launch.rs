//! How the app gets opened from outside: its command line, and
//! roon-toasted:// links (e.g. from a Stream Deck button), which Windows
//! passes to the app as a command-line argument.
//!
//! - `roon-toasted://open` (or just `roon-toasted://`): the Toaster, as the player
//! - `roon-toasted://search` or `--search`: the Toaster on Search, ready to type
//! - `roon-toasted://toggle`: like the Open Toaster hotkey (hides it if it's
//!   open and in front, otherwise opens it)
//!
//! Starting the app plainly (no link or option, e.g. with Windows) leaves it in
//! the tray; starting it again while it's running opens the Toaster. When a
//! link starts the app, the Toaster may open before its page has loaded, so
//! the page also asks for the mode once it's ready (`take_startup_mode`).
//!
//! The link works because the app registers roon-toasted:// for the current
//! user every time it starts (HKEY_CURRENT_USER\Software\Classes\roon-toasted),
//! pointing at wherever the app is. With a dev build and an installed copy on
//! the same PC, the one started last is the one links open.

use std::sync::Mutex;

use tauri::{AppHandle, Manager, State};

use crate::ToasterMode;

/// What a launch asks for.
#[derive(Clone, Copy)]
pub enum Request {
    Open(ToasterMode),
    Toggle,
}

/// The mode the app was started in by a link or `--search`, until the
/// Toaster's page picks it up.
#[derive(Default)]
pub struct StartupMode(Mutex<Option<&'static str>>);

/// Reads a launch's arguments (the first is the app itself). None for a
/// plain start.
pub fn request(args: &[String]) -> Option<Request> {
    for arg in args.iter().skip(1) {
        let arg = arg.to_lowercase();
        if arg == "--search" {
            return Some(Request::Open(ToasterMode::Search));
        }
        if let Some(rest) = arg.strip_prefix("roon-toasted:") {
            // "//search/", "//search?x" and "search" all mean search
            let action = rest
                .trim_start_matches('/')
                .split(['/', '?', '#'])
                .next()
                .unwrap_or_default();
            return Some(match action {
                "search" => Request::Open(ToasterMode::Search),
                "toggle" => Request::Toggle,
                _ => Request::Open(ToasterMode::Player),
            });
        }
    }
    None
}

/// Does what a launch asked for.
pub fn handle(app: &AppHandle, request: Request) {
    match request {
        Request::Open(mode) => crate::open_toaster(app, mode),
        Request::Toggle => crate::toggle_toaster(app),
    }
}

/// First start: registers the link, and opens the Toaster if the app was
/// started by a link or `--search` (in setup, after the window is ready).
pub fn startup(app: &AppHandle) {
    #[cfg(windows)]
    if !register_link() {
        eprintln!("[launch] couldn't register roon-toasted:// links");
    }

    let args: Vec<String> = std::env::args().collect();
    let Some(request) = request(&args) else {
        return;
    };
    let mode = match request {
        Request::Open(ToasterMode::Search) => "search",
        _ => "player",
    };
    *app.state::<StartupMode>().0.lock().unwrap() = Some(mode);
    handle(app, request);
}

/// The Toaster's page asks this once it has loaded: "search" or "player" if
/// the app was just started by a link or `--search` (answered only once).
#[tauri::command]
pub fn take_startup_mode(startup: State<'_, StartupMode>) -> Option<&'static str> {
    startup.0.lock().unwrap().take()
}

/// Registers roon-toasted:// links for the current user, pointing at this copy
/// of the app. Returns false if Windows refused.
#[cfg(windows)]
fn register_link() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let exe = exe.display().to_string();
    let base = r"Software\Classes\roon-toasted";
    set_value(base, None, "URL:Roon: Toasted")
        && set_value(base, Some("URL Protocol"), "")
        && set_value(&format!(r"{base}\DefaultIcon"), None, &format!("\"{exe}\",0"))
        && set_value(
            &format!(r"{base}\shell\open\command"),
            None,
            &format!("\"{exe}\" \"%1\""),
        )
}

#[cfg(windows)]
/// Writes one text value under HKEY_CURRENT_USER, creating the key if needed
/// (`name` None = the key's default value).
fn set_value(path: &str, name: Option<&str>, value: &str) -> bool {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};

    let path = HSTRING::from(path);
    let name = name.map(HSTRING::from);
    let name = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
    // Text for the registry: UTF-16 with a closing NUL
    let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: valid strings, and a pointer to `data` with its size in bytes.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &path,
            name,
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        ) == ERROR_SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::{request, Request};
    use crate::ToasterMode;

    fn parse(arg: &str) -> Option<&'static str> {
        let args = vec!["roon-toasted.exe".to_string(), arg.to_string()];
        request(&args).map(|r| match r {
            Request::Open(ToasterMode::Search) => "search",
            Request::Open(ToasterMode::Player) => "player",
            Request::Toggle => "toggle",
        })
    }

    #[test]
    fn launch_arguments() {
        assert_eq!(parse("--search"), Some("search"));
        assert_eq!(parse("roon-toasted://search"), Some("search"));
        assert_eq!(parse("roon-toasted://search/"), Some("search"));
        assert_eq!(parse("ROON-TOASTED://Search?from=deck"), Some("search"));
        assert_eq!(parse("roon-toasted://toggle"), Some("toggle"));
        assert_eq!(parse("roon-toasted://open"), Some("player"));
        assert_eq!(parse("roon-toasted://"), Some("player"));
        assert_eq!(parse("--something-else"), None);
        assert!(request(&["roon-toasted.exe".to_string()]).is_none());
    }
}
