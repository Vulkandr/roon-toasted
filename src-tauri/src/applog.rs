// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The app's log file, `toasted-log.txt` in the app data folder, kept in
//! release builds too, and the watchdog that notices a stuck main thread.
//!
//! - Every `println!` / `eprintln!` in the app lands here as well as in the
//!   terminal (lib.rs shadows both macros for the whole crate), and so do
//!   the libraries' warnings (tracing). Lines from before the app data
//!   folder is known are held and written once it is.
//! - Startup runs as named steps (`step`), each logged with how long it took.
//! - The file starts over past 512 KB; the previous one is kept as
//!   `toasted-log.old.txt`.
//! - The watchdog asks the main thread to answer every second. Quiet for
//!   10 s: it logs which step the main thread was last in and saves a
//!   snapshot (minidump) of every thread, once per run, as
//!   `hang-<time>.dmp` (the last 3 are kept). Read it against that build's
//!   symbols (`symbols/<version>/`, saved by scripts/latest-json.mjs).
//!   The snapshot is written by a second copy of the app started just for
//!   that (`--hang-dump <pid> <file>`, see `dump_mode`): a process writing
//!   its own snapshot can lock up while doing it, which would turn a slow
//!   start into a real freeze.
//! - A crash (a Rust panic, which in a release build ends the app on the
//!   spot) is logged with its message and code location, and a snapshot of
//!   that moment is saved as `crash-<time>.dmp` (last 3 kept) before the
//!   app closes (`install_crash_handler`).
//! - `TOASTED_TEST_FREEZE=<seconds>` in the environment holds the main
//!   thread that long at startup, to try the watchdog out;
//!   `TOASTED_TEST_CRASH=1` crashes it at startup, to try the crash handler.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::AppHandle;

const LOG_NAME: &str = "toasted-log.txt";
const OLD_NAME: &str = "toasted-log.old.txt";
const MAX_BYTES: u64 = 512 * 1024;
/// Lines held before the folder is known (more are dropped).
const MAX_PENDING: usize = 2000;
/// How long the main thread may stay quiet before it counts as stuck.
const STUCK_AFTER: Duration = Duration::from_secs(10);
const DUMPS_KEPT: usize = 3;
/// The app data folder's name under %APPDATA% (tauri.conf.json's
/// identifier), for a crash before Tauri has said where it is.
const APP_ID: &str = "com.vulkan.roon-toasted";

struct Log {
    dir: Option<PathBuf>,
    file: Option<File>,
    size: u64,
    pending: Vec<String>,
}

static LOG: Mutex<Log> = Mutex::new(Log { dir: None, file: None, size: 0, pending: Vec::new() });
/// What the main thread is doing, as far as the steps say.
static STEP: Mutex<&'static str> = Mutex::new("starting");
static STARTED: OnceLock<Instant> = OnceLock::new();
/// When the main thread last answered (ms since `STARTED`).
static BEAT: AtomicU64 = AtomicU64::new(0);
static DUMPED: AtomicBool = AtomicBool::new(false);

fn since_start() -> u64 {
    STARTED.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// "2026-11-09 16:37:05.123", local time where the system says it.
fn stamp() -> String {
    #[cfg(windows)]
    {
        // SAFETY: fills a plain struct.
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
        )
    }
    #[cfg(not(windows))]
    {
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let secs = (ms / 1000) as u64;
        let day = secs % 86_400;
        format!("{:02}:{:02}:{:02}.{:03} UTC", day / 3600, (day % 3600) / 60, day % 60, ms % 1000)
    }
}

/// One line into the file (or held until the folder is known).
fn write_line(text: &str) {
    let line = format!("{} {}\n", stamp(), text.trim_end());
    let Ok(mut log) = LOG.lock() else {
        return;
    };
    if log.dir.is_none() {
        if log.pending.len() < MAX_PENDING {
            log.pending.push(line);
        }
        return;
    }
    if log.size + line.len() as u64 > MAX_BYTES {
        rotate(&mut log);
    }
    if let Some(file) = log.file.as_mut() {
        // (written straight through, so a freeze or a kill loses nothing)
        if file.write_all(line.as_bytes()).is_ok() {
            log.size += line.len() as u64;
        }
    }
}

/// `write_line` for the crash handler: never waits for the lock (the crash
/// may have happened while this very thread held it), and before the
/// folder is known it uses %APPDATA%'s, so the lines land before the app
/// closes.
fn write_line_now(text: &str) {
    let line = format!("{} {}\n", stamp(), text.trim_end());
    let Some(mut log) = lock_soon() else {
        std::eprintln!("{line}");
        return;
    };
    if log.dir.is_none() {
        let Some(dir) = std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join(APP_ID)) else {
            return;
        };
        let _ = fs::create_dir_all(&dir);
        log.file = OpenOptions::new().create(true).append(true).open(dir.join(LOG_NAME)).ok();
        log.dir = Some(dir);
        let pending = std::mem::take(&mut log.pending);
        if let Some(file) = log.file.as_mut() {
            for held in pending {
                let _ = file.write_all(held.as_bytes());
            }
        }
    }
    if let Some(file) = log.file.as_mut() {
        let _ = file.write_all(line.as_bytes());
    }
}

/// The folder the log is in, once known.
fn log_dir() -> Option<PathBuf> {
    lock_soon().and_then(|log| log.dir.clone())
}

/// The log's lock if it comes free within a moment (another thread writing
/// a line), else None (this thread held it when it crashed).
fn lock_soon() -> Option<std::sync::MutexGuard<'static, Log>> {
    for _ in 0..50 {
        match LOG.try_lock() {
            Ok(log) => return Some(log),
            Err(std::sync::TryLockError::Poisoned(p)) => return Some(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    None
}

/// Logs a crash and saves a snapshot of that moment before the app closes.
/// Installed first thing in `run`, keeping the standard handler (which
/// prints the message in a dev build's terminal).
pub fn install_crash_handler() {
    let standard = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed").to_string();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".into());
        let place = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_else(|| "?".into());
        write_line_now(&format!("[crash] thread '{name}' crashed at {place}: {message}"));
        if let Some(dir) = log_dir() {
            match save_snapshot(&dir, "crash") {
                Ok(note) => write_line_now(&format!("[crash] {note}")),
                Err(e) => write_line_now(&format!("[crash] snapshot failed: {e}")),
            }
        }
        write_line_now("[crash] Toasted closes now");
        standard(info);
    }));
}

/// Crashes the app at startup if `TOASTED_TEST_CRASH` is set.
pub fn test_crash() {
    if std::env::var_os("TOASTED_TEST_CRASH").is_some() {
        step("test crash (TOASTED_TEST_CRASH)", || panic!("a test crash (TOASTED_TEST_CRASH is set)"));
    }
}

/// The current file becomes the old one; a new one starts.
fn rotate(log: &mut Log) {
    let Some(dir) = log.dir.clone() else {
        return;
    };
    log.file = None;
    let _ = fs::rename(dir.join(LOG_NAME), dir.join(OLD_NAME));
    log.file = OpenOptions::new().create(true).write(true).truncate(true).open(dir.join(LOG_NAME)).ok();
    log.size = 0;
}

/// The `println!` / `eprintln!` stand-ins (see lib.rs): the terminal, as
/// before, and the file.
pub fn print(error: bool, args: std::fmt::Arguments) {
    let text = args.to_string();
    if error {
        std::eprintln!("{text}");
    } else {
        std::println!("{text}");
    }
    write_line(&text);
}

/// The libraries' warnings (tracing), into the file.
pub struct TracingWriter;

impl Write for TracingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            write_line(&format!("[lib] {line}"));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Opens the file in the app data folder (once), writes what came before,
/// and heads this run.
pub fn init(app: &AppHandle) {
    use tauri::Manager;
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    let _ = fs::create_dir_all(&dir);
    {
        let Ok(mut log) = LOG.lock() else {
            return;
        };
        if log.dir.is_some() {
            return;
        }
        let path = dir.join(LOG_NAME);
        // (a big file from last time becomes the old one; otherwise this
        // run carries on in it, so a freeze's run and the next are together)
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        log.dir = Some(dir.clone());
        if size > MAX_BYTES / 2 {
            rotate(&mut log);
        } else {
            log.file = OpenOptions::new().create(true).append(true).open(&path).ok();
            log.size = size;
        }
        let pending = std::mem::take(&mut log.pending);
        for line in pending {
            if let Some(file) = log.file.as_mut() {
                if file.write_all(line.as_bytes()).is_ok() {
                    log.size += line.len() as u64;
                }
            }
        }
    }
    let autostart = std::env::args().any(|a| a == "--autostart");
    write_line("");
    write_line(&format!(
        "===== Roon: Toasted {} (build {}) started{}; Windows up for {} =====",
        app.package_info().version,
        crate::discover::BUILD,
        if autostart { " with Windows" } else { "" },
        uptime()
    ));
}

/// How long Windows has been running, "3 min 12 s".
fn uptime() -> String {
    #[cfg(windows)]
    {
        // SAFETY: no arguments.
        let ms = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
        let secs = ms / 1000;
        if secs >= 3600 {
            format!("{} h {} min", secs / 3600, (secs % 3600) / 60)
        } else {
            format!("{} min {} s", secs / 60, secs % 60)
        }
    }
    #[cfg(not(windows))]
    {
        "unknown".into()
    }
}

/// Runs one named startup step on the main thread, logged with how long it
/// took. The watchdog names it if the main thread gets stuck inside.
pub fn step<R>(name: &'static str, f: impl FnOnce() -> R) -> R {
    if let Ok(mut current) = STEP.lock() {
        *current = name;
    }
    let started = Instant::now();
    let result = f();
    let took = started.elapsed().as_millis();
    // (quick steps in one short line; slow ones stand out)
    if took >= 500 {
        write_line(&format!("[start] {name}: {took} ms (slow)"));
    } else {
        write_line(&format!("[start] {name}: {took} ms"));
    }
    result
}

/// Startup is over: the main thread is running the app's events.
pub fn startup_done() {
    if let Ok(mut current) = STEP.lock() {
        *current = "the event loop (after startup)";
    }
    write_line(&format!("[start] done in {} ms", since_start()));
}

/// Holds the main thread for `TOASTED_TEST_FREEZE` seconds, if set.
pub fn test_freeze() {
    let Some(secs) = std::env::var("TOASTED_TEST_FREEZE").ok().and_then(|v| v.trim().parse::<u64>().ok()) else {
        return;
    };
    let secs = secs.min(120);
    step("test freeze (TOASTED_TEST_FREEZE)", || std::thread::sleep(Duration::from_secs(secs)));
}

/// The watchdog (see the top of this file).
pub fn start_watchdog(app: &AppHandle) {
    let _ = since_start();
    BEAT.store(since_start(), Ordering::Relaxed);
    let app = app.clone();
    let _ = std::thread::Builder::new().name("watchdog".into()).spawn(move || {
        let mut stuck_since: Option<u64> = None;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            // (queued for the main thread; it sets the beat when it gets to it)
            let _ = app.run_on_main_thread(|| BEAT.store(since_start(), Ordering::Relaxed));
            let now = since_start();
            let quiet = now.saturating_sub(BEAT.load(Ordering::Relaxed));
            match stuck_since {
                None if quiet >= STUCK_AFTER.as_millis() as u64 => {
                    stuck_since = Some(now - quiet);
                    let step = STEP.lock().map(|s| *s).unwrap_or("?");
                    write_line(&format!("[watchdog] the main thread hasn't answered for {} s; last step: {step}", quiet / 1000));
                    if !DUMPED.swap(true, Ordering::Relaxed) {
                        snapshot(&app);
                    }
                }
                Some(since) if quiet < 2000 => {
                    write_line(&format!("[watchdog] the main thread answered again after {} s", (now - since) / 1000));
                    stuck_since = None;
                }
                _ => {}
            }
        }
    });
}

/// Has a second copy of the app save the hang snapshot, and logs how it went.
fn snapshot(app: &AppHandle) {
    use tauri::Manager;
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    match save_snapshot(&dir, "hang") {
        Ok(note) => write_line(&format!("[watchdog] {note}")),
        Err(e) => write_line(&format!("[watchdog] snapshot failed: {e}")),
    }
}

/// Has a second copy of the app write `<kind>-<time>.dmp` of this one into
/// `dir` (waiting up to a minute), and keeps the newest few of that kind.
fn save_snapshot(dir: &Path, kind: &str) -> Result<String, String> {
    let name = format!("{kind}-{}.dmp", stamp().replace([' ', ':'], "-").replace('.', "-"));
    let path = dir.join(&name);
    let started = Instant::now();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new(exe)
        .arg("--hang-dump")
        .arg(std::process::id().to_string())
        .arg(&path)
        .spawn()
        .map_err(|e| format!("couldn't start the helper ({e})"))?;
    // (it takes a second or two; given up on after a minute)
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < Duration::from_secs(60) => std::thread::sleep(Duration::from_millis(200)),
            _ => {
                let _ = child.kill();
                break None;
            }
        }
    };
    match status {
        Some(status) if status.success() && path.exists() => {
            let size = fs::metadata(&path).map(|m| m.len() / 1024).unwrap_or(0);
            prune_dumps(dir, kind);
            Ok(format!("snapshot saved: {name} ({size} KB, {} ms)", started.elapsed().as_millis()))
        }
        Some(status) => Err(format!("helper exit code {:?}", status.code())),
        None => Err("the helper didn't finish within a minute".into()),
    }
}

/// `--hang-dump <pid> <file>`: this copy of the app only writes a snapshot
/// of the other one and quits (called first thing in `run`). Returns the
/// exit code, or None when it isn't that.
pub fn dump_mode() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    let at = args.iter().position(|a| a == "--hang-dump")?;
    let pid = args.get(at + 1)?.parse::<u32>().ok()?;
    let path = PathBuf::from(args.get(at + 2)?);
    Some(match write_dump(pid, &path) {
        Ok(()) => 0,
        Err(e) => {
            std::eprintln!("snapshot failed: {e}");
            1
        }
    })
}

/// Only the newest few snapshots of a kind ("hang", "crash") stay.
fn prune_dumps(dir: &Path, kind: &str) {
    let prefix = format!("{kind}-");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut dumps: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix) && e.file_name().to_string_lossy().ends_with(".dmp"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    dumps.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, old) in dumps.into_iter().skip(DUMPS_KEPT) {
        let _ = fs::remove_file(old);
    }
}

/// Writes process `pid`'s minidump to `path`.
#[cfg(windows)]
fn write_dump(pid: u32, path: &Path) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Diagnostics::Debug::{MiniDumpWithThreadInfo, MiniDumpWithUnloadedModules, MiniDumpWriteDump, MINIDUMP_TYPE};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};
    // SAFETY: opening a process by id; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) }.map_err(|e| e.to_string())?;
    let result = File::create(path).map_err(|e| e.to_string()).and_then(|file| {
        // Every thread's stack and registers, the loaded modules, thread times
        let kind = MINIDUMP_TYPE(MiniDumpWithThreadInfo.0 | MiniDumpWithUnloadedModules.0);
        // SAFETY: a valid process handle, its id, and an open file's handle.
        unsafe { MiniDumpWriteDump(process, pid, HANDLE(file.as_raw_handle()), kind, None, None, None) }.map_err(|e| e.to_string())
    });
    // SAFETY: the handle from OpenProcess.
    let _ = unsafe { CloseHandle(process) };
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

#[cfg(not(windows))]
fn write_dump(_pid: u32, _path: &Path) -> Result<(), String> {
    Err("snapshots are Windows only".into())
}
