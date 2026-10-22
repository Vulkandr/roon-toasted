// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Ollama for tier 2, set up from Wrapped's Settings without a terminal.
//! The page shows three steps, each ticked off once it's done:
//! 1. Install: is Ollama answering? If not, either start it (installed) or
//!    install it: download Ollama's installer from ollama.com, check its
//!    signature, point Ollama's models folder at the drive the user picked,
//!    and run the installer with no questions (it shows its own small
//!    progress window). Ollama installs for the user alone, no admin needed.
//! 2. A model: the graphics card's memory decides which size is suggested;
//!    the download goes through Ollama itself (its pull API).
//! 3. Test: judge.rs's Test.
//! Progress goes to the page as "ollama-setup" events. One job at a time.
//!
//! Who runs Ollama: by default Wrapped does (`ollama_manual` off). A job
//! that needs Ollama (a build's tier 2, Test, a model download) takes a
//! `Lease`; when Ollama isn't answering, Wrapped starts its server hidden
//! (`ollama.exe serve`: no window, no tray icon, no account screen) and
//! closes it a minute after the last lease ends, so the graphics card's
//! memory comes back. An Ollama Wrapped didn't start (its app, opened by
//! hand) is used and left running. Ollama's app puts itself in Windows'
//! startup; Wrapped moves that shortcut aside while it runs Ollama, and
//! back when "Leave Ollama to me" is ticked.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::wrapped::WrappedSettings;

const INSTALLER_URL: &str = "https://ollama.com/download/OllamaSetup.exe";
/// Where Windows keeps Ollama's install details (its installer's AppId).
const UNINSTALL_KEY: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{44E83376-CE68-45EB-8FC1-393500EB558C}_is1";
pub const DEFAULT_URL: &str = "http://127.0.0.1:11434";
/// Ollama's program needs about 4 GB; a model comes on top.
const INSTALL_NEEDS_GB: f64 = 6.0;
/// How long the graphics card and drive details are kept before asking
/// Windows again.
const INFO_KEEP: Duration = Duration::from_secs(60);

/// The sizes offered, biggest first: name, download in GB, graphics memory
/// it wants in GB (with room for the prompt), and a word for the page.
const SIZES: [(&str, f64, f64, &str); 4] = [
    ("gemma4:26b", 18.0, 22.0, "biggest"),
    ("gemma4:12b", 8.0, 10.0, "middle"),
    ("gemma4:e4b", 6.6, 7.0, "small"),
    ("gemma4:e2b", 4.6, 0.0, "smallest"),
];

static BUSY: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
static PROGRESS: Mutex<Option<Progress>> = Mutex::new(None);
static INFO: Mutex<Option<(Instant, SysInfo)>> = Mutex::new(None);
static ASKING: AtomicBool = AtomicBool::new(false);
static READING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const STOPPED: &str = "Stopped.";

/// What the page shows while a job runs, and how it ended.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// "install" or "pull"
    job: String,
    /// download, check, install, start, pull, then done, stopped or error
    stage: String,
    label: String,
    done: u64,
    total: u64,
}

/// Everything the page's three steps need.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaStatus {
    url: String,
    /// The address is this PC, so installing and the graphics card count.
    local: bool,
    running: bool,
    version: Option<String>,
    installed: bool,
    install_dir: Option<String>,
    /// OLLAMA_MODELS for this Windows user, when it's set.
    models_dir: Option<String>,
    models: Vec<HaveModel>,
    /// The model tier 2 uses (the saved one, or the default).
    model: String,
    model_ready: bool,
    gpu: Option<Gpu>,
    drives: Vec<Drive>,
    sizes: Vec<SizeChoice>,
    suggested: String,
    /// A job running now (an install or a download).
    job: Option<Progress>,
    /// Still asking Windows about the graphics card, drives and install.
    checking: bool,
    /// "Leave Ollama to me": Wrapped doesn't start or stop it.
    manual: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HaveModel {
    name: String,
    gb: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    name: String,
    gb: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Drive {
    letter: String,
    free_gb: f64,
    label: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeChoice {
    name: String,
    download_gb: f64,
    word: String,
    /// Fits this PC's graphics card.
    fits: bool,
}

/// What Windows says about this PC (asked through PowerShell).
#[derive(Debug, Clone, Default)]
struct SysInfo {
    gpus: Vec<Gpu>,
    drives: Vec<Drive>,
    install_dir: Option<String>,
    models_dir: Option<String>,
}

/// The raw answer of `SYSINFO_SCRIPT`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawInfo {
    gpus: serde_json::Value,
    drives: serde_json::Value,
    install: String,
    models: String,
}

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Graphics cards and their memory (Windows' display adapter entries;
/// qwMemorySize is exact past 4 GB), local drives with their free space,
/// where Ollama is installed, and OLLAMA_MODELS.
const SYSINFO_SCRIPT: &str = r#"
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$ErrorActionPreference = 'SilentlyContinue'
$gpus = @(Get-ChildItem -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}' | ForEach-Object {
  $p = Get-ItemProperty -LiteralPath $_.PSPath
  if ($p.DriverDesc) {
    $b = $p.'HardwareInformation.qwMemorySize'
    if (-not $b) {
      $m = $p.'HardwareInformation.MemorySize'
      if ($m -is [byte[]]) { $b = [BitConverter]::ToUInt32($m, 0) } else { $b = $m }
    }
    [pscustomobject]@{ name = [string]$p.DriverDesc; bytes = [double]$b }
  }
})
$drives = @(Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=3' | ForEach-Object {
  [pscustomobject]@{ letter = [string]$_.DeviceID; free = [double]$_.FreeSpace; label = [string]$_.VolumeName }
})
$u = Get-ItemProperty -LiteralPath '__UNINSTALL_KEY__'
[pscustomobject]@{
  gpus = $gpus
  drives = $drives
  install = [string]$u.InstallLocation
  models = [string][Environment]::GetEnvironmentVariable('OLLAMA_MODELS', 'User')
} | ConvertTo-Json -Compress -Depth 4
"#;

/// A PowerShell script's output (run hidden; the script travels encoded, so
/// no quoting can trip it up).
fn powershell(script: &str) -> Result<String, String> {
    let wide: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &base64(&wide)]);
    hide_console(&mut cmd);
    let out = cmd.output().map_err(|e| format!("PowerShell: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn powershell_async(script: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || powershell(&script)).await.map_err(|e| e.to_string())?
}

#[cfg(windows)]
fn hide_console(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_cmd: &mut std::process::Command) {}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ABC[(n >> 18) as usize & 63] as char);
        out.push(ABC[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ABC[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ABC[n as usize & 63] as char } else { '=' });
    }
    out
}

/// A string for inside single quotes in PowerShell.
fn ps_quote(s: &str) -> String {
    s.replace('\'', "''")
}

/// PowerShell gives one item as an object and several as a list.
fn items(v: &serde_json::Value) -> Vec<serde_json::Value> {
    match v {
        serde_json::Value::Array(list) => list.clone(),
        serde_json::Value::Object(_) => vec![v.clone()],
        _ => Vec::new(),
    }
}

fn parse_info(text: &str) -> SysInfo {
    let raw: RawInfo = serde_json::from_str(text).unwrap_or_default();
    let round = |gb: f64| (gb * 10.0).round() / 10.0;
    let gpus = items(&raw.gpus)
        .iter()
        .filter_map(|g| {
            let name = g["name"].as_str()?.trim().to_string();
            let bytes = g["bytes"].as_f64().unwrap_or(0.0);
            (!name.is_empty()).then(|| Gpu { name, gb: round(bytes / GB) })
        })
        .collect();
    let mut drives: Vec<Drive> = items(&raw.drives)
        .iter()
        .filter_map(|d| {
            let letter = d["letter"].as_str()?.trim().to_string();
            (!letter.is_empty()).then(|| Drive {
                letter,
                free_gb: round(d["free"].as_f64().unwrap_or(0.0) / GB),
                label: d["label"].as_str().unwrap_or("").trim().to_string(),
            })
        })
        .collect();
    drives.sort_by(|a, b| a.letter.cmp(&b.letter));
    let some = |s: String| {
        let s = s.trim().trim_end_matches('\\').to_string();
        (!s.is_empty()).then_some(s)
    };
    SysInfo { gpus, drives, install_dir: some(raw.install), models_dir: some(raw.models) }
}

/// What Windows said last; past `INFO_KEEP` it's still used while it's
/// asked again in the background, so only the first time (or `fresh`)
/// waits for PowerShell.
async fn sysinfo(fresh: bool) -> SysInfo {
    let cached = INFO.lock().unwrap().clone();
    match cached {
        Some((at, info)) if !fresh => {
            if at.elapsed() >= INFO_KEEP && !ASKING.swap(true, Ordering::SeqCst) {
                tauri::async_runtime::spawn(async {
                    read_sysinfo().await;
                    ASKING.store(false, Ordering::SeqCst);
                });
            }
            info
        }
        _ => read_sysinfo().await,
    }
}

/// What Windows said, without waiting: None the first time. Missing or
/// old, it's asked again in the background and the page is told
/// ("ollama-info") when the answer is in.
fn sysinfo_soon(app: &AppHandle) -> Option<SysInfo> {
    let cached = INFO.lock().unwrap().clone();
    let old = cached.as_ref().map_or(true, |(at, _)| at.elapsed() >= INFO_KEEP);
    if old && !ASKING.swap(true, Ordering::SeqCst) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            read_sysinfo().await;
            ASKING.store(false, Ordering::SeqCst);
            let _ = app.emit("ollama-info", ());
        });
    }
    cached.map(|(_, info)| info)
}

async fn read_sysinfo() -> SysInfo {
    let asked = Instant::now();
    // (one PowerShell at a time; an answer that came in while this one
    // waited its turn will do)
    let _turn = READING.lock().await;
    if let Some((at, info)) = INFO.lock().unwrap().clone() {
        if at >= asked {
            return info;
        }
    }
    let script = SYSINFO_SCRIPT.replace("__UNINSTALL_KEY__", &ps_quote(UNINSTALL_KEY));
    let info = match powershell_async(script).await {
        Ok(text) => parse_info(&text),
        Err(_) => SysInfo::default(),
    };
    *INFO.lock().unwrap() = Some((Instant::now(), info.clone()));
    info
}

/// The address from the settings, or this PC's.
/// "localhost" is asked as 127.0.0.1, where Ollama listens: Windows tries
/// localhost's IPv6 address first and takes its time being refused.
pub fn address(settings: &WrappedSettings) -> String {
    let url = settings.llm_url.trim().trim_end_matches('/');
    if url.is_empty() {
        DEFAULT_URL.to_string()
    } else {
        url.replacen("://localhost", "://127.0.0.1", 1)
    }
}

/// Host and port of an address ("http://127.0.0.1:11434" -> 127.0.0.1, 11434).
fn host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let hostport = rest.split(['/', '?']).next()?;
    let default_port = if scheme.eq_ignore_ascii_case("https") { 443 } else { 80 };
    if let Some(v6) = hostport.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = after.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default_port);
        return Some((host.to_string(), port));
    }
    match hostport.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((hostport.to_string(), default_port)),
    }
}

fn is_local(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split(['/', '?']).next().unwrap_or("");
    let host = if host.starts_with('[') { host.split(']').next().unwrap_or("").trim_start_matches('[') } else { host.split(':').next().unwrap_or("") };
    matches!(host.to_lowercase().as_str(), "localhost" | "127.0.0.1" | "::1" | "0.0.0.0")
}

/// What to send as `think` for a model so it thinks as little as it can:
/// false when it can turn thinking off, its lowest level when it only has
/// levels, nothing for a model that doesn't think (or when Ollama can't
/// say). Asked once per address and model (`/api/show`).
pub async fn least_thinking(url: &str, model: &str) -> Option<serde_json::Value> {
    static KNOWN: Mutex<Option<std::collections::HashMap<String, Option<serde_json::Value>>>> = Mutex::new(None);
    let key = format!("{url} {model}");
    if let Some(known) = KNOWN.lock().unwrap().as_ref().and_then(|m| m.get(&key).cloned()) {
        return known;
    }
    let shown: serde_json::Value = quick_client()
        .post(format!("{url}/api/show"))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let setting = thinking_setting(&shown);
    KNOWN.lock().unwrap().get_or_insert_with(Default::default).insert(key, setting.clone());
    setting
}

fn thinking_setting(shown: &serde_json::Value) -> Option<serde_json::Value> {
    if let Some(values) = shown["thinking"]["values"].as_array() {
        if values.iter().any(|v| v == &serde_json::Value::Bool(false)) {
            return Some(serde_json::Value::Bool(false));
        }
        if let Some(low) = values.iter().find(|v| v.as_str() == Some("low")) {
            return Some(low.clone());
        }
        return None;
    }
    let thinks = shown["capabilities"].as_array().is_some_and(|c| c.iter().any(|v| v.as_str() == Some("thinking")));
    thinks.then_some(serde_json::Value::Bool(false))
}

fn quick_client() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(3)).build().unwrap_or_default()
}

/// Ollama's version when it answers at `url`.
pub async fn version(url: &str) -> Option<String> {
    // On this PC, a quick knock first: Windows takes a couple of seconds to
    // give up on a port nothing listens on, while a running Ollama answers
    // at once
    if is_local(url) {
        let (host, port) = host_port(url)?;
        let knock = tokio::net::TcpStream::connect((host.as_str(), port));
        tokio::time::timeout(Duration::from_millis(300), knock).await.ok()?.ok()?;
    }
    let v: serde_json::Value = quick_client().get(format!("{url}/api/version")).send().await.ok()?.json().await.ok()?;
    v["version"].as_str().map(str::to_string)
}

async fn have_models(url: &str) -> Vec<HaveModel> {
    let Ok(response) = quick_client().get(format!("{url}/api/tags")).send().await else {
        return Vec::new();
    };
    let v: serde_json::Value = response.json().await.unwrap_or_default();
    let mut list: Vec<HaveModel> = v["models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|m| {
                    Some(HaveModel {
                        name: m["name"].as_str()?.to_string(),
                        gb: (m["size"].as_f64().unwrap_or(0.0) / GB * 10.0).round() / 10.0,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

/// "llama3.1" and "llama3.1:latest" are the same model to Ollama.
fn same_model(have: &str, want: &str) -> bool {
    let norm = |s: &str| {
        let s = s.trim().to_lowercase();
        if s.contains(':') {
            s
        } else {
            format!("{s}:latest")
        }
    };
    norm(have) == norm(want)
}

/// The installed Ollama's folder: where its installer says, or the
/// default, when the program is there.
fn installed_dir(info: &SysInfo) -> Option<String> {
    let mut places: Vec<String> = info.install_dir.iter().cloned().collect();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        places.push(format!("{local}\\Programs\\Ollama"));
    }
    places.into_iter().find(|dir| std::path::Path::new(&format!("{dir}\\ollama app.exe")).exists() || std::path::Path::new(&format!("{dir}\\ollama.exe")).exists())
}

/// The size to suggest for this much graphics memory.
fn suggest(gb: Option<f64>) -> &'static str {
    let gb = gb.unwrap_or(0.0);
    SIZES.iter().find(|s| gb >= s.2).map(|s| s.0).unwrap_or(SIZES[SIZES.len() - 1].0)
}

#[tauri::command]
pub async fn ollama_status(app: AppHandle, fresh: Option<bool>) -> OllamaStatus {
    let settings = WrappedSettings::load(&app);
    let url = address(&settings);
    let local = is_local(&url);
    let version = version(&url).await;
    // (Windows' details without waiting for them, unless asked for fresh:
    // the first time they're asked in the background and the page hears
    // "ollama-info" when they're in)
    let (info, checking) = if !local {
        (SysInfo::default(), false)
    } else if fresh.unwrap_or(false) {
        (sysinfo(true).await, false)
    } else {
        match sysinfo_soon(&app) {
            Some(info) => (info, false),
            None => (SysInfo::default(), true),
        }
    };
    let gpu = info.gpus.iter().max_by(|a, b| a.gb.total_cmp(&b.gb)).cloned().filter(|g| g.gb > 0.0);
    let gpu_gb = gpu.as_ref().map(|g| g.gb);
    let install_dir = if local { installed_dir(&info) } else { None };
    // (the models Ollama has: from Ollama when it runs, else from its
    // folder, so the page knows them while Wrapped keeps Ollama closed)
    let models = if version.is_some() {
        have_models(&url).await
    } else if local {
        models_on_disk(info.models_dir.as_deref())
    } else {
        Vec::new()
    };
    let model = if settings.llm_model.trim().is_empty() { crate::judge::default_model("ollama").to_string() } else { settings.llm_model.trim().to_string() };
    let model_ready = models.iter().any(|m| same_model(&m.name, &model));
    let sizes = SIZES
        .iter()
        .map(|s| SizeChoice { name: s.0.to_string(), download_gb: s.1, word: s.3.to_string(), fits: gpu_gb.is_some_and(|gb| gb >= s.2) })
        .collect();
    let job = BUSY.load(Ordering::SeqCst).then(|| PROGRESS.lock().unwrap().clone()).flatten();
    OllamaStatus {
        url,
        local,
        running: version.is_some(),
        version,
        installed: install_dir.is_some(),
        install_dir,
        models_dir: info.models_dir.clone(),
        models,
        model,
        model_ready,
        suggested: if local { suggest(gpu_gb) } else { "gemma4:12b" }.to_string(),
        gpu,
        drives: info.drives,
        sizes,
        job,
        checking,
        manual: settings.ollama_manual,
    }
}

/// The models in Ollama's folder (`OLLAMA_MODELS`, else the default under
/// the user's folder): one manifest per model and tag, under
/// manifests/<registry>/<namespace>/<model>/<tag>, listing its layers.
fn models_on_disk(models_dir: Option<&str>) -> Vec<HaveModel> {
    let dir = match models_dir {
        Some(d) => std::path::PathBuf::from(d),
        None => match std::env::var("USERPROFILE") {
            Ok(home) => std::path::Path::new(&home).join(".ollama").join("models"),
            Err(_) => return Vec::new(),
        },
    };
    let names = |p: &std::path::Path| -> Vec<(String, std::path::PathBuf)> {
        std::fs::read_dir(p)
            .map(|entries| entries.flatten().map(|e| (e.file_name().to_string_lossy().into_owned(), e.path())).collect())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for (registry, registry_path) in names(&dir.join("manifests")) {
        for (namespace, namespace_path) in names(&registry_path) {
            for (model, model_path) in names(&namespace_path) {
                for (tag, manifest) in names(&model_path) {
                    if !manifest.is_file() {
                        continue;
                    }
                    let name = if registry == "registry.ollama.ai" && namespace == "library" {
                        model.clone()
                    } else if registry == "registry.ollama.ai" {
                        format!("{namespace}/{model}")
                    } else {
                        format!("{registry}/{namespace}/{model}")
                    };
                    let v: serde_json::Value = std::fs::read(&manifest).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
                    let bytes: f64 = v["layers"].as_array().map(|l| l.iter().filter_map(|x| x["size"].as_f64()).sum()).unwrap_or(0.0);
                    out.push(HaveModel { name: format!("{name}:{tag}"), gb: (bytes / GB * 10.0).round() / 10.0 });
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Holds the one-job-at-a-time spot until dropped.
struct Busy;

impl Busy {
    fn take() -> Result<Busy, String> {
        if BUSY.swap(true, Ordering::SeqCst) {
            return Err("Something is already being set up. Wait for it to finish.".into());
        }
        CANCEL.store(false, Ordering::SeqCst);
        Ok(Busy)
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

fn report(app: &AppHandle, job: &str, stage: &str, label: impl Into<String>, done: u64, total: u64) {
    let progress = Progress { job: job.into(), stage: stage.into(), label: label.into(), done, total };
    *PROGRESS.lock().unwrap() = Some(progress.clone());
    let _ = app.emit("ollama-setup", progress);
}

/// The end of a job, for the page: done, stopped, or the error.
fn finish(app: &AppHandle, job: &str, result: &Result<(), String>) {
    *INFO.lock().unwrap() = None; // (free space changed)
    match result {
        Ok(()) => report(app, job, "done", "", 0, 0),
        Err(e) if e == STOPPED => report(app, job, "stopped", STOPPED, 0, 0),
        Err(e) => report(app, job, "error", e.clone(), 0, 0),
    }
}

/// Stops the download that's running (the installer, or a model).
#[tauri::command]
pub fn ollama_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Downloads and runs Ollama's installer, Ollama and its models going to
/// `drive` (like "E:"), then waits for Ollama to answer.
#[tauri::command]
pub async fn ollama_install(app: AppHandle, drive: String) -> Result<(), String> {
    let _busy = Busy::take()?;
    let result = install(&app, &drive).await;
    finish(&app, "install", &result);
    result
}

async fn install(app: &AppHandle, drive: &str) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("Installing Ollama from here works on Windows only.".into());
    }
    let url = address(&WrappedSettings::load(app));
    if !is_local(&url) {
        return Err(format!("The address is another computer ({url}); install Ollama there."));
    }
    let info = sysinfo(true).await;
    let chosen = info.drives.iter().find(|d| d.letter.eq_ignore_ascii_case(drive.trim())).ok_or("That drive isn't there anymore.")?;
    if chosen.free_gb < INSTALL_NEEDS_GB {
        return Err(format!("{} has only {:.1} GB free. Ollama needs about 4 GB, plus room for a model.", chosen.letter, chosen.free_gb));
    }
    let base = format!("{}\\Ollama", chosen.letter);
    let app_dir = format!("{base}\\app");
    // (an OLLAMA_MODELS the user set already stays as it is)
    let models_dir = info.models_dir.clone().unwrap_or_else(|| format!("{base}\\models"));
    std::fs::create_dir_all(&models_dir).map_err(|e| format!("Couldn't make {models_dir}: {e}"))?;

    let setup = std::env::temp_dir().join("roon-toasted-OllamaSetup.exe");
    let result = async {
        download(app, &setup).await?;
        report(app, "install", "check", "Checking the installer's signature", 0, 0);
        let signer = check_signature(&setup).await?;
        if info.models_dir.is_none() {
            let script = format!("[Environment]::SetEnvironmentVariable('OLLAMA_MODELS', '{}', 'User')", ps_quote(&models_dir));
            powershell_async(script).await?;
        }
        report(app, "install", "install", format!("Installing Ollama (signed by {signer})"), 0, 0);
        let (exe, dir, models) = (setup.clone(), app_dir.clone(), models_dir.clone());
        let ran = tauri::async_runtime::spawn_blocking(move || {
            let mut cmd = std::process::Command::new(&exe);
            cmd.args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", &format!("/DIR={dir}")])
                // (the installer starts Ollama at the end, which then finds
                // its models folder here)
                .env("OLLAMA_MODELS", &models);
            detach(&mut cmd);
            cmd.status()
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("Couldn't run Ollama's installer: {e}"))?;
        match ran.code() {
            Some(0) => {}
            Some(2) | Some(5) => return Err("The install was cancelled.".into()),
            Some(code) => return Err(format!("Ollama's installer stopped with an error (code {code}).")),
            None => return Err("Ollama's installer stopped.".into()),
        }
        report(app, "install", "start", "Starting Ollama", 0, 0);
        if WrappedSettings::load(app).ollama_manual {
            return wait_until_up(&url, Some((app_dir.as_str(), models_dir.as_str())), Duration::from_secs(90)).await;
        }
        // Wrapped runs Ollama: the app the installer opened (and its
        // account screen) closes again once it's up, and leaves Windows'
        // startup; Wrapped starts the server when it's needed
        let _ = wait_until_up(&url, None, Duration::from_secs(30)).await;
        close_ollama_apps();
        hide_from_startup(app);
        Ok(())
    }
    .await;
    let _ = std::fs::remove_file(&setup);
    result
}

/// The installer from ollama.com, with progress.
async fn download(app: &AppHandle, to: &std::path::Path) -> Result<(), String> {
    report(app, "install", "download", "Downloading Ollama", 0, 0);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .user_agent("roon-toasted")
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = client
        .get(INSTALLER_URL)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Couldn't download Ollama: {e}"))?;
    let total = response.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(to).map_err(|e| format!("Couldn't save the installer: {e}"))?;
    let (mut done, mut shown) = (0u64, Instant::now());
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            return Err(STOPPED.into());
        }
        match response.chunk().await {
            Ok(Some(chunk)) => {
                file.write_all(&chunk).map_err(|e| format!("Couldn't save the installer: {e}"))?;
                done += chunk.len() as u64;
                if shown.elapsed() >= Duration::from_millis(200) {
                    shown = Instant::now();
                    report(app, "install", "download", "Downloading Ollama", done, total);
                }
            }
            Ok(None) => break,
            Err(e) => return Err(format!("The download stopped: {e}")),
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    if total > 0 && done < total {
        return Err("The download stopped before it finished.".into());
    }
    report(app, "install", "download", "Downloading Ollama", done, done);
    Ok(())
}

/// The installer runs only with a valid signature; the signer's name comes
/// back for the page.
async fn check_signature(file: &std::path::Path) -> Result<String, String> {
    let script = format!(
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8; $s = Get-AuthenticodeSignature -LiteralPath '{}'; [pscustomobject]@{{ status = [string]$s.Status; subject = [string]$s.SignerCertificate.Subject }} | ConvertTo-Json -Compress",
        ps_quote(&file.to_string_lossy())
    );
    let text = powershell_async(script).await?;
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    let status = v["status"].as_str().unwrap_or("unknown");
    if status != "Valid" {
        return Err(format!("The installer's signature didn't check out ({status}), so it wasn't run."));
    }
    let subject = v["subject"].as_str().unwrap_or("");
    Ok(signer_name(subject))
}

/// "CN=Ollama Inc., O=Ollama Inc., C=US" -> "Ollama Inc."
fn signer_name(subject: &str) -> String {
    let Some(at) = subject.find("CN=") else {
        return subject.trim().to_string();
    };
    let rest = &subject[at + 3..];
    // (a name with a comma in it comes in quotes)
    let name = match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or(quoted),
        None => rest.split(", ").next().unwrap_or(rest),
    };
    name.trim().to_string()
}

/// Starts the installed Ollama (its tray app, which runs the server).
fn start(dir: &str, models_dir: Option<&str>) -> Result<(), String> {
    let exe = format!("{dir}\\ollama app.exe");
    let mut cmd = std::process::Command::new(&exe);
    cmd.current_dir(dir);
    // (as Ollama's installer starts it: its folder first on the PATH)
    let path = std::env::var("PATH").unwrap_or_default();
    cmd.env("PATH", format!("{dir};{path}"));
    // (this app's own environment is from before OLLAMA_MODELS was set)
    if let Some(models) = models_dir {
        cmd.env("OLLAMA_MODELS", models);
    }
    detach(&mut cmd);
    cmd.spawn().map(|_| ()).map_err(|e| format!("Couldn't start Ollama: {e}"))
}

/// Starts a program on its own: not tied to this app's console (in a dev
/// build, Ctrl+C or closing the terminal there would take Ollama down with
/// it, and its log would print into ours), and still running when this app
/// closes.
#[cfg(windows)]
fn detach(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
}

#[cfg(not(windows))]
fn detach(cmd: &mut std::process::Command) {
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
}

/// The server Wrapped started (when it runs Ollama), how many jobs use
/// Ollama now, and one start at a time.
static OURS: Mutex<Option<std::process::Child>> = Mutex::new(None);
static USERS: AtomicUsize = AtomicUsize::new(0);
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// How long Wrapped's Ollama stays up after the last job (a Test and then a
/// build start it once).
const LINGER: Duration = Duration::from_secs(60);

/// A job's hold on Ollama: while one is held, Wrapped's Ollama stays up.
pub struct Lease(());

impl Drop for Lease {
    fn drop(&mut self) {
        if USERS.fetch_sub(1, Ordering::SeqCst) == 1 {
            tauri::async_runtime::spawn(async {
                tokio::time::sleep(LINGER).await;
                if USERS.load(Ordering::SeqCst) == 0 {
                    close_ours();
                }
            });
        }
    }
}

/// Ollama for a job: answering at the address, started first when Wrapped
/// runs it and it's installed on this PC. Also says whether it was started
/// just now. Keep the lease for as long as the job needs Ollama.
pub async fn lease(app: &AppHandle) -> Result<(Lease, bool), String> {
    let settings = WrappedSettings::load(app);
    let url = address(&settings);
    USERS.fetch_add(1, Ordering::SeqCst);
    let held = Lease(());
    // (Wrapped runs Ollama: not from Windows' startup any more, also when
    // it's running now, so from the next sign-in it's Wrapped's to start)
    if !settings.ollama_manual && is_local(&url) {
        hide_from_startup(app);
    }
    if version(&url).await.is_some() {
        return Ok((held, false));
    }
    if !is_local(&url) {
        return Err(format!("Ollama isn't answering at {url}"));
    }
    if settings.ollama_manual {
        return Err("Ollama isn't running (start it, or let Wrapped run it: untick \"Leave Ollama to me\" in Wrapped's Settings)".into());
    }
    let _turn = STARTING.lock().await;
    // (another job may have started it while this one waited)
    if version(&url).await.is_some() {
        return Ok((held, false));
    }
    let info = sysinfo(false).await;
    let dir = installed_dir(&info).ok_or("Ollama isn't installed (Wrapped's Settings, tier 2, installs it)")?;
    serve(&dir, info.models_dir.as_deref(), &url)?;
    wait_until_up(&url, None, Duration::from_secs(30)).await.map_err(|_| format!("Ollama was started but isn't answering at {url}"))?;
    Ok((held, true))
}

/// Starts Ollama's server hidden, as Wrapped's own (`ollama.exe serve`).
fn serve(dir: &str, models_dir: Option<&str>, url: &str) -> Result<(), String> {
    let mut ours = OURS.lock().unwrap();
    // (one that stopped answering goes first)
    if let Some(mut old) = ours.take() {
        kill_tree(&mut old);
    }
    let mut cmd = std::process::Command::new(format!("{dir}\\ollama.exe"));
    cmd.arg("serve").current_dir(dir);
    // (this app's own environment may be from before OLLAMA_MODELS was set)
    if let Some(models) = models_dir {
        cmd.env("OLLAMA_MODELS", models);
    }
    if let Some((host, port)) = host_port(url) {
        let host = if host.contains(':') { format!("[{host}]") } else { host };
        cmd.env("OLLAMA_HOST", format!("{host}:{port}"));
    }
    hide_console(&mut cmd);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    *ours = Some(cmd.spawn().map_err(|e| format!("Couldn't start Ollama: {e}"))?);
    Ok(())
}

/// Ends a process and the ones it started (Ollama's server runs each model
/// in a process of its own, holding the graphics card's memory).
fn kill_tree(child: &mut std::process::Child) {
    if cfg!(windows) {
        let mut cmd = std::process::Command::new("taskkill");
        cmd.args(["/F", "/T", "/PID", &child.id().to_string()]);
        hide_console(&mut cmd);
        let _ = cmd.output();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Closes the Ollama Wrapped started, if it's running (also when the app
/// quits).
pub fn close_ours() {
    if let Some(mut child) = OURS.lock().unwrap().take() {
        kill_tree(&mut child);
    }
}

/// Closes Ollama's own app and its server (the ones the installer opened).
fn close_ollama_apps() {
    if cfg!(windows) {
        let mut cmd = std::process::Command::new("taskkill");
        cmd.args(["/F", "/T", "/IM", "ollama app.exe", "/IM", "ollama.exe"]);
        hide_console(&mut cmd);
        let _ = cmd.output();
    }
}

/// Ollama's shortcut in Windows' startup folder, and where Wrapped keeps it
/// while it runs Ollama itself.
fn startup_shortcut() -> Option<std::path::PathBuf> {
    let roaming = std::env::var("APPDATA").ok()?;
    Some(std::path::Path::new(&roaming).join("Microsoft\\Windows\\Start Menu\\Programs\\Startup\\Ollama.lnk"))
}

fn kept_shortcut(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("ollama-startup.lnk"))
}

fn move_file(from: &std::path::Path, to: &std::path::Path) -> bool {
    if let Some(parent) = to.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::rename(from, to).is_ok() || (std::fs::copy(from, to).is_ok() && std::fs::remove_file(from).is_ok())
}

/// Ollama out of Windows' startup (Wrapped starts it when needed).
fn hide_from_startup(app: &AppHandle) {
    if let (Some(link), Some(kept)) = (startup_shortcut(), kept_shortcut(app)) {
        if link.exists() {
            if kept.exists() {
                let _ = std::fs::remove_file(&kept);
            }
            move_file(&link, &kept);
        }
    }
}

/// Ollama back in Windows' startup, as it put itself there.
fn restore_startup(app: &AppHandle) {
    if let (Some(link), Some(kept)) = (startup_shortcut(), kept_shortcut(app)) {
        if kept.exists() && !link.exists() {
            move_file(&kept, &link);
        }
    }
}

/// Wrapped's settings changed: "Leave Ollama to me" ticked puts Ollama's
/// startup shortcut back; unticked, Wrapped runs Ollama again.
pub fn settings_changed(app: &AppHandle, before: &WrappedSettings, now: &WrappedSettings) {
    if before.ollama_manual == now.ollama_manual {
        return;
    }
    if now.ollama_manual {
        restore_startup(app);
    } else if now.llm_provider == "ollama" {
        hide_from_startup(app);
    }
}

/// Waits for Ollama to answer; after 15 seconds without an answer it's
/// started from `start_from` (folder, models folder), once.
async fn wait_until_up(url: &str, start_from: Option<(&str, &str)>, longest: Duration) -> Result<(), String> {
    let began = Instant::now();
    let mut started = false;
    while began.elapsed() < longest {
        if version(url).await.is_some() {
            return Ok(());
        }
        if !started && began.elapsed() >= Duration::from_secs(15) {
            if let Some((dir, models)) = start_from {
                let _ = start(dir, Some(models));
            }
            started = true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err("Ollama is installed but isn't answering yet. Try Start Ollama in a moment.".into())
}

/// Starts the installed Ollama and waits for it to answer.
#[tauri::command]
pub async fn ollama_start(app: AppHandle) -> Result<(), String> {
    let url = address(&WrappedSettings::load(&app));
    let info = sysinfo(true).await;
    let dir = installed_dir(&info).ok_or("Ollama isn't installed.")?;
    start(&dir, info.models_dir.as_deref())?;
    wait_until_up(&url, None, Duration::from_secs(45)).await
}

/// Downloads a model through Ollama, with progress.
#[tauri::command]
pub async fn ollama_pull(app: AppHandle, model: String) -> Result<(), String> {
    let _busy = Busy::take()?;
    let result = pull(&app, model.trim()).await;
    finish(&app, "pull", &result);
    result
}

fn valid_model_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 120 && name.chars().all(|c| c.is_ascii_alphanumeric() || "._:/-".contains(c))
}

async fn pull(app: &AppHandle, model: &str) -> Result<(), String> {
    if !valid_model_name(model) {
        return Err(format!("\"{model}\" isn't a model name Ollama knows."));
    }
    let url = address(&WrappedSettings::load(app));
    let label = format!("Downloading {model}");
    report(app, "pull", "pull", "Starting Ollama", 0, 0);
    let _ollama = lease(app).await?.0;
    report(app, "pull", "pull", label.clone(), 0, 0);
    let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build().map_err(|e| e.to_string())?;
    let mut response = client
        .post(format!("{url}/api/pull"))
        .json(&serde_json::json!({ "model": model, "stream": true }))
        .send()
        .await
        .map_err(|e| if e.is_connect() { format!("Ollama isn't answering at {url}.") } else { format!("Ollama: {e}") })?;
    if !response.status().is_success() {
        let v: serde_json::Value = response.json().await.unwrap_or_default();
        return Err(format!("Ollama: {}", v["error"].as_str().unwrap_or("the download didn't start")));
    }
    // Ollama sends a line of JSON per step: each layer of the model with its
    // size and how much has come in, then checks, then "success"
    let mut layers: std::collections::HashMap<String, (u64, u64)> = std::collections::HashMap::new();
    let (mut pending, mut shown, mut success) = (Vec::<u8>::new(), Instant::now(), false);
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            return Err(STOPPED.into());
        }
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(e) => return Err(format!("The download stopped: {e}")),
        };
        pending.extend_from_slice(&chunk);
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(error) = v["error"].as_str() {
                return Err(format!("Ollama: {error}"));
            }
            let status = v["status"].as_str().unwrap_or("");
            if let (Some(digest), Some(total)) = (v["digest"].as_str(), v["total"].as_u64()) {
                layers.insert(digest.to_string(), (v["completed"].as_u64().unwrap_or(0), total));
            }
            if status == "success" {
                success = true;
            }
            if shown.elapsed() >= Duration::from_millis(200) || success {
                shown = Instant::now();
                let done: u64 = layers.values().map(|l| l.0).sum();
                let total: u64 = layers.values().map(|l| l.1).sum();
                let step = if status.starts_with("verifying") {
                    "Checking the download".to_string()
                } else if status.starts_with("writing") || status.starts_with("removing") || status == "success" {
                    "Finishing up".to_string()
                } else {
                    label.clone()
                };
                report(app, "pull", "pull", step, done, total);
            }
        }
    }
    if success {
        // (the new model is the one tier 2 uses from now on)
        crate::wrapped::wrapped_settings_update(app.clone(), serde_json::json!({ "llmModel": model }))?;
        Ok(())
    } else {
        Err("The download stopped before it finished.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn reads_powershell_answers() {
        let one = r#"{"gpus":{"name":"NVIDIA GeForce RTX 4090","bytes":25757220864},"drives":[{"letter":"E:","free":886996254720,"label":"Crucial"},{"letter":"C:","free":42949672960,"label":""}],"install":"","models":""}"#;
        let info = parse_info(one);
        assert_eq!(info.gpus.len(), 1);
        assert_eq!(info.gpus[0].gb, 24.0);
        assert_eq!(info.drives[0].letter, "C:");
        assert_eq!(info.drives[1].free_gb, 826.1);
        assert!(info.install_dir.is_none() && info.models_dir.is_none());
        let info = parse_info(r#"{"gpus":[],"drives":[],"install":"E:\\Ollama\\app\\","models":"E:\\Ollama\\models"}"#);
        assert_eq!(info.install_dir.as_deref(), Some("E:\\Ollama\\app"));
        assert_eq!(info.models_dir.as_deref(), Some("E:\\Ollama\\models"));
        assert!(parse_info("").gpus.is_empty());
    }

    #[test]
    fn suggests_by_memory() {
        assert_eq!(suggest(Some(24.0)), "gemma4:26b");
        assert_eq!(suggest(Some(16.0)), "gemma4:12b");
        assert_eq!(suggest(Some(8.0)), "gemma4:e4b");
        assert_eq!(suggest(Some(4.0)), "gemma4:e2b");
        assert_eq!(suggest(None), "gemma4:e2b");
    }

    #[test]
    fn turns_thinking_down() {
        use serde_json::json;
        assert_eq!(thinking_setting(&json!({ "capabilities": ["completion", "thinking"] })), Some(json!(false)));
        assert_eq!(thinking_setting(&json!({ "capabilities": ["completion"] })), None);
        assert_eq!(thinking_setting(&json!({ "thinking": { "values": [true, false], "default": true } })), Some(json!(false)));
        assert_eq!(thinking_setting(&json!({ "thinking": { "values": ["low", "medium", "high"] }, "capabilities": ["thinking"] })), Some(json!("low")));
        assert_eq!(thinking_setting(&json!({ "thinking": { "values": [false] } })), Some(json!(false)));
    }

    #[test]
    fn reads_models_from_the_folder() {
        let dir = std::env::temp_dir().join(format!("rt-ollama-test-{}", std::process::id()));
        let library = dir.join("manifests").join("registry.ollama.ai").join("library").join("gemma4");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(library.join("26b"), r#"{"layers":[{"size":1073741824},{"size":1073741824}]}"#).unwrap();
        let hf = dir.join("manifests").join("hf.co").join("someone").join("model");
        std::fs::create_dir_all(&hf).unwrap();
        std::fs::write(hf.join("Q4_K_M"), "{}").unwrap();
        let found = models_on_disk(Some(dir.to_str().unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "gemma4:26b");
        assert_eq!(found[0].gb, 2.0);
        assert_eq!(found[1].name, "hf.co/someone/model:Q4_K_M");
    }

    #[test]
    fn reads_addresses() {
        assert_eq!(host_port("http://127.0.0.1:11434"), Some(("127.0.0.1".into(), 11434)));
        assert_eq!(host_port("http://[::1]:11434/"), Some(("::1".into(), 11434)));
        assert_eq!(host_port("https://ollama.home"), Some(("ollama.home".into(), 443)));
        assert_eq!(host_port("pooplexserver:11434"), Some(("pooplexserver".into(), 11434)));
        let mut s = WrappedSettings::default();
        assert_eq!(address(&s), "http://127.0.0.1:11434");
        s.llm_url = "http://localhost:11434/".into();
        assert_eq!(address(&s), "http://127.0.0.1:11434");
    }

    #[test]
    fn knows_this_pc() {
        assert!(is_local("http://localhost:11434"));
        assert!(is_local("http://127.0.0.1:11434/"));
        assert!(is_local("http://[::1]:11434"));
        assert!(!is_local("http://192.168.1.20:11434"));
        assert!(!is_local("http://pooplexserver:11434"));
    }

    #[test]
    fn matches_model_names() {
        assert!(same_model("llama3.1:latest", "llama3.1"));
        assert!(same_model("gemma4:26b", "Gemma4:26b"));
        assert!(!same_model("gemma4:12b", "gemma4:26b"));
        assert!(valid_model_name("gemma4:26b") && valid_model_name("hf.co/user/model:Q4_K_M"));
        assert!(!valid_model_name("gemma4; rm") && !valid_model_name(""));
    }

    #[test]
    fn names_the_signer() {
        assert_eq!(signer_name("CN=Ollama Inc., O=Ollama Inc., L=Palo Alto, S=California, C=US"), "Ollama Inc.");
        assert_eq!(signer_name("CN=\"Ollama, Inc.\", O=x"), "Ollama, Inc.");
    }
}
