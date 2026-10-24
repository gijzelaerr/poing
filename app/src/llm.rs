//! Poing's language model: what writes song sheets, MIDI parts and effects
//! and runs the assistant behind the Describe bar.
//!
//! Two providers. **Local** (the default) is a Qwen3.5 model on this
//! machine's GPU, in `poing-engine chat` (Makepad's pure-Rust LLM engine; a
//! helper process, so Poing itself never links CUDA). It is downloaded on
//! first use, loaded when a request comes, and released when the GPU is
//! wanted for something else (`release`, before every ACE-Step or analysis
//! job) or after a minute of nothing. **Claude Code** (`claude -p`, the
//! person's own login) is used when it is chosen and on PATH.
//!
//! Every request to the engine carries the whole conversation; the engine
//! only caches it (see engine/src/chat.rs), so a released model costs one
//! reload and nothing else.

use makepad_strict_json::{self as json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// Poing's own model on this machine (poing-engine chat).
    Local,
    /// Claude Code headless (`claude -p`).
    ClaudeCode,
}

/// 0 unset (read `POING_LLM` once), 1 local, 2 Claude Code.
static PROVIDER: AtomicU8 = AtomicU8::new(0);

/// Which provider requests go to. `POING_LLM=claude` picks Claude Code
/// at start; Settings changes it with `set_provider`.
pub fn provider() -> Provider {
    match PROVIDER.load(Ordering::Relaxed) {
        1 => Provider::Local,
        2 => Provider::ClaudeCode,
        _ => {
            let p = if std::env::var("POING_LLM").is_ok_and(|v| v.eq_ignore_ascii_case("claude")) { Provider::ClaudeCode } else { Provider::Local };
            set_provider(p);
            p
        }
    }
}

pub fn set_provider(p: Provider) {
    PROVIDER.store(if p == Provider::Local { 1 } else { 2 }, Ordering::Relaxed);
}

/// Who is writing, for status lines ("Claude is writing the part").
pub fn name() -> &'static str {
    match provider() {
        Provider::Local => "Poing's model",
        Provider::ClaudeCode => "Claude",
    }
}

/// A program on PATH (with Windows' extensions).
pub fn on_path(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .flat_map(|d| exts.iter().map(move |e| d.join(format!("{name}{e}"))))
        .find(|p| p.is_file())
}

/// Why the chosen provider cannot answer here (None: it can).
pub fn unavailable() -> Option<String> {
    match provider() {
        Provider::ClaudeCode if on_path("claude").is_none() => Some("Claude Code (`claude`) is not on PATH".into()),
        Provider::Local if crate::audio::engine_exe().is_none() => Some("poing-engine was not found next to poing".into()),
        _ => None,
    }
}

/// The model manager's pack of the local model (app/models.json).
pub const PACK: &str = "llm-qwen35-4b";

/// The feature to check with the model manager before a request, when the
/// local model is the provider (Claude Code needs no download).
pub fn feature() -> Option<&'static str> {
    (provider() == Provider::Local).then_some("llm")
}

/// The model outside the model manager (a development setup): `POING_LLM_MODEL`,
/// or the default model in Makepad's shared weights folder.
pub fn dev_model() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("POING_LLM_MODEL").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let weights = std::env::var_os("MAKEPAD_ASSET_AI_CACHE").map(PathBuf::from).or_else(|| Some(home()?.join(".makepad").join("weights")))?;
    Some(weights.join("llm").join("Qwen3.5-4B-Q5_K_M.gguf")).filter(|p| p.is_file())
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

// ------------------------------------------------------------------ status

/// The language model id Local uses (None: the engine's default).
static MODEL: Mutex<Option<String>> = Mutex::new(None);

/// Pick the local model (`qwen3.5-9b`, `qwen3.5-4b`); takes effect on the next load.
#[allow(dead_code)] // the Settings panel's
pub fn set_model(id: Option<String>) {
    // the next request starts the engine on it (a running one finishes first)
    *MODEL.lock().unwrap() = id;
}

pub fn model() -> Option<String> {
    MODEL.lock().unwrap().clone().or_else(|| std::env::var("POING_LLM_MODEL").ok().filter(|m| !m.ends_with(".gguf")))
}

/// One local model as `poing-engine status` lists it.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // the Settings panel's
pub struct LocalModel {
    pub id: String,
    /// Where it is on disk (None: not downloaded yet).
    pub path: Option<String>,
    pub bytes: u64,
    pub vram_gb: f64,
    pub default: bool,
}

/// What Settings shows about the language model.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // the Settings panel's
pub struct Status {
    pub provider: Option<Provider>,
    pub claude_on_path: bool,
    pub engine_found: bool,
    /// The GPU store the engine uses: "cuda" or "metal".
    pub device: String,
    pub models: Vec<LocalModel>,
    /// What the engine is doing now: "Downloading … 40%", "Loading …", "Ready: …", "".
    pub activity: String,
    /// The model is loaded (holding GPU memory).
    pub loaded: bool,
}

/// What the engine is doing, for `status()`.
static ACTIVITY: Mutex<String> = Mutex::new(String::new());

fn set_activity(text: &str) {
    *ACTIVITY.lock().unwrap() = text.to_string();
}

/// The provider, the local models and what the engine is doing. Runs
/// `poing-engine status` (no GPU, a few ms): call it off the UI thread or rarely.
#[allow(dead_code)] // the Settings panel's
pub fn status() -> Status {
    let mut st = Status {
        provider: Some(provider()),
        claude_on_path: on_path("claude").is_some(),
        activity: ACTIVITY.lock().unwrap().clone(),
        // a request running holds the lock: loaded
        loaded: ENGINE.try_lock().map(|e| e.is_some()).unwrap_or(true),
        ..Default::default()
    };
    let Some(mut cmd) = crate::audio::engine_exe() else { return st };
    st.engine_found = true;
    no_window(&mut cmd);
    let Ok(out) = cmd.arg("status").stderr(Stdio::null()).output() else { return st };
    if let Ok(v) = json::parse(&out.stdout) {
        st.device = v.get("device").and_then(|d| d.as_str()).unwrap_or_default().to_string();
        for m in v.get("llm").and_then(|l| l.as_arr()).unwrap_or_default() {
            st.models.push(LocalModel {
                id: m.get("id").and_then(|x| x.as_str()).unwrap_or_default().into(),
                path: m.get("path").and_then(|x| x.as_str()).map(String::from),
                bytes: m.get("bytes").and_then(|x| x.as_u64()).unwrap_or(0),
                vram_gb: m.get("vram_gb").and_then(num).unwrap_or(0.0),
                default: m.get("default").and_then(|x| x.as_bool()).unwrap_or(false),
            });
        }
    }
    st
}

/// Download the local model now (in the background; `status().activity`
/// follows it), rather than on the first request.
#[allow(dead_code)] // the Settings panel's
pub fn install(model: Option<String>) {
    let _ = std::thread::Builder::new().name("poing-llm-install".into()).spawn(move || {
        let Some(mut cmd) = crate::audio::engine_exe() else { return };
        no_window(&mut cmd);
        cmd.arg("llm-install");
        if let Some(m) = &model {
            cmd.arg(m);
        }
        let Ok(mut child) = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { return };
        for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
            if let Ok(v) = json::parse(line.as_bytes()) {
                set_activity(&progress_text(&v));
            }
        }
        let _ = child.wait();
        set_activity("");
    });
}

fn progress_text(v: &Value) -> String {
    let message = v.get("message").and_then(|m| m.as_str()).unwrap_or_default();
    match v.get("stage").or_else(|| v.get("event")).and_then(|s| s.as_str()).unwrap_or_default() {
        "install" => format!("Downloading the model: {message}"),
        "loading" => format!("Loading the model ({:.0}%)", v.get("progress").and_then(num).unwrap_or(0.0) * 100.0),
        "ready" => format!("Ready: {message}"),
        "error" => format!("Model error: {message}"),
        _ => message.to_string(),
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::F64(f) => Some(*f),
        _ => None,
    }
}

fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

// ------------------------------------------------------------- the engine

struct Engine {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    model: Option<String>,
    next_id: u64,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // closing stdin ends it; kill in case it is mid-turn
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The running engine (None: not loaded). Held while a request runs.
static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);
static LAST_USED: Mutex<Option<Instant>> = Mutex::new(None);
/// A loaded model with nothing to do is released after this long.
const IDLE: Duration = Duration::from_secs(60);

/// Free the GPU: stop the engine (between requests; a running one finishes
/// first). The next request loads the model again.
pub fn release() {
    if let Ok(mut e) = ENGINE.lock() {
        if e.take().is_some() {
            set_activity("");
        }
    }
}

fn start_engine(model: Option<String>) -> Result<Engine, String> {
    let mut cmd = crate::audio::engine_exe().ok_or("poing-engine was not found next to poing")?;
    no_window(&mut cmd);
    cmd.arg("chat");
    // the model manager's copy, else the engine finds (or downloads) it by id
    let pack = crate::models::installed_pack(PACK).map(|d| d.join("Qwen3.5-4B-Q5_K_M.gguf")).filter(|f| f.is_file() && model.is_none());
    match (&pack, &model) {
        (Some(f), _) => {
            cmd.arg(f);
        }
        (None, Some(m)) => {
            cmd.arg(m);
        }
        _ => {}
    }
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| format!("poing-engine did not start ({e})"))?;
    let stdin = child.stdin.take().ok_or("no stdin")?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
    // Wait for the load (and a first-use download).
    loop {
        let mut line = String::new();
        if stdout.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            let _ = child.wait();
            set_activity("");
            return Err("the local model did not load (see `poing-engine chat`)".into());
        }
        let Ok(v) = json::parse(line.trim().as_bytes()) else { continue };
        set_activity(&progress_text(&v));
        match v.get("event").or_else(|| v.get("stage")).and_then(|s| s.as_str()) {
            Some("ready") => break,
            Some("error") => {
                let _ = child.kill();
                let message = v.get("message").and_then(|m| m.as_str()).unwrap_or("the local model did not load");
                // the installer's (and CI's) engine is built without CUDA
                if message.contains("not compiled into this build") {
                    let pack = crate::models::pack(crate::models::ENGINE_PACK).map_or("the GPU engine", |p| p.title.as_str());
                    return Err(format!(
                        "this poing-engine was built without GPU support, and the model needs it on Windows. \
                         Download {pack} in Settings → Models, or pick Claude Code there"
                    ));
                }
                return Err(message.to_string());
            }
            _ => {}
        }
    }
    idle_reaper();
    Ok(Engine { child, stdin, stdout, model, next_id: 1 })
}

/// Once: a thread that releases a model nobody has used for `IDLE`.
fn idle_reaper() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new().name("poing-llm-idle".into()).spawn(|| loop {
            std::thread::sleep(Duration::from_secs(5));
            let idle = LAST_USED.lock().unwrap().is_some_and(|t| t.elapsed() > IDLE);
            // a request running holds the lock: not idle
            if let (true, Ok(mut e)) = (idle, ENGINE.try_lock()) {
                if e.take().is_some() {
                    set_activity("");
                }
            }
        });
    });
}

/// One message of a conversation with the local model.
#[derive(Clone, Debug)]
pub struct Message {
    /// "user", "assistant" (its `raw` turn) or "tool" (a result).
    pub role: &'static str,
    pub content: String,
    pub error: bool,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Message { role: "user", content: text.into(), error: false }
    }
}

/// A tool the local model may call (the manifest's ToolDef, as Qwen reads it).
#[derive(Clone, Debug)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// A JSON-schema object, verbatim.
    pub parameters: String,
}

/// One turn of the local model.
#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct Reply {
    pub text: String,
    /// The turn as generated (goes back as the assistant message).
    pub raw: String,
    pub tool_calls: Vec<(String, Vec<(String, String)>)>,
    pub prompt_tokens: usize,
    pub tokens: usize,
    /// It hit the token budget: the answer is cut off.
    pub truncated: bool,
    pub secs: f64,
}

/// A request's knobs.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub max_tokens: usize,
    /// 0: greedy.
    pub temperature: f32,
    /// Pushes down every token already used (Qwen suggests 1.5 against loops).
    pub presence_penalty: f32,
}

impl Budget {
    /// The writers' sampling. `POING_LLM_TEMPERATURE` / `POING_LLM_PRESENCE` override it.
    pub fn ask() -> Budget {
        let env = |k: &str, d: f32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        Budget { max_tokens: 4096, temperature: env("POING_LLM_TEMPERATURE", 0.0), presence_penalty: env("POING_LLM_PRESENCE", 0.0) }
    }
}

/// One turn of the local model on `messages` (the whole conversation so far).
pub fn chat(system: &str, tools: &[Tool], messages: &[Message], budget: Budget) -> Result<Reply, String> {
    let mut guard = ENGINE.lock().map_err(|_| "the model engine crashed")?;
    let want = model();
    if guard.as_ref().is_some_and(|e| e.model != want) {
        *guard = None;
    }
    if guard.is_none() {
        *guard = Some(start_engine(want)?);
    }
    let result = send(guard.as_mut().unwrap(), system, tools, messages, budget);
    if result.is_err() {
        // a dead or confused engine: start over next time
        *guard = None;
    }
    *LAST_USED.lock().unwrap() = Some(Instant::now());
    result
}

fn send(e: &mut Engine, system: &str, tools: &[Tool], messages: &[Message], budget: Budget) -> Result<Reply, String> {
    let id = e.next_id;
    e.next_id += 1;
    let request = json::obj(vec![
        ("id", Value::Int(id as i64)),
        ("system", json::s(system)),
        (
            "tools",
            Value::Arr(
                tools
                    .iter()
                    .map(|t| json::obj(vec![("name", json::s(t.name.clone())), ("description", json::s(t.description.clone())), ("parameters", json::s(t.parameters.clone()))]))
                    .collect(),
            ),
        ),
        (
            "messages",
            Value::Arr(messages.iter().map(|m| json::obj(vec![("role", json::s(m.role)), ("content", json::s(m.content.clone())), ("error", Value::Bool(m.error))])).collect()),
        ),
        ("max_tokens", Value::Int(budget.max_tokens as i64)),
        ("temperature", Value::F64(budget.temperature as f64)),
        ("presence_penalty", Value::F64(budget.presence_penalty as f64)),
    ]);
    let mut line = request.to_json();
    line.push('\n');
    e.stdin.write_all(line.as_bytes()).and_then(|_| e.stdin.flush()).map_err(|err| format!("the model engine stopped ({err})"))?;
    set_activity("Writing");
    loop {
        let mut line = String::new();
        if e.stdout.read_line(&mut line).map_err(|err| err.to_string())? == 0 {
            return Err("the model engine stopped".into());
        }
        let Ok(v) = json::parse(line.trim().as_bytes()) else { continue };
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
        match v.get("event").and_then(|x| x.as_str()) {
            Some("reply") if v.get("id").and_then(|x| x.as_u64()) == Some(id) => {
                set_activity("");
                let tool_calls = v
                    .get("tool_calls")
                    .and_then(|c| c.as_arr())
                    .unwrap_or_default()
                    .iter()
                    .map(|c| {
                        let name = c.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
                        let args = c
                            .get("args")
                            .and_then(|a| a.as_arr())
                            .unwrap_or_default()
                            .iter()
                            .filter_map(|kv| match kv.as_arr()? {
                                [k, v] => Some((k.as_str()?.to_string(), v.as_str()?.to_string())),
                                _ => None,
                            })
                            .collect();
                        (name, args)
                    })
                    .collect();
                return Ok(Reply {
                    text: s("text"),
                    raw: s("raw"),
                    tool_calls,
                    prompt_tokens: v.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
                    tokens: v.get("tokens").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
                    truncated: v.get("truncated").and_then(|x| x.as_bool()).unwrap_or(false),
                    secs: v.get("secs").and_then(num).unwrap_or(0.0),
                });
            }
            Some("error") if v.get("id").and_then(|x| x.as_u64()) == Some(id) => return Err(format!("the local model failed: {}", s("message"))),
            _ => {}
        }
    }
}

// -------------------------------------------------------------- one answer

/// Ask once, with the chosen provider: `system` and one prompt, the text
/// of the answer back. `max_tokens` bounds the local model's answer (Claude
/// Code sets its own). `work` keeps Claude's system file.
pub fn ask(system: &str, prompt: &str, max_tokens: usize, work: &Path) -> Result<String, String> {
    ask_with(system, prompt, Budget { max_tokens, ..Budget::ask() }, work)
}

/// `ask` for a writer's repair round: the local model samples (temperature
/// 0.7, presence penalty 1.5), as greedy decoding hands back the same answer
/// it gave before, problems and all.
pub fn ask_again(system: &str, prompt: &str, max_tokens: usize, work: &Path) -> Result<String, String> {
    ask_with(system, prompt, Budget { max_tokens, temperature: 0.7, presence_penalty: 1.5 }, work)
}

fn ask_with(system: &str, prompt: &str, budget: Budget, work: &Path) -> Result<String, String> {
    match provider() {
        Provider::ClaudeCode => ask_claude(system, prompt, work),
        Provider::Local => {
            let _ = std::fs::create_dir_all(work);
            let log = |reply: &Reply| {
                if let Ok(mut log) = std::fs::OpenOptions::new().create(true).append(true).open(work.join("llm.log")) {
                    let _ = writeln!(log, "prompt_tokens={} tokens={} secs={:.1} truncated={}", reply.prompt_tokens, reply.tokens, reply.secs, reply.truncated);
                }
            };
            let mut reply = chat(system, &[], &[Message::user(prompt)], budget)?;
            log(&reply);
            // Greedy decoding can loop until the budget runs out, and would
            // loop the same way again: once more, sampled as Qwen suggests.
            if reply.truncated && budget.temperature == 0.0 {
                reply = chat(system, &[], &[Message::user(prompt)], Budget { temperature: 0.7, presence_penalty: 1.5, ..budget })?;
                log(&reply);
            }
            // Still cut off: it fails the writer's check like any other bad
            // answer, and goes back for a fix.
            Ok(reply.text)
        }
    }
}

/// Claude Code once (headless, no tools); its text answer.
fn ask_claude(system: &str, prompt: &str, work: &Path) -> Result<String, String> {
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let system_file = work.join("system.md");
    std::fs::write(&system_file, system).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(on_path("claude").ok_or("Claude Code (`claude`) is not on PATH")?);
    cmd.args(["-p", "--tools", "", "--no-session-persistence", "--output-format", "text", "--system-prompt-file"])
        .arg(&system_file)
        .current_dir(work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Ok(model) = std::env::var("POING_CLAUDE_MODEL") {
        cmd.arg("--model").arg(model);
    }
    no_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("Claude Code (`claude`) did not start ({e})"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("claude exited with {}: {}", out.status, err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provider_can_be_switched() {
        set_provider(Provider::ClaudeCode);
        assert_eq!(provider(), Provider::ClaudeCode);
        assert_eq!(name(), "Claude");
        set_provider(Provider::Local);
        assert_eq!(provider(), Provider::Local);
    }

    #[test]
    fn engine_lines_read_as_activity() {
        let v = json::parse(br#"{"event":"loading","id":0,"message":"load llm parse","progress":0.4}"#).unwrap();
        assert_eq!(progress_text(&v), "Loading the model (40%)");
        let v = json::parse(br#"{"stage":"install","progress":0.1,"message":"x.gguf: 600 / 6000 MB"}"#).unwrap();
        assert_eq!(progress_text(&v), "Downloading the model: x.gguf: 600 / 6000 MB");
    }
}

// (the path spelled out: poing-node includes this file by #[path] too)
#[cfg(test)]
#[path = "llm/bench.rs"]
mod bench;
