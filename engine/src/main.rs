//! poing-engine: Poing's native models, in a helper process.
//!
//!     poing-engine status                         which native models are installed, and the GPU store
//!     poing-engine install [--accept-licence]     download the analysis models (asks for licences first)
//!     poing-engine analyze <audio> <bundle_dir>   stems, beats and lyrics
//!
//! `analyze` writes `<bundle>/stems/{vocals,drums,bass,other}.wav` (BS-RoFormer)
//! and `<bundle>/analysis.json` with tempo, beats and downbeats (Beat This!)
//! and lyrics (Whisper on the vocal stem). Sections, key and chords are not
//! native yet: the app runs `sidecar/poing_analyze.py --only structure` after
//! this, which adds them to the same analysis.json.
//!
//! Progress goes to stdout as one JSON object per line, the sidecar's format:
//! `{"stage": "...", "progress": 0..1, "message": "..."}`, then `{"stage": "done"}`.
//! When the native path can't run here (weights not installed, no GPU), it
//! prints `{"stage": "unavailable", ...}` and exits with code 2 before writing
//! anything, and the app falls back to the Python analysis.
//!
//! It is a separate process so that poing itself never links CUDA, a model
//! crash doesn't take the mix down, and the GPU memory is free again when it exits.

mod lyrics;
mod models;

use makepad_ai_beats::BeatsModel;
use makepad_ai_stems::{demix_all, StemsModel, StereoBuf, SAMPLE_RATE, STEM_NAMES};
use makepad_micro_serde::*;
use models::Models;
use poing_model::LyricLine;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

/// One progress line, as the app's `SidecarLine` reads it.
#[derive(SerJson)]
struct Line {
    stage: String,
    progress: f64,
    message: String,
}

pub fn emit(stage: &str, progress: f64, message: &str) {
    let line = Line { stage: stage.into(), progress: (progress * 1000.0).round() / 1000.0, message: message.into() };
    println!("{}", line.serialize_json());
}

/// (stage, share of the native run's time) for one overall progress number.
const STAGES: [(&str, f64); 3] = [("stems", 0.6), ("beats", 0.05), ("lyrics", 0.35)];

fn overall(stage: &str, within: f64) -> f64 {
    let mut done = 0.0;
    for (name, share) in STAGES {
        if name == stage {
            return done + share * within.clamp(0.0, 1.0);
        }
        done += share;
    }
    done
}

/// What `analyze` writes to analysis.json (the sidecar adds key, chords and sections).
#[derive(SerJson)]
struct NativeAnalysis {
    tempo: Option<f64>,
    beats: Vec<f64>,
    downbeats: Vec<f64>,
    lyrics: Vec<LyricLine>,
    stems: StemFiles,
    /// Which native models wrote this, for provenance.
    native: Provenance,
}

#[derive(SerJson)]
struct StemFiles {
    vocals: String,
    drums: String,
    bass: String,
    other: String,
}

#[derive(SerJson)]
struct Provenance {
    stems: String,
    beats: String,
    lyrics: String,
    language: Option<String>,
    seconds: f64,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("status") => status(),
        Some("install") => install(args.iter().any(|a| a == "--accept-licence" || a == "--accept-license")),
        Some("analyze") if args.len() == 3 => analyze(Path::new(&args[1]), Path::new(&args[2])),
        Some("lyrics") if args.len() == 2 => lyrics_only(Path::new(&args[1])),
        Some("quantize") if args.len() == 3 => match models::quantize_q8(Path::new(&args[1]), Path::new(&args[2])) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: poing-engine status | install [--accept-licence] | analyze <audio> <bundle_dir> | lyrics <vocals>");
            ExitCode::from(64)
        }
    }
}

fn status() -> ExitCode {
    match Models::open() {
        Ok(models) => {
            println!("{}", models.status_json());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn install(accept: bool) -> ExitCode {
    let mut models = match Models::open() {
        Ok(m) => m,
        Err(e) => {
            emit("error", 0.0, &e);
            return ExitCode::FAILURE;
        }
    };
    match models.install(accept) {
        Ok(()) => {
            emit("done", 1.0, "Analysis models installed");
            ExitCode::SUCCESS
        }
        Err(e) => {
            emit("error", 0.0, &e);
            ExitCode::FAILURE
        }
    }
}

#[derive(SerJson)]
struct LyricsOnly {
    language: Option<String>,
    lyrics: Vec<LyricLine>,
}

/// Lyrics of a vocal stem, as JSON on stdout (for trying lyric settings on a
/// song that already has stems).
fn lyrics_only(vocals: &Path) -> ExitCode {
    let result = Models::open().and_then(|m| m.analysis_paths()).and_then(|paths| {
        let track = decode(vocals)?;
        let (lyrics, language) = lyrics::transcribe(&paths.whisper, &mono_at(&track.left, &track.right, SAMPLE_RATE, 16_000))?;
        Ok(LyricsOnly { language, lyrics })
    });
    match result {
        Ok(out) => {
            println!("{}", out.serialize_json());
            ExitCode::SUCCESS
        }
        Err(e) => {
            emit("error", 0.0, &e);
            ExitCode::FAILURE
        }
    }
}

fn analyze(audio: &Path, bundle: &Path) -> ExitCode {
    let began = Instant::now();
    let unavailable = |why: String| {
        emit("unavailable", 0.0, &why);
        ExitCode::from(2)
    };
    let paths = match Models::open().and_then(|m| m.analysis_paths()) {
        Ok(p) => p,
        Err(why) => return unavailable(why),
    };
    // Load the separator first: it fails when there is no GPU store, before
    // anything is written.
    emit("start", 0.0, "Loading the stem separator");
    let mut separator = match StemsModel::load(&paths.stems) {
        Ok(m) => m,
        Err(e) => return unavailable(format!("stems model: {e}")),
    };
    match run(audio, bundle, &paths, &mut separator, began) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            emit("error", 0.0, &e);
            ExitCode::FAILURE
        }
    }
}

fn run(audio: &Path, bundle: &Path, paths: &models::AnalysisPaths, separator: &mut StemsModel, began: Instant) -> Result<(), String> {
    let track = decode(audio)?;
    let seconds = track.left.len() as f64 / SAMPLE_RATE as f64;

    emit("stems", overall("stems", 0.0), "Separating vocals, drums, bass and other");
    let stems = demix_all(separator, &track, |done, total| {
        emit("stems", overall("stems", done as f64 / total.max(1) as f64), &format!("Separating ({done}/{total})"));
    })
    .map_err(|e| format!("stems: {e}"))?;
    std::fs::create_dir_all(bundle.join("stems")).map_err(|e| format!("{}: {e}", bundle.display()))?;
    for (name, stem) in STEM_NAMES.iter().zip(&stems) {
        let path = bundle.join("stems").join(format!("{name}.wav"));
        write_wav(&path, &stem.left, &stem.right, SAMPLE_RATE).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let vocals = &stems[STEM_NAMES.iter().position(|n| *n == "vocals").unwrap()];
    let vocals_16k = mono_at(&vocals.left, &vocals.right, SAMPLE_RATE, 16_000);
    drop(stems);
    emit("stems", overall("stems", 1.0), "Stems ready");

    emit("beats", overall("beats", 0.0), "Finding beats and downbeats");
    let mono_22k = mono_at(&track.left, &track.right, SAMPLE_RATE, makepad_ai_beats::SAMPLE_RATE);
    drop(track);
    let beats = {
        let mut model = BeatsModel::load(&paths.beats).map_err(|e| format!("beats model: {e}"))?;
        model.analyze(&mono_22k).map_err(|e| format!("beats: {e}"))?
    };
    let round3 = |t: f64| (t * 1000.0).round() / 1000.0;
    let beat_times: Vec<f64> = beats.beats_secs.iter().copied().map(round3).collect();
    let downbeats: Vec<f64> = beats.downbeats_secs.iter().copied().map(round3).collect();
    let tempo = tempo_of(&beat_times);
    emit("beats", overall("beats", 1.0), &format!("{} beats, {} bpm", beat_times.len(), tempo.map_or("?".into(), |t| t.to_string())));

    emit("lyrics", overall("lyrics", 0.0), "Transcribing lyrics");
    let (lines, language) = lyrics::transcribe(&paths.whisper, &vocals_16k)?;
    emit("lyrics", overall("lyrics", 1.0), &format!("{} lyric lines", lines.len()));

    let model_name = |p: &Path| p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let analysis = NativeAnalysis {
        tempo,
        beats: beat_times,
        downbeats,
        lyrics: lines,
        stems: StemFiles {
            vocals: "stems/vocals.wav".into(),
            drums: "stems/drums.wav".into(),
            bass: "stems/bass.wav".into(),
            other: "stems/other.wav".into(),
        },
        native: Provenance {
            stems: model_name(&paths.stems),
            beats: model_name(&paths.beats),
            lyrics: model_name(&paths.whisper),
            language,
            seconds: (seconds * 10.0).round() / 10.0,
        },
    };
    let path = bundle.join("analysis.json");
    std::fs::write(&path, analysis.serialize_json()).map_err(|e| format!("{}: {e}", path.display()))?;
    emit("done", 1.0, &format!("Native analysis done in {:.0}s", began.elapsed().as_secs_f64()));
    Ok(())
}

/// The song as the separator's 44.1 kHz stereo.
fn decode(audio: &Path) -> Result<StereoBuf, String> {
    let bytes = std::fs::read(audio).map_err(|e| format!("{}: {e}", audio.display()))?;
    let decoded = makepad_audio_decode::decode_any(&bytes).map_err(|e| format!("{}: {e:?}", audio.display()))?;
    if decoded.channels == 0 || decoded.rate == 0 || decoded.frames() == 0 {
        return Err(format!("{}: no audio", audio.display()));
    }
    let left = decoded.channel(0);
    let right = if decoded.channels > 1 { decoded.channel(1) } else { left.clone() };
    if decoded.rate == SAMPLE_RATE {
        return Ok(StereoBuf { left, right });
    }
    use makepad_ai_hub::resample::resample_channel;
    Ok(StereoBuf { left: resample_channel(&left, decoded.rate, SAMPLE_RATE), right: resample_channel(&right, decoded.rate, SAMPLE_RATE) })
}

fn mono_at(left: &[f32], right: &[f32], from: u32, to: u32) -> Vec<f32> {
    let mono: Vec<f32> = left.iter().zip(right).map(|(l, r)| 0.5 * (l + r)).collect();
    if from == to {
        mono
    } else {
        makepad_ai_hub::resample::resample_channel(&mono, from, to)
    }
}

/// 60 / the median beat gap, to 0.1 bpm (as the sidecar computes it).
fn tempo_of(beats: &[f64]) -> Option<f64> {
    let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0).collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some((600.0 / gaps[gaps.len() / 2]).round() / 10.0)
}

/// 24-bit stereo PCM WAV, as the app writes them.
fn write_wav(path: &Path, l: &[f32], r: &[f32], sr: u32) -> std::io::Result<()> {
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
            let v = (x.clamp(-1.0, 1.0) * 8_388_607.0) as i32;
            b.extend_from_slice(&v.to_le_bytes()[..3]);
        }
    }
    std::fs::write(path, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_is_the_median_gap() {
        let beats: Vec<f64> = (0..20).map(|i| i as f64 * 0.5).collect();
        assert_eq!(tempo_of(&beats), Some(120.0));
        assert_eq!(tempo_of(&[1.0]), None);
    }

    #[test]
    fn progress_shares_add_up() {
        assert!((overall("lyrics", 1.0) - 1.0).abs() < 1e-9);
        assert_eq!(overall("stems", 0.0), 0.0);
    }
}
