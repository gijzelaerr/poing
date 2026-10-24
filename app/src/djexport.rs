//! Export for DJ: the song as it plays and chosen versions, rendered as
//! 44.1 kHz WAV (or 320k MP3), with a rekordbox XML library that carries
//! Poing's beatgrid, key and section cues. rekordbox imports it from its
//! "rekordbox xml" pane and exports to USB / OneLibrary for the OMNIS-DUO.
//!
//! Every song exported lands in one folder (`<songs>/rekordbox`) beside one
//! `Poing.xml`, rebuilt from the per-song manifests there, so rekordbox's
//! preference points at a single file that grows a playlist per song.

use crate::audio::{self, mix_into, JobUpdate, MixParams, Stems};
use crate::export::{safe_name, unique_names, xml_escape};
use crate::fx::FxRack;
use makepad_micro_serde::*;
use poing_model::Section;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;

/// Peak level of the renders (-1 dBFS): room for the MP3 encoder.
const PEAK: f32 = 0.891;
/// The sample rate rekordbox and AlphaTheta players are happiest with.
const DJ_RATE: u32 = 44100;

/// One file to render: a mix of the song, its own rack, whether it keeps the
/// analysed timing (then it gets the grid and cues).
pub struct DjRender {
    pub label: String,
    pub mix: MixParams,
    pub rack: Box<FxRack>,
    pub frames: usize,
    pub timed: bool,
}

/// What the analysis says about the song's timing and sections.
#[derive(Clone, Debug, Default)]
pub struct DjTiming {
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub per_bar: u32,
    pub tempo: Option<f64>,
    pub key: Option<String>,
    pub sections: Vec<Section>,
}

pub struct DjJob {
    pub stems: Stems,
    pub sr: u32,
    pub song: String,
    pub renders: Vec<DjRender>,
    pub mp3: bool,
    /// The folder with the files, the manifests and `Poing.xml`.
    pub dir: PathBuf,
    /// This song's manifest name (unique per song bundle).
    pub manifest: String,
    pub timing: DjTiming,
}

// ------------------------------------------------------------- the manifest

#[derive(SerJson, DeJson, Clone, Debug, PartialEq)]
pub struct DjMark {
    /// Seconds.
    pub start: f64,
    pub bpm: f64,
    /// Beat of the bar at `start` (1 = the downbeat).
    pub beat: u32,
}

#[derive(SerJson, DeJson, Clone, Debug, PartialEq)]
pub struct DjCue {
    pub name: String,
    pub start: f64,
    /// -1 a memory cue; 0..=3 hot cue A..D.
    pub num: i32,
}

#[derive(SerJson, DeJson, Clone, Debug)]
pub struct DjTrack {
    pub name: String,
    pub version: String,
    pub file: String,
    pub kind: String,
    pub size: u64,
    pub seconds: f64,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub per_bar: u32,
    pub grid: Vec<DjMark>,
    pub cues: Vec<DjCue>,
}

/// One song's exported tracks (`<manifest>.json` in the DJ folder).
#[derive(SerJson, DeJson, Clone, Debug)]
pub struct DjSong {
    pub name: String,
    pub date: String,
    pub tracks: Vec<DjTrack>,
}

// --------------------------------------------------------------- the job

pub fn run(job: DjJob, tx: &Sender<JobUpdate>) -> Result<String, String> {
    std::fs::create_dir_all(&job.dir).map_err(|e| format!("{}: {e}", job.dir.display()))?;
    let ff = audio::ffmpeg();
    let total = job.renders.len();
    let names = unique_names(&job.renders.iter().map(|r| safe_name(&format!("{} - {}", job.song, r.label), 120)).collect::<Vec<_>>());
    let mut tracks = Vec::new();
    let mut notes = Vec::new();
    for (i, (mut r, name)) in job.renders.into_iter().zip(names).enumerate() {
        audio::send(tx, JobUpdate::Status(format!("Export for DJ ({}/{total}: {})...", i + 1, r.label)));
        let (mut l, mut rr) = (vec![0f32; r.frames], vec![0f32; r.frames]);
        mix_into(&job.stems, &r.mix, job.sr, 0, &mut l, &mut rr, Some(&mut r.rack));
        let peak = l.iter().chain(rr.iter()).fold(0f32, |a, x| a.max(x.abs()));
        let gain = if peak > 1e-6 { PEAK / peak } else { 1.0 };
        let (path, kind, bitrate, rate, note) = encode(&job.dir, &name, &l, &rr, job.sr, gain, job.mp3, ff.as_deref(), &format!("{} ({})", job.song, r.label))?;
        if let Some(n) = note {
            notes.push(n);
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let seconds = r.frames as f64 / job.sr as f64;
        let (grid, bpm) = if r.timed { dj_grid(&job.timing.downbeats, &job.timing.beats, job.timing.per_bar, seconds) } else { (Vec::new(), None) };
        let bpm = bpm.or(if r.timed { job.timing.tempo.map(round2) } else { None });
        let cues = if r.timed && !grid.is_empty() { dj_cues(&job.timing.sections, &grid, job.timing.per_bar, seconds) } else { Vec::new() };
        tracks.push(DjTrack {
            name: format!("{} ({})", job.song, r.label),
            version: r.label.clone(),
            file: path.to_string_lossy().into_owned(),
            kind: kind.into(),
            size,
            seconds,
            bitrate,
            sample_rate: rate,
            bpm,
            key: if r.timed { job.timing.key.as_deref().and_then(rekordbox_key) } else { None },
            per_bar: job.timing.per_bar.max(1),
            grid,
            cues,
        });
    }
    let n = tracks.len();
    let song = DjSong { name: job.song.clone(), date: today(), tracks };
    let manifest = job.dir.join(format!("{}.json", job.manifest));
    std::fs::write(&manifest, song.serialize_json()).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let xml_path = job.dir.join("Poing.xml");
    let songs = read_manifests(&job.dir);
    std::fs::write(&xml_path, rekordbox_xml(&songs)).map_err(|e| format!("{}: {e}", xml_path.display()))?;
    let mut line = format!("Exported {n} track{} for DJ into {} (rekordbox: {})", if n == 1 { "" } else { "s" }, job.dir.display(), xml_path.display());
    if !notes.is_empty() {
        notes.dedup();
        line.push_str(&format!("; {}", notes.join("; ")));
    }
    Ok(line)
}

/// Write one render as the DJ format: (path, rekordbox kind, kbps, rate, a note when it fell back).
#[allow(clippy::too_many_arguments)]
fn encode(dir: &Path, name: &str, l: &[f32], r: &[f32], sr: u32, gain: f32, mp3: bool, ff: Option<&Path>, title: &str) -> Result<(PathBuf, &'static str, u32, u32, Option<String>), String> {
    let wav = dir.join(format!("{name}.wav"));
    let tmp = dir.join(format!("{name}.part.wav"));
    let fail = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let wav_kbps = |rate: u32| rate * 2 * 24 / 1000;
    let Some(ff) = ff else {
        // no ffmpeg: a WAV at the song's rate (rekordbox plays it; not converted)
        audio::write_wav(&wav, l, r, sr, gain).map_err(|e| fail(&wav, e))?;
        return Ok((wav, "WAV File", wav_kbps(sr), sr, Some(format!("no ffmpeg: WAV at {sr} Hz"))));
    };
    if !mp3 && sr == DJ_RATE {
        audio::write_wav(&wav, l, r, sr, gain).map_err(|e| fail(&wav, e))?;
        return Ok((wav, "WAV File", wav_kbps(sr), sr, None));
    }
    let scaled = |x: &[f32]| x.iter().map(|v| v * gain).collect::<Vec<f32>>();
    std::fs::write(&tmp, crate::export::wav_f32_bytes(&scaled(l), &scaled(r), sr)).map_err(|e| fail(&tmp, e))?;
    // ffmpeg resamples (and encodes the MP3); a WAV it would write as
    // WAVE_FORMAT_EXTENSIBLE, which some DJ players refuse, so it hands back
    // raw samples and Poing writes a plain PCM WAV
    let raw = dir.join(format!("{name}.part.f32"));
    let out = if mp3 { dir.join(format!("{name}.mp3")) } else { raw.clone() };
    let codec: &[&str] = if mp3 { &["-c:a", "libmp3lame", "-b:a", "320k"] } else { &["-f", "f32le", "-c:a", "pcm_f32le", "-ac", "2"] };
    let convert = |resampler: Option<&str>| {
        let mut c = Command::new(ff);
        c.args(["-v", "error", "-y", "-i"]).arg(&tmp);
        if let Some(af) = resampler {
            c.args(["-af", af]);
        }
        c.args(["-ar", "44100"]).args(codec).args(["-metadata"]).arg(format!("title={title}")).arg(&out);
        c.status().map(|s| s.success()).unwrap_or(false)
    };
    let ok = convert(Some("aresample=resampler=soxr")) || convert(None);
    let _ = std::fs::remove_file(&tmp);
    if !ok {
        let _ = std::fs::remove_file(&raw);
        return Err(format!("ffmpeg could not write {}", out.display()));
    }
    if mp3 {
        return Ok((out, "MP3 File", 320, DJ_RATE, None));
    }
    let bytes = std::fs::read(&raw).map_err(|e| fail(&raw, e));
    let _ = std::fs::remove_file(&raw);
    let samples: Vec<f32> = bytes?.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    let (l44, r44): (Vec<f32>, Vec<f32>) = samples.chunks_exact(2).map(|f| (f[0], f[1])).unzip();
    audio::write_wav(&wav, &l44, &r44, DJ_RATE, 1.0).map_err(|e| fail(&wav, e))?;
    Ok((wav, "WAV File", wav_kbps(DJ_RATE), DJ_RATE, None))
}

fn read_manifests(dir: &Path) -> Vec<DjSong> {
    let mut songs: Vec<DjSong> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                .filter_map(|s| DjSong::deserialize_json(&s).ok())
                .map(|mut s| {
                    s.tracks.retain(|t| Path::new(&t.file).exists());
                    s
                })
                .filter(|s| !s.tracks.is_empty())
                .collect()
        })
        .unwrap_or_default();
    songs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    songs
}

// ------------------------------------------------------------- beat grid

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Least-squares line through (x, y): (intercept, slope).
fn fit(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = points.len() as f64;
    if points.len() < 2 {
        return None;
    }
    let (sx, sy) = points.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
    let (mx, my) = (sx / n, sy / n);
    let (sxx, sxy) = points.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + (x - mx) * (x - mx), b + (x - mx) * (y - my)));
    (sxx > 0.0).then(|| {
        let slope = sxy / sxx;
        (my - slope * mx, slope)
    })
}

/// Marks along `bars` (each with its beats up to the next; one beat each when
/// `per` is 1) as rekordbox stores them (bpm to 2 decimals): a mark carries on
/// until it is more than 8 ms off the next bar, then a new mark starts there.
/// After a short bar a mark starts too, since rekordbox counts bars from a
/// mark's beat 1.
fn marks_along(bars: &[(f64, u32)], per: u32) -> Vec<DjMark> {
    let mut marks: Vec<DjMark> = Vec::new();
    // beats from the last mark to bar i
    let mut beats = 0u32;
    let mut after_short = false;
    for w in bars.windows(2) {
        let ((t0, n), t1) = (w[0], w[1].0);
        let bpm = round2(60.0 * n as f64 / (t1 - t0));
        let on_track = !after_short
            && marks.last().is_some_and(|m| {
                let p0 = m.start + beats as f64 * 60.0 / m.bpm;
                let p1 = p0 + n as f64 * 60.0 / m.bpm;
                (p0 - t0).abs() <= 0.008 && (p1 - t1).abs() <= 0.008
            });
        if !on_track {
            marks.push(DjMark { start: t0, bpm, beat: 1 });
            beats = 0;
        }
        beats += n;
        after_short = n != per;
    }
    marks
}

/// The beatgrid for rekordbox from the analysis: when the bars sit on a
/// steady tempo (the usual case; the first beats of an analysis can be
/// noise), one tempo from a line fitted through the downbeats, starting at
/// the first bar at or after 0 s; otherwise a mark wherever the tempo moves.
/// Also the average bpm. Empty when the analysis has no usable grid.
pub fn dj_grid(downbeats: &[f64], beats: &[f64], per_bar: u32, seconds: f64) -> (Vec<DjMark>, Option<f64>) {
    let per_bar = per_bar.max(1);
    let (times, per) = if downbeats.len() >= 4 { (downbeats, per_bar) } else { (beats, 1) };
    let mut t: Vec<f64> = times.iter().copied().filter(|x| x.is_finite() && *x >= -1.0 && *x <= seconds + 1.0).collect();
    t.sort_by(f64::total_cmp);
    t.dedup_by(|a, b| (*a - *b).abs() < 0.05);
    if t.len() < 4 {
        return (Vec::new(), None);
    }
    let mut gaps: Vec<f64> = t.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_by(f64::total_cmp);
    let unit = gaps[gaps.len() / 2];
    // number each time by its bar from the middle one, fit, drop outliers, fit again
    let mid = t[t.len() / 2];
    let pts: Vec<(f64, f64)> = t.iter().map(|&x| (((x - mid) / unit).round(), x)).collect();
    let Some(mut line) = fit(&pts) else { return (Vec::new(), None) };
    let mut inliers = pts.clone();
    for _ in 0..2 {
        inliers = pts.iter().copied().filter(|(k, x)| (line.0 + line.1 * k - x).abs() < 0.1 * unit).collect();
        if let Some(l) = fit(&inliers) {
            line = l;
        }
    }
    let worst = inliers.iter().map(|(k, x)| (line.0 + line.1 * k - x).abs()).fold(0.0, f64::max);
    let (a, b) = line;
    let avg = round2(60.0 * per as f64 / b);
    let steady = inliers.len() * 4 >= pts.len() * 3 && worst < 0.03 && b > 0.0;
    let marks = if steady {
        // the fitted bars from the first at or after 0 s to the end
        let k0 = ((-a) / b - 1e-9).ceil();
        let k1 = ((seconds - a) / b).floor();
        let fitted: Vec<f64> = (k0 as i64..=k1 as i64 + 1).map(|k| a + b * k as f64).collect();
        marks_along(&fitted.iter().map(|&x| (x, per)).collect::<Vec<_>>(), per)
    } else {
        marks_along(&follow(&t, unit, per), per)
    };
    // per beat (no downbeats): rekordbox still wants beat 1 of a bar
    (marks, Some(avg))
}

/// The bars of a drifting song from the analysis: a stray time between two
/// bars goes (of two close ones, the one that keeps the bar nearer the usual
/// length stays), a missed bar is filled in evenly, so the grid only moves
/// where the music does.
///
/// Each bar comes with its beats up to the next one: a bar shorter by whole
/// beats is kept as such (the analysis moves the bar there, and the sections
/// follow it), so the tempo holds and only the bar line moves.
fn follow(t: &[f64], unit: f64, per: u32) -> Vec<(f64, u32)> {
    let beat = unit / per as f64;
    let mut kept = vec![(t[0], per)];
    for &x in &t[1..] {
        let last = kept[kept.len() - 1].0;
        let gap = x - last;
        // a bar of about the usual length is one bar, however it drifts;
        // only a clearly short one is counted in beats
        let q = gap / beat;
        let k = if gap >= 1.5 * unit {
            per * (gap / unit).round() as u32
        } else if gap >= 0.875 * unit {
            per
        } else {
            q.round() as u32
        };
        if k == 0 || (gap < 0.875 * unit && (q - k as f64).abs() > 0.3) {
            // a stray: x replaces the last bar if it makes the bar before it better
            let n = kept.len();
            let better = if n >= 2 {
                let prev = kept[n - 2].0;
                ((x - prev) - unit).abs() < ((last - prev) - unit).abs()
            } else {
                // the first bar: keep the one whose next bar is nearer the usual length
                t.iter().find(|&&y| y > x + 0.75 * unit).is_some_and(|&y| ((y - x) - unit).abs() < ((y - last) - unit).abs())
            };
            if better {
                kept[n - 1].0 = x;
            }
            continue;
        }
        // k beats: whole bars (a missed one filled in), then what is left as a short bar
        let step = gap / k as f64;
        let n = kept.len();
        kept[n - 1].1 = k.min(per);
        let mut done = k.min(per);
        while done < k {
            let len = (k - done).min(per);
            kept.push((last + done as f64 * step, len));
            done += len;
        }
        kept.push((x, per));
    }
    // two short bars that make one whole bar: a stray inside it, not a move
    let mut i = 0;
    while i + 1 < kept.len() {
        if kept[i].1 < per && kept[i + 1].1 < per && kept[i].1 + kept[i + 1].1 == per {
            kept[i].1 = per;
            kept.remove(i + 1);
        }
        i += 1;
    }
    // a bar the analysis put off the beat: the two whole bars on each side
    // agree where it belongs, and it is not there
    for i in 2..kept.len().saturating_sub(2) {
        if kept[i - 2..=i + 1].iter().any(|b| b.1 != per) {
            continue;
        }
        let left = 2.0 * kept[i - 1].0 - kept[i - 2].0;
        let right = 2.0 * kept[i + 1].0 - kept[i + 2].0;
        let mid = 0.5 * (left + right);
        if (left - right).abs() < 0.04 && (kept[i].0 - mid).abs() > 0.04 {
            kept[i].0 = mid;
        }
    }
    kept
}

/// Beat and bar line times of a grid up to `seconds`: (beats, bars).
pub fn grid_lines(grid: &[DjMark], per_bar: u32, seconds: f64) -> (Vec<f64>, Vec<f64>) {
    let (mut beats, mut bars) = (Vec::new(), Vec::new());
    // the first mark's grid also runs back to the start, as rekordbox draws it
    if let Some(m) = grid.first() {
        let step = 60.0 / m.bpm;
        let mut back = Vec::new();
        let mut j = 1usize;
        while m.start - j as f64 * step >= 0.0 {
            back.push((m.start - j as f64 * step, (m.beat as i64 - 1 - j as i64).rem_euclid(per_bar.max(1) as i64) == 0));
            j += 1;
        }
        for (t, bar) in back.into_iter().rev() {
            beats.push(t);
            if bar {
                bars.push(t);
            }
        }
    }
    for (i, m) in grid.iter().enumerate() {
        let until = grid.get(i + 1).map_or(seconds, |n| n.start - 1e-6);
        let step = 60.0 / m.bpm;
        let mut k = 0usize;
        loop {
            let t = m.start + k as f64 * step;
            if t > until {
                break;
            }
            beats.push(t);
            if (m.beat as usize - 1 + k) % per_bar.max(1) as usize == 0 {
                bars.push(t);
            }
            k += 1;
        }
    }
    (beats, bars)
}

// ------------------------------------------------------------------- cues

fn title_case(s: &str) -> String {
    let s = s.replace(['_', '-'], " ");
    let mut c = s.trim().chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => "Section".into(),
    }
}

/// Memory cues at each section (named, numbered when a name repeats) and hot
/// cues A intro, B first chorus, C bridge or drop (else the last chorus),
/// D outro (else the last section), on the bar (or beat) nearest each start.
pub fn dj_cues(sections: &[Section], grid: &[DjMark], per_bar: u32, seconds: f64) -> Vec<DjCue> {
    let (beats, bars) = grid_lines(grid, per_bar, seconds);
    let beat_len = grid.first().map_or(0.5, |m| 60.0 / m.bpm);
    let nearest = |lines: &[f64], t: f64, within: f64| lines.iter().copied().min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs())).filter(|x| (x - t).abs() <= within);
    let snap = |t: f64| nearest(&bars, t, beat_len * 0.5).or_else(|| nearest(&beats, t, beat_len * 0.5)).unwrap_or(t).max(0.0);
    // the first section's cues sit on the first bar, so a mix-in starts on the one
    let first_bar = |t: f64| bars.iter().copied().find(|b| *b >= t - beat_len * 0.5).unwrap_or(snap(t));
    let mut sections: Vec<&Section> = sections.iter().filter(|s| s.start < seconds).collect();
    sections.sort_by(|a, b| a.start.total_cmp(&b.start));
    let labels: Vec<String> = sections.iter().map(|s| title_case(&s.label)).collect();
    let mut cues = Vec::new();
    let mut count = std::collections::HashMap::<&str, usize>::new();
    for (i, (s, label)) in sections.iter().zip(&labels).enumerate() {
        let repeats = labels.iter().filter(|l| *l == label).count() > 1;
        let n = count.entry(label.as_str()).or_insert(0);
        *n += 1;
        let name = if repeats { format!("{label} {n}") } else { label.clone() };
        cues.push(DjCue { name, start: if i == 0 { first_bar(s.start) } else { snap(s.start) }, num: -1 });
    }
    let is = |s: &Section, words: &[&str]| {
        let l = s.label.to_lowercase();
        words.iter().any(|w| l.contains(w))
    };
    let chorus = |s: &Section| is(s, &["chorus", "refrein", "hook"]) && !is(s, &["pre"]);
    let find = |f: &dyn Fn(&Section) -> bool| sections.iter().position(|s| f(s));
    let rfind = |f: &dyn Fn(&Section) -> bool| sections.iter().rposition(|s| f(s));
    let mut hot: Vec<(i32, usize)> = Vec::new();
    if !sections.is_empty() {
        hot.push((0, 0));
    }
    let first_chorus = find(&chorus);
    if let Some(i) = first_chorus {
        hot.push((1, i));
    }
    if let Some(i) = find(&|s| is(s, &["bridge", "drop", "break"])).or_else(|| rfind(&chorus).filter(|i| Some(*i) != first_chorus)) {
        hot.push((2, i));
    }
    let last = sections.len().checked_sub(1).filter(|l| hot.iter().all(|(_, h)| h != l));
    if let Some(i) = rfind(&|s| is(s, &["outro", "end", "coda"])).or(last) {
        hot.push((3, i));
    }
    // hot cues are named as the section's memory cue ("Chorus 3"), at the same place
    for (num, i) in hot {
        let DjCue { name, start, .. } = cues[i].clone();
        cues.push(DjCue { name, start, num });
    }
    cues
}

// ------------------------------------------------------------ rekordbox XML

/// A key from the analysis ("G major", "F# minor", "Bbm") in rekordbox's
/// spelling (as in its own key list: "Abm", "Db", "F#", "F#m", ...).
pub fn rekordbox_key(key: &str) -> Option<String> {
    let k = key.trim();
    let mut chars = k.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    let base = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    let (shift, rest) = match rest.chars().next() {
        Some('#') | Some('♯') => (1, &rest[rest.chars().next().unwrap().len_utf8()..]),
        Some('b') | Some('♭') => (-1, &rest[rest.chars().next().unwrap().len_utf8()..]),
        _ => (0, rest),
    };
    let r = rest.trim_start_matches([':', ' ', '_', '-']).trim().to_lowercase();
    let minor = r.starts_with("min") || r == "m" || r.starts_with("m ") || r == "minor";
    let pc = (base + shift + 12) % 12;
    const MAJOR: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
    const MINOR: [&str; 12] = ["Cm", "Dbm", "Dm", "Ebm", "Em", "Fm", "F#m", "Gm", "Abm", "Am", "Bbm", "Bm"];
    Some(if minor { MINOR[pc as usize] } else { MAJOR[pc as usize] }.to_string())
}

/// A file path as rekordbox's Location: a file://localhost/ URI, UTF-8
/// percent-encoded.
pub fn file_uri(path: &str) -> String {
    let p = path.replace('\\', "/");
    let p = p.strip_prefix("//?/").unwrap_or(&p);
    let mut out = String::from("file://localhost");
    if !p.starts_with('/') {
        out.push('/');
    }
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Today as yyyy-mm-dd (UTC).
pub fn today() -> String {
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 86400).unwrap_or(0) as i64;
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The rekordbox XML library: every song's tracks in the collection, with
/// grid and cues, and a playlist per song in a "Poing" folder.
pub fn rekordbox_xml(songs: &[DjSong]) -> String {
    let e = xml_escape;
    let total: usize = songs.iter().map(|s| s.tracks.len()).sum();
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<DJ_PLAYLISTS Version=\"1.0.0\">\n");
    x.push_str(&format!("  <PRODUCT Name=\"Poing\" Version=\"{}\" Company=\"Poing\"/>\n", env!("CARGO_PKG_VERSION")));
    x.push_str(&format!("  <COLLECTION Entries=\"{total}\">\n"));
    let mut id = 0;
    for s in songs {
        for t in &s.tracks {
            id += 1;
            x.push_str(&format!(
                "    <TRACK TrackID=\"{id}\" Name=\"{}\" Artist=\"\" Album=\"{}\" Genre=\"\" Kind=\"{}\" Size=\"{}\" TotalTime=\"{}\"{} DateAdded=\"{}\" BitRate=\"{}\" SampleRate=\"{}\" Comments=\"Made with Poing\" Rating=\"0\" Location=\"{}\"{} Mix=\"{}\">\n",
                e(&t.name),
                e(&s.name),
                e(&t.kind),
                t.size,
                t.seconds.floor() as i64,
                // unknown: left out, so rekordbox analyses it
                t.bpm.map_or(String::new(), |b| format!(" AverageBpm=\"{b:.2}\"")),
                e(&s.date),
                t.bitrate,
                t.sample_rate,
                e(&file_uri(&t.file)),
                t.key.as_deref().map_or(String::new(), |k| format!(" Tonality=\"{}\"", e(k))),
                e(&t.version),
            ));
            for m in &t.grid {
                x.push_str(&format!("      <TEMPO Inizio=\"{:.3}\" Bpm=\"{:.2}\" Metro=\"{}/4\" Battito=\"{}\"/>\n", m.start, m.bpm, t.per_bar, m.beat));
            }
            for c in &t.cues {
                x.push_str(&format!("      <POSITION_MARK Name=\"{}\" Type=\"0\" Start=\"{:.3}\" Num=\"{}\"/>\n", e(&c.name), c.start, c.num));
            }
            x.push_str("    </TRACK>\n");
        }
    }
    x.push_str("  </COLLECTION>\n  <PLAYLISTS>\n    <NODE Type=\"0\" Name=\"ROOT\" Count=\"1\">\n");
    x.push_str(&format!("      <NODE Type=\"0\" Name=\"Poing\" Count=\"{}\">\n", songs.len()));
    let mut id = 0;
    for s in songs {
        x.push_str(&format!("        <NODE Name=\"{}\" Type=\"1\" KeyType=\"0\" Entries=\"{}\">\n", e(&s.name), s.tracks.len()));
        for _ in &s.tracks {
            id += 1;
            x.push_str(&format!("          <TRACK Key=\"{id}\"/>\n"));
        }
        x.push_str("        </NODE>\n");
    }
    x.push_str("      </NODE>\n    </NODE>\n  </PLAYLISTS>\n</DJ_PLAYLISTS>\n");
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(start: f64, end: f64, label: &str) -> Section {
        Section { start, end, label: label.into() }
    }

    #[test]
    fn keys_in_rekordbox_spelling() {
        let k = |s: &str| rekordbox_key(s).unwrap();
        assert_eq!(k("G major"), "G");
        assert_eq!(k("E minor"), "Em");
        assert_eq!(k("F# minor"), "F#m");
        assert_eq!(k("Gb major"), "F#");
        assert_eq!(k("C# minor"), "Dbm");
        assert_eq!(k("G# minor"), "Abm");
        assert_eq!(k("Bbm"), "Bbm");
        assert_eq!(k("Bb"), "Bb");
        assert_eq!(k("B major"), "B");
        assert_eq!(k("A#:min"), "Bbm");
        assert_eq!(rekordbox_key("?"), None);
    }

    #[test]
    fn locations_are_file_uris() {
        assert_eq!(file_uri(r"C:\Users\gijs\Music\Poing\rekordbox\Zomer op de Farm - Mix.wav"), "file://localhost/C:/Users/gijs/Music/Poing/rekordbox/Zomer%20op%20de%20Farm%20-%20Mix.wav");
        assert_eq!(file_uri(r"\\?\C:\a\Café & #1.mp3"), "file://localhost/C:/a/Caf%C3%A9%20%26%20%231.mp3");
        assert_eq!(file_uri("/Users/gijs/Music/a b.wav"), "file://localhost/Users/gijs/Music/a%20b.wav");
    }

    #[test]
    fn date_is_iso() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert!(d.as_str() > "2026-01-01");
    }

    /// Zomer op de Farm's analysis: three noisy beats and a stray downbeat
    /// at 0 s, then a steady 113.2 bpm from 0.69 s (rounded to 10 ms, as the
    /// analysis writes them); the sections start on downbeats.
    fn zomer() -> (Vec<f64>, Vec<f64>) {
        let mut beats = vec![0.0, 0.22, 0.43];
        let period = 60.0 / 113.2;
        for i in 0..330 {
            beats.push(((0.69 + i as f64 * period) * 100.0).round() / 100.0);
        }
        let mut downbeats = vec![0.0];
        downbeats.extend(beats[3..].iter().step_by(4).copied());
        (beats, downbeats)
    }

    #[test]
    fn steady_song_gets_one_tempo_on_its_downbeats() {
        let (beats, downbeats) = zomer();
        let (grid, bpm) = dj_grid(&downbeats, &beats, 4, 150.0);
        assert_eq!(grid.len(), 1, "{grid:?}");
        assert!((bpm.unwrap() - 113.2).abs() < 0.02, "{bpm:?}");
        assert!((grid[0].bpm - 113.2).abs() < 0.02);
        assert_eq!(grid[0].beat, 1);
        // the grid's bar lines are within 10 ms of every analysed downbeat
        let (_, bars) = grid_lines(&grid, 4, 150.0);
        for d in downbeats.iter().filter(|d| **d > 0.0 && **d < 149.0) {
            let near = bars.iter().map(|b| (b - d).abs()).fold(f64::MAX, f64::min);
            assert!(near < 0.01, "downbeat {d} is {near} s off the grid");
        }
        // the first bar line at or after 0 s, before the first downbeat
        assert!(grid[0].start >= 0.0 && grid[0].start <= downbeats[1] + 1e-6);
    }

    #[test]
    fn a_tempo_change_gets_new_marks() {
        // 8 bars at 120, then 8 at 100
        let mut downbeats = Vec::new();
        let mut t = 1.0;
        for i in 0..16 {
            downbeats.push(t);
            t += if i < 8 { 2.0 } else { 2.4 };
        }
        let (grid, _) = dj_grid(&downbeats, &[], 4, t);
        assert!(grid.len() >= 2, "{grid:?}");
        assert!((grid[0].bpm - 120.0).abs() < 1e-9 && (grid[0].start - 1.0).abs() < 1e-9);
        let last = grid.last().unwrap();
        assert!((last.bpm - 100.0).abs() < 1e-9);
        let (_, bars) = grid_lines(&grid, 4, t);
        for d in &downbeats {
            assert!(bars.iter().any(|b| (b - d).abs() < 0.009), "downbeat {d} not on a bar line: {bars:?}");
        }
    }

    #[test]
    fn a_drifting_song_keeps_its_bars_without_the_strays() {
        // a band drifting between 120 and 130 bpm (It's Autumn), with what its
        // analysis gets wrong: a stray time before the first bar, one inside a
        // bar, and a missed bar
        let mut bars = Vec::new();
        let mut t = 0.34;
        for i in 0..40 {
            bars.push(t);
            t += 240.0 / (125.0 + 5.0 * (i as f64 / 4.0).sin());
        }
        let mut downbeats = vec![0.01];
        for (i, &b) in bars.iter().enumerate() {
            if i != 20 {
                downbeats.push(b);
            }
            if i == 10 {
                downbeats.push(b + 0.48);
            }
        }
        let (grid, bpm) = dj_grid(&downbeats, &[], 4, t);
        assert!(grid.len() > 2, "not steady, so it follows the bars: {grid:?}");
        for m in &grid {
            assert!((115.0..135.0).contains(&m.bpm), "a jump to {} bpm at {}", m.bpm, m.start);
        }
        assert!((grid[0].start - 0.34).abs() < 1e-9, "starts on the real first bar: {}", grid[0].start);
        let (_, lines) = grid_lines(&grid, 4, t);
        for b in &bars[..39] {
            assert!(lines.iter().any(|l| (l - b).abs() < 0.03), "bar {b} not on the grid");
        }
        assert!(!lines.iter().any(|l| (l - (bars[10] + 0.48)).abs() < 0.03), "the stray became a bar line");
        assert!((120.0..130.0).contains(&bpm.unwrap()));
    }

    /// It's Autumn's analysed downbeats: a band drifting around 125 bpm, a
    /// stray before the first bar, and the bar moved by whole beats where the
    /// choruses and the interlude start (37.49, 55.23, 86.83, 103.18, 112.55).
    const ITS_AUTUMN: [f64; 74] = [
        0.01, 0.34, 2.29, 3.89, 5.9, 7.85, 9.76, 11.65, 13.5, 15.37, 17.23, 19.11, 20.91, 22.79, 24.67, 26.57, 28.46, 30.35, 32.19, 34.11, 36.05, 37.49, 39.41, 41.3, 43.21,
        45.11, 47.02, 48.91, 50.83, 52.79, 54.75, 55.23, 57.19, 59.11, 61.03, 62.86, 64.75, 66.65, 68.55, 70.46, 72.35, 74.2, 76.05, 77.86, 79.7, 81.54, 83.43, 85.41, 86.83,
        88.79, 90.67, 92.55, 94.42, 96.33, 98.23, 100.17, 102.15, 103.18, 105.1, 107.1, 109.08, 111.07, 112.55, 114.5, 116.42, 118.34, 120.26, 122.19, 124.12, 126.07, 128.06,
        130.11, 132.03, 133.95,
    ];

    #[test]
    fn a_real_band_keeps_its_tempo_where_the_bar_moves() {
        let (grid, bpm) = dj_grid(&ITS_AUTUMN, &[], 4, 134.6);
        for m in &grid {
            assert!((115.0..140.0).contains(&m.bpm), "a jump to {} bpm at {}: {grid:?}", m.bpm, m.start);
        }
        assert!((grid[0].start - 0.34).abs() < 1e-9);
        let (_, bars) = grid_lines(&grid, 4, 134.6);
        for s in [37.49, 55.23, 86.83, 103.18, 112.55] {
            assert!(bars.iter().any(|b| (b - s).abs() < 0.01), "the bar at {s} is not on the grid");
        }
        assert!((120.0..130.0).contains(&bpm.unwrap()));
    }

    #[test]
    fn no_analysis_no_grid() {
        assert_eq!(dj_grid(&[], &[0.0, 0.5], 4, 10.0), (Vec::new(), None));
    }

    #[test]
    fn cues_on_sections_with_hot_cues_a_to_d() {
        let (beats, downbeats) = zomer();
        let (grid, _) = dj_grid(&downbeats, &beats, 4, 150.0);
        let sections = vec![
            sec(0.0, 9.18, "intro"),
            sec(9.18, 30.42, "verse"),
            sec(30.42, 47.42, "chorus"),
            sec(47.42, 72.9, "verse"),
            sec(72.9, 89.89, "chorus"),
            sec(89.89, 106.89, "verse"),
            sec(106.89, 132.37, "chorus"),
            sec(132.37, 150.0, "outro"),
        ];
        let cues = dj_cues(&sections, &grid, 4, 150.0);
        let memory: Vec<&str> = cues.iter().filter(|c| c.num == -1).map(|c| c.name.as_str()).collect();
        assert_eq!(memory, ["Intro", "Verse 1", "Chorus 1", "Verse 2", "Chorus 2", "Verse 3", "Chorus 3", "Outro"]);
        let hot: Vec<(i32, &str)> = cues.iter().filter(|c| c.num >= 0).map(|c| (c.num, c.name.as_str())).collect();
        // no bridge or drop: C is the last chorus
        assert_eq!(hot, [(0, "Intro"), (1, "Chorus 1"), (2, "Chorus 3"), (3, "Outro")]);
        let c = cues.iter().find(|c| c.num == 2).unwrap();
        assert!((c.start - 106.89).abs() < 0.6, "{c:?}");
        // every cue on a bar line
        let (_, bars) = grid_lines(&grid, 4, 150.0);
        for c in &cues {
            assert!(bars.iter().any(|b| (b - c.start).abs() < 1e-9), "{c:?} is not on a bar");
        }
        // A is the first bar of the song, not 0 s before the first beat
        assert!((cues.iter().find(|c| c.num == 0).unwrap().start - 0.69).abs() < 0.01);
    }

    #[test]
    fn a_bridge_is_hot_cue_c() {
        let grid = vec![DjMark { start: 0.0, bpm: 120.0, beat: 1 }];
        let sections = vec![sec(0.0, 8.0, "intro"), sec(8.0, 16.0, "chorus"), sec(16.0, 24.0, "bridge"), sec(24.0, 32.0, "chorus"), sec(32.0, 40.0, "end")];
        let cues = dj_cues(&sections, &grid, 4, 40.0);
        let hot: Vec<(i32, f64)> = cues.iter().filter(|c| c.num >= 0).map(|c| (c.num, c.start)).collect();
        assert_eq!(hot, [(0, 0.0), (1, 8.0), (2, 16.0), (3, 32.0)]);
    }

    #[test]
    fn xml_has_the_collection_grid_cues_and_playlists() {
        let track = |name: &str, file: &str| DjTrack {
            name: name.into(),
            version: "Mix".into(),
            file: file.into(),
            kind: "WAV File".into(),
            size: 1234,
            seconds: 150.7,
            bitrate: 2116,
            sample_rate: 44100,
            bpm: Some(113.2),
            key: Some("G".into()),
            per_bar: 4,
            grid: vec![DjMark { start: 1.22, bpm: 113.2, beat: 1 }],
            cues: vec![DjCue { name: "Intro".into(), start: 1.22, num: -1 }, DjCue { name: "Chorus & more".into(), start: 30.4, num: 1 }],
        };
        let songs = vec![
            DjSong { name: "A & B".into(), date: "2026-10-23".into(), tracks: vec![track("A (Mix)", r"C:\x\A - Mix.wav"), track("A (Disco)", r"C:\x\A - Disco.wav")] },
            DjSong { name: "Zomer".into(), date: "2026-10-23".into(), tracks: vec![track("Zomer (Mix)", r"C:\x\Zomer - Mix.wav")] },
        ];
        let x = rekordbox_xml(&songs);
        assert!(x.contains("<COLLECTION Entries=\"3\">"));
        assert!(x.contains("TrackID=\"3\" Name=\"Zomer (Mix)\""));
        assert!(x.contains("TotalTime=\"150\" AverageBpm=\"113.20\""));
        assert!(x.contains("Location=\"file://localhost/C:/x/A%20-%20Disco.wav\""));
        assert!(x.contains("Tonality=\"G\""));
        assert!(x.contains("<TEMPO Inizio=\"1.220\" Bpm=\"113.20\" Metro=\"4/4\" Battito=\"1\"/>"));
        assert!(x.contains("<POSITION_MARK Name=\"Chorus &amp; more\" Type=\"0\" Start=\"30.400\" Num=\"1\"/>"));
        assert!(x.contains("<NODE Name=\"A &amp; B\" Type=\"1\" KeyType=\"0\" Entries=\"2\">"));
        assert!(x.contains("<NODE Type=\"0\" Name=\"Poing\" Count=\"2\">"));
        assert!(x.contains("<TRACK Key=\"3\"/>"));
        // a version without the song's timing: no bpm or key, rekordbox analyses it
        let mut loose = track("A (Version 2)", r"C:\x\A - Version 2.wav");
        (loose.bpm, loose.key, loose.grid, loose.cues) = (None, None, Vec::new(), Vec::new());
        let x = rekordbox_xml(&[DjSong { name: "A".into(), date: "2026-10-23".into(), tracks: vec![loose] }]);
        assert!(!x.contains("AverageBpm") && !x.contains("Tonality") && !x.contains("<TEMPO"), "{x}");
        // manifests round-trip
        let back = DjSong::deserialize_json(&songs[0].serialize_json()).unwrap();
        assert_eq!(back.tracks[0].cues, songs[0].tracks[0].cues);
    }
}
