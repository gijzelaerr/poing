//! Stem loading (makepad-audio-decode: wav/flac/mp3/ogg), waveform peaks, the
//! mixer (shared by live playback and the offline render) and the job worker.

use crate::model::Project;
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

/// Everything the mixer needs; rebuilt from the Project whenever it changes.
#[derive(Clone, Default)]
pub struct MixParams {
    pub version: u64,
    pub gains: Vec<f32>,
    pub regions: Vec<MixRegion>,
}

impl MixParams {
    pub fn from_project(p: &Project, version: u64) -> Self {
        let regions = p
            .regions()
            .iter()
            .filter_map(|r| {
                let track = p.track_index(&r.track)?;
                let factor = r.factor();
                (factor != 1.0).then(|| MixRegion { track, start: r.start, end: r.end, factor })
            })
            .collect();
        Self { version, gains: p.track_gains(), regions }
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
pub fn mix_into(stems: &[Stem], mix: &MixParams, sr: u32, start: usize, out_l: &mut [f32], out_r: &mut [f32]) {
    out_l.iter_mut().for_each(|x| *x = 0.0);
    out_r.iter_mut().for_each(|x| *x = 0.0);
    let inv_sr = 1.0 / sr as f64;
    for (ti, s) in stems.iter().enumerate() {
        if mix.gains.get(ti).copied().unwrap_or(0.0) == 0.0 {
            continue;
        }
        let has_regions = mix.regions.iter().any(|r| r.track == ti);
        let flat = mix.gain_at(ti, start as f64 * inv_sr);
        for i in 0..out_l.len() {
            let f = start + i;
            if f >= s.l.len() {
                break;
            }
            let g = if has_regions { mix.gain_at(ti, f as f64 * inv_sr) } else { flat };
            out_l[i] += s.l[f] * g;
            out_r[i] += s.r[f] * g;
        }
    }
}

/// State shared with the audio callback: transport flags and the published mix.
/// The callback only `try_lock`s `mix` and keeps its own copy when the UI holds it.
pub struct Shared {
    pub playing: AtomicBool,
    pub pos: AtomicU64,
    pub mix: Mutex<MixParams>,
}

impl Shared {
    pub fn new(mix: MixParams) -> Self {
        Self { playing: AtomicBool::new(false), pos: AtomicU64::new(0), mix: Mutex::new(mix) }
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
pub fn start_output(cx: &mut Cx, stems: Arc<Vec<Stem>>, shared: Arc<Shared>, sr: u32) {
    let total = stems.iter().map(|s| s.l.len()).max().unwrap_or(0);
    let muted = std::env::var_os("POING_MUTE").is_some();
    let mut cache = MixParams::default();
    let (mut l, mut r) = (Vec::<f32>::with_capacity(8192), Vec::<f32>::with_capacity(8192));
    cx.audio_output(0, move |info, out| {
        out.zero();
        if let Ok(m) = shared.mix.try_lock() {
            if m.version != cache.version || cache.gains.len() != m.gains.len() {
                cache.clone_from(&m);
            }
        }
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
    Render { stems: Arc<Vec<Stem>>, mix: MixParams, sr: u32, out_dir: PathBuf, name: String },
}

/// Progress and results the worker sends to the UI (after each send it raises
/// the UI signal; the App drains the channel on `Event::Signal`).
pub enum JobUpdate {
    Status(String),
    Done(String),
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
pub fn render(stems: &[Stem], mix: &MixParams, sr: u32, out_dir: &Path, name: &str, tx: &Sender<JobUpdate>) {
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
