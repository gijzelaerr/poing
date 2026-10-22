//! Stem loading (makepad-audio-decode: wav/flac/mp3/ogg), waveform peaks, the
//! mixer (shared by live playback and the offline render) and the job worker.
use makepad_micro_serde::*;

use crate::model::{Project, Take};
use makepad_widgets::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Frames per waveform peak block.
pub const PEAK_BLOCK: usize = 256;
/// Region edges fade over this many seconds, in playback and render alike.
const REGION_RAMP_S: f64 = 0.010;

/// A song's lanes, in track order. Each stem is shared, so adding lanes copies
/// pointers, not audio.
pub type Stems = Arc<Vec<Arc<Stem>>>;

pub struct Stem {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    /// (min, max) of the mono sum per PEAK_BLOCK frames
    pub peaks: Vec<(f32, f32)>,
    /// largest |peak|, so quiet stems (reverb) still draw a visible waveform
    pub peak_abs: f32,
}

pub fn load_stem(path: &Path, target_sr: u32) -> Result<Stem, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let audio = makepad_audio_decode::decode_any(&bytes).map_err(|e| format!("{}: {e:?}", path.display()))?;
    let ch = audio.channels.max(1) as usize;
    let frames = audio.frames();
    let pcm = &audio.pcm_interleaved_f32;
    let mut l = Vec::with_capacity(frames);
    let mut r = Vec::with_capacity(frames);
    for f in 0..frames {
        l.push(pcm[f * ch]);
        r.push(pcm[f * ch + if ch > 1 { 1 } else { 0 }]);
    }
    if audio.rate != target_sr && audio.rate > 0 {
        l = resample_linear(&l, audio.rate, target_sr);
        r = resample_linear(&r, audio.rate, target_sr);
    }
    let peaks: Vec<(f32, f32)> = l
        .chunks(PEAK_BLOCK)
        .zip(r.chunks(PEAK_BLOCK))
        .map(|(a, b)| {
            let (mut mn, mut mx) = (f32::MAX, f32::MIN);
            for (x, y) in a.iter().zip(b) {
                let m = 0.5 * (x + y);
                mn = mn.min(m);
                mx = mx.max(m);
            }
            (mn, mx)
        })
        .collect();
    let peak_abs = peaks.iter().fold(1e-6f32, |acc, (a, b)| acc.max(a.abs()).max(b.abs()));
    Ok(Stem { l, r, peaks, peak_abs })
}

fn resample_linear(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    let n = (x.len() as f64 * to as f64 / from as f64) as usize;
    let step = from as f64 / to as f64;
    (0..n)
        .map(|i| {
            let p = i as f64 * step;
            let j = p as usize;
            let f = (p - j as f64) as f32;
            let a = x.get(j).copied().unwrap_or(0.0);
            let b = x.get(j + 1).copied().unwrap_or(a);
            a + (b - a) * f
        })
        .collect()
}

#[derive(Clone, Default)]
pub struct MixRegion {
    pub track: usize,
    pub start: f64,
    pub end: f64,
    pub factor: f32,
}

/// The take being auditioned: stem `stem` (after the tracks in the stem set)
/// plays in place of the lanes over [start, end]; a song take replaces them all.
#[derive(Clone, Copy, Default)]
pub struct MixTake {
    pub stem: usize,
    pub start: f64,
    pub end: f64,
    pub song: bool,
}

/// Everything the mixer needs; rebuilt from the Project whenever it changes.
#[derive(Clone, Default)]
pub struct MixParams {
    pub version: u64,
    pub gains: Vec<f32>,
    pub regions: Vec<MixRegion>,
    pub take: Option<MixTake>,
}

impl MixParams {
    pub fn from_project(p: &Project, version: u64, audition: Option<&str>) -> Self {
        let regions = p
            .regions()
            .iter()
            .filter_map(|r| {
                let track = p.track_index(&r.track)?;
                let factor = r.factor();
                (factor != 1.0).then(|| MixRegion { track, start: r.start, end: r.end, factor })
            })
            .collect();
        let take = audition.and_then(|id| p.take_index(id)).map(|i| {
            let t = &p.takes()[i];
            MixTake { stem: p.tracks.len() + i, start: t.start, end: t.end, song: t.scope == "song" }
        });
        Self { version, gains: p.track_gains(), regions, take }
    }

    /// How much of the take is heard at t (0 = lanes only, 1 = take only).
    #[inline]
    fn take_weight(&self, t: f64) -> f32 {
        match self.take {
            None => 0.0,
            Some(k) if k.song => 1.0,
            Some(k) => (((t - k.start) / REGION_RAMP_S).clamp(0.0, 1.0) * ((k.end - t) / REGION_RAMP_S).clamp(0.0, 1.0)) as f32,
        }
    }

    /// Gain of one track at time t (seconds), with short linear ramps at region edges.
    #[inline]
    pub fn gain_at(&self, track: usize, t: f64) -> f32 {
        let mut g = self.gains.get(track).copied().unwrap_or(0.0);
        if g == 0.0 {
            return 0.0;
        }
        for r in &self.regions {
            if r.track != track || t < r.start - REGION_RAMP_S || t > r.end + REGION_RAMP_S {
                continue;
            }
            let a = ((t - r.start) / REGION_RAMP_S + 0.5).clamp(0.0, 1.0);
            let b = ((r.end - t) / REGION_RAMP_S + 0.5).clamp(0.0, 1.0);
            g *= 1.0 + (a * b) as f32 * (r.factor - 1.0);
        }
        g
    }
}

/// Mix frames [start, start + out_l.len()) of all stems into out_l / out_r
/// (overwrites). Resampling to the device rate is the caller's business.
pub fn mix_into(stems: &[Arc<Stem>], mix: &MixParams, sr: u32, start: usize, out_l: &mut [f32], out_r: &mut [f32]) {
    out_l.iter_mut().for_each(|x| *x = 0.0);
    out_r.iter_mut().for_each(|x| *x = 0.0);
    let inv_sr = 1.0 / sr as f64;
    let t0 = start as f64 * inv_sr;
    let t1 = (start + out_l.len()) as f64 * inv_sr;
    // The take's span touches this block: lanes fade out under it.
    let take_here = mix.take.filter(|k| k.song || (k.end > t0 - REGION_RAMP_S && k.start < t1 + REGION_RAMP_S));
    if take_here.is_some_and(|k| k.song) {
        // A song take is the whole song: the lanes are silent.
    } else {
        for (ti, s) in stems.iter().enumerate() {
            if mix.gains.get(ti).copied().unwrap_or(0.0) == 0.0 {
                continue;
            }
            let varies = take_here.is_some() || mix.regions.iter().any(|r| r.track == ti);
            let flat = mix.gain_at(ti, t0);
            for i in 0..out_l.len() {
                let f = start + i;
                if f >= s.l.len() {
                    break;
                }
                let g = if varies {
                    let t = f as f64 * inv_sr;
                    mix.gain_at(ti, t) * (1.0 - mix.take_weight(t))
                } else {
                    flat
                };
                out_l[i] += s.l[f] * g;
                out_r[i] += s.r[f] * g;
            }
        }
    }
    if let Some(k) = take_here {
        let Some(s) = stems.get(k.stem) else { return };
        let offset = (k.start.max(0.0) * sr as f64) as usize;
        for i in 0..out_l.len() {
            let f = start + i;
            if f < offset || f - offset >= s.l.len() {
                continue;
            }
            let w = mix.take_weight(f as f64 * inv_sr);
            out_l[i] += s.l[f - offset] * w;
            out_r[i] += s.r[f - offset] * w;
        }
    }
}

/// State shared with the audio callback: transport flags and the published mix.
/// The callback only `try_lock`s `mix` and keeps its own copy when the UI holds it.
pub struct Shared {
    pub playing: AtomicBool,
    pub pos: AtomicU64,
    pub mix: Mutex<MixParams>,
    /// Swapped by generation: the UI publishes a new set and keeps the old one
    /// until `stems_seen` shows the callback switched, so a stem set's final
    /// drop never happens on the audio thread.
    pub stems: Mutex<Stems>,
    pub stems_gen: AtomicU64,
    pub stems_seen: AtomicU64,
}

impl Shared {
    pub fn new(mix: MixParams, stems: Stems) -> Self {
        Self {
            playing: AtomicBool::new(false),
            pos: AtomicU64::new(0),
            mix: Mutex::new(mix),
            stems: Mutex::new(stems),
            stems_gen: AtomicU64::new(0),
            stems_seen: AtomicU64::new(0),
        }
    }
    pub fn pos(&self) -> usize {
        self.pos.load(Ordering::Relaxed) as usize
    }
    pub fn seek(&self, frame: usize) {
        self.pos.store(frame as u64, Ordering::Relaxed)
    }
    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
    pub fn set_playing(&self, v: bool) {
        self.playing.store(v, Ordering::Relaxed)
    }
}

/// Install the stem player on audio output 0. Owns its scratch buffers; the hot
/// path neither allocates (after the first callbacks size them) nor blocks.
/// `POING_MUTE` keeps agent/test instances silent.
pub fn start_output(cx: &mut Cx, shared: Arc<Shared>, sr: u32) {
    let muted = std::env::var_os("POING_MUTE").is_some();
    let mut cache = MixParams::default();
    let mut stems: Stems = shared.stems.lock().map(|s| s.clone()).unwrap_or_default();
    let mut stems_gen = shared.stems_gen.load(Ordering::Acquire);
    let (mut l, mut r) = (Vec::<f32>::with_capacity(8192), Vec::<f32>::with_capacity(8192));
    cx.audio_output(0, move |info, out| {
        out.zero();
        if let Ok(m) = shared.mix.try_lock() {
            if m.version != cache.version || cache.gains.len() != m.gains.len() {
                cache.clone_from(&m);
            }
        }
        let gen = shared.stems_gen.load(Ordering::Acquire);
        if gen != stems_gen {
            if let Ok(s) = shared.stems.try_lock() {
                // The UI still holds the previous set: never its last reference here.
                stems = s.clone();
                stems_gen = gen;
                shared.stems_seen.store(gen, Ordering::Release);
            }
        }
        let total = stems.iter().map(|s| s.l.len()).max().unwrap_or(0);
        if !shared.playing() {
            return;
        }
        let start = shared.pos();
        if start >= total {
            shared.set_playing(false);
            return;
        }
        // Device frames map 1:1 to stem frames only at the stem rate; at another
        // device rate step through the stems by the ratio (nearest frame, fine
        // for monitoring; the render is exact).
        let ratio = sr as f64 / info.sample_rate.max(1.0);
        let dev_frames = out.frame_count();
        let n_src = ((dev_frames as f64 * ratio).ceil() as usize).min(total - start);
        l.resize(n_src, 0.0);
        r.resize(n_src, 0.0);
        mix_into(&stems, &cache, sr, start, &mut l, &mut r);
        if !muted {
            let chans = out.channel_count();
            for i in 0..dev_frames {
                let j = ((i as f64 * ratio) as usize).min(n_src.saturating_sub(1));
                if n_src == 0 {
                    break;
                }
                if chans >= 2 {
                    out.channel_mut(0)[i] = l[j];
                    out.channel_mut(1)[i] = r[j];
                } else {
                    out.channel_mut(0)[i] = 0.5 * (l[j] + r[j]);
                }
            }
        }
        shared.seek(start + n_src);
    });
}

// ---------------------------------------------------------------- jobs worker

pub enum Job {
    Render { stems: Stems, mix: MixParams, sr: u32, out_dir: PathBuf, name: String },
    /// Understand a song: run the analysis sidecar on `audio`, writing into the
    /// `bundle`, then decode the new stems at `sr`.
    Analyze { audio: PathBuf, bundle: PathBuf, sr: u32 },
    /// Render a take with native YuE2 from `fixture` into `bundle/<take.file>`.
    Generate { bundle: PathBuf, take: Take, fixture: Yue2Fixture, sr: u32 },
}

/// What `yue2_render --fixture` reads: style, section-tagged lyrics, seed,
/// chain of thought ("full" keeps the score's chords) and the ABC score.
#[derive(SerJson, Clone)]
pub struct Yue2Fixture {
    pub style: String,
    pub lyrics: String,
    pub seed: u64,
    pub cot: String,
    pub abc: Option<String>,
}

/// Progress and results the worker sends to the UI (after each send it raises
/// the UI signal; the App drains the channel on `Event::Signal`).
pub enum JobUpdate {
    Status(String),
    Done(String),
    /// Analysis progress, 0..1 over the whole job.
    Progress { message: String, progress: f32 },
    /// analysis.json is written and its stems decoded: (track id, file, stem).
    Analyzed { bundle: PathBuf, stems: Vec<(String, String, Stem)> },
    /// A take finished rendering and is decoded at the project rate.
    TakeReady { take: Take, stem: Stem },
}

pub struct Jobs {
    tx: Sender<Job>,
    pub rx: Receiver<JobUpdate>,
}

impl Jobs {
    /// One long-lived worker for the app's lifetime, fed over a channel.
    pub fn start(cx: &Cx) -> Jobs {
        let (tx, job_rx) = std::sync::mpsc::channel::<Job>();
        let (up_tx, rx) = std::sync::mpsc::channel::<JobUpdate>();
        let _ = cx
            .thread_spawner()
            .spawn_worker(ThreadOptions { name: Some("poing-jobs".into()), ..Default::default() }, move || {
                while let Ok(job) = job_rx.recv() {
                    match job {
                        Job::Render { stems, mix, sr, out_dir, name } => render(&stems, &mix, sr, &out_dir, &name, &up_tx),
                        Job::Analyze { audio, bundle, sr } => analyze(&audio, &bundle, sr, &up_tx),
                        Job::Generate { bundle, take, fixture, sr } => generate(&bundle, take, &fixture, sr, &up_tx),
                    }
                }
            })
            .map(|h| h.detach());
        Jobs { tx, rx }
    }

    pub fn submit(&self, job: Job) -> bool {
        self.tx.send(job).is_ok()
    }
}

fn send(tx: &Sender<JobUpdate>, u: JobUpdate) {
    let _ = tx.send(u);
    SignalToUI::set_ui_signal();
}

fn ffmpeg() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("ffmpeg")];
    if let Ok(p) = std::env::var("FFMPEG") {
        candidates.insert(0, PathBuf::from(p));
    }
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        candidates.push(Path::new(&la).join("Microsoft").join("WinGet").join("Links").join("ffmpeg.exe"));
    }
    candidates.into_iter().find(|c| Command::new(c).arg("-version").output().map(|o| o.status.success()).unwrap_or(false))
}

/// 24-bit stereo PCM WAV.
fn write_wav(path: &Path, l: &[f32], r: &[f32], sr: u32, gain: f32) -> std::io::Result<()> {
    let frames = l.len().min(r.len());
    let data_len = (frames * 2 * 3) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&sr.to_le_bytes());
    b.extend_from_slice(&(sr * 6).to_le_bytes());
    b.extend_from_slice(&6u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        for x in [l[i], r[i]] {
            let v = ((x * gain).clamp(-1.0, 1.0) * 8_388_607.0) as i32;
            b.extend_from_slice(&v.to_le_bytes()[..3]);
        }
    }
    std::fs::write(path, b)
}

/// Full mix -> 24-bit WAV, then (when ffmpeg exists) two-pass loudnorm to
/// -14 LUFS / -1 dBTP as a 320k MP3.
pub fn render(stems: &[Arc<Stem>], mix: &MixParams, sr: u32, out_dir: &Path, name: &str, tx: &Sender<JobUpdate>) {
    send(tx, JobUpdate::Status("Rendering mix...".into()));
    let frames = stems.iter().map(|s| s.l.len()).max().unwrap_or(0);
    let (mut l, mut r) = (vec![0f32; frames], vec![0f32; frames]);
    mix_into(stems, mix, sr, 0, &mut l, &mut r);
    let peak = l.iter().chain(r.iter()).fold(0f32, |a, x| a.max(x.abs()));
    let norm = if peak > 0.99 { 0.99 / peak } else { 1.0 };
    let _ = std::fs::create_dir_all(out_dir);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let wav = out_dir.join(format!("mix-{stamp}.wav"));
    if let Err(e) = write_wav(&wav, &l, &r, sr, norm) {
        return send(tx, JobUpdate::Done(format!("Render failed: {e}")));
    }
    let Some(ff) = ffmpeg() else {
        return send(tx, JobUpdate::Done(format!("Rendered {} (no ffmpeg for MP3)", wav.display())));
    };
    send(tx, JobUpdate::Status("Mastering MP3 (loudness pass 1/2)...".into()));
    let target = "I=-14:TP=-1:LRA=11";
    let probe = Command::new(&ff)
        .args(["-hide_banner", "-i"])
        .arg(&wav)
        .args(["-af", &format!("loudnorm={target}:print_format=json"), "-f", "null", "-"])
        .output();
    let measured = probe.ok().and_then(|o| {
        let err = String::from_utf8_lossy(&o.stderr).to_string();
        let s = err.rfind('{')?;
        let e = err[s..].find('}')? + s + 1;
        Some(err[s..e].to_string())
    });
    let Some(m) = measured else {
        return send(tx, JobUpdate::Done(format!("Rendered {} (loudness measurement failed)", wav.display())));
    };
    // ffmpeg prints `"input_i" : "-13.10",` lines; pick the values out without a JSON dependency.
    let field = |k: &str| -> String {
        m.find(&format!("\"{k}\""))
            .and_then(|i| m[i..].split('"').nth(3).map(|v| v.to_string()))
            .unwrap_or_else(|| "0".into())
    };
    let af = format!(
        "loudnorm={target}:measured_I={}:measured_TP={}:measured_LRA={}:measured_thresh={}:offset={}:linear=true",
        field("input_i"),
        field("input_tp"),
        field("input_lra"),
        field("input_thresh"),
        field("target_offset")
    );
    send(tx, JobUpdate::Status("Mastering MP3 (pass 2/2)...".into()));
    let mp3 = wav.with_extension("mp3");
    let ok = Command::new(&ff)
        .args(["-v", "error", "-y", "-i"])
        .arg(&wav)
        .args(["-af", &af, "-ar", "44100", "-codec:a", "libmp3lame", "-b:a", "320k", "-metadata"])
        .arg(format!("title={name}"))
        .arg(&mp3)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    send(
        tx,
        JobUpdate::Done(if ok {
            format!("Rendered {} (input {} LUFS -> -14)", mp3.display(), field("input_i"))
        } else {
            format!("Rendered {} (MP3 encode failed)", wav.display())
        }),
    );
}

// ------------------------------------------------------------ analysis sidecar

/// Where the analysis sidecar (a uv project) lives: `POING_SIDECAR`, else the
/// repository's `sidecar/` next to this crate (development builds).
pub fn sidecar_dir() -> PathBuf {
    std::env::var_os("POING_SIDECAR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("sidecar"))
}

/// One progress line the sidecar prints on stdout.
#[derive(makepad_micro_serde::DeJson)]
struct SidecarLine {
    stage: String,
    progress: Option<f64>,
    message: Option<String>,
}

/// Run the sidecar on `audio` into `bundle`, relaying its progress, then decode
/// the stems it wrote. Its stderr goes to `bundle/analysis.log`.
fn analyze(audio: &Path, bundle: &Path, sr: u32, tx: &Sender<JobUpdate>) {
    use makepad_micro_serde::DeJson;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let progress = |message: &str, progress: f32| send(tx, JobUpdate::Progress { message: message.into(), progress });
    progress("Starting analysis", 0.0);
    let dir = sidecar_dir();
    let log = match std::fs::File::create(bundle.join("analysis.log")) {
        Ok(f) => f,
        Err(e) => return send(tx, JobUpdate::Done(format!("Analysis failed: {e}"))),
    };
    let mut cmd = Command::new("uv");
    cmd.arg("run").arg("--project").arg(&dir).arg("python").arg(dir.join("poing_analyze.py")).arg(audio).arg(bundle)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log));
    // SheetSage2 decodes audio with the ffmpeg CLI; put the one we found on PATH.
    if let Some(ff) = ffmpeg().and_then(|f| f.parent().map(Path::to_path_buf)).filter(|p| !p.as_os_str().is_empty()) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![ff];
        paths.extend(std::env::split_paths(&path));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return send(tx, JobUpdate::Done(format!("Analysis needs `uv` on PATH ({e})"))),
    };
    let mut error = None;
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(|l| l.ok()) {
            let Ok(l) = SidecarLine::deserialize_json_lenient(&line) else { continue };
            let message = l.message.unwrap_or_default();
            if l.stage == "error" {
                error = Some(message);
            } else {
                progress(&message, l.progress.unwrap_or(0.0) as f32);
            }
        }
    }
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    if !ok || error.is_some() {
        let why = error.unwrap_or_else(|| "the sidecar exited with an error".into());
        return send(tx, JobUpdate::Done(format!("Analysis failed: {why} (see {})", bundle.join("analysis.log").display())));
    }
    progress("Loading stems", 1.0);
    let mut stems = Vec::new();
    for (id, _) in poing_model::STEMS {
        let file = format!("stems/{id}.wav");
        match load_stem(&bundle.join(&file), sr) {
            Ok(stem) => stems.push((id.to_string(), file, stem)),
            Err(e) => return send(tx, JobUpdate::Done(format!("Analysis failed: {e}"))),
        }
    }
    send(tx, JobUpdate::Analyzed { bundle: bundle.to_path_buf(), stems });
}

/// A Hugging Face model's local snapshot: `HF_HUB_CACHE`, `HF_HOME/hub`, else
/// `~/.cache/huggingface/hub`, newest snapshot first.
fn hf_snapshot(repo: &str) -> Option<PathBuf> {
    let hub = std::env::var_os("HF_HUB_CACHE").map(PathBuf::from).or_else(|| std::env::var_os("HF_HOME").map(|h| PathBuf::from(h).join("hub"))).or_else(|| {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        Some(PathBuf::from(home).join(".cache").join("huggingface").join("hub"))
    })?;
    let snaps = hub.join(format!("models--{}", repo.replace('/', "--"))).join("snapshots");
    let mut dirs: Vec<_> = std::fs::read_dir(snaps).ok()?.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).collect();
    dirs.sort_by_key(|e| std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()));
    dirs.first().map(|e| e.path())
}

/// Native YuE2 (Makepad's `yue2_render`): `POING_YUE2_RENDER`, else on PATH.
/// Weights: `POING_YUE2_MODEL` / `POING_YUE2_VAE`, else the Hugging Face cache.
fn yue2_command() -> Result<(Command, PathBuf, PathBuf), String> {
    let exe = std::env::var_os("POING_YUE2_RENDER").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("yue2_render"));
    let model = std::env::var_os("POING_YUE2_MODEL").map(PathBuf::from).or_else(|| hf_snapshot("m-a-p/YuE2-3B"))
        .ok_or("YuE2-3B weights not found (set POING_YUE2_MODEL)")?;
    let vae = std::env::var_os("POING_YUE2_VAE").map(PathBuf::from).or_else(|| hf_snapshot("m-a-p/YuE2-Vae"))
        .ok_or("YuE2-Vae weights not found (set POING_YUE2_VAE)")?;
    let mut cmd = Command::new(exe);
    // The CUDA runtime DLLs live in the toolkit's bin\x64 since CUDA 13.
    #[cfg(windows)]
    if let Some(cuda) = cuda_root() {
        let mut paths = vec![cuda.join("bin").join("x64"), cuda.join("bin")];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    Ok((cmd, model, vae))
}

/// The CUDA toolkit: `CUDA_PATH` (set by its installer, but missing in
/// processes started before the install), else the newest default install.
#[cfg(windows)]
fn cuda_root() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CUDA_PATH") {
        return Some(PathBuf::from(p));
    }
    let base = PathBuf::from(std::env::var_os("ProgramFiles")?).join("NVIDIA GPU Computing Toolkit").join("CUDA");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(base).ok()?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
    versions.sort_by_key(|p| {
        let v = p.file_name().and_then(|n| n.to_str()).unwrap_or("").trim_start_matches('v').to_string();
        v.split('.').map(|n| n.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>()
    });
    versions.pop()
}

/// Map a `yue2_render` progress line ("Codec { done: 812, max: 6000 }") to 0..1
/// over the whole render.
fn yue2_progress(line: &str) -> Option<(&'static str, f32)> {
    let (stage, rest) = line.split_once(" {")?;
    let nums: Vec<f32> = rest.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect();
    let frac = match nums.as_slice() {
        [done, total, ..] if *total > 0.0 => (done / total).clamp(0.0, 1.0),
        _ => return None,
    };
    let (label, a, b) = match stage.trim() {
        "Score" => ("Planning", 0.0, 0.05),
        "Codec" => ("Singing", 0.05, 0.5),
        "Flow" => ("Shaping the sound", 0.5, 0.95),
        "Decode" => ("Decoding", 0.95, 1.0),
        _ => return None,
    };
    Some((label, a + (b - a) * frac))
}

/// Render one take with native YuE2, relaying its progress, then decode it.
/// The fixture, the render log and the raw output stay in `takes/<id>/`.
fn generate(bundle: &Path, take: Take, fixture: &Yue2Fixture, sr: u32, tx: &Sender<JobUpdate>) {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let fail = |why: String| send(tx, JobUpdate::Done(format!("Take {} failed: {why}", take.id)));
    let progress = |message: &str, progress: f32| send(tx, JobUpdate::Progress { message: message.into(), progress });
    progress(&format!("Take {}  ·  Starting YuE2", take.id.trim_start_matches('t')), 0.0);
    let work = bundle.join("takes").join(&take.id);
    if let Err(e) = std::fs::create_dir_all(&work) {
        return fail(e.to_string());
    }
    let fixture_path = work.join("fixture.json");
    if let Err(e) = std::fs::write(&fixture_path, fixture.serialize_json()) {
        return fail(e.to_string());
    }
    let (mut cmd, model, vae) = match yue2_command() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    cmd.arg("--model").arg(model).arg("--vae").arg(vae).arg("--fixture").arg(&fixture_path).arg("--out").arg(&work)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("yue2_render did not start ({e}); set POING_YUE2_RENDER")),
    };
    let mut log = std::fs::File::create(work.join("render.log")).ok();
    if let Some(err) = child.stderr.take() {
        for line in BufReader::new(err).lines().map_while(|l| l.ok()) {
            if let Some(f) = log.as_mut() {
                let _ = writeln!(f, "{line}");
            }
            if let Some((label, p)) = yue2_progress(&line) {
                progress(&format!("Take {}  ·  {label}", take.id.trim_start_matches('t')), p);
            }
        }
    }
    match child.wait() {
        Ok(s) if s.success() => {}
        Ok(s) => {
            // 0xC0000135 on Windows: a DLL (the CUDA runtime) was not found.
            let code = s.code().map(|c| format!("{:#X}", c as u32)).unwrap_or_default();
            return fail(format!("yue2_render exited with {code} (see {})", work.join("render.log").display()));
        }
        Err(e) => return fail(e.to_string()),
    }
    let wav = work.join("song.wav");
    let dest = bundle.join(&take.file);
    if let Err(e) = std::fs::rename(&wav, &dest).or_else(|_| std::fs::copy(&wav, &dest).map(|_| ())) {
        return fail(format!("move {}: {e}", wav.display()));
    }
    match load_stem(&dest, sr) {
        Ok(stem) => send(tx, JobUpdate::TakeReady { take, stem }),
        Err(e) => fail(e),
    }
}

#[cfg(test)]
mod tests {
    use super::{mix_into, yue2_progress, MixParams, MixTake, Stem};
    use std::sync::Arc;

    fn constant(v: f32, frames: usize) -> Arc<Stem> {
        Arc::new(Stem { l: vec![v; frames], r: vec![v; frames], peaks: Vec::new(), peak_abs: v })
    }

    #[test]
    fn a_song_take_replaces_the_lanes() {
        let stems = vec![constant(0.25, 1000), constant(1.0, 1500)];
        let mix = MixParams { gains: vec![1.0], take: Some(MixTake { stem: 1, start: 0.0, end: 1.5, song: true }), ..Default::default() };
        let (mut l, mut r) = (vec![0.0; 100], vec![0.0; 100]);
        mix_into(&stems, &mix, 1000, 1200, &mut l, &mut r);
        assert!(l.iter().chain(&r).all(|&x| x == 1.0), "past the lanes' end the take still plays alone");
    }

    #[test]
    fn a_section_take_crossfades_in_its_range() {
        // lanes 0.25 everywhere; a 0.4 s take placed at 0.3 s
        let stems = vec![constant(0.25, 1000), constant(1.0, 400)];
        let mix = MixParams { gains: vec![1.0], take: Some(MixTake { stem: 1, start: 0.3, end: 0.7, song: false }), ..Default::default() };
        let (mut l, mut r) = (vec![0.0; 1000], vec![0.0; 1000]);
        mix_into(&stems, &mix, 1000, 0, &mut l, &mut r);
        assert_eq!(l[100], 0.25, "before the take: lanes");
        assert_eq!(l[500], 1.0, "inside: the take alone");
        assert_eq!(l[900], 0.25, "after: lanes again");
        assert!(l[305] > 0.25 && l[305] < 1.0, "the edge is a ramp, not a click");
    }

    #[test]
    fn yue2_progress_lines() {
        assert_eq!(yue2_progress("Flow { done: 16, total: 32 }"), Some(("Shaping the sound", 0.725)));
        assert_eq!(yue2_progress("Codec { done: 0, max: 6000 }"), Some(("Singing", 0.05)));
        assert_eq!(yue2_progress("codec: 3306 tokens 18.34s (196.3 tok/s) = 132.2s audio"), None);
    }
}