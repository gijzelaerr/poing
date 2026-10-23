//! Poing as an AI service (Makepad's `makepad-ai-services`): a manifest of
//! tools an agent can call (Claude Code through `makepad-remote mcp poing`,
//! or a Makepad chat host), what the song looks like to it, and reading its
//! arguments. What each tool does lives with the App (main.rs), which owns
//! the state, the jobs and the UI.

use crate::state::{named_take, EditorState, Selection};
use makepad_ai_services::wire::{Risk, ServiceManifest, ToolDef};
use makepad_strict_json::{self as json, Value};

pub fn manifest() -> ServiceManifest {
    let tool = |name: &str, what: &str, schema: &str, risk: Risk| ToolDef::new(name, what, schema, risk);
    ServiceManifest::new(
        "poing",
        "Poing",
        "A music workstation with one song open: its lanes (stems and MIDI parts), sections, takes (versions, remixes, repaints) and effects. Read `song` first; lanes, sections and effects are addressed by their index in it, takes by id.",
    )
    .with_tool(tool(
        "song",
        "The open song: key, tempo, length, lanes (level, mute, solo, effects with their knobs), sections, takes (which are on), whether a job is running (`busy`), the playhead and the last status line. Read it again after a job to see its result.",
        r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        Risk::Read,
    ))
    .with_tool(tool(
        "describe",
        "Ask for a change in words, exactly as a person types it in Poing's Describe bar. With `lane` it works on that lane (\"less echo\", \"tape saturation\", \"funky slap bass\", \"play it with Serum 2\"); with `section` it repaints that section; with neither it works on the whole song (\"add a funky bassline\" writes a MIDI part, \"... on the master\" an effect, else whole-song remixes). Starts a job (seconds to minutes): `song.busy` is true until it is done, `song.status` says what came of it.",
        r#"{"type":"object","properties":{"text":{"type":"string","maxLength":500},"lane":{"type":"integer","minimum":0},"section":{"type":"integer","minimum":0}},"required":["text"],"additionalProperties":false}"#,
        Risk::Request,
    ))
    .with_tool(tool(
        "set_lane",
        "Set a lane's level in dB (-36..12), mute or solo.",
        r#"{"type":"object","properties":{"lane":{"type":"integer","minimum":0},"gain_db":{"type":"number","minimum":-36,"maximum":12},"mute":{"type":"boolean"},"solo":{"type":"boolean"}},"required":["lane"],"additionalProperties":false}"#,
        Risk::Act,
    ))
    .with_tool(tool(
        "effect",
        "Turn an effect's knob (by its name in `song`) or bypass it. `lane` absent means the master chain; `effect` is the index in that chain.",
        r#"{"type":"object","properties":{"lane":{"type":"integer","minimum":0},"effect":{"type":"integer","minimum":0},"knob":{"type":"string"},"value":{"type":"number"},"bypass":{"type":"boolean"}},"required":["effect"],"additionalProperties":false}"#,
        Risk::Act,
    ))
    .with_tool(tool(
        "take",
        "Switch a take on or off by id (a whole-song take replaces the lanes while on; \"original\" switches every take off). With `use: true` on a new song's version, that version becomes the song and gets analysed.",
        r#"{"type":"object","properties":{"id":{"type":"string"},"on":{"type":"boolean"},"use":{"type":"boolean"}},"required":["id"],"additionalProperties":false}"#,
        Risk::Act,
    ))
    .with_tool(tool(
        "combine",
        "Take one section from a remix (a take whose `aligned` is true): the section plays from that remix, the rest of the song stays as it is. Repeat with other remixes and sections to combine them.",
        r#"{"type":"object","properties":{"section":{"type":"integer","minimum":0},"take":{"type":"string"}},"required":["section","take"],"additionalProperties":false}"#,
        Risk::Act,
    ))
    .with_tool(tool(
        "transport",
        "Play or pause, jump to a time in seconds, or loop a range ([start, end] seconds; null stops looping).",
        r#"{"type":"object","properties":{"play":{"type":"boolean"},"seek":{"type":"number","minimum":0},"loop":{"type":["array","null"],"items":{"type":"number"},"minItems":2,"maxItems":2}},"additionalProperties":false}"#,
        Risk::Act,
    ))
    .with_tool(tool(
        "export",
        "Render the song as it plays now (lanes, takes, effects) to a WAV and a mastered MP3 in the song's renders folder; `song.status` names the file when it is done.",
        r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        Risk::Request,
    ))
}

/// A call's arguments as an object's fields.
pub fn args(raw: &str) -> Result<Vec<(String, Value)>, String> {
    if raw.len() > 16 * 1024 {
        return Err("arguments too large".into());
    }
    match json::parse(if raw.trim().is_empty() { b"{}" } else { raw.as_bytes() }) {
        Ok(Value::Obj(pairs)) => Ok(pairs),
        Ok(_) => Err("arguments must be an object".into()),
        Err(_) => Err("invalid arguments".into()),
    }
}

pub fn get<'a>(args: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    args.iter().find(|(k, _)| k == key).map(|(_, v)| v).filter(|v| !v.is_null())
}

pub fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::F64(f) => Some(*f),
        _ => None,
    }
}

pub fn index(args: &[(String, Value)], key: &str) -> Option<usize> {
    get(args, key).and_then(|v| v.as_u64()).map(|i| i as usize)
}

fn f(x: f64) -> Value {
    Value::F64((x * 1000.0).round() / 1000.0)
}

/// The song as an agent sees it (`song`'s data).
pub fn summary(st: &EditorState, status: &str) -> String {
    let p = &st.project;
    let a = p.analysis.clone().unwrap_or_default();
    let chain = |lane: Option<usize>| -> Value {
        Value::Arr(
            p.chain(lane)
                .iter()
                .enumerate()
                .map(|(i, fx)| {
                    let knobs = st
                        .fx_knobs(&fx.id)
                        .into_iter()
                        .map(|(name, value, min, max)| json::obj(vec![("name", json::s(name)), ("value", f(value)), ("min", f(min)), ("max", f(max))]))
                        .collect();
                    json::obj(vec![
                        ("index", Value::Int(i as i64)),
                        ("name", json::s(fx.name.clone())),
                        ("plugin", Value::Bool(fx.plugin.is_some())),
                        ("bypass", Value::Bool(fx.bypassed())),
                        ("knobs", Value::Arr(knobs)),
                    ])
                })
                .collect(),
        )
    };
    let lanes = p
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut fields = vec![
                ("index", Value::Int(i as i64)),
                ("id", json::s(t.id.clone())),
                ("name", json::s(t.name.clone())),
                ("gain_db", f(t.gain())),
                ("mute", Value::Bool(t.muted())),
                ("solo", Value::Bool(t.soloed())),
                ("effects", chain(Some(i))),
            ];
            if let Some(notes) = &t.notes {
                fields.push(("midi_notes", Value::Int(notes.len() as i64)));
                let by = t.instrument.as_ref().filter(|x| x.plugin.is_some()).map(|x| x.name.clone()).unwrap_or_else(|| "Poing's built-in synth".into());
                fields.push(("instrument", json::s(by)));
            }
            json::obj(fields)
        })
        .collect();
    let sections = p
        .sections()
        .iter()
        .enumerate()
        .map(|(i, s)| json::obj(vec![("index", Value::Int(i as i64)), ("label", json::s(s.label.clone())), ("start", f(s.start)), ("end", f(s.end))]))
        .collect();
    let takes = p
        .takes()
        .iter()
        .map(|t| {
            let name = if named_take(t) { t.prompt.clone() } else { format!("Take {}: {}", t.id.trim_start_matches('t'), t.prompt) };
            json::obj(vec![
                ("id", json::s(t.id.clone())),
                ("name", json::s(name)),
                ("scope", json::s(t.scope.clone())),
                ("start", f(t.start)),
                ("end", f(t.end)),
                ("on", Value::Bool(st.take_on(&t.id))),
                ("aligned", Value::Bool(t.engine == "acestep-cover")),
            ])
        })
        .collect();
    let selected = match st.sel {
        Selection::Track(i) => format!("lane {i}"),
        Selection::Section(i) => format!("section {i}"),
        Selection::Region(i) => format!("region {i}"),
        Selection::Fx(Some(l), i) => format!("effect {i} on lane {l}"),
        Selection::Fx(None, i) => format!("master effect {i}"),
        _ => "nothing".into(),
    };
    json::obj(vec![
        ("name", json::s(p.name.clone())),
        ("key", a.key.map(json::s).unwrap_or(Value::Null)),
        ("bpm", a.tempo.map(|t| f(t.round())).unwrap_or(Value::Null)),
        ("duration", f(st.duration())),
        ("analysed", Value::Bool(p.is_analysed())),
        ("draft", Value::Bool(p.is_draft())),
        ("busy", st.analysing.as_ref().map(|(what, pr)| json::s(format!("{what} {:.0}%", pr * 100.0))).unwrap_or(Value::Bool(false))),
        ("status", json::s(status)),
        ("playing", Value::Bool(st.shared.playing())),
        ("playhead", f(st.playhead())),
        ("selected", json::s(selected)),
        ("lanes", Value::Arr(lanes)),
        ("master_effects", chain(None)),
        ("sections", Value::Arr(sections)),
        ("takes", Value::Arr(takes)),
    ])
    .to_json()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_is_valid_and_arguments_read() {
        let m = manifest();
        assert!(m.validate().is_ok(), "{:?}", m.validate());
        assert_eq!(m.id, "poing");
        let a = args(r#"{"lane": 2, "gain_db": -3.5, "mute": true}"#).unwrap();
        assert_eq!(index(&a, "lane"), Some(2));
        assert_eq!(get(&a, "gain_db").and_then(num), Some(-3.5));
        assert_eq!(get(&a, "mute").and_then(|v| v.as_bool()), Some(true));
        assert!(args("[1]").is_err());
        assert!(args("").unwrap().is_empty());
    }
}
