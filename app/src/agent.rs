//! Poing as an AI service (Makepad's `makepad-ai-services`): a manifest of
//! tools an agent can call (Poing's own model through the process's service
//! registry, Claude Code through `makepad-remote mcp poing`, or a Makepad
//! chat host), what the song looks like to it, and reading its arguments. What each tool does lives with the App (main.rs), which owns
//! the state, the jobs and the UI.

use crate::state::{named_take, EditorState, Selection};
use makepad_ai_services::engine::RegistryUp;
use makepad_ai_services::wire::{EndpointId, Risk, ServiceCall, ServiceManifest, ToolDef, ToolOutcome, ToolResult};
use makepad_ai_services::ServiceRegistry;
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
        "listen",
        "Hear a range of the song (seconds, at most 30; about a second to compute): the mix as it plays now, or with `lane` that lane alone (its level, takes and effects, before the master). Returns loudness (`lufs` integrated, `short_term_max_lufs`), `true_peak_dbtp`, `clipped_samples`, `crest_db` (peak over RMS: under ~8 dense or squashed, over ~14 punchy or spiky), stereo `correlation` (1 mono, 0 unrelated, below 0 phase trouble) and `width` (side over mid: 0 mono, ~1 very wide), and `band_db`, the level in each band: sub 20-60 Hz, bass 60-120, upper_bass 120-250, low_mid 250-500 (mud), mid 500-1k (boxy), upper_mid 1-2k (nasal), high_mid 2-4k (harsh, vocal clarity), presence 4-8k (bite, sibilance), air 8-20k. `notes` says what the measurement leaves out (VST3 plugins cannot be run a second time: their effects are measured dry, their instruments silent).",
        r#"{"type":"object","properties":{"start":{"type":"number","minimum":0},"end":{"type":"number","minimum":0},"lane":{"type":"integer","minimum":0}},"required":["start","end"],"additionalProperties":false}"#,
        Risk::Read,
    ))
    .with_tool(tool(
        "compare",
        "Hear each lane alone and the mix over one range (seconds, at most 30), in one call: per lane its loudness, true peak, crest, width and `band_db` (bands as in `listen`), and `band_leaders`, the loudest lanes in each band with their level: two lanes within a few dB in one band mask each other there. `lanes` picks lanes by index (all when absent).",
        r#"{"type":"object","properties":{"start":{"type":"number","minimum":0},"end":{"type":"number","minimum":0},"lanes":{"type":"array","items":{"type":"integer","minimum":0}}},"required":["start","end"],"additionalProperties":false}"#,
        Risk::Read,
    ))
    .with_tool(tool(
        "describe",
        "Ask for a change in words, exactly as a person types it in Poing's Describe bar. With `lane` it works on that lane (\"less echo\", \"tape saturation\", \"funky slap bass\", \"play it with Serum 2\"); with `section` it repaints that section; with neither it works on the whole song (\"... on the master\" writes a master effect, anything else makes three whole-song remixes that replace the lanes while one is on). For a new MIDI part use `add_part` instead. Starts a job (seconds to minutes): `song.busy` is true until it is done, `song.status` says what came of it.",
        r#"{"type":"object","properties":{"text":{"type":"string","maxLength":500},"lane":{"type":"integer","minimum":0},"section":{"type":"integer","minimum":0}},"required":["text"],"additionalProperties":false}"#,
        Risk::Request,
    ))
    .with_tool(tool(
        "add_part",
        "Have Claude write a new MIDI part on the song's key, chords and beat grid, as a new lane: `role` is bass, keys, lead or drums; `text` says the style (\"walking upright bass, smooth jazz\"); `section` limits it to that section; `instrument` names an installed VST3 instrument to play it (else Poing's built-in synth). Returns when the part is in (about a minute).",
        r#"{"type":"object","properties":{"role":{"type":"string","enum":["bass","keys","lead","drums"]},"text":{"type":"string","maxLength":500},"section":{"type":"integer","minimum":0},"instrument":{"type":"string"}},"required":["role","text"],"additionalProperties":false}"#,
        Risk::Request,
    ))
    .with_tool(tool(
        "set_lane",
        "Change a lane's level, mute or solo it. `change_db` moves the level by that much from where it is now (+2 louder, -2 quieter); `gain_db` sets the level itself (-36..12, the lane's `gain_db` in `song`).",
        r#"{"type":"object","properties":{"lane":{"type":"integer","minimum":0},"change_db":{"type":"number","minimum":-24,"maximum":24},"gain_db":{"type":"number","minimum":-36,"maximum":12},"mute":{"type":"boolean"},"solo":{"type":"boolean"}},"required":["lane"],"additionalProperties":false}"#,
        Risk::Act,
    ))
    .with_tool(tool(
        "effect",
        "Turn an effect's knob (by its name in `song`) or bypass it. `lane` absent means the master chain; `effect` is the index in that chain. `own_process` runs a VST3 plugin in a process of its own (a crash there leaves Poing playing and the plugin is reloaded as it was; its audio comes ~43 ms later, which Poing compensates) or back in Poing.",
        r#"{"type":"object","properties":{"lane":{"type":"integer","minimum":0},"effect":{"type":"integer","minimum":0},"knob":{"type":"string"},"value":{"type":"number"},"bypass":{"type":"boolean"},"own_process":{"type":"boolean"}},"required":["effect"],"additionalProperties":false}"#,
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
        "arrange",
        "Change the song's order by section (indices as in `song`): `move` section `section` to before section `to` (the section count puts it at the end), `duplicate` it (a copy before `to`, by default right after itself), `delete` it, or `extend` the song by `seconds` at its end (ACE-Step goes on past the end: a job, see `song.busy`). Every lane, take, MIDI note, chord, lyric line and region in the section goes with it, spliced on downbeats; the user can undo each edit. Sections are renumbered after an edit: read `song` again.",
        r#"{"type":"object","properties":{"op":{"type":"string","enum":["move","duplicate","delete","extend"]},"section":{"type":"integer","minimum":0},"to":{"type":"integer","minimum":0},"seconds":{"type":"number","minimum":5,"maximum":120}},"required":["op"],"additionalProperties":false}"#,
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
                    let mut fields = vec![
                        ("index", Value::Int(i as i64)),
                        ("name", json::s(fx.name.clone())),
                        ("plugin", Value::Bool(fx.plugin.is_some())),
                        ("bypass", Value::Bool(fx.bypassed())),
                        ("knobs", Value::Arr(knobs)),
                    ];
                    fields.extend(plugin_fields(st, &fx.id));
                    json::obj(fields)
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
                // the lane's meter as shown (dBFS; -inf is silence)
                ("level_db", f(st.meter_show.get(i).map_or(-120.0, |v| (20.0 * v.max(1e-6).log10()).round() as f64))),
            ];
            if let Some(notes) = &t.notes {
                fields.push(("midi_notes", Value::Int(notes.len() as i64)));
                let by = t.instrument.as_ref().filter(|x| x.plugin.is_some()).map(|x| x.name.clone()).unwrap_or_else(|| "Poing's built-in synth".into());
                fields.push(("instrument", json::s(by)));
                fields.extend(plugin_fields(st, &t.id));
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
        Selection::Chord(i) => format!("chord {i}"),
        Selection::Lyric(i) => format!("lyric line {i}"),
        Selection::None => "nothing".into(),
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
        ("recording", match &st.rec {
            Some(r) if st.shared.live.counting_in() => json::s(format!("counting in on lane {}", r.lane)),
            Some(r) => json::s(format!("lane {}", r.lane)),
            None => Value::Bool(false),
        }),
        #[cfg(windows)]
        ("plugin_missed_blocks", Value::Int(crate::plugins::MISSED_BLOCKS.load(std::sync::atomic::Ordering::Relaxed) as i64)),
        ("lanes", Value::Arr(lanes)),
        ("master_effects", chain(None)),
        ("sections", Value::Arr(sections)),
        ("takes", Value::Arr(takes)),
    ])
    .to_json()
}

/// A loaded plugin's latency and where it runs, for `song`.
fn plugin_fields(st: &EditorState, id: &str) -> Vec<(&'static str, Value)> {
    let Some(i) = st.plugin_info(id) else { return Vec::new() };
    let mut out = vec![
        ("latency_ms", f(i.latency as f64 * 1000.0 / st.sr() as f64)),
        ("own_process", Value::Bool(i.isolated)),
    ];
    if let Some(pid) = i.pid {
        out.push(("process_id", Value::Int(pid as i64)));
    }
    if i.recoveries > 0 {
        out.push(("reloads_after_crash", Value::Int(i.recoveries as i64)));
    }
    if let Some((blocks, worst_us)) = i.bridge {
        out.push(("process_blocks", Value::Int(blocks as i64)));
        out.push(("process_slowest_ms", f(worst_us as f64 / 1000.0)));
    }
    out
}

// ------------------------------------------------- the assistant behind the Describe bar

use crate::llm::{self, on_path, Provider};

/// Makepad's MCP bridge: `POING_MAKEPAD_REMOTE`, else `makepad-remote` on PATH.
pub fn makepad_remote() -> Option<std::path::PathBuf> {
    match std::env::var_os("POING_MAKEPAD_REMOTE") {
        Some(p) => Some(std::path::PathBuf::from(p)).filter(|p| p.is_file()),
        None => on_path("makepad-remote"),
    }
}

/// Is Claude Code (`claude`) installed here?
pub fn claude_installed() -> bool {
    on_path("claude").is_some()
}

/// Why the assistant cannot work on the song here (None: it can).
pub fn unavailable() -> Option<String> {
    if let Some(why) = llm::unavailable() {
        return Some(why);
    }
    match llm::provider() {
        Provider::ClaudeCode if makepad_remote().is_none() => Some("Makepad's `makepad-remote` was not found (set POING_MAKEPAD_REMOTE or put it on PATH)".into()),
        Provider::Local if ServiceRegistry::process().pick_instance("poing", None).is_none() => Some("Poing's tools are not open to an assistant in this window".into()),
        _ => None,
    }
}

/// Words that ask to change the mix rather than make new music.
pub fn is_mix_text(text: &str) -> bool {
    let t = text.to_lowercase();
    const WORDS: [&str; 30] = [
        "louder", "quieter", "softer", "volume", "level", "balance", "mix", "punch", "up a bit", "down a bit", "turn up", "turn down",
        "mute", "solo", "warmer", "brighter", "wider", "pan", "combine", "use remix", "chorus from", "verse from", "muddy", "buried",
        "harsh", "thin", "boomy", "boxy", "clarity", "clipping",
    ];
    WORDS.iter().any(|w| t.contains(w))
}

const BRIEF: &str = r#"You are the assistant inside Poing, a music workstation. The person typed a request in Poing's Describe bar about the song that is open. Do it with the poing tools:

- Read `song` first. Lanes, sections and effects are addressed by their index there, takes by id.
- Listen before you change the sound and again after: pick a representative range (a chorus, or the section the request is about; up to 30 s) and call `compare` (every lane and the mix, with which lanes lead each band) or `listen` (the mix, or one lane). Prefer a range where no section take plays (a result's `notes` say where one does): there the lanes are what you hear. Use the numbers to decide what to change and by how much: who is loud in low_mid when it is muddy, how far the vocals sit under the other lanes in high_mid and presence when they are buried, true peak and clipping before raising anything. Then measure the same range again to check the change did what was asked.
- Change the mix with set_lane (`change_db` moves a level from where it is: prefer small moves of 1-3 dB unless the measurements call for more), effect (knobs by name, bypass), take, combine and transport.
- To add an instrument part (bass line, chords, drums, a melody) use `add_part`. Use `describe` to have Poing make other new things: an effect written for a lane, a lane played again as another instrument, a section repaint, or whole-song remixes (only when the person asks for a new version of the whole song: a remix replaces every lane while it plays). Both run a job and return when it is done (that can take minutes).
- To change the song's order ("repeat the chorus", "cut the second verse", "move the bridge after the last chorus", "make it 30 s longer") use `arrange`; read `song` again after each edit, the sections are renumbered.
- `song.status` is the last message Poing showed, not a request.
- Do not export unless asked. Do not undo the person's earlier choices unless the request says so.
- Finish with at most two short plain sentences (about 40 words; Poing shows them in its status line): what you changed and what it measured before and after, for example "Bass -2 dB: low-mids -1.8 dB in the verse; vocals +2 dB, now 3 dB over the guitars at 2-4 kHz." Add a few words when something stands in the way (clipping, a plugin the measurement left out). No markdown, no lists."#;

/// Run the assistant on `request` with this Poing's tools, on its own
/// thread: the jobs it starts run on the job worker meanwhile. `done` gets
/// its answer (or why it failed). Every tool call is logged to
/// `work/calls.jsonl`.
pub fn run(request: String, work: std::path::PathBuf, done: std::sync::mpsc::Sender<Result<String, String>>) {
    let _ = std::thread::Builder::new().name("poing-agent".into()).spawn(move || {
        let result = std::fs::create_dir_all(&work).map_err(|e| e.to_string()).and_then(|_| match llm::provider() {
            Provider::ClaudeCode => run_claude(&request, &work),
            Provider::Local => run_local(&request, &work),
        });
        let _ = std::fs::write(work.join("answer.md"), result.as_deref().unwrap_or_else(|e| e));
        let _ = done.send(result.map(|text| text.split_whitespace().collect::<Vec<_>>().join(" ")));
        makepad_widgets::SignalToUI::set_ui_signal();
    });
}

fn log_call(work: &std::path::Path, tool: &str, args: &str, outcome: &str) {
    use std::io::Write;
    let line = json::obj(vec![("tool", json::s(tool)), ("args", json::s(args)), ("outcome", json::s(outcome))]).to_json();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(work.join("calls.jsonl")) {
        let _ = writeln!(f, "{line}");
    }
}

/// Claude Code with this Poing's tools (its own MCP bridge, tools only: no
/// clicks or screenshots).
fn run_claude(request: &str, work: &std::path::Path) -> Result<String, String> {
    let bridge = makepad_remote().ok_or("makepad-remote not found")?;
    let config = json::obj(vec![(
        "mcpServers",
        json::obj(vec![(
            "poing",
            json::obj(vec![
                ("command", json::s(bridge.to_string_lossy())),
                ("args", Value::Arr(vec![json::s("mcp"), json::s(std::process::id().to_string()), json::s("--no-builtins")])),
            ]),
        )]),
    )]);
    let config_file = work.join("mcp.json");
    std::fs::write(&config_file, config.to_json()).map_err(|e| e.to_string())?;
    std::fs::write(work.join("system.md"), BRIEF).map_err(|e| e.to_string())?;
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(on_path("claude").ok_or("claude not found")?);
    cmd.args(["-p", "--strict-mcp-config", "--allowedTools", "mcp__poing", "--no-session-persistence", "--output-format", "stream-json", "--verbose", "--mcp-config"])
        .arg(&config_file)
        .arg("--append-system-prompt")
        .arg(BRIEF)
        .current_dir(work)
        // a part or a remix can take minutes; the bridge itself waits up to 15
        .env("MCP_TOOL_TIMEOUT", "900000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // to a file: stdout is read to its end first, so a full stderr pipe would hang both
        .stderr(std::fs::File::create(work.join("claude-stderr.log")).map(Stdio::from).unwrap_or_else(|_| Stdio::null()));
    if let Ok(model) = std::env::var("POING_CLAUDE_MODEL") {
        cmd.arg("--model").arg(model);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("Claude Code did not start ({e})"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    }
    // stream-json: one event per line; tool calls are logged, the result is the answer
    let mut answer = String::new();
    for line in std::io::BufReader::new(child.stdout.take().ok_or("no stdout")?).lines().map_while(Result::ok) {
        let Ok(v) = json::parse(line.as_bytes()) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                for c in v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_arr()).unwrap_or_default() {
                    if c.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let name = c.get("name").and_then(|n| n.as_str()).unwrap_or_default();
                        log_call(work, name.trim_start_matches("mcp__poing__").trim_start_matches("poing__"), &c.get("input").map(|i| i.to_json()).unwrap_or_default(), "");
                    }
                }
            }
            Some("result") => answer = v.get("result").and_then(|r| r.as_str()).unwrap_or_default().trim().to_string(),
            _ => {}
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        let err = std::fs::read_to_string(work.join("claude-stderr.log")).unwrap_or_default();
        return Err(format!("Claude Code stopped: {}", err.trim()));
    }
    Ok(answer)
}

/// Tool rounds the local model gets for one request.
const MAX_TOOL_ROUNDS: usize = 12;
/// What one tool result may put into the model's window.
const MAX_RESULT_CHARS: usize = 12_000;

/// Poing's own model with Poing's tools: the tool loop runs here, and each
/// call goes to this window's service port through Makepad's process
/// registry, as `makepad-remote mcp` sends Claude's (so App::agent_call runs
/// it, and a call that starts a job is answered when the job is done).
fn run_local(request: &str, work: &std::path::Path) -> Result<String, String> {
    let registry = ServiceRegistry::process();
    let endpoint = registry.pick_instance("poing", None).ok_or("Poing's tools are not open in this window")?;
    let m = manifest();
    let tools: Vec<llm::Tool> = m.tools.iter().map(|t| llm::Tool { name: t.name.clone(), description: t.description.clone(), parameters: t.parameters.clone() }).collect();
    let system = format!("{BRIEF}\n\n{}", m.brief);
    let mut messages = vec![llm::Message::user(request)];
    // what each call was (name and arguments), and what the changes said
    let mut made: Vec<String> = Vec::new();
    let mut changes: Vec<String> = Vec::new();
    let mut repeats = 0;
    for round in 0..MAX_TOOL_ROUNDS {
        let reply = llm::chat(&system, &tools, &messages, llm::Budget { max_tokens: 1500, ..llm::Budget::ask() })?;
        messages.push(llm::Message { role: "assistant", content: reply.raw.clone(), error: false });
        if reply.tool_calls.is_empty() {
            return Ok(reply.text);
        }
        for (n, (name, raw_args)) in reply.tool_calls.iter().enumerate() {
            let args = match m.tools.iter().find(|t| &t.name == name) {
                Some(t) => json_args(&t.parameters, raw_args),
                None => {
                    messages.push(llm::Message { role: "tool", content: format!("there is no tool `{name}`"), error: true });
                    continue;
                }
            };
            // A small model can call the same thing over and over: once is
            // enough for anything but reading the song again after a job.
            let key = format!("{name} {args}");
            if made.contains(&key) && name != "song" {
                repeats += 1;
                if repeats >= 3 {
                    return Ok(done_so_far(&changes));
                }
                messages.push(llm::Message { role: "tool", content: format!("Already done: `{name}` with these arguments was called before. Do not repeat it; reply with your short summary now."), error: true });
                continue;
            }
            made.push(key);
            let call = ServiceCall { call_id: format!("local-{}-{round}-{n}", std::process::id()), tool: name.clone(), args: args.clone(), caller: None };
            let result = call_tool(&registry, &endpoint, call);
            let ok = result.outcome == ToolOutcome::Ok;
            log_call(work, name, &args, &format!("{:?}: {}", result.outcome, result.text));
            if ok && !matches!(name.as_str(), "song" | "listen" | "compare" | "transport") {
                changes.push(result.text.clone());
            }
            let mut content = if result.data.is_empty() { result.text } else { format!("{}\n{}", result.text, result.data) };
            if content.len() > MAX_RESULT_CHARS {
                let cut = (0..=MAX_RESULT_CHARS).rev().find(|&i| content.is_char_boundary(i)).unwrap_or(0);
                content.truncate(cut);
                content.push_str(" …(cut)");
            }
            messages.push(llm::Message { role: "tool", content, error: !ok });
        }
    }
    Ok(done_so_far(&changes))
}

/// The status line when the model stopped without a summary of its own.
fn done_so_far(changes: &[String]) -> String {
    if changes.is_empty() {
        "Nothing was changed.".into()
    } else {
        // the first sentence of each change
        changes.iter().map(|c| c.split(". ").next().unwrap_or(c).trim_end_matches('.').to_string()).collect::<Vec<_>>().join("; ") + "."
    }
}

/// One call through the registry, waiting for its result (a job can take minutes).
fn call_tool(registry: &ServiceRegistry, endpoint: &EndpointId, call: ServiceCall) -> ToolResult {
    let id = call.call_id.clone();
    let Some(rx) = registry.call_remote(endpoint, call) else { return ToolResult::unavailable(&id, "Poing's window is gone") };
    let began = std::time::Instant::now();
    let result = loop {
        // nothing else drains the link for a caller outside the conversation
        registry.pump_remote();
        match rx.recv_timeout(std::time::Duration::from_millis(30)) {
            Ok(RegistryUp::Result(_, r)) => break r,
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) if began.elapsed().as_secs() < 900 => {}
            _ => break ToolResult::unavailable(&id, "no answer from Poing in 15 minutes"),
        }
    };
    registry.end_remote(&id);
    result
}

/// The local model's arguments (every value a string) as the JSON object the
/// tool's schema asks for: a number, boolean, array or null where the schema
/// says so, the text itself where it says string.
pub fn json_args(schema: &str, args: &[(String, String)]) -> String {
    let schema = json::parse(schema.as_bytes()).unwrap_or(Value::Null);
    let props = schema.get("properties");
    let pairs = args
        .iter()
        .map(|(k, v)| {
            let types: Vec<String> = match props.and_then(|p| p.get(k)).and_then(|p| p.get("type")) {
                Some(Value::Str(t)) => vec![t.clone()],
                Some(Value::Arr(ts)) => ts.iter().filter_map(|t| t.as_str().map(String::from)).collect(),
                _ => vec![],
            };
            let text = v.trim();
            let lower = text.to_ascii_lowercase();
            let text = if lower == "true" || lower == "false" { lower.as_str() } else { text };
            let parsed = json::parse(text.as_bytes()).ok();
            let typed = parsed.filter(|p| match p {
                Value::Int(_) => types.iter().any(|t| t == "integer" || t == "number"),
                Value::F64(f) => types.iter().any(|t| t == "number") || (f.fract() == 0.0 && types.iter().any(|t| t == "integer")),
                Value::Bool(_) => types.iter().any(|t| t == "boolean"),
                Value::Arr(_) => types.iter().any(|t| t == "array"),
                Value::Null => types.iter().any(|t| t == "null"),
                _ => types.is_empty(),
            });
            let typed = match typed {
                Some(Value::F64(f)) if !types.iter().any(|t| t == "number") => Some(Value::Int(f as i64)),
                other => other,
            };
            (k.as_str(), typed.unwrap_or_else(|| json::s(v.clone())))
        })
        .collect();
    json::obj(pairs).to_json()
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

    #[test]
    fn local_arguments_get_the_schema_types() {
        let m = manifest();
        let params = |name: &str| m.tools.iter().find(|t| t.name == name).unwrap().parameters.clone();
        let kv = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        let a = args(&json_args(&params("set_lane"), &kv(&[("lane", "2"), ("gain_db", "-2.5"), ("mute", "false")]))).unwrap();
        assert_eq!(index(&a, "lane"), Some(2));
        assert_eq!(get(&a, "gain_db").and_then(num), Some(-2.5));
        assert_eq!(get(&a, "mute").and_then(|v| v.as_bool()), Some(false));
        let a = args(&json_args(&params("describe"), &kv(&[("text", "80"), ("lane", "1.0")]))).unwrap();
        assert_eq!(get(&a, "text").and_then(|v| v.as_str()), Some("80"), "a string stays a string");
        assert_eq!(index(&a, "lane"), Some(1));
        let a = args(&json_args(&params("transport"), &kv(&[("loop", "[10, 20]"), ("play", "True")]))).unwrap();
        assert_eq!(get(&a, "loop").and_then(|v| v.as_arr()).map(|l| l.len()), Some(2));
        assert_eq!(get(&a, "play").and_then(|v| v.as_bool()), Some(true), "Python-style booleans");
        assert_eq!(done_so_far(&["Vocals: +6.0 dB (was +4.0)".into(), "Bass: -4.0 dB. More text.".into()]), "Vocals: +6.0 dB (was +4.0); Bass: -4.0 dB.");
    }

    #[test]
    fn mixing_words_are_told_apart_from_new_music() {
        assert!(is_mix_text("vocals a bit louder, bass a bit quieter"));
        assert!(is_mix_text("make the mix punchier"));
        assert!(is_mix_text("the bass is a bit muddy and the vocals are buried"));
        assert!(!is_mix_text("80s disco with Dutch female vocals"));
    }
}
