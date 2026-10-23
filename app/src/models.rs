//! The model manager: what each AI feature has to download, and doing it.
//!
//! `models.json` (compiled in) lists packs and the features that need them.
//! A pack is one folder in the models folder (`POING_MODELS`, else
//! `%LOCALAPPDATA%\Poing\models` and the like). Its files come down resumably
//! and hash-checked with Makepad's downloader; a runtime pack then builds its
//! Python env with the bundled uv. `pack.json` is written last: only then does
//! the pack count as installed. Remove deletes the folder, env and all.

use crate::paths;
use makepad_ai_hub::backend::CancelToken;
use makepad_ai_hub::download::{DownloadProgress, Downloader};
use makepad_ai_hub::registry::FileSpec;
use makepad_micro_serde::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(DeJson, Clone)]
pub struct Registry {
    pub version: u64,
    pub packs: Vec<Pack>,
    pub features: Vec<Feature>,
}

#[derive(DeJson, Clone)]
pub struct Pack {
    pub id: String,
    pub title: String,
    /// One line on what is in it.
    pub what: String,
    pub licence: String,
    pub licence_url: String,
    /// False: personal, non-commercial use only.
    pub commercial: bool,
    pub needs: Needs,
    pub files: Vec<PackFile>,
    pub setup: Option<Setup>,
}

#[derive(DeJson, Clone, Default)]
pub struct Needs {
    /// "cuda": an NVIDIA GPU.
    pub gpu: Option<String>,
    pub vram_gb: Option<f64>,
    /// The oldest NVIDIA driver that runs it.
    pub driver: Option<String>,
}

#[derive(DeJson, Clone)]
pub struct PackFile {
    pub url: String,
    /// Where it goes in the pack's folder.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// A Python env built with uv once the files are in.
#[derive(DeJson, Clone)]
pub struct Setup {
    /// A source zip among the files, unpacked into `project` (its top folder dropped).
    pub unzip: Option<String>,
    /// The uv project: a folder in the pack, or "@sidecar" for the installed sidecar.
    pub project: String,
    /// Python that must run in the new env before the pack counts as installed.
    pub smoke: String,
    /// About how much the env takes on disk.
    pub bytes: u64,
}

#[derive(DeJson, Clone)]
pub struct Feature {
    pub id: String,
    pub title: String,
    pub packs: Vec<String>,
    /// Packs that make it better but are not needed: the Models page lists every
    /// pack, so only the registry test reads this.
    #[allow(dead_code)]
    pub optional: Option<Vec<String>>,
}

pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry::deserialize_json_lenient(include_str!("../models.json")).expect("models.json"))
}

pub fn pack(id: &str) -> Option<&'static Pack> {
    registry().packs.iter().find(|p| p.id == id)
}

fn feature(id: &str) -> Option<&'static Feature> {
    registry().features.iter().find(|f| f.id == id)
}

/// The models folder: `POING_MODELS`, else the per-user data folder.
pub fn models_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("POING_MODELS") {
        return PathBuf::from(d);
    }
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    if cfg!(windows) {
        if let Some(d) = env("LOCALAPPDATA") {
            return d.join("Poing").join("models");
        }
    }
    let home = env("HOME").or_else(|| env("USERPROFILE")).unwrap_or_default();
    if cfg!(target_os = "macos") {
        return home.join("Library").join("Application Support").join("Poing").join("models");
    }
    env("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share")).join("poing").join("models")
}

/// What `pack.json` records: written last, so its presence means installed.
#[derive(SerJson, DeJson)]
struct Receipt {
    pack: String,
    registry: u64,
    /// The licence the user said yes to, and when (Unix seconds).
    licence: String,
    accepted_at: u64,
    /// "path sha256" per file, to spot a registry that moved on.
    files: Vec<String>,
}

fn file_ids(p: &Pack) -> Vec<String> {
    p.files.iter().map(|f| format!("{} {}", f.path, f.sha256)).collect()
}

/// The pack's folder when it is installed (for this registry's files).
pub fn installed_pack(id: &str) -> Option<PathBuf> {
    let p = pack(id)?;
    let dir = models_dir().join(id);
    let text = std::fs::read_to_string(dir.join("pack.json")).ok()?;
    let r = Receipt::deserialize_json_lenient(&text).ok()?;
    (r.files == file_ids(p)).then_some(dir)
}

/// Download size plus what its setup builds.
pub fn pack_bytes(p: &Pack) -> u64 {
    p.files.iter().map(|f| f.size).sum::<u64>() + p.setup.as_ref().map_or(0, |s| s.bytes)
}

/// Bytes a folder takes (0 when it isn't there).
pub fn folder_bytes(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.filter_map(|e| e.ok())
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => folder_bytes(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

pub fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

// ------------------------------------------------------------------ checks

pub struct Gpu {
    pub name: String,
    pub vram_mb: u64,
    pub driver: String,
}

/// The NVIDIA GPU, from `nvidia-smi` (asked once per run).
pub fn gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        let mut cmd = Command::new("nvidia-smi");
        cmd.args(["--query-gpu=name,memory.total,driver_version", "--format=csv,noheader,nounits"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = cmd.output().ok().filter(|o| o.status.success())?;
        let line = String::from_utf8_lossy(&out.stdout).lines().next()?.to_string();
        let parts: Vec<&str> = line.split(',').map(str::trim).collect();
        Some(Gpu { name: parts.first()?.to_string(), vram_mb: parts.get(1)?.parse().ok()?, driver: parts.get(2)?.to_string() })
    })
    .as_ref()
}

fn version(v: &str) -> Vec<u32> {
    v.split('.').map(|n| n.trim().parse().unwrap_or(0)).collect()
}

/// Why this PC can't run the pack, if it can't.
fn cant_run(p: &Pack) -> Option<String> {
    if p.needs.gpu.as_deref() != Some("cuda") {
        return None;
    }
    let vram = p.needs.vram_gb.unwrap_or(0.0);
    let Some(g) = gpu() else { return Some(format!("needs an NVIDIA GPU with {vram:.0} GB")) };
    if (g.vram_mb as f64) < vram * 1000.0 * 0.95 {
        return Some(format!("needs {vram:.0} GB of graphics memory; the {} has {:.0} GB", g.name, g.vram_mb as f64 / 1024.0));
    }
    match &p.needs.driver {
        Some(min) if version(&g.driver) < version(min) => Some(format!("update your NVIDIA driver: it needs {min} or newer, this PC has {}", g.driver)),
        _ => None,
    }
}

/// Free bytes on the drive that holds `path` (its nearest existing folder).
#[cfg(windows)]
fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, free_to_caller: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }
    let dir = path.ancestors().find(|a| a.is_dir())?;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: a NUL-terminated path and a valid out pointer; the others may be null.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

// ---------------------------------------------------------------- features

pub enum Readiness {
    Ready,
    /// These packs are missing; together this many bytes.
    Needs(Vec<&'static Pack>, u64),
    /// It can't run on this PC, and why.
    CantRun(String),
}

/// Can `feature` run now, or what does it need first?
pub fn readiness(feature_id: &str) -> Readiness {
    let Some(f) = feature(feature_id) else { return Readiness::Ready };
    if paths::dev_ready(feature_id) {
        return Readiness::Ready;
    }
    let packs: Vec<&Pack> = f.packs.iter().filter_map(|id| pack(id)).collect();
    if let Some(why) = packs.iter().find_map(|p| cant_run(p)) {
        return Readiness::CantRun(format!("{} {why}", f.title));
    }
    let missing: Vec<&Pack> = packs.into_iter().filter(|p| installed_pack(&p.id).is_none()).collect();
    if missing.is_empty() {
        return Readiness::Ready;
    }
    let bytes = missing.iter().map(|p| pack_bytes(p)).sum();
    Readiness::Needs(missing, bytes)
}

/// "Repainting a range needs ACE-Step runtime + ACE-Step 1.5 turbo (12.5 GB, MIT). Download?"
pub fn offer_text(feature_id: &str) -> Option<String> {
    let Readiness::Needs(packs, bytes) = readiness(feature_id) else { return None };
    let title = feature(feature_id).map_or("This", |f| f.title.as_str());
    let names: Vec<&str> = packs.iter().map(|p| p.title.as_str()).collect();
    let mut licences: Vec<&str> = packs.iter().map(|p| p.licence.as_str()).collect();
    licences.dedup();
    let nc = if packs.iter().any(|p| !p.commercial) { "; personal, non-commercial use only" } else { "" };
    // SheetSage2 and Demucs decode with the ffmpeg CLI, which no pack brings yet
    let ffmpeg = if feature_id == "analysis" && crate::audio::ffmpeg().is_none() { " It also needs ffmpeg on PATH for now." } else { "" };
    Some(format!("{title} needs a download: {} ({}, {} licence{nc}).{ffmpeg} Download?", names.join(" + "), gb(bytes), licences.join(", ")))
}

// ------------------------------------------------------------- downloading

pub enum Update {
    Progress(String),
    /// Every pack of the feature is in.
    Done(String),
    Failed(String),
}

/// One install at a time, on its own thread (a pack is GBs: it must not hold
/// up the jobs worker). Updates wake the UI like job updates do.
#[derive(Default)]
pub struct Downloads {
    rx: Option<Receiver<Update>>,
    cancel: Option<CancelToken>,
    /// The feature (or pack) being installed.
    pub what: Option<String>,
}

impl Downloads {
    pub fn busy(&self) -> bool {
        self.what.is_some()
    }

    /// Install the packs `feature_id` is missing (or the one pack named so).
    pub fn start(&mut self, feature_id: &str) {
        if self.busy() {
            return;
        }
        let ids: Vec<String> = match feature(feature_id) {
            Some(f) => f.packs.clone(),
            None => vec![feature_id.to_string()],
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = CancelToken::new();
        self.rx = Some(rx);
        self.cancel = Some(cancel.clone());
        self.what = Some(feature_id.to_string());
        let id = feature_id.to_string();
        std::thread::spawn(move || {
            let result = ids.iter().filter_map(|i| pack(i)).filter(|p| installed_pack(&p.id).is_none()).try_for_each(|p| install(p, &tx, &cancel));
            let _ = tx.send(match result {
                Ok(()) => Update::Done(id),
                Err(e) => Update::Failed(e),
            });
            makepad_widgets::SignalToUI::set_ui_signal();
        });
    }

    /// Stop: what came down so far stays (`.part` files resume next time).
    pub fn stop(&self) {
        if let Some(c) = &self.cancel {
            c.cancel();
        }
    }

    pub fn poll(&mut self) -> Vec<Update> {
        let Some(rx) = &self.rx else { return Vec::new() };
        let updates: Vec<Update> = rx.try_iter().collect();
        if updates.iter().any(|u| !matches!(u, Update::Progress(..))) {
            self.rx = None;
            self.cancel = None;
            self.what = None;
        }
        updates
    }
}

fn say(tx: &Sender<Update>, text: String) {
    let _ = tx.send(Update::Progress(text));
    makepad_widgets::SignalToUI::set_ui_signal();
}

fn install(p: &Pack, tx: &Sender<Update>, cancel: &CancelToken) -> Result<(), String> {
    if let Some(why) = cant_run(p) {
        return Err(format!("{} {why}", p.title));
    }
    let dir = models_dir().join(&p.id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let need = pack_bytes(p).saturating_sub(folder_bytes(&dir));
    if let Some(free) = free_bytes(&dir) {
        if free < need + need / 10 {
            return Err(format!("{} needs {}, {} has {} free. Set POING_MODELS to a bigger drive.", p.title, gb(need), dir.display(), gb(free)));
        }
    }
    let total: u64 = p.files.iter().map(|f| f.size).sum();
    let downloader = Downloader::from_env().map_err(|e| e.to_string())?;
    let mut before = 0u64;
    let mut last = Instant::now() - Duration::from_secs(1);
    for f in &p.files {
        let spec = FileSpec {
            role: None,
            repo: String::new(),
            path: f.url.clone(),
            revision: None,
            cache_as: f.path.clone(),
            size: Some(f.size),
            sha256: Some(f.sha256.clone()),
            local: false,
            optional: false,
            converts_to: None,
            conversion: None,
        };
        let mut progress = |d: DownloadProgress| {
            if last.elapsed() >= Duration::from_millis(250) {
                last = Instant::now();
                let done = before + d.done;
                say(tx, format!("Downloading {}: {} of {} ({:.0}%)", p.title, gb(done), gb(total), 100.0 * done as f64 / total.max(1) as f64));
            }
        };
        downloader.ensure_file(&spec, &dir, &mut progress, cancel).map_err(|e| {
            if cancel.is_cancelled() {
                format!("Stopped. {} resumes where it was next time.", p.title)
            } else {
                format!("{}: {e}", f.path)
            }
        })?;
        before += f.size;
    }
    if let Some(setup) = &p.setup {
        build_env(p, setup, &dir, tx, cancel)?;
    }
    let receipt = Receipt {
        pack: p.id.clone(),
        registry: registry().version,
        licence: p.licence.clone(),
        accepted_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        files: file_ids(p),
    };
    std::fs::write(dir.join("pack.json"), receipt.serialize_json()).map_err(|e| e.to_string())
}

/// Unpack the source (if any), `uv sync --frozen` an env into the pack, and
/// check it imports. Python, uv's cache and the env all live in the pack.
fn build_env(p: &Pack, setup: &Setup, dir: &Path, tx: &Sender<Update>, cancel: &CancelToken) -> Result<(), String> {
    let project = if setup.project == "@sidecar" { paths::sidecar_dir() } else { dir.join(&setup.project) };
    if let Some(zip) = &setup.unzip {
        say(tx, format!("Unpacking {}", p.title));
        unzip(&dir.join(zip), &project)?;
    }
    let log_path = dir.join("setup.log");
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(paths::uv());
    cmd.args(["sync", "--frozen", "--no-config", "--project"])
        .arg(&project)
        .env("UV_PROJECT_ENVIRONMENT", dir.join("venv"))
        .env("UV_CACHE_DIR", dir.join("uv-cache"))
        .env("UV_PYTHON_INSTALL_DIR", dir.join("python"))
        .env("UV_PYTHON_PREFERENCE", "only-managed")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    say(tx, format!("Setting up {} (several minutes)", p.title));
    let mut child = cmd.spawn().map_err(|e| format!("uv did not start ({e}): {}", paths::uv().display()))?;
    if let Some(err) = child.stderr.take() {
        use std::io::{BufRead, BufReader, Write};
        for line in BufReader::new(err).lines().map_while(|l| l.ok()) {
            let _ = writeln!(&log, "{line}");
            if cancel.is_cancelled() {
                let _ = child.kill();
                break;
            }
            let line = line.trim();
            if !line.is_empty() {
                say(tx, format!("Setting up {}: {line}", p.title));
            }
        }
    }
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    if cancel.is_cancelled() {
        return Err(format!("Stopped setting up {}.", p.title));
    }
    if !ok {
        return Err(format!("Setting up {} failed (see {})", p.title, log_path.display()));
    }
    say(tx, format!("Checking {}", p.title));
    let mut smoke = Command::new(paths::venv_python(dir));
    smoke.arg("-c").arg(&setup.smoke).current_dir(&project);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        smoke.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = smoke.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        return Err(format!("{} does not run here: {last}", p.title));
    }
    // The env is built; uv's cache is only for building it again.
    let _ = std::fs::remove_dir_all(dir.join("uv-cache"));
    Ok(())
}

/// Unpack a GitHub source zip into `into`, dropping its top folder.
fn unzip(zip: &Path, into: &Path) -> Result<(), String> {
    use makepad_zip_file::zip_read_central_directory;
    let mut file = std::io::BufReader::new(std::fs::File::open(zip).map_err(|e| format!("{}: {e}", zip.display()))?);
    let dir = zip_read_central_directory(&mut file).map_err(|e| format!("{}: {e:?}", zip.display()))?;
    let _ = std::fs::remove_dir_all(into);
    for h in &dir.file_headers {
        let Some((_, rel)) = h.file_name.split_once('/') else { continue };
        if rel.is_empty() || rel.ends_with('/') || rel.split('/').any(|c| c == "..") {
            continue;
        }
        let dest = into.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let data = h.extract(&mut file).map_err(|e| format!("{rel}: {e:?}"))?;
        std::fs::write(&dest, data).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(())
}

/// Delete an installed pack; the bytes it freed.
pub fn remove(id: &str) -> Result<u64, String> {
    let dir = models_dir().join(id);
    let bytes = folder_bytes(&dir);
    // pack.json first: a half-removed pack must not look installed
    let _ = std::fs::remove_file(dir.join("pack.json"));
    std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_parses_and_features_name_real_packs() {
        let r = registry();
        assert!(!r.packs.is_empty());
        for f in &r.features {
            for id in f.packs.iter().chain(f.optional.iter().flatten()) {
                assert!(pack(id).is_some(), "feature {} names unknown pack {id}", f.id);
            }
        }
        for p in &r.packs {
            for f in &p.files {
                assert_eq!(f.sha256.len(), 64, "{}: {} needs a sha256", p.id, f.path);
                assert!(!f.url.contains("/resolve/main/"), "{}: {} must pin a commit", p.id, f.url);
            }
        }
    }

    #[test]
    fn driver_versions_compare_by_number() {
        assert!(version("572.16") > version("528.33"));
        assert!(version("528.4") < version("528.33"));
    }
}
