//! Local model against Claude Code on the same requests, through the real
//! writers. Not run by default (each case is a model call):
//!
//!     POING_BENCH_PROVIDER=local|claude POING_LLM_MODEL=qwen3.5-4b \
//!     POING_ENGINE=target/release/poing-engine.exe POING_BENCH_OUT=bench.jsonl \
//!     POING_BENCH_SONG=…/its-autumn.poing/project.json \
//!     cargo test --release -p poing bench_ -- --ignored --nocapture --test-threads 1
//!
//! One JSON line per case goes to POING_BENCH_OUT.

use crate::llm::{self, Provider};
use crate::{fx_writer, midi_writer, song_writer};
use makepad_strict_json::{self as json, Value};
use poing_model::{BeatGrid, Project};
use std::path::PathBuf;
use std::time::Instant;

fn setup() -> (String, PathBuf) {
    let p = match std::env::var("POING_BENCH_PROVIDER").as_deref() {
        Ok("claude") => Provider::ClaudeCode,
        _ => Provider::Local,
    };
    llm::set_provider(p);
    let label = match p {
        Provider::ClaudeCode => format!("claude:{}", std::env::var("POING_CLAUDE_MODEL").unwrap_or_else(|_| "default".into())),
        Provider::Local => format!("local:{}", llm::model().unwrap_or_else(|| "default".into())),
    };
    let work = std::env::temp_dir().join("poing-bench").join(label.replace(':', "-"));
    (label, work)
}

/// `POING_BENCH_ONLY=fx,midi` runs just those.
fn skip(kind: &str) -> bool {
    std::env::var("POING_BENCH_ONLY").is_ok_and(|only| !only.split(',').any(|k| k == kind))
}

/// Other model processes on the GPU (not this worktree's engine): their
/// work slows ours, so such a row's times are contended.
fn gpu_others() -> Vec<String> {
    let out = std::process::Command::new("nvidia-smi").args(["--query-compute-apps=process_name", "--format=csv,noheader"]).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    text.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| ["poing", "python", "engine", "yue", "ace"].iter().any(|k| l.to_lowercase().contains(k)) && !l.contains("local-llm"))
        .map(|l| l.rsplit(['\\', '/']).next().unwrap_or_default().to_string())
        .collect()
}

fn record(mut fields: Vec<(&str, Value)>) {
    fields.push(("gpu_others", Value::Arr(gpu_others().into_iter().map(json::s).collect())));
    let line = json::obj(fields).to_json();
    println!("{line}");
    if let Ok(out) = std::env::var("POING_BENCH_OUT") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(out) {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// The model calls a case made (from the writer's llm.log): tokens written, any cut off.
fn calls(dir: &std::path::Path) -> Vec<(&'static str, Value)> {
    let log = std::fs::read_to_string(dir.join("llm.log")).unwrap_or_default();
    let field = |l: &str, k: &str| l.split_whitespace().find_map(|w| w.strip_prefix(k)).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    let tokens: f64 = log.lines().map(|l| field(l, "tokens=")).sum();
    let gen_secs: f64 = log.lines().map(|l| field(l, "secs=")).sum();
    vec![
        ("calls", Value::Int(log.lines().count() as i64)),
        ("tokens", Value::Int(tokens as i64)),
        ("tok_per_s", Value::F64(if gen_secs > 0.0 { (tokens / gen_secs).round() } else { 0.0 })),
        ("cut_off", Value::Int(log.lines().filter(|l| l.contains("truncated=true")).count() as i64)),
    ]
}

fn secs(t: Instant) -> Value {
    Value::F64((t.elapsed().as_secs_f64() * 10.0).round() / 10.0)
}

/// Share of words that are common Dutch or English function words.
fn language_of(lyrics: &str) -> &'static str {
    const NL: [&str; 16] = ["de", "het", "een", "en", "ik", "je", "niet", "van", "dat", "wij", "we", "zijn", "mijn", "op", "naar", "met"];
    const EN: [&str; 16] = ["the", "and", "i", "you", "of", "to", "my", "in", "it", "we", "is", "your", "on", "me", "with", "all"];
    let words: Vec<String> = lyrics.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect();
    let nl = words.iter().filter(|w| NL.contains(&w.as_str()) && !EN.contains(&w.as_str())).count();
    let en = words.iter().filter(|w| EN.contains(&w.as_str()) && !NL.contains(&w.as_str())).count();
    if nl + en < 5 {
        "?"
    } else if nl > en * 2 {
        "nl"
    } else if en > nl * 2 {
        "en"
    } else {
        "mixed"
    }
}

#[test]
#[ignore]
fn bench_sheets() {
    if skip("sheets") {
        return;
    }
    let (label, work) = setup();
    let cases = [
        ("een zomerlied over de boerderij in Namibië, akoestische gitaar, vrolijk", "nl"),
        ("a melancholic piano ballad in Dutch about rain in Amsterdam", "nl"),
        ("an upbeat 80s synth-pop song about driving at night", "en"),
        ("a country song about a goat farm, fiddle and banjo, female vocals", "en"),
    ];
    for (i, (prompt, want)) in cases.iter().enumerate() {
        let dir = work.join(format!("sheet-{i}"));
        let _ = std::fs::remove_dir_all(&dir);
        let t = Instant::now();
        let r = song_writer::write_sheet(prompt, &dir);
        let first_try = !dir.join("sheet-2.md").exists();
        let mut fields = calls(&dir);
        fields.extend(vec![("kind", json::s("sheet")), ("provider", json::s(label.clone())), ("case", json::s(*prompt)), ("want", json::s(*want)), ("secs", secs(t))]);
        match r {
            Ok(s) => {
                fields.push(("valid_first_try", Value::Bool(first_try)));
                fields.push(("language_field", json::s(s.language.clone().unwrap_or_default())));
                fields.push(("lyrics_language", json::s(language_of(&s.lyrics))));
                fields.push(("lyric_lines", Value::Int(s.lyrics.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('[')).count() as i64)));
                fields.push(("title", json::s(s.title)));
                fields.push(("caption", json::s(s.caption)));
            }
            Err(e) => fields.push(("error", json::s(e))),
        }
        record(fields);
    }
    llm::release();
}

fn song() -> Project {
    let path = std::env::var("POING_BENCH_SONG").expect("POING_BENCH_SONG: a project.json");
    Project::load(std::path::Path::new(&path)).expect("the bench song loads")
}

#[test]
#[ignore]
fn bench_midi() {
    if skip("midi") {
        return;
    }
    let (label, work) = setup();
    let p = song();
    let bars = midi_writer::Bars::of(&p);
    let grid = BeatGrid::of(&p);
    let sheet = midi_writer::song_sheet(&p, bars);
    let key = p.analysis.clone().unwrap_or_default().key.unwrap_or_default();
    let in_key = midi_writer::scale(&key);
    let whole = (bars.first_beat, grid.beat_at(p.duration).floor());
    let chorus = p.sections().iter().find(|s| s.label == "chorus").map(|s| (grid.beat_at(s.start).round(), grid.beat_at(s.end).round())).unwrap_or(whole);
    let cases = [
        ("bass", "walking upright bass, smooth jazz", whole, "whole song"),
        ("keys", "warm Rhodes chords, laid back", chorus, "chorus"),
        ("drums", "brushed jazz swing groove", chorus, "chorus"),
        ("lead", "a simple catchy synth melody", chorus, "chorus"),
    ];
    for (i, (role, request, range, place)) in cases.iter().enumerate() {
        let dir = work.join(format!("midi-{i}"));
        let _ = std::fs::remove_dir_all(&dir);
        let t = Instant::now();
        let r = midi_writer::write_part(request, role, &sheet, bars, *range, &dir, &|_, _| {});
        let mut fields = calls(&dir);
        fields.extend(vec![("kind", json::s("midi")), ("provider", json::s(label.clone())), ("case", json::s(format!("{role}: {request} ({place})"))), ("secs", secs(t))]);
        match r {
            Ok(w) => {
                let pitched = *role != "drums";
                let share = in_key.filter(|_| pitched).map(|sc| w.notes.iter().filter(|n| sc[n.pitch as usize % 12]).count() as f64 / w.notes.len().max(1) as f64);
                fields.push(("rounds", Value::Int(w.rounds as i64)));
                fields.push(("problems_left", Value::Int(w.problems.len() as i64)));
                fields.push(("problems", json::s(w.problems.join("; "))));
                fields.push(("notes", Value::Int(w.notes.len() as i64)));
                fields.push(("in_key", share.map(|s| Value::F64((s * 1000.0).round() / 1000.0)).unwrap_or(Value::Null)));
                fields.push(("name", json::s(w.name)));
            }
            Err(e) => fields.push(("error", json::s(e))),
        }
        record(fields);
    }
    llm::release();
}

#[test]
#[ignore]
fn bench_fx() {
    if skip("fx") {
        return;
    }
    let (label, work) = setup();
    let cases = [
        ("tape saturation", "the Bass lane (its bass stem) of a song in E minor at 125 bpm; its level is about -22 dBFS RMS with peaks near -6 dBFS"),
        ("warm stereo chorus", "the Other lane (its other stem) of a song in E minor at 125 bpm; its level is about -24 dBFS RMS with peaks near -8 dBFS"),
        ("lo-fi bitcrusher", "the Drums lane (its drums stem) of a song in E minor at 125 bpm; its level is about -20 dBFS RMS with peaks near -3 dBFS"),
        ("ping-pong delay, dotted eighths", "the Vocals lane (its vocals stem) of a song in E minor at 125 bpm; its level is about -21 dBFS RMS with peaks near -5 dBFS"),
        ("gentle bus compressor", "the master bus (the whole mix) of a song in E minor at 125 bpm"),
    ];
    for (i, (request, context)) in cases.iter().enumerate() {
        let dir = work.join(format!("fx-{i}"));
        let _ = std::fs::remove_dir_all(&dir);
        let t = Instant::now();
        let r = fx_writer::write_effect(request, context, &dir, &|_, _| {});
        let mut fields = calls(&dir);
        fields.extend(vec![("kind", json::s("fx")), ("provider", json::s(label.clone())), ("case", json::s(*request)), ("secs", secs(t))]);
        match r {
            Ok(w) => {
                let ops = makepad_script_audio_aot::check(&w.code).map(|r| r.ops_per_frame).ok();
                fields.push(("passed", Value::Bool(w.problems.is_empty())));
                fields.push(("rounds", Value::Int(w.rounds as i64)));
                fields.push(("problems", json::s(w.problems.join("; "))));
                fields.push(("ops_per_sample", ops.map(|o| Value::F64(o.round())).unwrap_or(Value::Null)));
                fields.push(("name", json::s(w.name)));
            }
            Err(e) => fields.push(("error", json::s(e))),
        }
        record(fields);
    }
    llm::release();
}

#[test]
fn lyrics_have_a_language() {
    assert_eq!(language_of("ik loop met jou naar de zee, en het is mijn dag"), "nl");
    assert_eq!(language_of("I drive with you to the sea and it is my day"), "en");
}
