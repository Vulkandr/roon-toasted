// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Media keys another app has registered for itself (Roon does, with
//! Play/Pause, Stop, Next and Previous): Windows refuses them to us as
//! hotkeys, so they're taken with a low-level keyboard hook instead, which
//! sees a key before Windows hands it to whoever registered it.
//!
//! Only for the keys in `set`'s mask, and only while there are any: with
//! none, the hook is taken out again. It runs on a thread of its own with
//! its own message loop, and does nothing there but compare the key and
//! hand it on (mediakeys.rs starts a task for it), so nothing Toasted is
//! busy or stuck with can hold up typing (see the mouse hook in
//! wrapper.rs for how that went wrong once).
//!
//! Windows doesn't ask a keyboard hook about keys pressed while a window
//! of the hook's own app is in front, if that app reads the keyboard as
//! "raw input". Tauri's windows did by default, so with the Wrapper (or
//! any Toasted window) in front the hook heard nothing and Roon got the
//! keys (Vulk 2026-11-10: typing in Toasted never reached the hook, typing
//! anywhere else did). lib.rs switches that raw input off
//! (`device_event_filter`); the app never used it.

#[cfg(windows)]
pub use imp::set;

/// Without Windows there's no hook: the keys stay refused.
#[cfg(not(windows))]
pub fn set(_app: &tauri::AppHandle, _keys: &[u32]) -> bool {
    false
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex, OnceLock};

    use tauri::AppHandle;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, KBDLLHOOKSTRUCT,
        MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    /// The media keys' codes all sit in 0xAD to 0xB3; a key's bit here is
    /// its code minus `FIRST`.
    const FIRST: u32 = 0xAD;
    const LAST: u32 = 0xB3;
    /// The keys to take (bits as above).
    static TAKEN: AtomicU32 = AtomicU32::new(0);
    /// The ones held down (Windows repeats a held key; only its first
    /// press and its release are handed on).
    static DOWN: AtomicU32 = AtomicU32::new(0);
    static APP: OnceLock<AppHandle> = OnceLock::new();
    /// The hook thread's id while it runs.
    static THREAD: Mutex<Option<u32>> = Mutex::new(None);

    /// Takes these keys (Windows key codes) with the hook, and no others;
    /// none takes the hook out. False if the hook couldn't be put in.
    pub fn set(app: &AppHandle, keys: &[u32]) -> bool {
        let _ = APP.set(app.clone());
        let mask = keys.iter().filter(|k| (FIRST..=LAST).contains(k)).fold(0, |m, k| m | 1 << (k - FIRST));
        TAKEN.store(mask, Ordering::Relaxed);
        let mut thread = THREAD.lock().unwrap_or_else(|e| e.into_inner());
        match (*thread, mask != 0) {
            (None, true) => {
                let (tx, rx) = std::sync::mpsc::channel();
                let started = std::thread::Builder::new().name("key-hook".into()).spawn(move || run(tx));
                *thread = started.ok().and_then(|_| rx.recv().ok().flatten());
                thread.is_some()
            }
            (Some(id), false) => {
                // SAFETY: posts a message to our own thread (gone already
                // at worst, and then nothing happens).
                unsafe {
                    let _ = PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0));
                }
                *thread = None;
                DOWN.store(0, Ordering::Relaxed);
                true
            }
            _ => true,
        }
    }

    /// The hook thread: puts the hook in, says whether that worked (its
    /// thread id), and delivers the hook's calls until told to quit.
    fn run(started: std::sync::mpsc::Sender<Option<u32>>) {
        // SAFETY: a valid callback that lives for the whole program; the
        // loop only delivers the hook's calls on this thread, and the hook
        // is taken out before the thread ends.
        unsafe {
            let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), None, 0) else {
                let _ = started.send(None);
                return;
            };
            let _ = started.send(Some(GetCurrentThreadId()));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {}
            let _ = UnhookWindowsHookEx(hook);
        }
    }

    unsafe extern "system" fn on_key(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            // SAFETY: for a keyboard hook, lparam points at this struct.
            let vk = (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode;
            if (FIRST..=LAST).contains(&vk) {
                let bit = 1 << (vk - FIRST);
                if TAKEN.load(Ordering::Relaxed) & bit != 0 {
                    let message = wparam.0 as u32;
                    let pressed = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
                    let released = message == WM_KEYUP || message == WM_SYSKEYUP;
                    let was_down = if pressed {
                        DOWN.fetch_or(bit, Ordering::Relaxed) & bit != 0
                    } else {
                        DOWN.fetch_and(!bit, Ordering::Relaxed) & bit != 0
                    };
                    if (pressed && !was_down) || (released && was_down) {
                        if let Some(app) = APP.get() {
                            crate::mediakeys::on_hooked(app, vk, pressed);
                        }
                    }
                    // Ours: it goes no further
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}
