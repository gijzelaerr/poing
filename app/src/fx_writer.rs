//! The language model writes an effect: a Describe like "tape saturation"
//! on a lane becomes a Splash audio shader. The model (`llm::ask`: Poing's
//! local one, or Claude Code) gets Makepad's shader guide, the prelude and
//! two example shaders as its system prompt. Every answer is compiled and
//! auditioned (Makepad's `check`); problems go back for another round.

use crate::fx;
use crate::llm;
use makepad_script_audio_aot as aot;
use std::path::Path;

/// Rounds of write, check, fix.
pub fn max_rounds() -> usize {
    if let Some(n) = std::env::var("POING_LLM_ROUNDS").ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    // the local 4B passed 4/10 test effects in 3 greedy rounds, 6/10 in 5
    // with sampled repairs; Claude passed 5/5 in at most 2
    match llm::provider() {
        llm::Provider::ClaudeCode => 3,
        llm::Provider::Local => 5,
    }
}
/// What one answer may run to (a shader with its comments is under ~1.6k tokens).
const MAX_TOKENS: usize = 2500;
/// What a lane effect may cost per sample (counted ops; Makepad calls 4096 expensive).
pub const MAX_OPS: f64 = 2000.0;

const BRIEF: &str = r#"You write one audio EFFECT for Poing, a music workstation, as a Splash audio shader.

Reply with exactly two things and nothing else:
1. A first line `NAME: <a 1-3 word name for the effect, e.g. Tape saturation>`
2. One fenced code block (```splash ... ```) holding the complete shader.

The shader:
- is an effect: `fn effect(l, r)` returning `vec2(left, right)`. Never voice().
- has 2 to 6 `param(default, min, max)` knobs a musician would want, named in
  snake_case (they become knob labels), with musical ranges and defaults that
  already sound good on the material described.
- includes a `mix` param (dry/wet, 0..1) unless the effect is a pure level or EQ.
- keeps the output level close to the input level at the defaults
  (compensate any gain the effect adds), and peaks well under 1.0.
- oversamples every nonlinearity (drive, saturation, clipping, folding) with Os4
  and puts dc_block() after asymmetric shaping.
- costs well under 2000 counted ops per sample.
- has short `//` comments saying what each part does.

The language reference, the prelude and two example shaders follow."#;

/// The system prompt: the brief, Makepad's guide, the prelude source and examples.
pub fn system_prompt() -> String {
    format!(
        "{BRIEF}\n\n{}\n\n## Prelude source\n\n```splash\n{}\n```\n\n## Example: oversampled waveshaper\n\n```splash\n{}\n```\n\n## Example: stereo reverb\n\n```splash\n{}\n```\n",
        aot::GUIDE,
        aot::PRELUDE,
        include_str!("fx_examples/waveshaper4x.splash"),
        include_str!("fx_examples/allpass_reverb.splash"),
    )
}

/// The name and code in an answer.
pub fn parse_answer(text: &str) -> Option<(String, String)> {
    let name = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("NAME:").map(|n| n.trim().trim_matches('`').to_string()))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Effect".into());
    let start = text.find("```")?;
    let after = &text[start + 3..];
    let body_start = after.find('\n')? + 1;
    let body = &after[body_start..];
    let end = body.find("```")?;
    let code = body[..end].trim_end().to_string() + "\n";
    Some((name, code))
}

/// What is wrong with a shader, in words the model can act on (empty: good).
pub fn problems(code: &str) -> Vec<String> {
    if let Err(e) = fx::compile_source(code) {
        return vec![format!("it does not compile: {e}")];
    }
    match aot::check(code) {
        Err(errors) => errors.iter().map(|e| format!("check failed: {e:?}")).collect(),
        Ok(report) => {
            let mut out = report.warnings.clone();
            if report.ops_per_frame > MAX_OPS {
                out.push(format!("too expensive: {:.0} ops per sample (keep it under {MAX_OPS:.0})", report.ops_per_frame));
            }
            out
        }
    }
}

/// The result of writing an effect.
pub struct Written {
    pub name: String,
    pub code: String,
    /// Problems still left after the last round (empty when it passed).
    pub problems: Vec<String>,
    pub rounds: usize,
}

/// Write an effect for `request` on `context` (e.g. "the Bass lane of a song
/// in F major at 122 bpm"), checking and fixing up to `max_rounds()` times.
/// Drafts and answers are kept in `work` for inspection.
pub fn write_effect(request: &str, context: &str, work: &Path, progress: &dyn Fn(&str, f32)) -> Result<Written, String> {
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let system = system_prompt();
    let rounds = max_rounds();
    let ask_for = format!("Write this effect for {context}: \"{request}\"");
    let mut prompt = ask_for.clone();
    let mut last: Option<Written> = None;
    for round in 1..=rounds {
        let who = llm::name();
        progress(&if round == 1 { format!("{who} is writing the effect") } else { format!("{who} is fixing the effect") }, (round - 1) as f32 / rounds as f32);
        let answer = if round == 1 { llm::ask(&system, &prompt, MAX_TOKENS, work)? } else { llm::ask_again(&system, &prompt, MAX_TOKENS, work)? };
        let _ = std::fs::write(work.join(format!("answer-{round}.md")), &answer);
        let Some((name, code)) = parse_answer(&answer) else {
            prompt = format!("{ask_for}\n\nYour last answer had no ```splash code block. Reply in the required format.");
            continue;
        };
        progress("Checking it", (round as f32 - 0.3) / rounds as f32);
        let found = problems(&code);
        let written = Written { name, code, problems: found, rounds: round };
        if written.problems.is_empty() {
            return Ok(written);
        }
        prompt = format!(
            "{ask_for}\n\nYour previous shader:\n\n```splash\n{}```\n\nProblems found when compiling and auditioning it:\n- {}\n\nFix them and reply in the same format.",
            written.code,
            written.problems.join("\n- ")
        );
        last = Some(written);
    }
    last.ok_or_else(|| format!("{} did not return a shader", llm::name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_parsed() {
        let a = "NAME: Tape saturation\n\n```splash\nfn effect(l, r) { vec2(l, r) }\n```\n";
        let (name, code) = parse_answer(a).unwrap();
        assert_eq!(name, "Tape saturation");
        assert_eq!(code, "fn effect(l, r) { vec2(l, r) }\n");
        assert!(parse_answer("no code here").is_none());
    }

    #[test]
    fn a_clean_effect_has_no_problems_and_a_voice_does() {
        assert!(problems("let mix = param(1, 0, 1)\nfn effect(l, r) { vec2(l * 0.5, r * 0.5) }\n").is_empty());
        assert!(!problems("fn voice() { 0.0 }").is_empty());
    }

    #[test]
    fn the_system_prompt_carries_the_guide_and_examples() {
        let s = system_prompt();
        assert!(s.contains("fn effect(l, r)") && s.contains("Os4") && s.contains("allpass"));
    }
}
