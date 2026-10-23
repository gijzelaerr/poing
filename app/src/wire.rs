//! The wire between Poing and poing-node (the Windows PC that runs the GPU
//! jobs). Shared by both sides: the jobs and their updates as JSON, paths
//! made bundle-relative, and the file manifests that mirror a song's bundle.
//!
//! A job on the wire names files by their path inside the song's bundle,
//! always with `/`. A file from outside the bundle travels as
//! `_inputs/<hash>.<ext>`. The node keeps its copy of each bundle in
//! `<node home>/bundles/<client>/<bundle id>/`; before a job the client
//! uploads what the node lacks (by sha256), and before each update that
//! names files the node sends a `Sync` of the files the job changed, which
//! the client downloads before it passes the update on.

// The app uses the client half; poing-node the serving half and Settings the rest.
#![allow(dead_code)]

use crate::audio::{Job, JobUpdate, LayerJob, LegoJob, RepaintJob, SongJob, Yue2Fixture};
use makepad_micro_serde::*;
use poing_model::Take;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Where files from outside the bundle go in the node's copy.
pub const INPUTS: &str = "_inputs";
/// Bundle files every job takes along when they exist (small; the sidecars read them).
pub const CONTEXT_FILES: &[&str] = &["project.json", "analysis.json", "lyrics.txt", "sheet.json", "prompt.txt"];
/// Bundle folders every job takes along (SheetSage2's score).
pub const CONTEXT_DIRS: &[&str] = &["score"];
/// Stands for the node's bundle folder in message text.
pub const BUNDLE_MARK: &str = "{bundle}";

/// A GPU job, as the node gets it: every path is bundle-relative.
#[derive(SerJson, DeJson, Clone)]
pub enum WireJob {
    Analyze { audio: String },
    Generate { take: Take, fixture: Yue2Fixture },
    Repaint { take: Take, src: String, job: RepaintJob },
    Dereverb { take: Take, lane: String },
    Layer { take: Take, lego: LegoJob, clean: LayerJob },
    /// Sing the bundle's sheet.json (the client wrote it with Claude).
    Sing { count: usize, seeds: Vec<u64> },
    Remix { takes: Vec<Take>, job: SongJob },
}

/// POST /bundle/<id>/job
#[derive(SerJson, DeJson, Clone)]
pub struct JobRequest {
    pub sr: u32,
    pub job: WireJob,
}

#[derive(SerJson, DeJson, Clone, Debug, PartialEq)]
pub struct StemFile {
    pub id: String,
    pub file: String,
}

/// One file of a bundle: its path inside it, size and sha256 (hex).
#[derive(SerJson, DeJson, Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha: String,
}

/// What the node streams back, one JSON object per line.
#[derive(SerJson, DeJson, Clone, Debug)]
pub enum WireEvent {
    Status { message: String },
    Starting { message: String, progress: f32 },
    Progress { message: String, progress: f32 },
    /// `in_bundle`: the failure belongs to the song (a new song: once it opened).
    Failed { message: String, in_bundle: bool },
    Finished { message: String },
    Analyzed { stems: Vec<StemFile> },
    TakeReady { take: Take },
    SongReady { project: String },
    /// Files the job changed: download them before the next update. `gone`:
    /// files the job made and removed again (its scratch files).
    Sync { files: Vec<FileEntry>, gone: Vec<String> },
    /// Still working (sent every few seconds, so silence means the node is gone).
    Ping { at: u64 },
    /// The job is over (nothing follows).
    End { at: u64 },
}

impl WireEvent {
    /// Does this update name files the client must have first?
    pub fn needs_files(&self) -> bool {
        matches!(
            self,
            WireEvent::Analyzed { .. } | WireEvent::TakeReady { .. } | WireEvent::SongReady { .. } | WireEvent::Finished { .. } | WireEvent::Failed { .. }
        )
    }
}

/// One line of JSON (micro_serde pretty-prints; strings escape their own newlines).
pub fn line<T: SerJson>(value: &T) -> String {
    let mut s: String = value.serialize_json().chars().filter(|c| *c != '\n' && *c != '\r').collect();
    s.push('\n');
    s
}

// ------------------------------------------------------------------- paths

/// `path` inside `bundle`, as a wire path (`/`-separated), or None when outside.
pub fn rel_path(bundle: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(bundle).ok()?;
    let parts: Vec<String> = rest
        .components()
        .map(|c| match c {
            Component::Normal(p) => Some(p.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// A wire path checked to stay inside the bundle (no `..`, no root, no
/// drive): the relative path to join, or None.
pub fn safe_rel(rel: &str) -> Option<PathBuf> {
    if rel.is_empty() || rel.len() > 512 || rel.starts_with('/') {
        return None;
    }
    let mut out = PathBuf::new();
    for part in rel.split('/') {
        let bad = part.is_empty() || part == "." || part == ".." || part.contains(['\\', ':', '\0']) || part.ends_with('.') || part.ends_with(' ');
        if bad {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// The wire name of a file from outside the bundle (one per source path).
pub fn input_name(path: &Path) -> String {
    let hash = makepad_core_util::sha256::sha256_hex(path.to_string_lossy().as_bytes());
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy().to_ascii_lowercase())).unwrap_or_default();
    format!("{INPUTS}/{}{ext}", &hash[..16])
}

/// A song's bundle id on the node: its local path, hashed.
pub fn bundle_id(bundle: &Path) -> String {
    makepad_core_util::sha256::sha256_hex(bundle.to_string_lossy().as_bytes())[..16].to_string()
}

pub fn valid_bundle_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Message text from the node: its bundle folder becomes [`BUNDLE_MARK`] and
/// the path after it uses `/` ("see {bundle}/takes/t3/render.log").
pub fn mark_bundle(text: &str, node_bundle: &Path) -> String {
    let full = node_bundle.to_string_lossy();
    let mut out = text.to_string();
    for variant in [full.to_string(), full.replace('\\', "/")] {
        if variant.is_empty() {
            continue;
        }
        out = out.replace(&variant, BUNDLE_MARK);
    }
    // the rest of each marked path: `/` separators
    let mut result = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(i) = rest.find(BUNDLE_MARK) {
        result.push_str(&rest[..i + BUNDLE_MARK.len()]);
        rest = &rest[i + BUNDLE_MARK.len()..];
        let end = rest.find(|c: char| c.is_whitespace() || c == ')' || c == '"' || c == '\'').unwrap_or(rest.len());
        result.push_str(&rest[..end].replace('\\', "/"));
        rest = &rest[end..];
    }
    result.push_str(rest);
    result
}

/// Message text from the node with [`BUNDLE_MARK`] made the local bundle.
pub fn unmark_bundle(text: &str, bundle: &Path) -> String {
    let local = bundle.to_string_lossy();
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(BUNDLE_MARK) {
        result.push_str(&rest[..i]);
        result.push_str(&local);
        rest = &rest[i + BUNDLE_MARK.len()..];
        let end = rest.find(|c: char| c.is_whitespace() || c == ')' || c == '"' || c == '\'').unwrap_or(rest.len());
        result.push_str(&rest[..end].replace('/', std::path::MAIN_SEPARATOR_STR));
        rest = &rest[end..];
    }
    result.push_str(rest);
    result
}

// ------------------------------------------------------------- client side

/// A job made ready for the node: the wire job and the files it needs
/// (wire path, local file).
pub struct Outbound {
    pub job: WireJob,
    pub files: Vec<(String, PathBuf)>,
}

impl Outbound {
    /// The wire path for `path`, taking the file along.
    fn take_along(&mut self, bundle: &Path, path: &Path) -> String {
        let rel = rel_path(bundle, path).unwrap_or_else(|| input_name(path));
        if !self.files.iter().any(|(r, _)| *r == rel) {
            self.files.push((rel.clone(), path.to_path_buf()));
        }
        rel
    }
}

/// The node's copy of `job` for the song in `bundle`, with the files it
/// needs; None for a job that always runs here (render, Claude's writers,
/// writing a new song's sheet).
pub fn outbound(job: &Job) -> Option<(PathBuf, u32, Outbound)> {
    let blank = || Outbound { job: WireJob::Sing { count: 0, seeds: Vec::new() }, files: Vec::new() };
    let (bundle, sr, out) = match job {
        Job::Analyze { audio, bundle, sr } => {
            let mut o = blank();
            o.job = WireJob::Analyze { audio: o.take_along(bundle, audio) };
            (bundle, *sr, o)
        }
        Job::Generate { bundle, take, fixture, sr } => {
            let mut o = blank();
            o.job = WireJob::Generate { take: take.clone(), fixture: fixture.clone() };
            (bundle, *sr, o)
        }
        Job::Repaint { bundle, take, src, job, sr } => {
            let mut o = blank();
            let src = o.take_along(bundle, src);
            o.job = WireJob::Repaint { take: take.clone(), src, job: job.clone() };
            (bundle, *sr, o)
        }
        Job::Dereverb { bundle, take, lane, sr } => {
            let mut o = blank();
            let lane = o.take_along(bundle, lane);
            o.job = WireJob::Dereverb { take: take.clone(), lane };
            (bundle, *sr, o)
        }
        Job::Layer { bundle, take, lego, clean, sr } => {
            let mut o = blank();
            let mut lego = lego.clone();
            let mut clean = clean.clone();
            if !lego.src.is_empty() {
                lego.src = o.take_along(bundle, Path::new(&lego.src));
            }
            clean.reference = clean.reference.take().map(|r| o.take_along(bundle, Path::new(&r)));
            clean.match_lane = clean.match_lane.take().map(|r| o.take_along(bundle, Path::new(&r)));
            o.job = WireJob::Layer { take: take.clone(), lego, clean };
            (bundle, *sr, o)
        }
        Job::Sing { bundle, count, seeds, sr } => {
            let mut o = blank();
            o.job = WireJob::Sing { count: *count, seeds: seeds.clone() };
            (bundle, *sr, o)
        }
        Job::Remix { bundle, takes, job, sr } => {
            let mut o = blank();
            let mut job = job.clone();
            job.src = job.src.take().map(|s| o.take_along(bundle, Path::new(&s)));
            o.job = WireJob::Remix { takes: takes.clone(), job };
            (bundle, *sr, o)
        }
        Job::Render { .. } | Job::WriteMidi { .. } | Job::WriteFx { .. } | Job::Song { .. } | Job::Export(_) => return None,
    };
    let mut out = out;
    for name in CONTEXT_FILES {
        let path = bundle.join(name);
        if path.is_file() {
            out.take_along(bundle, &path);
        }
    }
    for dir in CONTEXT_DIRS {
        for (rel, _) in scan(&bundle.join(dir)) {
            let path = bundle.join(dir).join(&rel);
            out.take_along(bundle, &path);
        }
    }
    Some((bundle.clone(), sr, out))
}

/// The update for the UI from a node's update, for the song in `bundle`
/// (decoding is the caller's: Analyzed and TakeReady name files).
pub enum Inbound {
    Update(JobUpdate),
    Analyzed(Vec<StemFile>),
    TakeReady(Take),
    Sync(Vec<FileEntry>, Vec<String>),
    Alive,
    End,
}

pub fn inbound(event: WireEvent, bundle: &Path) -> Inbound {
    let text = |t: String| unmark_bundle(&t, bundle);
    let b = bundle.to_path_buf();
    match event {
        WireEvent::Status { message } => Inbound::Update(JobUpdate::Status(text(message))),
        WireEvent::Starting { message, progress } => Inbound::Update(JobUpdate::Starting { message: text(message), progress }),
        WireEvent::Progress { message, progress } => Inbound::Update(JobUpdate::Progress { bundle: b, message: text(message), progress }),
        WireEvent::Failed { message, in_bundle } => {
            Inbound::Update(JobUpdate::Failed { bundle: if in_bundle { b } else { PathBuf::new() }, message: text(message) })
        }
        WireEvent::Finished { message } => Inbound::Update(JobUpdate::Finished { bundle: b, message: text(message) }),
        WireEvent::SongReady { project } => {
            Inbound::Update(JobUpdate::SongReady { project: safe_rel(&project).map(|p| bundle.join(p)).unwrap_or_else(|| bundle.join("project.json")) })
        }
        WireEvent::Analyzed { stems } => Inbound::Analyzed(stems),
        WireEvent::TakeReady { take } => Inbound::TakeReady(take),
        WireEvent::Sync { files, gone } => Inbound::Sync(files, gone),
        WireEvent::Ping { .. } => Inbound::Alive,
        WireEvent::End { .. } => Inbound::End,
    }
}

// --------------------------------------------------------------- node side

/// The job to run on the node for `wire`, in the node's copy `bundle`.
pub fn job_on_node(wire: WireJob, bundle: &Path, sr: u32) -> Result<Job, String> {
    let at = |rel: &str| safe_rel(rel).map(|r| bundle.join(r)).ok_or_else(|| format!("bad path {rel:?}"));
    let at_text = |rel: &str| at(rel).map(|p| p.to_string_lossy().into_owned());
    let b = bundle.to_path_buf();
    Ok(match wire {
        WireJob::Analyze { audio } => Job::Analyze { audio: at(&audio)?, bundle: b, sr },
        WireJob::Generate { take, fixture } => {
            safe_rel(&take.file).ok_or("bad take file")?;
            Job::Generate { bundle: b, take, fixture, sr }
        }
        WireJob::Repaint { take, src, job } => {
            safe_rel(&take.file).ok_or("bad take file")?;
            Job::Repaint { bundle: b, take, src: at(&src)?, job, sr }
        }
        WireJob::Dereverb { take, lane } => {
            safe_rel(&take.file).ok_or("bad take file")?;
            Job::Dereverb { bundle: b, take, lane: at(&lane)?, sr }
        }
        WireJob::Layer { take, mut lego, mut clean } => {
            safe_rel(&take.file).ok_or("bad take file")?;
            if !lego.src.is_empty() {
                lego.src = at_text(&lego.src)?;
            }
            clean.reference = clean.reference.map(|r| at_text(&r)).transpose()?;
            clean.match_lane = clean.match_lane.map(|r| at_text(&r)).transpose()?;
            Job::Layer { bundle: b, take, lego, clean, sr }
        }
        WireJob::Sing { count, seeds } => Job::Sing { bundle: b, count: count.clamp(1, 8), seeds, sr },
        WireJob::Remix { takes, mut job } => {
            for t in &takes {
                safe_rel(&t.file).ok_or("bad take file")?;
            }
            job.src = job.src.map(|s| at_text(&s)).transpose()?;
            Job::Remix { bundle: b, takes, job, sr }
        }
    })
}

/// A node update for the wire (stems and takes travel as files).
pub fn outbound_event(update: JobUpdate, bundle: &Path) -> Option<WireEvent> {
    let text = |t: String| mark_bundle(&t, bundle);
    let rel = |p: &Path| rel_path(bundle, p).unwrap_or_default();
    Some(match update {
        JobUpdate::Status(message) | JobUpdate::Done(message) => WireEvent::Status { message: text(message) },
        JobUpdate::Starting { message, progress } => WireEvent::Starting { message: text(message), progress },
        JobUpdate::Progress { message, progress, .. } => WireEvent::Progress { message: text(message), progress },
        JobUpdate::Failed { bundle: b, message } => WireEvent::Failed { message: text(message), in_bundle: !b.as_os_str().is_empty() },
        JobUpdate::Finished { message, .. } => WireEvent::Finished { message: text(message) },
        JobUpdate::Analyzed { stems, .. } => WireEvent::Analyzed { stems: stems.into_iter().map(|(id, file, _)| StemFile { id, file }).collect() },
        JobUpdate::TakeReady { take, .. } => WireEvent::TakeReady { take },
        JobUpdate::SongReady { project } => WireEvent::SongReady { project: rel(&project) },
        // Claude's writers and the worker's own bookkeeping never run on the node
        JobUpdate::MidiReady { .. } | JobUpdate::FxReady { .. } | JobUpdate::JobDone { .. } => return None,
    })
}

// ---------------------------------------------------------------- manifests

/// When a file last changed, as far as a manifest cares.
pub type Stamp = (u64, std::time::SystemTime);

/// Every file under `dir` (recursively) by wire path, with its size and
/// mtime; `_inputs` is left out.
pub fn scan(dir: &Path) -> BTreeMap<String, Stamp> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Stamp>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                if rel != INPUTS {
                    walk(&e.path(), &rel, out);
                }
            } else if meta.is_file() {
                out.insert(rel, (meta.len(), meta.modified().unwrap_or(std::time::UNIX_EPOCH)));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, "", &mut out);
    out
}

/// Files in `now` that are new or changed since `before`.
pub fn changed(before: &BTreeMap<String, Stamp>, now: &BTreeMap<String, Stamp>) -> Vec<String> {
    now.iter().filter(|(k, v)| before.get(*k) != Some(*v)).map(|(k, _)| k.clone()).collect()
}

/// Files in `before` that are not in `now` and were not there at `start`
/// (made and removed since).
pub fn gone(start: &BTreeMap<String, Stamp>, before: &BTreeMap<String, Stamp>, now: &BTreeMap<String, Stamp>) -> Vec<String> {
    before.keys().filter(|k| !now.contains_key(*k) && !start.contains_key(*k)).cloned().collect()
}

/// Remove `rel` from `bundle`, and the folders it leaves empty.
pub fn remove(bundle: &Path, rel: &str) {
    let Some(r) = safe_rel(rel) else { return };
    let _ = std::fs::remove_file(bundle.join(&r));
    let mut dir = r.parent();
    while let Some(d) = dir.filter(|d| !d.as_os_str().is_empty()) {
        if std::fs::remove_dir(bundle.join(d)).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// sha256 (hex) of a file, streamed.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = makepad_core_util::sha256::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(makepad_core_util::sha256::to_hex(&h.finish()))
}

/// The manifest entry for `rel` in `bundle`.
pub fn entry(bundle: &Path, rel: &str) -> std::io::Result<FileEntry> {
    let path = bundle.join(safe_rel(rel).ok_or_else(|| std::io::Error::other("bad path"))?);
    Ok(FileEntry { path: rel.to_string(), size: std::fs::metadata(&path)?.len(), sha: sha256_file(&path)? })
}

/// Of `wanted`, the files `bundle` does not have with that content.
pub fn missing(bundle: &Path, wanted: &[FileEntry]) -> Vec<String> {
    wanted
        .iter()
        .filter(|f| match safe_rel(&f.path).map(|r| bundle.join(r)) {
            Some(p) => std::fs::metadata(&p).map(|m| m.len()).ok() != Some(f.size) || sha256_file(&p).ok().as_deref() != Some(f.sha.as_str()),
            None => false,
        })
        .map(|f| f.path.clone())
        .collect()
}

/// Write `data` as `rel` in `bundle`: checked against `sha`, through a
/// temporary file so a reader never sees half of it.
pub fn store(bundle: &Path, rel: &str, sha: Option<&str>, data: &[u8]) -> Result<(), String> {
    let dest = bundle.join(safe_rel(rel).ok_or_else(|| format!("bad path {rel:?}"))?);
    if let Some(sha) = sha {
        let got = makepad_core_util::sha256::sha256_hex(data);
        if got != sha {
            return Err(format!("{rel}: content does not match its sha256"));
        }
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let tmp = dest.with_file_name(format!(".{}.part", dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    std::fs::write(&tmp, data).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))
}

/// `%XX` for anything but unreserved characters and `/` (query values).
pub fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn url_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The value of `key` in a request target's query.
pub fn query<'a>(target: &'a str, key: &str) -> Option<String> {
    let q = target.split_once('?')?.1;
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| url_decode(v)).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(id: &str) -> Take {
        Take {
            file: format!("takes/{id}.wav"),
            id: id.into(),
            scope: "song".into(),
            start: 0.0,
            end: 0.0,
            engine: "acestep-cover".into(),
            prompt: "close · \"jazz\"".into(),
            seed: 7,
            track: None,
            blend: None,
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("poing-wire-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn events_round_trip_as_single_lines() {
        let events = vec![
            WireEvent::Progress { message: "Take 3  ·  Singing\nnext".into(), progress: 0.25 },
            WireEvent::Failed { message: "x".into(), in_bundle: false },
            WireEvent::Analyzed { stems: vec![StemFile { id: "vocals".into(), file: "stems/vocals.wav".into() }] },
            WireEvent::TakeReady { take: take("t4") },
            WireEvent::Sync { files: vec![FileEntry { path: "takes/t4.wav".into(), size: 12, sha: "ab".into() }], gone: vec!["takes/remix-1/job.json".into()] },
            WireEvent::Ping { at: 5 },
            WireEvent::End { at: 6 },
        ];
        for e in events {
            let l = line(&e);
            assert_eq!(l.matches('\n').count(), 1, "{l}");
            let back = WireEvent::deserialize_json(l.trim()).unwrap();
            assert_eq!(format!("{e:?}"), format!("{back:?}"));
        }
    }

    #[test]
    fn jobs_round_trip() {
        let job = JobRequest {
            sr: 48000,
            job: WireJob::Remix {
                takes: vec![take("t1"), take("t2")],
                job: SongJob { task: "cover".into(), src: Some("original.wav".into()), strengths: Some(vec![0.3, 0.6]), seeds: Some(vec![1, 2]), ..Default::default() },
            },
        };
        let back = JobRequest::deserialize_json(line(&job).trim()).unwrap();
        let WireJob::Remix { takes, job } = back.job else { panic!("not a remix") };
        assert_eq!(takes[1].file, "takes/t2.wav");
        assert_eq!(takes[0].prompt, "close · \"jazz\"");
        assert_eq!(job.src.as_deref(), Some("original.wav"));
        assert_eq!(job.strengths, Some(vec![0.3, 0.6]));
        assert_eq!(back.sr, 48000);
    }

    #[test]
    fn wire_paths_stay_inside_the_bundle() {
        assert_eq!(safe_rel("stems/vocals.wav"), Some(PathBuf::from("stems").join("vocals.wav")));
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a//b", "C:/x", "a\\..\\b", "a/./b", "con.", "x/"] {
            assert_eq!(safe_rel(bad), None, "{bad}");
        }
        let b = Path::new("/songs/a.poing");
        assert_eq!(rel_path(b, &b.join("stems").join("bass.wav")).as_deref(), Some("stems/bass.wav"));
        assert_eq!(rel_path(b, Path::new("/songs/b.mp3")), None);
        assert_eq!(rel_path(b, b), None);
        assert!(valid_bundle_id(&bundle_id(b)));
        assert!(!valid_bundle_id("../../etc"));
        assert!(input_name(Path::new("/music/Song.MP3")).starts_with("_inputs/") && input_name(Path::new("/music/Song.MP3")).ends_with(".mp3"));
    }

    #[test]
    fn node_paths_in_messages_become_the_local_bundle() {
        let node = Path::new(r"C:\Users\g\AppData\Local\Poing\node\bundles\mac\0123456789abcdef");
        let msg = format!(r"Analysis failed: boom (see {}\takes\t3\render.log)", node.display());
        let marked = mark_bundle(&msg, node);
        assert_eq!(marked, "Analysis failed: boom (see {bundle}/takes/t3/render.log)");
        let local = Path::new("/Users/gijs/Music/Poing/Song.poing");
        let back = unmark_bundle(&marked, local);
        let sep = std::path::MAIN_SEPARATOR_STR;
        assert_eq!(back, format!("Analysis failed: boom (see /Users/gijs/Music/Poing/Song.poing{sep}takes{sep}t3{sep}render.log)"));
        assert_eq!(mark_bundle("no paths here", node), "no paths here");
    }

    #[test]
    fn outbound_jobs_carry_their_files_bundle_relative() {
        let b = tmp("outbound");
        std::fs::create_dir_all(b.join("score")).unwrap();
        std::fs::write(b.join("lyrics.txt"), "la").unwrap();
        std::fs::write(b.join("score").join("lead.json"), "{}").unwrap();
        std::fs::write(b.join("original.wav"), "RIFF").unwrap();
        let outside = std::env::temp_dir().join("poing-wire-elsewhere.mp3");
        let job = Job::Layer {
            bundle: b.clone(),
            take: take("t9"),
            lego: LegoJob { src: b.join("original.wav").to_string_lossy().into_owned(), ..Default::default() },
            clean: LayerJob { match_lane: Some(outside.to_string_lossy().into_owned()), ..Default::default() },
            sr: 44100,
        };
        let (bundle, sr, out) = outbound(&job).unwrap();
        assert_eq!((bundle, sr), (b.clone(), 44100));
        let WireJob::Layer { lego, clean, .. } = &out.job else { panic!() };
        assert_eq!(lego.src, "original.wav");
        let lane = clean.match_lane.clone().unwrap();
        assert!(lane.starts_with("_inputs/"));
        let names: Vec<&str> = out.files.iter().map(|(r, _)| r.as_str()).collect();
        assert!(names.contains(&"original.wav") && names.contains(&"lyrics.txt") && names.contains(&"score/lead.json") && names.contains(&lane.as_str()));
        // on the node every path lands inside its copy
        let node = Path::new("/node/bundles/mac/0123456789abcdef");
        let Job::Layer { lego, clean, .. } = job_on_node(out.job, node, 44100).unwrap() else { panic!() };
        assert_eq!(PathBuf::from(lego.src), node.join("original.wav"));
        assert!(PathBuf::from(clean.match_lane.unwrap()).starts_with(node.join(INPUTS)));
        // and a hostile path is refused
        assert!(job_on_node(WireJob::Dereverb { take: take("t1"), lane: "../../secret".into() }, node, 44100).is_err());
        let mut evil = take("t1");
        evil.file = "../x.wav".into();
        assert!(job_on_node(WireJob::Generate { take: evil, fixture: Yue2Fixture { style: String::new(), lyrics: String::new(), seed: 0, cot: String::new(), abc: None } }, node, 44100).is_err());
        assert!(outbound(&Job::WriteFx { bundle: b.clone(), lane: None, request: String::new(), context: String::new() }).is_none());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn manifests_find_changes_and_missing_files() {
        let b = tmp("manifest");
        std::fs::create_dir_all(b.join(INPUTS)).unwrap();
        std::fs::write(b.join("a.txt"), "one").unwrap();
        std::fs::write(b.join(INPUTS).join("x.wav"), "in").unwrap();
        let before = scan(&b);
        assert!(before.contains_key("a.txt") && !before.keys().any(|k| k.starts_with(INPUTS)));
        std::fs::create_dir_all(b.join("takes")).unwrap();
        std::fs::write(b.join("takes").join("t1.wav"), "new take").unwrap();
        let after = scan(&b);
        assert_eq!(changed(&before, &after), vec!["takes/t1.wav".to_string()]);
        // a scratch file made and removed during the job is gone; one from before is not
        std::fs::create_dir_all(b.join("takes").join("remix-1")).unwrap();
        std::fs::write(b.join("takes").join("remix-1").join("job.json"), "{}").unwrap();
        let mid = scan(&b);
        std::fs::remove_dir_all(b.join("takes").join("remix-1")).unwrap();
        std::fs::remove_file(b.join("a.txt")).unwrap();
        let end = scan(&b);
        assert_eq!(gone(&before, &mid, &end), vec!["takes/remix-1/job.json".to_string()]);
        std::fs::write(b.join("a.txt"), "one").unwrap();
        let other_side = tmp("manifest-gone");
        store(&other_side, "takes/remix-1/job.json", None, b"{}").unwrap();
        store(&other_side, "takes/t1.wav", None, b"x").unwrap();
        remove(&other_side, "takes/remix-1/job.json");
        assert!(!other_side.join("takes").join("remix-1").exists() && other_side.join("takes").join("t1.wav").exists());
        remove(&other_side, "../outside");
        let _ = std::fs::remove_dir_all(&other_side);
        let after = scan(&b);
        let e = entry(&b, "takes/t1.wav").unwrap();
        assert_eq!(e.size, 8);
        assert_eq!(e.sha, makepad_core_util::sha256::sha256_hex(b"new take"));
        // the other side has a.txt with other content and no take
        let other = tmp("manifest-other");
        std::fs::write(other.join("a.txt"), "two").unwrap();
        let wanted = vec![entry(&b, "a.txt").unwrap(), e.clone()];
        assert_eq!(missing(&other, &wanted), vec!["a.txt".to_string(), "takes/t1.wav".to_string()]);
        store(&other, "a.txt", Some(&wanted[0].sha), b"one").unwrap();
        store(&other, "takes/t1.wav", Some(&e.sha), b"new take").unwrap();
        assert!(missing(&other, &wanted).is_empty());
        assert!(store(&other, "a.txt", Some(&e.sha), b"one").is_err(), "wrong content is refused");
        assert!(store(&other, "../escape.txt", None, b"x").is_err());
        let _ = std::fs::remove_dir_all(&b);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn query_values_decode() {
        let target = format!("/bundle/x/put?path={}&sha=ab", url_encode("takes/remix 1/ü.wav"));
        assert_eq!(query(&target, "path").as_deref(), Some("takes/remix 1/ü.wav"));
        assert_eq!(query(&target, "sha").as_deref(), Some("ab"));
        assert_eq!(query("/x", "path"), None);
    }
}
