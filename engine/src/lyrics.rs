//! Lyrics: Whisper (Makepad's native port) on the vocal stem, the same way
//! the Python sidecar's `lyrics()` does it (see `sidecar/poing_analyze.py`):
//!
//! - the stem is cut at pauses into pieces of at most 20 s, decoded one at a
//!   time (long-form decoding skips whole verses on sung vocals);
//! - each piece also hears up to 5 s of the audio before it, and words in
//!   that lead-in are dropped (a piece that starts mid-line is misheard);
//! - a piece that comes back as boilerplate or with too few words is decoded
//!   again with the previous words as a prompt;
//! - the language is forced: `sheet.json`, else detected once from the three
//!   longest pieces;
//! - word times become lines split at pauses and sentence ends, at most 10 s;
//! - when the bundle has `lyrics.txt`, its lines are timed against the heard
//!   words instead of transcribing.

use makepad_ai_speech::whisper::{release_device_state, WhisperModel, WhisperParams, WhisperState};
use makepad_micro_serde::*;
use poing_model::LyricLine;
use std::path::Path;

const SR: usize = 16_000;
const FPS: usize = 10; // loudness frames per second

/// A heard word: times in seconds in the song, and the piece it came from.
#[derive(Clone, Debug)]
struct Word {
    start: f64,
    end: f64,
    text: String,
    chunk: usize,
}

/// The fields of `sheet.json` this reads.
#[derive(DeJson)]
struct Sheet {
    language: Option<String>,
}

/// Lyrics of 16 kHz mono vocals: the lines and the language used. `bundle`
/// supplies `sheet.json` (the language) and `lyrics.txt` (known words).
pub fn transcribe(model_path: &Path, vocals_16k: &[f32], bundle: Option<&Path>) -> Result<(Vec<LyricLine>, Option<String>), String> {
    let mut language = std::env::var("POING_LYRICS_LANGUAGE").ok().filter(|l| !l.is_empty());
    let mut known = Vec::new();
    if let Some(bundle) = bundle {
        if language.is_none() {
            language = std::fs::read_to_string(bundle.join("sheet.json"))
                .ok()
                .and_then(|t| Sheet::deserialize_json_lenient(&t).ok())
                .and_then(|s| s.language)
                .filter(|l| !l.is_empty());
        }
        if let Ok(text) = std::fs::read_to_string(bundle.join("lyrics.txt")) {
            known = known_lines(&text);
        }
    }
    let (rms, loud) = loud_frames(vocals_16k);
    let chunks = voiced_chunks(&rms, &loud, 20.0);
    if chunks.is_empty() {
        return Ok((Vec::new(), language));
    }
    // load_file maps the file, and Makepad's loader has no mmap on Windows: read it there.
    let model = WhisperModel::load_file(&model_path.to_string_lossy())
        .or_else(|_| {
            let file = std::fs::File::open(model_path)?;
            WhisperModel::load(&mut std::io::BufReader::with_capacity(1 << 20, file))
        })
        .map_err(|e| format!("whisper load {}: {e}", model_path.display()))?;
    let mut state = WhisperState::new(&model);
    let piece = |a: f64, b: f64| &vocals_16k[((a * SR as f64) as usize).min(vocals_16k.len())..((b * SR as f64) as usize).min(vocals_16k.len())];
    let language = match language {
        Some(l) => l,
        None => detect_language(&model, &mut state, &chunks.iter().map(|&(a, b)| piece(a, b)).collect::<Vec<_>>()),
    };
    let mut ear = Ear { model: &model, state: &mut state, vocals: vocals_16k, rms: &rms, language: &language };
    let mut words = Vec::new();
    let mut prev: Vec<String> = Vec::new();
    for (n, &(a, b)) in chunks.iter().enumerate() {
        // Each piece is heard with up to 5 s of the audio before it: Whisper often mishears
        // or drops a piece that starts mid-line. Words in that lead-in belong to the piece before.
        let at = if n > 0 { (a - 5.0).max(b - 30.0).max(chunks[n - 1].0) } else { a };
        let out = ear.hear(a, b, at, &prev, 0);
        if std::env::var_os("POING_LYRICS_DEBUG").is_some() {
            eprintln!("piece {n} {a:.1}-{b:.1}: {:?}", out.iter().map(|(s, e, w)| format!("{s:.1}-{e:.1} {w}")).collect::<Vec<_>>());
        }
        if out.is_empty() {
            continue;
        }
        prev = out.iter().map(|w| w.2.clone()).collect();
        for (s, e, w) in out {
            let (f, g) = ((s * FPS as f64) as usize, ((e * FPS as f64) as usize).max((s * FPS as f64) as usize + 1));
            // No words over silence.
            if loud.get(f..g.min(loud.len())).is_some_and(|w| w.iter().any(|l| *l)) {
                words.push(Word { start: s, end: e.max(s + 0.05), text: w, chunk: n });
            }
        }
    }
    drop(state);
    drop(model);
    // The CUDA backend keeps Whisper's weights on the device per thread.
    release_device_state();
    let lines = if known.is_empty() { words_to_lines(&words) } else { align_known(&known, &words) };
    let round2 = |t: f64| (t * 100.0).round() / 100.0;
    let lines = lines.into_iter().map(|(s, e, text)| LyricLine { start: round2(s), end: round2(e), text }).collect();
    Ok((lines, Some(language)))
}

/// Whisper with what it needs to hear one piece of the vocals.
struct Ear<'a> {
    model: &'a WhisperModel,
    state: &'a mut WhisperState,
    vocals: &'a [f32],
    rms: &'a [f32],
    language: &'a str,
}

impl Ear<'_> {
    /// The words sung in `a..b` s, decoded from `at` (the lead-in, see the
    /// module notes); `prev` is what the piece before ended with. Nothing when
    /// it is boilerplate or a loop.
    fn hear(&mut self, a: f64, b: f64, at: f64, prev: &[String], depth: usize) -> Vec<(f64, f64, String)> {
        let (model, vocals, language) = (self.model, self.vocals, self.language);
        let mut out = after_lead_in(decode(model, self.state, vocals, at, b, language, ""), a, prev);
        if !usable(&out, b - a) && !prev.is_empty() {
            // "MUZIEK" or a word or two over singing: the words before it as a prompt bring
            // them back (a real instrumental stays empty).
            let retry = decode(model, self.state, vocals, a, b, language, &prev.join(" "));
            if usable(&retry, b - a) || retry.len() > out.len() {
                out = retry;
            }
        }
        if !usable(&out, b - a) && b - a > 8.0 && depth < 2 {
            // Still a loop ("la la la …") or nearly empty, and on the first piece there are no
            // words for a prompt yet: hear it as two halves, cut at its quietest frame, each
            // kept or dropped on its own (a "la la" intro before the first verse).
            let lo = ((a + 2.0) * FPS as f64) as usize;
            let hi = (((b - 2.0) * FPS as f64) as usize).min(self.rms.len());
            let mid = (lo..hi).min_by(|&i, &j| self.rms[i].partial_cmp(&self.rms[j]).unwrap()).map_or((a + b) / 2.0, |i| i as f64 / FPS as f64);
            let mut halves = self.hear(a, mid, at, prev, depth + 1);
            let first: Vec<String> = if halves.is_empty() { prev.to_vec() } else { halves.iter().map(|w| w.2.clone()).collect() };
            halves.extend(self.hear(mid, b, (mid - 5.0).max(a), &first, depth + 1));
            if !halves.is_empty() && (usable(&halves, b - a) || !kept(&out) || halves.len() > out.len()) {
                out = halves;
            }
        }
        if kept(&out) {
            out
        } else {
            Vec::new()
        }
    }
}

/// Not boilerplate, not a loop, not empty.
fn kept(words: &[(f64, f64, String)]) -> bool {
    let text = join(words);
    !text.is_empty() && !is_boilerplate(&text) && !looped(&text)
}

/// The words Whisper hears in `from..to` s of the vocals, with times in the
/// song. The text is decoded on the GPU (`transcribe`), then its tokens are
/// force-aligned to the piece for word times: one decoder pass, where
/// `transcribe_aligned` decodes every token on the CPU to capture the same
/// attention (`POING_LYRICS_WORD_TIMES=1`, three times slower on a song).
/// With a prompt, or when the alignment fails, each segment's time is shared
/// out over its words by length (`POING_LYRICS_WORD_TIMES=0` always does that).
fn decode(model: &WhisperModel, state: &mut WhisperState, vocals: &[f32], from: f64, to: f64, language: &str, prompt: &str) -> Vec<(f64, f64, String)> {
    let piece = &vocals[((from * SR as f64) as usize).min(vocals.len())..((to * SR as f64) as usize).min(vocals.len())];
    let mut params = WhisperParams::default();
    params.language = language.to_string();
    params.no_timestamps = false;
    params.single_segment = false;
    params.temperature = 0.0;
    params.suppress_blank = true;
    let mode = std::env::var("POING_LYRICS_WORD_TIMES").unwrap_or_default();
    let mut words = Vec::new();
    if prompt.is_empty() && mode == "1" {
        for seg in state.transcribe_aligned(model, piece, &params) {
            if seg.words.is_empty() {
                share_out(&mut words, seg.start_ms, seg.end_ms, &seg.text);
            }
            for w in seg.words {
                words.push((w.start_ms as f64 / 1000.0, w.end_ms as f64 / 1000.0, w.text.trim().to_string()));
            }
        }
    } else {
        if !prompt.is_empty() {
            let skip = prompt.char_indices().rev().nth(399).map_or(0, |(i, _)| i);
            params.prompt = prompt[skip..].to_string();
        }
        let t = std::time::Instant::now();
        let segments = state.transcribe(model, piece, &params);
        let t_decode = t.elapsed().as_secs_f64();
        let text = segments.iter().map(|s| s.text.trim()).collect::<Vec<_>>().join(" ");
        let aligned = (prompt.is_empty() && mode != "0" && !text.is_empty())
            .then(|| state.force_align(model, piece, &model.tokenizer().encode(&format!(" {text}")), language))
            .flatten()
            .filter(|w| !w.is_empty() && !collapsed(w));
        if std::env::var_os("POING_LYRICS_DEBUG").is_some() {
            eprintln!("  timing {from:.1}-{to:.1}: decode {t_decode:.2}s, align {:.2}s", t.elapsed().as_secs_f64() - t_decode);
        }
        match aligned {
            Some(aligned) => {
                if std::env::var_os("POING_LYRICS_DEBUG").is_some() {
                    eprintln!("  aligned {from:.1}-{to:.1}: {:?}", aligned.iter().map(|w| format!("{:.1} {} {:.2}", from + w.start_ms as f64 / 1000.0, w.text.trim(), w.score)).collect::<Vec<_>>());
                }
                words.extend(aligned.into_iter().map(|w| (w.start_ms as f64 / 1000.0, w.end_ms as f64 / 1000.0, w.text.trim().to_string())))
            }
            None => {
                for seg in segments {
                    share_out(&mut words, seg.start_ms, seg.end_ms, &seg.text);
                }
            }
        }
    }
    // "♪" and other wordless tokens aren't lyrics.
    words.retain(|(_, _, w)| w.chars().any(char::is_alphanumeric));
    for w in &mut words {
        w.0 += from;
        w.1 += from;
    }
    words
}

/// A piece's words without those in its lead-in: the ones that start before
/// `a` (with 0.3 s grace), then a run near its start that repeats the end of
/// the piece before. Word times can put lead-in words after `a`: a shared-out
/// segment time by a word or two, a failed cross-attention alignment all of them.
fn after_lead_in(words: Vec<(f64, f64, String)>, a: f64, before: &[String]) -> Vec<(f64, f64, String)> {
    let words: Vec<_> = words.into_iter().filter(|(s, _, _)| *s >= a - 0.3).collect();
    let heard: Vec<String> = words.iter().map(|w| norm(&w.2)).collect();
    let tail: Vec<String> = before.iter().map(|w| norm(w)).collect();
    // The longest tail of `before` (up to 12 words) that starts within the first 3 words
    // heard; two words at least when it doesn't start at the first.
    let mut drop = 0;
    'k: for k in (1..=tail.len().min(heard.len()).min(12)).rev() {
        for j in 0..=3.min(heard.len().saturating_sub(k)) {
            if (j == 0 || k >= 2) && heard[j..j + k] == tail[tail.len() - k..] {
                drop = j + k;
                break 'k;
            }
        }
    }
    words.into_iter().skip(drop).collect()
}

/// A failed alignment: three or more words in a row on one instant with no
/// attention on the audio (the DTW path ran along the window's edge).
fn collapsed(words: &[makepad_ai_speech::whisper::WordSpan]) -> bool {
    let mut run = 1;
    for pair in words.windows(2) {
        run = if pair[1].start_ms == pair[0].start_ms && pair[1].score < 0.05 { run + 1 } else { 1 };
        if run >= 3 {
            return true;
        }
    }
    false
}

fn share_out(words: &mut Vec<(f64, f64, String)>, start_ms: i64, end_ms: i64, text: &str) {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let total: usize = parts.iter().map(|w| w.chars().count() + 1).sum();
    let (s, e) = (start_ms as f64 / 1000.0, end_ms as f64 / 1000.0);
    let mut t = s;
    for w in parts {
        let d = (e - s) * (w.chars().count() + 1) as f64 / total as f64;
        words.push((t, t + d, w.to_string()));
        t += d;
    }
}

fn join(words: &[(f64, f64, String)]) -> String {
    words.iter().map(|(_, _, w)| w.as_str()).collect::<Vec<_>>().join(" ")
}

/// Most common language Whisper hears at the start of the three longest pieces.
fn detect_language(model: &WhisperModel, state: &mut WhisperState, pieces: &[&[f32]]) -> String {
    let mut longest: Vec<&[f32]> = pieces.to_vec();
    longest.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let mut votes: Vec<(String, usize)> = Vec::new();
    for piece in longest.into_iter().take(3) {
        let mut params = WhisperParams::default();
        params.language = "auto".into();
        params.max_tokens = 1;
        state.transcribe(model, piece, &params);
        if let Some((code, _)) = state.detected_language() {
            match votes.iter_mut().find(|(c, _)| c == code) {
                Some(v) => v.1 += 1,
                None => votes.push((code.to_string(), 1)),
            }
        }
    }
    // Ties go to the longest piece's language (the first counted).
    let best = votes.iter().map(|(_, n)| *n).max().unwrap_or(0);
    votes.into_iter().find(|(_, n)| *n == best).map_or_else(|| "en".to_string(), |(c, _)| c)
}

/// Words that look like singing: not boilerplate, not a loop, and about one per 1.5 s.
fn usable(words: &[(f64, f64, String)], seconds: f64) -> bool {
    let text = join(words);
    !text.is_empty() && !is_boilerplate(&text) && !looped(&text) && words.len() as f64 >= seconds / 1.5
}

/// Whisper's subtitle-trained boilerplate that appears over instrumentals, or
/// no words at all ("***"). Whole lines only: "muziek" is also a real Dutch word.
fn is_boilerplate(text: &str) -> bool {
    let core = text.trim_matches(|c: char| !c.is_alphanumeric() && c != '_').to_lowercase();
    core.is_empty()
        || ["zang en muziek", "muziek", "tv gelderland", "music"].contains(&core.as_str())
        || ["ondertiteling", "ondertiteld", "thanks for watching", "subtitles by"].iter().any(|p| core.starts_with(p))
        || core.contains("amara.org")
}

/// Whisper stuck repeating itself ("zang en muziek zang en muziek ..."): it
/// compresses more than 3:1 (zlib, as the sidecar measures it).
fn looped(text: &str) -> bool {
    let data = text.as_bytes();
    data.len() > 80 && data.len() as f64 / makepad_fast_inflate::zlib_compress(data, 6).len() as f64 > 3.0
}

/// RMS of 0.2 s frames every 0.1 s (librosa's centred `rms`), and which of
/// them are louder than 5 % of the 95th percentile (and above -80 dB).
fn loud_frames(audio: &[f32]) -> (Vec<f32>, Vec<bool>) {
    let (hop, win) = (SR / FPS, 2 * SR / FPS);
    let rms: Vec<f32> = (0..=audio.len() / hop)
        .map(|i| {
            let c = (i * hop) as isize;
            let (a, b) = ((c - win as isize / 2).max(0) as usize, ((c + win as isize / 2) as usize).min(audio.len()));
            let sum: f32 = audio[a.min(b)..b].iter().map(|x| x * x).sum();
            (sum / win as f32).sqrt()
        })
        .collect();
    let mut sorted = rms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = sorted[((sorted.len() - 1) as f32 * 0.95) as usize];
    let thold = (p95 * 0.05).max(1e-4);
    let loud = rms.iter().map(|r| *r > thold).collect();
    (rms, loud)
}

/// Pieces Whisper decodes one at a time: voiced stretches split at pauses, at
/// most `max_len` s, in seconds with 0.2 s either side.
fn voiced_chunks(rms: &[f32], loud: &[bool], max_len: f64) -> Vec<(f64, f64)> {
    let mut regions: Vec<(usize, usize)> = Vec::new();
    for (i, _) in loud.iter().enumerate().filter(|(_, l)| **l) {
        match regions.last_mut() {
            Some(r) if i - r.1 <= FPS / 2 => r.1 = i + 1,
            _ => regions.push((i, i + 1)),
        }
    }
    regions.retain(|(a, b)| b - a >= 3);
    let longest = (max_len * FPS as f64) as usize;
    let mut split = Vec::new();
    while !regions.is_empty() {
        let (a, b) = regions.remove(0);
        // Break stretches that are too long at their quietest frame.
        if b - a <= longest {
            split.push((a, b));
        } else {
            let lo = a + 2 * FPS;
            let quietest = (lo..b - 2 * FPS).min_by(|&i, &j| rms[i].partial_cmp(&rms[j]).unwrap()).unwrap_or(lo);
            regions.insert(0, (quietest, b));
            regions.insert(0, (a, quietest));
        }
    }
    let mut chunks: Vec<(usize, usize)> = Vec::new();
    for (a, b) in split {
        // Join neighbours while they fit and the gap is short.
        match chunks.last_mut() {
            Some(c) if b - c.0 <= longest && a - c.1 <= 2 * FPS => c.1 = b,
            _ => chunks.push((a, b)),
        }
    }
    let end = rms.len() as f64 / FPS as f64;
    chunks.into_iter().map(|(a, b)| ((a as f64 / FPS as f64 - 0.2).max(0.0), (b as f64 / FPS as f64 + 0.2).min(end))).collect()
}

/// Group words into sung lines at pauses, sentence ends and piece edges; a
/// line still longer than 10 s is split at its longest pause.
fn words_to_lines(words: &[Word]) -> Vec<(f64, f64, String)> {
    let mut lines: Vec<Vec<&Word>> = Vec::new();
    let mut cur: Vec<&Word> = Vec::new();
    for w in words {
        if let Some(last) = cur.last() {
            if w.chunk != last.chunk || w.start - last.end >= 1.0 || last.text.ends_with(['.', '?', '!']) {
                lines.push(std::mem::take(&mut cur));
            }
        }
        cur.push(w);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    let mut done = Vec::new();
    while !lines.is_empty() {
        let line = lines.remove(0);
        if line.last().unwrap().end - line[0].start <= 10.0 || line.len() < 6 {
            done.push(line);
            continue;
        }
        // The longest pause (a comma first) that leaves each half at least a third of the line.
        let (t0, t1) = (line[0].start, line.last().unwrap().end);
        let fair: Vec<usize> = (1..line.len()).filter(|&i| (line[i].start - t0).min(t1 - line[i].start) >= (t1 - t0) / 3.0).collect();
        let key = |i: usize| (line[i - 1].text.ends_with([',', ';', ':']), line[i].start - line[i - 1].end);
        let cut = match fair.iter().copied().max_by(|&i, &j| key(i).partial_cmp(&key(j)).unwrap()) {
            Some(cut) => cut,
            None => (1..line.len()).min_by(|&i, &j| (line[i].start - (t0 + t1) / 2.0).abs().partial_cmp(&(line[j].start - (t0 + t1) / 2.0).abs()).unwrap()).unwrap(),
        };
        let (a, b) = line.split_at(cut);
        lines.insert(0, b.to_vec());
        lines.insert(0, a.to_vec());
    }
    done.into_iter()
        .map(|l| (l[0].start, l.last().unwrap().end, l.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")))
        .collect()
}

fn norm(word: &str) -> String {
    word.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// The song's words from lyrics.txt, one entry per sung line ([Section] tags dropped).
fn known_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !(l.starts_with('[') && l.ends_with(']')) && l.split_whitespace().any(|w| !norm(w).is_empty()))
        .map(str::to_string)
        .collect()
}

/// How alike two words are, 0..1: 2 * longest common subsequence / total
/// length (close to the sidecar's `difflib` ratio on single words).
fn similarity(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut prev = vec![0usize; b.len() + 1];
    for x in &a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y { prev[j] + 1 } else { prev[j + 1].max(cur[j]) };
        }
        prev = cur;
    }
    2.0 * prev[b.len()] as f64 / (a.len() + b.len()) as f64
}

/// Time the known lines with the heard words: an edit-distance alignment of
/// the two word lists, so skipped, repeated or misheard lines don't throw the
/// rest off. A known line takes the span of the words aligned to it; lines
/// nobody sang are dropped, and sung stretches the text doesn't have keep
/// their transcription.
fn align_known(lines: &[String], words: &[Word]) -> Vec<(f64, f64, String)> {
    let known: Vec<(String, usize)> =
        lines.iter().enumerate().flat_map(|(i, l)| l.split_whitespace().map(norm).filter(|w| !w.is_empty()).map(move |w| (w, i))).collect();
    let heard: Vec<String> = words.iter().map(|w| norm(&w.text)).collect();
    let (n, m) = (known.len(), heard.len());
    let sub = |a: &str, b: &str| if a == b { 0.0 } else if similarity(a, b) >= 0.6 { 0.5 } else { 1.0 };
    let mut cost = vec![vec![0.0f64; m + 1]; n + 1];
    for (i, row) in cost.iter_mut().enumerate() {
        row[0] = i as f64;
    }
    for j in 0..=m {
        cost[0][j] = j as f64;
    }
    for i in 1..=n {
        for j in 1..=m {
            let c = sub(&known[i - 1].0, &heard[j - 1]);
            cost[i][j] = (cost[i - 1][j] + 1.0).min(cost[i][j - 1] + 1.0).min(cost[i - 1][j - 1] + c);
        }
    }
    let mut spans: Vec<Vec<usize>> = vec![Vec::new(); lines.len()];
    let mut used = vec![false; m];
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        let c = sub(&known[i - 1].0, &heard[j - 1]);
        if cost[i][j] == cost[i - 1][j - 1] + c {
            if c < 1.0 {
                spans[known[i - 1].1].push(j - 1);
                used[j - 1] = true;
            }
            i -= 1;
            j -= 1;
        } else if cost[i][j] == cost[i - 1][j] + 1.0 {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    let mut out = Vec::new();
    for (li, js) in spans.iter().enumerate() {
        let size = known.iter().filter(|(_, l)| *l == li).count();
        // At least a third of the line was heard.
        if !js.is_empty() && js.len() * 3 >= size {
            let start = js.iter().map(|&j| words[j].start).fold(f64::MAX, f64::min);
            let end = js.iter().map(|&j| words[j].end).fold(f64::MIN, f64::max);
            out.push((start, end, lines[li].clone()));
        }
    }
    // Stretches of three or more words the text doesn't have (extra repeats, ad-libs).
    let mut run: Vec<Word> = Vec::new();
    for j in 0..=m {
        if j < m && !used[j] {
            run.push(words[j].clone());
            continue;
        }
        if run.len() >= 3 {
            out.extend(words_to_lines(&run).into_iter().filter(|l| l.2.split_whitespace().count() >= 3));
        }
        run.clear();
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    for k in 1..out.len() {
        out[k - 1].1 = out[k - 1].1.min(out[k].0);
    }
    out.retain(|l| l.1 > l.0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(start: f64, end: f64, text: &str, chunk: usize) -> Word {
        Word { start, end, text: text.into(), chunk }
    }

    #[test]
    fn boilerplate_is_whole_lines_only() {
        assert!(is_boilerplate("MUZIEK"));
        assert!(is_boilerplate("***"));
        assert!(is_boilerplate("Ondertiteling TV Gelderland 2021"));
        assert!(!is_boilerplate("Ik hou van muziek"));
        assert!(!is_boilerplate("It's autumn"));
    }

    #[test]
    fn lead_in_words_are_dropped() {
        let before: Vec<String> = ["changed", "to", "gold", "and", "brown."].iter().map(|w| w.to_string()).collect();
        let heard = vec![(30.0, 31.0, "gold".into()), (36.9, 38.5, "brown.".into()), (38.5, 39.6, "It's".into()), (39.6, 41.3, "autumn,".into())];
        let kept = after_lead_in(heard, 36.7, &before);
        assert_eq!(kept.iter().map(|w| w.2.as_str()).collect::<Vec<_>>(), ["It's", "autumn,"]);
    }

    #[test]
    fn loops_compress() {
        assert!(looped(&"zang en muziek ".repeat(12)));
        assert!(!looped("Ik zoek een mooie, rijke vrouw. Mooie, rijke vrouw. Ik zoek een mooie, rijke vrouw, zonder problemen."));
    }

    #[test]
    fn chunks_split_at_pauses() {
        // Voice 1-3 s, a 0.3 s breath, 3.3-5 s, then silence, then 40 s of voice.
        let mut loud = vec![false; 1000];
        for i in (10..30).chain(33..50).chain(400..800) {
            loud[i] = true;
        }
        let rms: Vec<f32> = loud.iter().enumerate().map(|(i, l)| if *l { 1.0 + (i % 7) as f32 } else { 0.0 }).collect();
        let chunks = voiced_chunks(&rms, &loud, 20.0);
        assert!((chunks[0].0 - 0.8).abs() < 1e-9 && (chunks[0].1 - 5.2).abs() < 1e-9, "{chunks:?}");
        assert!(chunks.iter().all(|(a, b)| b - a <= 20.4 + 1e-9));
        assert!(chunks.len() >= 3, "the 40 s stretch is split: {chunks:?}");
    }

    #[test]
    fn lines_split_at_pauses_and_sentences() {
        let words = vec![word(0.0, 0.4, "Hey,", 0), word(0.5, 0.9, "look.", 0), word(1.0, 1.3, "Then", 0), word(3.0, 3.3, "after", 0)];
        let lines = words_to_lines(&words);
        assert_eq!(lines.iter().map(|l| l.2.as_str()).collect::<Vec<_>>(), ["Hey, look.", "Then", "after"]);
        let long: Vec<Word> = (0..20).map(|i| word(i as f64 * 0.7, i as f64 * 0.7 + 0.5, "la", 0)).collect();
        assert!(words_to_lines(&long).iter().all(|l| l.1 - l.0 <= 10.0));
    }

    #[test]
    fn known_lines_take_the_heard_times() {
        let known = known_lines("[Verse]\nDe zon komt op\nDe haan roept luid\n\n[Chorus]\nNooit gezongen regel hier");
        assert_eq!(known.len(), 3);
        let heard = vec![
            word(1.0, 1.2, "de", 0),
            word(1.3, 1.5, "zon", 0),
            word(1.6, 1.8, "komt", 0),
            word(1.9, 2.1, "op", 0),
            word(3.0, 3.2, "de", 1),
            word(3.3, 3.5, "haan", 1),
            word(3.6, 3.8, "roep", 1),
            word(3.9, 4.1, "luid", 1),
        ];
        let lines = align_known(&known, &heard);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0], (1.0, 2.1, "De zon komt op".to_string()));
        assert_eq!(lines[1].2, "De haan roept luid");
        assert!((lines[1].0 - 3.0).abs() < 1e-9);
    }
}
