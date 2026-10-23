//! Lyrics: Whisper (Makepad's native port) on the vocal stem, then the
//! sidecar's filters: subtitle boilerplate Whisper hallucinates over
//! instrumentals, text placed over silence in the vocal stem, and the
//! repetition loops greedy decoding falls into on sung vocals.

use makepad_ai_speech::whisper::{release_device_state, WhisperModel, WhisperParams, WhisperState};
use poing_model::LyricLine;
use std::path::Path;

/// Whisper's subtitle-trained boilerplate (lowercase substrings), as in the sidecar.
const HALLUCINATIONS: [&str; 11] = [
    "zang en muziek",
    "muziek",
    "tv gelderland",
    "ondertiteling",
    "ondertiteld",
    "thanks for watching",
    "subtitles by",
    "amara.org",
    "[music]",
    "♪",
    "(music)",
];

/// Transcribe 16 kHz mono vocals: the kept lines and the detected language.
pub fn transcribe(model_path: &Path, vocals_16k: &[f32]) -> Result<(Vec<LyricLine>, Option<String>), String> {
    // load_file maps the file, and Makepad's loader has no mmap on Windows: read it there.
    let model = WhisperModel::load_file(&model_path.to_string_lossy())
        .or_else(|_| {
            let file = std::fs::File::open(model_path)?;
            WhisperModel::load(&mut std::io::BufReader::with_capacity(1 << 20, file))
        })
        .map_err(|e| format!("whisper load {}: {e}", model_path.display()))?;
    let mut state = WhisperState::new(&model);
    let mut params = WhisperParams::default();
    params.language = std::env::var("POING_LYRICS_LANGUAGE").unwrap_or_else(|_| "auto".into());
    params.no_timestamps = false;
    params.single_segment = false;
    params.temperature = 0.0;
    params.suppress_blank = true;
    // One long-form pass with Whisper's own 30 s seeking. (Cutting the stem
    // into phrase windows at its silences lost whole choruses: more deleted
    // words on both test songs, with either model.)
    let segments = state.transcribe(&model, vocals_16k, &params);
    let language = state.detected_language().map(|(code, _)| code.to_string()).or_else(|| (params.language != "auto").then(|| params.language.clone()));
    drop(state);
    drop(model);
    // The CUDA backend keeps Whisper's weights on the device per thread.
    release_device_state();
    let loud = loud_frames(vocals_16k);
    let lines = segments
        .into_iter()
        .filter_map(|s| keep(s.start_ms as f64 / 1000.0, s.end_ms as f64 / 1000.0, s.text.trim(), &loud))
        .collect();
    Ok((lines, language))
}

/// A lyric line, or None when it is boilerplate, over silence, or a loop.
fn keep(start: f64, end: f64, text: &str, loud: &[bool]) -> Option<LyricLine> {
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    if HALLUCINATIONS.iter().any(|h| lower.contains(h)) || is_loop(&lower) {
        return None;
    }
    let (a, b) = ((start * 10.0) as usize, ((end * 10.0) as usize).max((start * 10.0) as usize + 1));
    if !loud.get(a..b.min(loud.len())).is_some_and(|w| w.iter().any(|l| *l)) {
        return None;
    }
    let round2 = |t: f64| (t * 100.0).round() / 100.0;
    Some(LyricLine { start: round2(start), end: round2(end), text: text.to_string() })
}

/// Ten frames a second: is the vocal stem louder than 5 % of its 95th
/// percentile RMS (and above -80 dB)?
fn loud_frames(audio: &[f32]) -> Vec<bool> {
    const HOP: usize = 1600;
    // librosa's rms centres a 2048 window on each hop.
    let rms: Vec<f32> = (0..audio.len().div_ceil(HOP).max(1))
        .map(|i| {
            let c = i * HOP;
            let (a, b) = (c.saturating_sub(1024), (c + 1024).min(audio.len()));
            let sum: f32 = audio[a..b].iter().map(|x| x * x).sum();
            (sum / 2048.0).sqrt()
        })
        .collect();
    let mut sorted = rms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = sorted[((sorted.len() - 1) as f32 * 0.95) as usize];
    let thold = (p95 * 0.05).max(1e-4);
    rms.iter().map(|r| *r > thold).collect()
}

/// Greedy decoding with no temperature fallback can loop ("la la la la …",
/// one phrase over and over). Python's Whisper rejects those by gzip
/// compression ratio. Choruses repeat too ("ik zoek een mooie, rijke vrouw"
/// is 8 different words in 28), so only a line that is almost all the same
/// few words, or one word over and over, counts as a loop.
fn is_loop(text: &str) -> bool {
    let words: Vec<String> = text.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string()).collect();
    let mut run = 1;
    for pair in words.windows(2) {
        run = if pair[0] == pair[1] { run + 1 } else { 1 };
        if run >= 8 {
            return true;
        }
    }
    if words.len() < 24 {
        return false;
    }
    let mut unique = words.clone();
    unique.sort_unstable();
    unique.dedup();
    (unique.len() as f64) / (words.len() as f64) < 0.15
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_boilerplate_silence_and_loops() {
        let loud = vec![false, false, true, true, false];
        assert!(keep(0.2, 0.4, "Hello there", &loud).is_some());
        assert!(keep(0.0, 0.1, "Hello there", &loud).is_none(), "over silence");
        assert!(keep(0.2, 0.4, "Ondertiteling TV Gelderland", &loud).is_none());
        assert!(keep(0.2, 0.4, "", &loud).is_none());
        assert!(keep(0.2, 0.4, &"oh no ".repeat(20), &loud).is_none());
        assert!(keep(0.2, 0.4, &"la ".repeat(9), &loud).is_none());
        let chorus = "Ik zoek een mooie, rijke vrouw. Mooie, rijke vrouw. ".repeat(3) + "Zonder problemen.";
        assert!(keep(0.2, 0.4, &chorus, &loud).is_some(), "a chorus is not a loop");
        assert!(!is_loop("the quick brown fox jumps over the lazy dog and runs far away home"));
    }

    #[test]
    fn loud_frames_follow_the_signal() {
        let mut audio = vec![0.0f32; 16_000];
        audio.extend((0..16_000).map(|i| (i as f32 * 0.05).sin() * 0.5));
        let loud = loud_frames(&audio);
        assert!(!loud[2] && loud[15]);
    }
}
