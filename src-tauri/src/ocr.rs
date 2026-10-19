//! Reading Roon's screen: the name on an artist page, which Roon's log
//! never gives (see roonnav.rs). A picture of Roon's client area (what
//! wrapper.rs takes for the sidebar), cropped to where the page's title
//! sits, goes through Windows' own OCR engine; the tallest line of text is
//! the name (two lines when it wraps). Nothing leaves the PC, and it only
//! runs when Wrapped's "Find artist" fallback is used.

use tauri::AppHandle;

/// What was read off the page: the name, as a search term.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenName {
    pub name: String,
}

/// The name on Roon's current artist (or album) page, read off the screen.
#[tauri::command]
pub async fn wrapper_read_page_name(app: AppHandle) -> Option<ScreenName> {
    let _ = &app;
    tauri::async_runtime::spawn_blocking(imp::read_page_name).await.ok().flatten().map(|name| ScreenName { name })
}

/// Runs Roon's search for `text`, then clicks the result whose name is
/// exactly `pick` (the first such line on the results page, which is the
/// Top result's), read off the screen within a few seconds. True when the
/// click was sent; false when the results showed nothing by that name
/// (Roon stays on its results page).
#[tauri::command]
pub async fn wrapper_open_in_roon(text: String, pick: String) -> Result<bool, String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(move || {
            if !crate::wrapper::roon_search_for(&text) {
                return Err("Roon couldn't be brought to the front for the search.".to_string());
            }
            let clicked = imp::click_text(&pick, std::time::Duration::from_millis(3500));
            if clicked {
                // Until Roon's new page has drawn and stopped moving, read
                // off its window: the results page gone, a page title up,
                // and the title in the same place on two reads in a row.
                // At most 4 s.
                let started = std::time::Instant::now();
                let settled = imp::wait_settled(std::time::Duration::from_millis(4000));
                println!("[ocr] Roon's page settled: {settled} after {} ms", started.elapsed().as_millis());
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            Ok(clicked)
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    {
        let _ = (text, pick);
        Ok(false)
    }
}

#[cfg(windows)]
mod imp {
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Security::Cryptography::CryptographicBuffer;

    /// A line the engine read, with where it sits (real pixels of the crop).
    #[derive(Clone)]
    struct Line {
        text: String,
        left: f32,
        top: f32,
        width: f32,
        height: f32,
    }

    pub fn read_page_name() -> Option<String> {
        // The title sits at the top of the page: Roon's page scrolled back
        // there first (the user may have scrolled down to read), and given
        // a moment to come to rest
        let sidebar = crate::skin::sidebar().map(|(w, _)| w).unwrap_or(0).max(0);
        if crate::wrapper::scroll_roon_to_top(sidebar) {
            let rested = wait_settled(std::time::Duration::from_millis(1500));
            println!("[ocr] scrolled Roon's page to the top; at rest: {rested}");
        }
        let Some((roon, _)) = crate::wrapper::visible_hole().or_else(crate::wrapper::covered_hole) else {
            println!("[ocr] title: no Roon window in the Wrapper");
            return None;
        };
        let Some((pixels, width, height)) = crate::wrapper::roon_picture(roon) else {
            println!("[ocr] title: no picture of Roon");
            return None;
        };
        // The title area: right of the sidebar, under the header row, the
        // upper part of the page, its left 60% first (Roon puts an artist's
        // photo on the right, and lettering in it could pass for a title);
        // the limits are Roon's layout at 100%, scaled by the picture's own
        // size against a 1440-wide window
        let scale = (width as f32 / 1440.0).max(0.6);
        let sidebar = crate::skin::sidebar().map(|(w, _)| w).unwrap_or(0).max(0);
        let left = (sidebar as f32 + 30.0 * scale) as i32;
        // (from just under Roon's header row: a page scrolled a little
        // lifts its title toward it)
        let top = (52.0 * scale) as i32;
        let narrow = (left as f32 + (width - left) as f32 * 0.6) as i32;
        let bottom = (height as f32 * 0.5) as i32;
        if narrow - left < 100 || bottom - top < 40 {
            return None;
        }
        let min_height = 26.0 * scale;
        // As the picture is, then (nothing found) with only the white
        // lettering kept and a little deeper: a wide window puts the title
        // over the middle of the artist's photo, where the engine reads
        // nothing at all (a maximized Roon on Absent in Body's page)
        if let Some(name) = read_title(&pixels, width, left, top, narrow, bottom, min_height, false) {
            return Some(name);
        }
        let deeper = ((height as f32) * 0.62) as i32;
        let name = read_title(&pixels, width, left, top, narrow, deeper, min_height, true);
        println!("[ocr] title read (white lettering only): {name:?}");
        name
    }

    /// The title in the crop from (`left`, `top`) to (`narrow`, `bottom`),
    /// read again out to the page's right edge when it runs into the
    /// crop's (an artist without a photo gets a placeholder circle that
    /// pushes the name right), keeping only the lines that start where the
    /// title starts. `bright`: only near-white pixels kept (see `bright_only`).
    #[allow(clippy::too_many_arguments)]
    fn read_title(pixels: &[u8], width: i32, left: i32, top: i32, narrow: i32, bottom: i32, min_height: f32, bright: bool) -> Option<String> {
        let lines = read_area(pixels, width, left, top, narrow, bottom, bright)?;
        let Some((name, title)) = pick_name(lines, min_height) else {
            println!("[ocr] title read: None");
            return None;
        };
        if title.left + title.width < (narrow - left) as f32 - 6.0 {
            println!("[ocr] title read: {name:?}");
            return Some(name);
        }
        let scale = (width as f32 / 1440.0).max(0.6);
        let wide = width - (20.0 * scale) as i32;
        let lines = read_area(pixels, width, left, top, wide, bottom, bright)?;
        let aligned: Vec<Line> = lines.into_iter().filter(|l| (l.left - title.left).abs() < title.height * 0.6).collect();
        let name = pick_name(aligned, min_height).map(|(name, _)| name);
        println!("[ocr] title read (cut at first, read wider): {name:?}");
        name
    }

    /// The lines in the crop of `pixels` from (`left`, `top`) to (`right`,
    /// `bottom`), logged.
    fn read_area(pixels: &[u8], width: i32, left: i32, top: i32, right: i32, bottom: i32, bright: bool) -> Option<Vec<Line>> {
        let height = (pixels.len() / 4) as i32 / width.max(1);
        let bottom = bottom.min(height);
        let crop = crop(pixels, width, left, top, right - left, bottom - top);
        let crop = if bright { super::bright_only(&crop) } else { crop };
        let lines = match recognize(&crop, right - left, bottom - top) {
            Ok(lines) => lines,
            Err(e) => {
                println!("[ocr] title: OCR failed: {e}");
                return None;
            }
        };
        let seen: Vec<String> = lines.iter().take(12).map(|l| format!("\"{}\"@{:.0},{:.0} w{:.0} h{:.0}", l.text, l.left, l.top, l.width, l.height)).collect();
        println!("[ocr] title area ({}x{} from {left},{top}): {}", right - left, bottom - top, seen.join("  "));
        // (the last try finding nothing at all: the picture kept to look at)
        if lines.is_empty() && bright {
            keep_miss(pixels, width, &crop);
        }
        Some(lines)
    }

    /// Nothing read at all: the crop's mean brightness logged and Roon's
    /// whole picture kept as `ocr-miss.bmp` in the app data folder, to see
    /// what the engine was given (a blank capture, or a page it can't read).
    fn keep_miss(pixels: &[u8], width: i32, crop: &[u8]) {
        let mean = |px: &[u8]| -> u64 {
            let n = (px.len() / 4) as u64;
            if n == 0 {
                return 0;
            }
            px.chunks_exact(4).map(|p| (p[0] as u64 + p[1] as u64 + p[2] as u64) / 3).sum::<u64>() / n
        };
        let height = if width > 0 { (pixels.len() / 4) as i32 / width } else { 0 };
        let kept = std::env::var("APPDATA")
            .ok()
            .map(|dir| std::path::Path::new(&dir).join("com.vulkan.roon-toasted").join("ocr-miss.bmp"))
            .is_some_and(|path| std::fs::write(path, super::bmp(pixels, width, height)).is_ok());
        println!(
            "[ocr] nothing read: the crop's mean brightness {} of 255, the whole picture's {} ({}x{}); kept as ocr-miss.bmp: {kept}",
            mean(crop),
            mean(pixels),
            width,
            height
        );
    }

    /// Reads Roon's content area until a line says `wanted` (letters and
    /// digits compared, case aside), then clicks its middle. Up to `wait`.
    pub fn click_text(wanted: &str, wait: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + wait;
        let want = plain(wanted);
        if want.is_empty() {
            return false;
        }
        let mut tries = 0;
        // The page slides into place: a click while it moves lands as a drag,
        // so the name has to be read in the same place twice
        let mut last: Option<(i32, i32)> = None;
        loop {
            tries += 1;
            match find_text(&want) {
                Ok(Some((x, y))) => {
                    if last.is_some_and(|(lx, ly)| (lx - x).abs() <= 2 && (ly - y).abs() <= 2) {
                        let sent = crate::wrapper::click_roon_at(x, y);
                        println!("[ocr] \"{wanted}\" found at ({x}, {y}) on try {tries}; click sent: {sent}");
                        return sent;
                    }
                    last = Some((x, y));
                }
                Ok(None) => last = None,
                Err(e) => println!("[ocr] try {tries}: {e}"),
            }
            if std::time::Instant::now() >= deadline {
                println!("[ocr] \"{wanted}\" not read on the page after {tries} tries");
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }

    /// Waits until Roon's page has stopped moving after a click on a search
    /// result: the "Search results for" heading gone, a large title line
    /// (a page's name) up, and that line at the same place on two reads in
    /// a row. False when `wait` runs out first.
    pub fn wait_settled(wait: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + wait;
        let mut last: Option<(f32, f32)> = None;
        loop {
            let now = page_title_position();
            if let (Some((x, y)), Some((lx, ly))) = (now, last) {
                if (x - lx).abs() <= 2.0 && (y - ly).abs() <= 2.0 {
                    return true;
                }
            }
            last = now;
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
    }

    /// Where the tallest line of text on Roon's page sits (its left and
    /// top, real pixels of the content area), when it is a page title: not
    /// on the search results page, and large.
    fn page_title_position() -> Option<(f32, f32)> {
        let (roon, _) = crate::wrapper::visible_hole().or_else(crate::wrapper::covered_hole)?;
        let (pixels, width, height) = crate::wrapper::roon_picture(roon)?;
        let scale = (width as f32 / 1440.0).max(0.6);
        let sidebar = crate::skin::sidebar().map(|(w, _)| w).unwrap_or(0).max(0);
        let (w, h) = (width - sidebar, (height as f32 * 0.6) as i32);
        if w < 100 || h < 40 {
            return None;
        }
        let crop = crop(&pixels, width, sidebar, 0, w, h);
        let lines = recognize(&crop, w, h).ok()?;
        if lines.iter().any(|l| plain(&l.text).starts_with("searchresultsfor")) {
            return None;
        }
        let title = lines.iter().max_by(|a, b| a.height.partial_cmp(&b.height).unwrap_or(std::cmp::Ordering::Equal))?;
        (title.height >= 26.0 * scale).then_some((title.left, title.top))
    }

    /// Letters and digits only, lower-cased, for comparing what the engine
    /// read with a name.
    fn plain(text: &str) -> String {
        text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
    }

    /// Where a line reading `want` (see `plain`) sits on Roon's page, in
    /// real pixels of its client area: the first in reading order.
    fn find_text(want: &str) -> Result<Option<(i32, i32)>, String> {
        let (roon, _) = crate::wrapper::visible_hole().or_else(crate::wrapper::covered_hole).ok_or("no Roon window")?;
        let (pixels, width, height) = crate::wrapper::roon_picture(roon).ok_or("no picture of Roon")?;
        let sidebar = crate::skin::sidebar().map(|(w, _)| w).unwrap_or(0).max(0);
        let (left, top) = (sidebar, 0);
        let (w, h) = (width - left, (height as f32 * 0.8) as i32);
        if w < 100 || h < 40 {
            return Err(format!("picture too small: {width}x{height}, sidebar {sidebar}"));
        }
        let crop = crop(&pixels, width, left, top, w, h);
        let mut lines = recognize(&crop, w, h).map_err(|e| format!("OCR failed: {e}"))?;
        lines.sort_by(|a, b| a.top.partial_cmp(&b.top).unwrap_or(std::cmp::Ordering::Equal));
        let seen: Vec<String> = lines.iter().take(14).map(|l| format!("\"{}\"@{:.0},{:.0}", l.text, l.left, l.top)).collect();
        println!("[ocr] page ({w}x{h} from {left},{top}): {}", seen.join("  "));
        // Only once Roon's results page is up (its "Search results for"
        // heading; before that the search popup shows the same name in its
        // recent searches), and only below the heading
        let Some(heading) = lines.iter().find(|l| plain(&l.text).starts_with("searchresultsfor")) else {
            return Ok(None);
        };
        let line = lines.iter().find(|l| l.top > heading.top + heading.height && plain(&l.text) == want);
        // Just inside the name's first letters (a line's width can run on)
        Ok(line.map(|line| (left + (line.left + line.height * 0.8) as i32, top + (line.top + line.height / 2.0) as i32)))
    }

    fn crop(pixels: &[u8], stride_px: i32, x: i32, y: i32, w: i32, h: i32) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in y..y + h {
            let start = ((row * stride_px + x) * 4) as usize;
            out.extend_from_slice(&pixels[start..start + (w * 4) as usize]);
        }
        out
    }

    fn recognize(bgra: &[u8], width: i32, height: i32) -> windows::core::Result<Vec<Line>> {
        let buffer = CryptographicBuffer::CreateFromByteArray(bgra)?;
        let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(&buffer, BitmapPixelFormat::Bgra8, width, height, BitmapAlphaMode::Ignore)?;
        let engine = OcrEngine::TryCreateFromUserProfileLanguages()?;
        let result = engine.RecognizeAsync(&bitmap)?.get()?;
        let mut lines = Vec::new();
        for line in result.Lines()? {
            let text = line.Text()?.to_string();
            let mut left = f32::MAX;
            let mut right = 0f32;
            let mut top = f32::MAX;
            let mut bottom = 0f32;
            for word in line.Words()? {
                let r = word.BoundingRect()?;
                left = left.min(r.X);
                right = right.max(r.X + r.Width);
                top = top.min(r.Y);
                bottom = bottom.max(r.Y + r.Height);
            }
            if text.trim().is_empty() || left == f32::MAX {
                continue;
            }
            lines.push(Line { text: text.trim().to_string(), left, top, width: right - left, height: bottom - top });
        }
        Ok(lines)
    }

    /// The tallest line, with the lines right above or below it in the same
    /// large type (a wrapped name), in reading order; and the tallest line
    /// itself, for where the title sits.
    /// `min_height`: how tall a page title's letters are at the least
    /// (real pixels); Roon's titles are large, so smaller text isn't one.
    fn pick_name(mut lines: Vec<Line>, min_height: f32) -> Option<(String, Line)> {
        // A line cut by the top of the crop isn't read whole
        lines.retain(|l| l.top >= 1.0);
        // A placeholder picture's initials ("ON" in a circle for October
        // Noir, who has no photo): a few capitals with large text to their
        // right on the same row
        let same_row = |a: &Line, b: &Line| a.top < b.top + b.height && b.top < a.top + a.height;
        let initials: Vec<bool> = lines
            .iter()
            .map(|l| {
                let text = l.text.trim();
                (1..=3).contains(&text.chars().count())
                    && text.chars().all(|c| c.is_alphabetic() && c.is_uppercase())
                    && lines.iter().any(|o| o.left > l.left + l.width && same_row(l, o) && o.height >= l.height * 0.6)
            })
            .collect();
        let mut keep = initials.iter().map(|i| !i);
        lines.retain(|_| keep.next().unwrap_or(true));
        if lines.is_empty() {
            return None;
        }
        lines.sort_by(|a, b| a.top.partial_cmp(&b.top).unwrap_or(std::cmp::Ordering::Equal));
        let tallest = lines.iter().map(|l| l.height).fold(0f32, f32::max);
        if tallest < min_height {
            return None;
        }
        // The title: the tallest line and the large lines directly over or
        // under it (a name wrapped onto two lines; the engine measures the
        // two lines' letters a little differently)
        let anchor = lines.iter().position(|l| l.height == tallest)?;
        let large = |l: &Line| l.height >= tallest * 0.6;
        let mut first = anchor;
        while first > 0 && large(&lines[first - 1]) && adjacent(&lines[first - 1], &lines[first]) {
            first -= 1;
        }
        let mut last = anchor;
        while last + 1 < lines.len() && large(&lines[last + 1]) && adjacent(&lines[last], &lines[last + 1]) {
            last += 1;
        }
        // It stands out: clearly taller than the page's other text (text
        // left of it on its rows, lettering on a cover or picture, aside)
        let (title_top, title_bottom) = (lines[first].top, lines[last].top + lines[last].height);
        let beside = |l: &Line| l.left + l.width <= lines[anchor].left && l.top < title_bottom && title_top < l.top + l.height;
        let body = lines
            .iter()
            .enumerate()
            .filter(|(i, l)| (*i < first || *i > last) && !beside(l))
            .map(|(_, l)| l.height)
            .fold(0f32, f32::max);
        if body > 0.0 && tallest < body * 1.5 {
            return None;
        }
        let name = lines[first..=last].iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" ");
        let name = name.trim().trim_end_matches(|c: char| c == ',' || c == '.' || c == '-').to_string();
        // A name, not a paragraph: two lines at most, and a sensible length
        let length = name.chars().count();
        (last - first < 2 && (2..=70).contains(&length) && name.chars().any(char::is_alphanumeric)).then(|| (name, lines[anchor].clone()))
    }

    fn adjacent(above: &Line, below: &Line) -> bool {
        (above.left - below.left).abs() < above.height * 0.6 && below.top - (above.top + above.height) < above.height * 0.8
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn read_page_name() -> Option<String> {
        None
    }
}

/// Only the near-white pixels of a BGRA picture, as black on white: Roon's
/// titles are white, and over a photo the engine finds them once the photo
/// is gone (Absent in Body's faces have no pixel that bright; the lettering
/// is 255).
#[cfg_attr(not(windows), allow(dead_code))]
fn bright_only(bgra: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        let v = if px[0].min(px[1]).min(px[2]) >= 225 { 0 } else { 255 };
        out.extend_from_slice(&[v, v, v, 255]);
    }
    out
}

/// A 32-bit top-down BMP of BGRA pixels (for looking at what OCR was given).
#[cfg_attr(not(windows), allow(dead_code))]
fn bmp(bgra: &[u8], width: i32, height: i32) -> Vec<u8> {
    let size = (width.max(0) * height.max(0) * 4) as usize;
    let size = size.min(bgra.len());
    let data = size as u32;
    let mut out = Vec::with_capacity(54 + size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + data).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&(-height).to_le_bytes()); // negative: rows top to bottom
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&data.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&bgra[..size]);
    out
}

#[cfg(test)]
mod bmp_tests {
    #[test]
    fn header_and_rows() {
        let px = [1u8, 2, 3, 255, 4, 5, 6, 255];
        let out = super::bmp(&px, 2, 1);
        assert_eq!(&out[0..2], b"BM");
        assert_eq!(out.len(), 54 + 8);
        assert_eq!(i32::from_le_bytes(out[22..26].try_into().unwrap()), -1);
        assert_eq!(&out[54..], &px);
    }

    #[test]
    fn bright_only_keeps_white_lettering() {
        let px = [255u8, 255, 255, 255, 200, 230, 240, 255, 230, 226, 228, 255];
        assert_eq!(super::bright_only(&px), vec![0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255]);
    }
}
