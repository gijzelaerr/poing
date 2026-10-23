//! `poing-engine chat`: Poing's local language model, one JSON request per
//! line on stdin, one JSON event per line on stdout, until stdin closes (and
//! the GPU memory is free again).
//!
//! The model is a Qwen3.5 GGUF from Makepad's model registry, run by
//! `makepad-ai-llm` (pure Rust on ggml: CUDA here, Metal on a Mac). It is
//! downloaded on first use with the hub's hash-checked downloader.
//!
//! Every request carries the whole conversation (system prompt, tools,
//! messages), so this process holds no conversation of its own: it is a
//! cache. A request that extends what the session already holds only
//! prefills its new tail; one that starts with a system prompt seen before
//! rewinds to that prompt's snapshot; anything else starts over. The app can
//! kill this process whenever the GPU is needed for something else, and the
//! next request rebuilds what it needs.
//!
//! The prompt format is Qwen's own chat template as Makepad's hub renders it
//! (`makepad_ai_hub::local_llm::build_prefix`: a `<tools>` block, calls as
//! `<tool_call><function=..><parameter=..>`). The turn templates and the
//! tool-call parser below are copies of the hub's (`local_llm.rs`,
//! `local_llm/worker.rs`), which are crate-private there.
//!
//!     -> {"id":1,"system":"...","tools":[{"name":..,"description":..,"parameters":"{..}"}],
//!         "messages":[{"role":"user","content":".."},{"role":"assistant","content":"<raw>"},
//!                     {"role":"tool","content":"..","error":false}],
//!         "max_tokens":4096,"temperature":0.0}
//!     <- {"event":"loading","progress":0.4,"message":"..."}
//!     <- {"event":"reply","id":1,"text":"..","raw":"..","tool_calls":[{"name":..,"args":[["k","v"]]}],..}
//!     <- {"event":"error","id":1,"message":".."}

use crate::models;
use makepad_ai_hub::local_llm::{build_prefix, ToolSpec};
use makepad_ai_llm::{LlamaSamplerState, LlamaSamplingParams, LlamaSession, LlamaSessionConfig, LlamaSessionState};
use makepad_micro_serde::*;
use std::io::BufRead;
use std::path::Path;
use std::time::Instant;

/// The session's window: the effect writer's system prompt (Makepad's
/// shader guide and prelude) is ~6k tokens, a whole-song MIDI part ~8k more.
const MAX_CONTEXT: u32 = 24 * 1024;
/// Rewind points kept (one per system prompt: the assistant's, a writer's).
const SNAPSHOTS: usize = 3;

#[derive(DeJson, Debug)]
struct Request {
    id: u64,
    system: String,
    tools: Option<Vec<Tool>>,
    messages: Vec<Message>,
    max_tokens: Option<usize>,
    temperature: Option<f32>,
    presence_penalty: Option<f32>,
}

#[derive(DeJson, Debug)]
struct Tool {
    name: String,
    description: String,
    parameters: String,
}

#[derive(DeJson, Debug)]
struct Message {
    role: String,
    content: String,
    error: Option<bool>,
}

#[derive(SerJson)]
struct Event {
    event: String,
    id: u64,
    message: String,
    progress: f64,
}

#[derive(SerJson)]
struct Reply {
    event: String,
    id: u64,
    /// What the person reads: the answer without tool calls or thinking.
    text: String,
    /// The whole turn as generated, for the next request's transcript.
    raw: String,
    tool_calls: Vec<Call>,
    prompt_tokens: usize,
    /// Prompt tokens that were already in the session (not prefilled again).
    reused_tokens: usize,
    tokens: usize,
    /// The turn ran out of `max_tokens` (its answer is cut off).
    truncated: bool,
    prefill_secs: f64,
    secs: f64,
}

#[derive(SerJson)]
struct Call {
    name: String,
    args: Vec<Vec<String>>,
}

fn event(event: &str, id: u64, message: &str, progress: f64) {
    println!("{}", Event { event: event.into(), id, message: message.into(), progress }.serialize_json());
}

/// `poing-engine chat [model id | path.gguf]`.
pub fn serve(model: Option<&str>) -> Result<(), String> {
    let path = match model.filter(|m| m.ends_with(".gguf")) {
        Some(p) => Path::new(p).to_path_buf(),
        None => models::Models::open()?.llm_path(model, true)?,
    };
    let began = Instant::now();
    let mut session = LlamaSession::load_with_progress(
        &path,
        LlamaSessionConfig { max_context: Some(MAX_CONTEXT), ..Default::default() },
        &mut |phase: &str, fraction: f64| event("loading", 0, phase, fraction),
    )
    .map_err(|e| format!("could not load {}: {e:?}", path.display()))?;
    let vram = session.memory().map(|m| m.total).unwrap_or(0);
    event("ready", 0, &format!("{} loaded in {:.1}s, {:.1} GB", path.display(), began.elapsed().as_secs_f64(), vram as f64 / 1e9), 1.0);

    let mut chat = Chat { session_text: String::new(), snapshots: Vec::new() };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let request = match Request::deserialize_json_lenient(&line) {
            Ok(r) => r,
            Err(e) => {
                event("error", 0, &format!("bad request: {e:?}"), 0.0);
                continue;
            }
        };
        match chat.answer(&mut session, &request) {
            Ok(reply) => println!("{}", reply.serialize_json()),
            Err(e) => {
                // A failed turn leaves the session in an unknown place.
                chat.session_text.clear();
                let _ = session.reset();
                event("error", request.id, &e, 0.0);
            }
        }
    }
    Ok(())
}

struct Chat {
    /// What the session holds, as text.
    session_text: String,
    /// (system prefix text, state after it), most recent first.
    snapshots: Vec<(String, LlamaSessionState)>,
}

impl Chat {
    fn answer(&mut self, session: &mut LlamaSession, r: &Request) -> Result<Reply, String> {
        let tools: Vec<ToolSpec> = r.tools.iter().flatten().map(|t| ToolSpec::new(&t.name, &t.description, &t.parameters)).collect();
        let prefix = build_prefix(&r.system, &tools);
        let prompt = format!("{prefix}{}", render(&r.messages));
        let began = Instant::now();

        // Bring the session to a point the prompt starts with.
        if !(prompt.starts_with(&self.session_text) && !self.session_text.is_empty()) {
            match self.snapshots.iter().position(|(text, _)| prompt.starts_with(text.as_str())) {
                Some(i) => {
                    let snap = self.snapshots.remove(i);
                    session.restore_state(&snap.1).map_err(|e| format!("restore: {e:?}"))?;
                    self.session_text = snap.0.clone();
                    self.snapshots.insert(0, snap);
                }
                None => {
                    session.reset().map_err(|e| format!("reset: {e:?}"))?;
                    self.session_text.clear();
                    append(session, &mut self.session_text, &prefix)?;
                    let state = session.save_state().map_err(|e| format!("snapshot: {e:?}"))?;
                    self.snapshots.insert(0, (prefix.clone(), state));
                    self.snapshots.truncate(SNAPSHOTS);
                }
            }
        }
        let reused = session.token_count();
        let tail = prompt[self.session_text.len()..].to_string();
        append(session, &mut self.session_text, &tail)?;
        let prompt_tokens = session.token_count();
        let prefill_secs = began.elapsed().as_secs_f64();

        let max_tokens = r.max_tokens.unwrap_or(2048);
        let room = session.remaining_context().saturating_sub(16);
        let (raw, tokens, stopped, end_held) = generate(session, max_tokens.min(room), r.temperature.unwrap_or(0.0), r.presence_penalty.unwrap_or(0.0))?;
        // The generated tokens are in the session already; close the turn so
        // it holds exactly what the next request's transcript renders for it.
        self.session_text.push_str(&raw);
        if end_held {
            self.session_text.push_str("<|im_end|>");
            append(session, &mut self.session_text, "\n")?;
        } else {
            append(session, &mut self.session_text, "<|im_end|>\n")?;
        }
        let (text, tool_calls) = split_turn(&raw);
        Ok(Reply {
            event: "reply".into(),
            id: r.id,
            text,
            raw,
            tool_calls: tool_calls.into_iter().map(|(name, args)| Call { name, args: args.into_iter().map(|(k, v)| vec![k, v]).collect() }).collect(),
            prompt_tokens,
            reused_tokens: reused,
            tokens,
            truncated: !stopped,
            prefill_secs: (prefill_secs * 100.0).round() / 100.0,
            secs: (began.elapsed().as_secs_f64() * 100.0).round() / 100.0,
        })
    }
}

/// Tokenize `text` (special tokens parsed) and prefill it.
fn append(session: &mut LlamaSession, held: &mut String, text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    let tokens = session.vocab().tokenize(text, false, true).map_err(|e| format!("tokenize: {e:?}"))?;
    if session.remaining_context() < tokens.len() + 64 {
        return Err(format!("the conversation is too long for the model's window ({} tokens)", session.token_count() + tokens.len()));
    }
    session.append_tokens(&tokens).map_err(|e| format!("prefill: {e:?}"))?;
    held.push_str(text);
    Ok(())
}

/// The template's empty thinking block: Qwen3.5 answers without thinking.
const ASSISTANT_OPEN: &str = "<|im_start|>assistant\n<think>\n\n</think>\n\n";

/// The messages after the system prefix, ending ready for the assistant.
/// Tool results go back as one user turn of `<tool_response>` blocks.
fn render(messages: &[Message]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < messages.len() {
        let m = &messages[i];
        match m.role.as_str() {
            "assistant" => out.push_str(&format!("{ASSISTANT_OPEN}{}<|im_end|>\n", m.content)),
            "tool" => {
                out.push_str("<|im_start|>user\n");
                while i < messages.len() && messages[i].role == "tool" {
                    out.push_str("<tool_response>\n");
                    if messages[i].error == Some(true) {
                        out.push_str("ERROR: ");
                    }
                    out.push_str(&messages[i].content);
                    out.push_str("\n</tool_response>\n");
                    i += 1;
                }
                out.push_str("<|im_end|>\n");
                continue;
            }
            _ => out.push_str(&format!("<|im_start|>user\n{}<|im_end|>\n", m.content)),
        }
        i += 1;
    }
    out.push_str(ASSISTANT_OPEN);
    out
}

/// One assistant turn: greedy at temperature 0, else sampled. Returns the
/// raw text, the tokens generated, whether the model ended the turn itself
/// (false: it hit `max_tokens`), and whether its end token is already in the
/// session (the sampler appends what it picks; the greedy step does not).
fn generate(session: &mut LlamaSession, max_tokens: usize, temperature: f32, presence_penalty: f32) -> Result<(String, usize, bool, bool), String> {
    let mut decoder = session.vocab().text_decoder();
    // Qwen3.5's suggested non-thinking sampling: top_p 0.8, top_k 20.
    // (at temperature 0 the decode is plain greedy: Makepad's sampler skips
    // its penalties there, and a near-zero temperature underflows its softmax)
    let params = LlamaSamplingParams { temperature, top_p: 0.8, top_k: 20, seed: seed(), presence_penalty, penalty_last_n: 1024, ..Default::default() };
    let mut sampler = LlamaSamplerState::new(params.seed);
    let stops = session.stop_tokens();
    let mut raw = String::new();
    let mut generated = 0;
    while generated < max_tokens {
        let token = if temperature <= 0.0 {
            match session.next_greedy_token().map_err(|e| format!("decode: {e:?}"))? {
                Some(t) => t,
                None => return Ok((raw, generated, true, false)),
            }
        } else {
            let step = session.continue_sampled_with(1, params, &mut sampler).map_err(|e| format!("decode: {e:?}"))?;
            match step.token_ids.first() {
                Some(&t) => t,
                None => return Ok((raw, generated, true, false)),
            }
        };
        generated += 1;
        if stops.contains(&token) {
            return Ok((raw, generated, true, true));
        }
        if let Some(text) = decoder.push_token(session.vocab(), token) {
            raw.push_str(&text);
        } else if let Some(piece) = special_piece(session, token) {
            raw.push_str(&piece);
        }
    }
    Ok((raw, generated, false, false))
}

/// Qwen's markup tokens (`<tool_call>`, `<think>`, ...) as their text, for
/// a decoder that renders control tokens as nothing.
fn special_piece(session: &LlamaSession, token: i32) -> Option<String> {
    ["<tool_call>", "</tool_call>", "<think>", "</think>"]
        .into_iter()
        .find(|p| session.vocab().token_id(p) == Some(token))
        .map(String::from)
}

fn seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7) ^ std::process::id() as u64
}

/// A turn's visible text and its tool calls.
fn split_turn(raw: &str) -> (String, Vec<(String, Vec<(String, String)>)>) {
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut rest = raw;
    // thinking is not shown
    if let Some(end) = rest.find("</think>") {
        rest = &rest[end + "</think>".len()..];
    }
    while let Some(open) = rest.find("<tool_call>") {
        text.push_str(&rest[..open]);
        let body = &rest[open + "<tool_call>".len()..];
        let close = body.find("</tool_call>").unwrap_or(body.len());
        if let Ok(call) = parse_tool_call(&body[..close]) {
            calls.push(call);
        }
        rest = body.get(close + "</tool_call>".len()..).unwrap_or("");
    }
    text.push_str(rest);
    (text.trim().to_string(), calls)
}

/// `<function=NAME>` and a run of `<parameter=key>\nvalue\n</parameter>`
/// (Makepad's `local_llm::parse_tool_call`). Values stay strings.
fn parse_tool_call(body: &str) -> Result<(String, Vec<(String, String)>), String> {
    let function_at = body.find("<function=").ok_or("missing <function=")?;
    let rest = &body[function_at + "<function=".len()..];
    let name_end = rest.find(['>', '\n']).ok_or("unterminated function name")?;
    let name = rest[..name_end].trim().to_string();
    if name.is_empty() {
        return Err("empty function name".into());
    }
    let mut args = Vec::new();
    let mut cursor = &rest[name_end..];
    while let Some(param_at) = cursor.find("<parameter=") {
        let param_rest = &cursor[param_at + "<parameter=".len()..];
        let Some(key_end) = param_rest.find('>') else { break };
        let key = param_rest[..key_end].trim().to_string();
        let value_rest = &param_rest[key_end + 1..];
        let value_end = value_rest.find("</parameter>").unwrap_or(value_rest.len());
        let raw = &value_rest[..value_end];
        let raw = raw.strip_prefix("\r\n").or_else(|| raw.strip_prefix('\n')).unwrap_or(raw);
        let raw = raw.strip_suffix("\r\n").or_else(|| raw.strip_suffix('\n')).unwrap_or(raw);
        args.push((key, raw.to_string()));
        cursor = &value_rest[value_end..];
    }
    Ok((name, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> Message {
        Message { role: role.into(), content: content.into(), error: None }
    }

    #[test]
    fn a_turn_splits_into_text_and_calls() {
        let raw = "Turning the bass down.\n<tool_call>\n<function=set_lane>\n<parameter=lane>\n2\n</parameter>\n<parameter=gain_db>\n-2\n</parameter>\n</function>\n</tool_call>";
        let (text, calls) = split_turn(raw);
        assert_eq!(text, "Turning the bass down.");
        assert_eq!(calls, vec![("set_lane".to_string(), vec![("lane".into(), "2".into()), ("gain_db".into(), "-2".into())])]);
        assert_eq!(split_turn("<think>\nhm\n</think>\n\nDone.").0, "Done.");
    }

    #[test]
    fn tool_results_are_one_user_turn() {
        let mut err = msg("tool", "no such lane");
        err.error = Some(true);
        let r = render(&[msg("user", "louder"), msg("assistant", "<tool_call>..</tool_call>"), msg("tool", "{}"), err]);
        assert!(r.starts_with("<|im_start|>user\nlouder<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n<tool_call>"));
        assert!(r.contains("<|im_start|>user\n<tool_response>\n{}\n</tool_response>\n<tool_response>\nERROR: no such lane\n</tool_response>\n<|im_end|>\n"));
        assert!(r.ends_with(ASSISTANT_OPEN));
    }

    #[test]
    fn requests_read() {
        let r = Request::deserialize_json_lenient(r#"{"id":3,"system":"s","messages":[{"role":"user","content":"hi"}],"max_tokens":10}"#).unwrap();
        assert_eq!(r.id, 3);
        assert!(r.tools.is_none() && r.temperature.is_none());
        assert_eq!(r.messages[0].content, "hi");
    }
}
