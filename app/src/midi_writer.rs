//! The language model writes a MIDI part: "add a funky bassline" becomes
//! notes on the song's own grid, fitted to its key, chords and sections
//! (`llm::ask`, as for effects); its answer is checked and snapped to 1/16,
//! and problems go back for another round.

use crate::llm;
use poing_model::{BeatGrid, Cc, Note, Project};
use std::path::Path;

/// Rounds of write, check, fix.
pub fn max_rounds() -> usize {
    if let Some(n) = std::env::var("POING_LLM_ROUNDS").ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    match llm::provider() {
        llm::Provider::ClaudeCode => 2,
        llm::Provider::Local => 2,
    }
}
/// What one answer may run to: ~14 tokens a note, a busy whole-song part ~700 notes.
const MAX_TOKENS: usize = 12_000;
pub const MAX_NOTES: usize = 3000;

/// The pitch range a part should stay in.
pub fn range_of(role: &str) -> (u8, u8) {
    match role {
        "bass" => (28, 60),
        "keys" => (45, 84),
        "drums" => (35, 59),
        "strings" => (36, 96),
        // one string section played by a sound of its own: that instrument's range
        "strings:violin" => (55, 100),
        "strings:viola" => (48, 88),
        "strings:cello" => (36, 76),
        "strings:bass" => (28, 60),
        _ => (55, 91),
    }
}

/// The strings role for a sound that is one section (its tags name the
/// instrument): "strings:violin" and so on; else the whole-section "strings".
pub fn strings_role_for(tags: &str) -> &'static str {
    let t = tags.to_lowercase();
    if t.contains("double bass") || t.contains("contrabass") {
        "strings:bass"
    } else if t.contains("cello") {
        "strings:cello"
    } else if t.contains("viola") {
        "strings:viola"
    } else if t.contains("violin") {
        "strings:violin"
    } else {
        "strings"
    }
}

fn is_strings(role: &str) -> bool {
    role == "strings" || role.starts_with("strings:")
}

/// Bars of the song: bar 1 starts on the first downbeat.
#[derive(Clone, Copy, Debug)]
pub struct Bars {
    pub first_beat: f64,
    pub per_bar: u32,
    pub count: u32,
}

impl Bars {
    pub fn of(p: &Project) -> Self {
        let g = BeatGrid::of(p);
        let first_beat = g.bar_offset;
        let end_beat = g.beat_at(p.duration);
        let count = (((end_beat - first_beat) / g.per_bar as f64).ceil().max(1.0)) as u32;
        Bars { first_beat, per_bar: g.per_bar, count }
    }
    /// The beat (of the grid) at bar (1-based) and beat in bar (1-based, fractional).
    pub fn beat(&self, bar: f64, beat: f64) -> f64 {
        self.first_beat + (bar - 1.0) * self.per_bar as f64 + (beat - 1.0)
    }
    /// (bar, beat in bar) of a grid beat.
    pub fn bar_beat(&self, b: f64) -> (u32, f64) {
        let rel = (b - self.first_beat).max(0.0);
        let bar = (rel / self.per_bar as f64).floor();
        (bar as u32 + 1, rel - bar * self.per_bar as f64 + 1.0)
    }
}

/// What the model is told about the song: key, tempo, bars, sections, chords.
pub fn song_sheet(p: &Project, bars: Bars) -> String {
    let g = BeatGrid::of(p);
    let a = p.analysis.clone().unwrap_or_default();
    let mut out = format!(
        "Key: {}. Tempo: about {:.0} bpm. {} beats per bar. {} bars.\n",
        a.key.clone().unwrap_or_else(|| "unknown".into()),
        60.0 / g.seconds_per_beat(),
        bars.per_bar,
        bars.count
    );
    let at = |t: f64| bars.bar_beat(g.beat_at(t));
    out.push_str("\nSections (bars):\n");
    for s in p.sections() {
        let (b0, _) = at(s.start);
        let (b1, beat1) = at(s.end);
        let last = if beat1 <= 1.01 { b1.saturating_sub(1).max(b0) } else { b1 };
        out.push_str(&format!("- {}: bars {}-{}\n", s.label, b0, last));
    }
    out.push_str("\nChords (bar.beat chord):\n");
    let mut line = Vec::new();
    for c in a.chords.unwrap_or_default().iter().filter(|c| c.label != "N") {
        let (bar, beat) = at(c.start);
        line.push(format!("{bar}.{} {}", (beat.round() as u32).clamp(1, bars.per_bar), c.label));
    }
    out.push_str(&line.join(", "));
    out.push('\n');
    out
}

const BRIEF: &str = r#"You write one MIDI part for a song in Poing, a music workstation.

Reply with exactly two things and nothing else:
1. A first line `NAME: <a 1-3 word name for the part, e.g. Funky bass>`
2. One fenced code block (```json ... ```) holding a JSON array of notes, each
   `[bar, beat, length, pitch, velocity]`:
   - bar: 1-based bar number; beat: 1-based beat in the bar, may be fractional
     on a 1/16 grid (1, 1.25, 1.5, 1.75, 2, ...)
   - length: in beats (0.25 = a sixteenth), pitch: MIDI note number,
     velocity: 1-127

Write music that fits: follow the chords as given (bar.beat chord), stay in the
key, shape the part to the sections (lighter in verses, fuller in choruses),
leave space where it suits the style, and keep to the requested bars and pitch
range. For drums use General MIDI numbers: 36 kick, 38 snare, 39 clap,
42 closed hi-hat, 46 open hi-hat, 49 crash, 45/47/50 toms.

You may add, after the notes, a line `DYNAMICS: [[bar, beat, level], ...]`: the
part's loudness over time (level 0-127; pp about 20, mp 60, f 100, ff 127),
straight lines between the points. Sampled orchestral instruments get their
dynamics from it, not from velocity."#;

/// What a strings part is asked for, on top of the brief.
const STRINGS: &str = "This is an orchestral string section played by a sampled library, so write it as string players would play it: three or four voices (low strings, violas, second and first violins) with smooth voice leading (each voice moves by step or holds), long legato notes (chords last a bar or more, melody notes at least a beat, a note may overlap the next by a sixteenth for a legato join), no short stabs or repeated notes, rests where the strings should breathe. Always give a DYNAMICS line: swell into new sections and peaks, fall back in breakdowns, never flat for long.";

/// The `DYNAMICS:` line of an answer as controller 1 points (in grid beats).
pub fn parse_dynamics(text: &str, bars: Bars) -> Result<Vec<Cc>, String> {
    use makepad_micro_serde::DeJson;
    let Some(line) = text.lines().find_map(|l| l.trim().trim_start_matches('`').strip_prefix("DYNAMICS:")) else { return Ok(Vec::new()) };
    let rows = Vec::<Vec<f64>>::deserialize_json_lenient(line.trim().trim_end_matches('`')).map_err(|e| format!("the DYNAMICS line is not valid JSON: {e:?}"))?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let [bar, beat, level] = r[..] else { return Err("each DYNAMICS point must be [bar, beat, level]".into()) };
        out.push(Cc { beat: bars.beat(bar, beat), num: 1, value: (level / 127.0).clamp(0.0, 1.0) });
    }
    out.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    Ok(out)
}

/// Read the model's answer: the name and the notes (in grid beats), snapped to 1/16.
pub fn parse_answer(text: &str, bars: Bars) -> Result<(String, Vec<Note>), String> {
    use makepad_micro_serde::DeJson;
    let name = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("NAME:").map(|n| n.trim().trim_matches('`').to_string()))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "MIDI part".into());
    // the notes are the array before any DYNAMICS line
    let notes_text = text.split("DYNAMICS:").next().unwrap_or(text);
    let start = notes_text.find('[').ok_or("no JSON array in the answer")?;
    let end = notes_text.rfind(']').ok_or("no JSON array in the answer")?;
    let text = notes_text;
    let rows = Vec::<Vec<f64>>::deserialize_json_lenient(&text[start..=end]).map_err(|e| format!("the notes are not valid JSON: {e:?}"))?;
    let mut notes = Vec::with_capacity(rows.len());
    for r in rows {
        let [bar, beat, len, pitch, vel] = r[..] else { return Err("each note must be [bar, beat, length, pitch, velocity]".into()) };
        notes.push(Note {
            start: BeatGrid::snap(bars.beat(bar, beat), 4.0),
            len: BeatGrid::snap(len, 4.0).max(0.25),
            pitch: pitch.round().clamp(0.0, 127.0) as u8,
            vel: (vel / 127.0).clamp(0.05, 1.0),
        });
    }
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    Ok((name, notes))
}

/// Trim a part to beats `range`: a note that runs over the end is shortened,
/// one starting up to a sixteenth outside is pulled in (no need to ask again).
pub fn fit(notes: &mut Vec<Note>, range: (f64, f64)) {
    for n in notes.iter_mut() {
        if n.start < range.0 && n.start >= range.0 - 0.25 {
            n.start = range.0;
        }
        if n.start < range.1 && n.start + n.len > range.1 {
            n.len = range.1 - n.start;
        }
    }
    notes.retain(|n| n.start >= range.0 - 0.01 && n.start < range.1);
}

/// Pitch classes of a key like "E minor" / "F# major" / "Bb major".
pub fn scale(key: &str) -> Option<[bool; 12]> {
    let mut parts = key.split_whitespace();
    let tonic = parts.next()?;
    let minor = parts.next().is_some_and(|m| m.starts_with("min"));
    let sharps = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let flats = ["C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B"];
    let root = sharps.iter().position(|n| *n == tonic).or_else(|| flats.iter().position(|n| *n == tonic))?;
    let steps: [usize; 7] = if minor { [0, 2, 3, 5, 7, 8, 10] } else { [0, 2, 4, 5, 7, 9, 11] };
    let mut out = [false; 12];
    for s in steps {
        out[(root + s) % 12] = true;
    }
    Some(out)
}

/// The key named in a `song_sheet` ("Key: E minor.").
pub fn key_of(sheet: &str) -> Option<String> {
    let rest = sheet.split("Key: ").nth(1)?;
    Some(rest[..rest.find('.')?].to_string()).filter(|k| k != "unknown")
}

/// The key's notes in a pitch range as MIDI numbers ("28 E, 30 F#, ..."):
/// a model that knows the key by name can still miss its numbers.
pub fn scale_pitches(key: &str, (lo, hi): (u8, u8)) -> Option<String> {
    let sc = scale(key)?;
    let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    Some((lo..=hi).filter(|p| sc[*p as usize % 12]).map(|p| format!("{p} {}", names[p as usize % 12])).collect::<Vec<_>>().join(", "))
}

/// Share of a pitched part's notes in the key's scale under which it goes
/// back (chromatic passing notes are fine; a part a third out of key is not).
pub const MIN_IN_KEY: f64 = 0.6;

/// What is wrong with a part, in words the model can act on (empty: good).
/// `key` (e.g. "E minor") checks a pitched part keeps to its scale.
pub fn problems(notes: &[Note], role: &str, range: (f64, f64), key: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if notes.is_empty() {
        out.push("there are no notes".into());
    }
    if notes.len() > MAX_NOTES {
        out.push(format!("{} notes is too many (at most {MAX_NOTES})", notes.len()));
    }
    let (lo, hi) = range_of(role);
    let outside = notes.iter().filter(|n| n.pitch < lo.saturating_sub(5) || n.pitch > hi + 5).count();
    if outside > 0 {
        out.push(format!("{outside} notes are outside the pitch range {lo}-{hi}"));
    }
    let off = notes.iter().filter(|n| n.start < range.0 - 0.01 || n.start + n.len > range.1 + 0.01).count();
    if off > 0 {
        out.push(format!("{off} notes fall outside the requested bars"));
    }
    if is_strings(role) && !notes.is_empty() {
        let short = notes.iter().filter(|n| n.len < 1.0).count() as f64 / notes.len() as f64;
        if short > 0.4 {
            out.push(format!("{:.0}% of the notes are shorter than a beat: strings hold their notes (chords a bar or more, melody notes at least a beat)", short * 100.0));
        }
        // a whole section in three or four voices; one section of it (divisi) in two
        let want = if role == "strings" { 3 } else { 2 };
        if voices(notes) < want {
            out.push(if want == 3 {
                "write three or four voices that sound together (low strings, violas, violins), not one line".into()
            } else {
                "write two voices that sound together (the section divided), not one line".into()
            });
        }
    }
    if let (Some(k), Some(sc), false, false) = (key, key.and_then(scale), role == "drums", notes.is_empty()) {
        let share = notes.iter().filter(|n| sc[n.pitch as usize % 12]).count() as f64 / notes.len() as f64;
        if share < MIN_IN_KEY {
            let numbers = scale_pitches(k, range_of(role)).map(|s| format!(" (its notes: {s})")).unwrap_or_default();
            out.push(format!("only {:.0}% of the notes are in {k}{numbers}: keep to its scale and the chords, with outside notes only as short passing notes", share * 100.0));
        }
    }
    out
}

/// How many notes sound together most of the time: the median, over the
/// part's note starts, of the notes sounding there.
pub fn voices(notes: &[Note]) -> usize {
    let mut at: Vec<usize> = notes.iter().map(|a| notes.iter().filter(|b| b.start <= a.start + 1e-6 && b.start + b.len > a.start + 1e-6).count()).collect();
    at.sort_unstable();
    at.get(at.len() / 2).copied().unwrap_or(0)
}

/// The result of writing a part.
pub struct Written {
    pub name: String,
    pub notes: Vec<Note>,
    /// Controller curves (the DYNAMICS line as controller 1).
    pub cc: Vec<Cc>,
    pub problems: Vec<String>,
    pub rounds: usize,
}

/// Write `request` as a `role` part over grid beats `range` of a song described by `sheet`.
pub fn write_part(request: &str, role: &str, sheet: &str, bars: Bars, range: (f64, f64), work: &Path, progress: &dyn Fn(&str, f32)) -> Result<Written, String> {
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let rounds = max_rounds();
    let key = key_of(sheet);
    let (lo, hi) = range_of(role);
    let (b0, _) = bars.bar_beat(range.0);
    let (b1, beat1) = bars.bar_beat(range.1);
    let last = if beat1 <= 1.01 { b1.saturating_sub(1).max(b0) } else { b1 };
    let in_key = match (role, key.as_deref().and_then(|k| Some((k, scale_pitches(k, (lo, hi))?)))) {
        ("drums", _) | (_, None) => String::new(),
        (_, Some((k, numbers))) => format!("\nThe notes of {k} in that range, as MIDI numbers: {numbers}."),
    };
    let extra = match role.strip_prefix("strings:") {
        Some(section) => format!("\n{STRINGS}\nOnly the {section} section plays this part (other sections may come as parts of their own): write it in two voices, divided, inside its range."),
        None if role == "strings" => format!("\n{STRINGS}"),
        None => String::new(),
    };
    let ask_for = format!(
        "The song:\n{sheet}\nWrite a {role} part for bars {b0}-{last}, pitch range {lo}-{hi}: \"{request}\"{in_key}{extra}"
    );
    let mut prompt = ask_for.clone();
    let mut last_written: Option<Written> = None;
    for round in 1..=rounds {
        let who = llm::name();
        progress(&if round == 1 { format!("{who} is writing the part") } else { format!("{who} is fixing the part") }, (round - 1) as f32 / rounds as f32);
        let answer = if round == 1 { llm::ask(BRIEF, &prompt, MAX_TOKENS, work)? } else { llm::ask_again(BRIEF, &prompt, MAX_TOKENS, work)? };
        let _ = std::fs::write(work.join(format!("answer-{round}.md")), &answer);
        match parse_answer(&answer, bars).and_then(|(name, notes)| Ok((name, notes, parse_dynamics(&answer, bars)?))) {
            Ok((name, mut notes, cc)) => {
                fit(&mut notes, range);
                let mut found = problems(&notes, role, range, key.as_deref());
                if is_strings(role) && cc.is_empty() {
                    found.push("give the DYNAMICS line (the strings' loudness over time)".into());
                }
                let w = Written { name, notes, cc, problems: found, rounds: round };
                if w.problems.is_empty() {
                    return Ok(w);
                }
                prompt = format!("{ask_for}\n\nYour previous answer had these problems:\n- {}\nFix them and reply in the same format.", w.problems.join("\n- "));
                last_written = Some(w);
            }
            Err(e) => {
                prompt = format!("{ask_for}\n\nYour previous answer could not be read: {e}. Reply in the required format.");
            }
        }
    }
    last_written.ok_or_else(|| format!("{} did not return notes", llm::name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_become_notes_on_the_grid() {
        let bars = Bars { first_beat: 2.0, per_bar: 4, count: 10 };
        let a = "NAME: Funky bass\n```json\n[[1, 1, 0.5, 40, 100], [2, 2.5, 0.26, 43, 90]]\n```";
        let (name, notes) = parse_answer(a, bars).unwrap();
        assert_eq!(name, "Funky bass");
        assert_eq!(notes[0], Note { start: 2.0, len: 0.5, pitch: 40, vel: 100.0 / 127.0 });
        assert_eq!(notes[1].start, 7.5, "bar 2 beat 2.5 = first beat + 4 + 1.5");
        assert_eq!(notes[1].len, 0.25, "snapped to 1/16");
        assert_eq!(bars.bar_beat(7.5), (2, 2.5));
        assert!(parse_answer("no notes", bars).is_err());
    }

    #[test]
    fn problems_are_named() {
        let n = |pitch| Note { start: 0.0, len: 1.0, pitch, vel: 0.8 };
        assert!(problems(&[n(40)], "bass", (0.0, 8.0), None).is_empty());
        assert!(!problems(&[n(100)], "bass", (0.0, 8.0), None).is_empty(), "out of range");
        assert!(!problems(&[], "bass", (0.0, 8.0), None).is_empty(), "empty");
        // E minor: E (40) is in it, F (41) is not
        assert!(problems(&[n(40), n(40), n(41)], "bass", (0.0, 8.0), Some("E minor")).is_empty());
        assert!(!problems(&[n(41), n(41), n(40)], "bass", (0.0, 8.0), Some("E minor")).is_empty(), "out of key");
        assert!(problems(&[n(41)], "drums", (0.0, 8.0), Some("E minor")).is_empty(), "drums have no key");
        assert_eq!(key_of("Key: E minor. Tempo: about 125 bpm."), Some("E minor".into()));
        assert_eq!(scale_pitches("E minor", (28, 36)).unwrap(), "28 E, 30 F#, 31 G, 33 A, 35 B, 36 C");
        assert!(scale("Bb major").unwrap()[10] && scale("E minor").unwrap()[7] && !scale("E minor").unwrap()[8]);
    }

    #[test]
    fn a_strings_answer_has_dynamics_and_held_voices() {
        let bars = Bars { first_beat: 0.0, per_bar: 4, count: 8 };
        let a = "NAME: Strings\n```json\n[[1, 1, 4, 45, 80], [1, 1, 4, 52, 80], [1, 1, 4, 60, 80], [2, 1, 4, 47, 80], [2, 1, 4, 55, 80], [2, 1, 4, 62, 80]]\n```\nDYNAMICS: [[1, 1, 30], [3, 1, 110]]";
        let (_, notes) = parse_answer(a, bars).unwrap();
        assert_eq!(notes.len(), 6, "the DYNAMICS array is not read as notes");
        let cc = parse_dynamics(a, bars).unwrap();
        assert_eq!(cc, vec![Cc { beat: 0.0, num: 1, value: 30.0 / 127.0 }, Cc { beat: 8.0, num: 1, value: 110.0 / 127.0 }]);
        assert_eq!(voices(&notes), 3);
        assert!(problems(&notes, "strings", (0.0, 32.0), Some("A minor")).is_empty(), "{:?}", problems(&notes, "strings", (0.0, 32.0), Some("A minor")));
        // stabs, one line at a time
        let stabs: Vec<Note> = (0..16).map(|i| Note { start: i as f64, len: 0.25, pitch: 57, vel: 0.8 }).collect();
        let p = problems(&stabs, "strings", (0.0, 32.0), Some("A minor"));
        assert!(p.iter().any(|x| x.contains("shorter than a beat")) && p.iter().any(|x| x.contains("voices")), "{p:?}");
        assert!(parse_dynamics("no line", bars).unwrap().is_empty());
        // a violins sound: its range, two voices are enough
        assert_eq!(strings_role_for("Bowed Strings, Violin"), "strings:violin");
        assert_eq!(strings_role_for("Bowed Strings, Double Bass"), "strings:bass");
        assert_eq!(strings_role_for("Synth Pad"), "strings");
        let low: Vec<Note> = [(45, 0.0), (60, 0.0)].iter().map(|&(p, s)| Note { start: s, len: 4.0, pitch: p, vel: 0.8 }).collect();
        let p = problems(&low, "strings:violin", (0.0, 32.0), Some("A minor"));
        assert!(p.iter().any(|x| x.contains("outside the pitch range 55-100")), "{p:?}");
        let two: Vec<Note> = [(60, 0.0), (64, 0.0)].iter().map(|&(p, s)| Note { start: s, len: 4.0, pitch: p, vel: 0.8 }).collect();
        assert!(problems(&two, "strings:violin", (0.0, 32.0), None).iter().all(|x| !x.contains("voices")));
    }

    #[test]
    fn a_part_is_trimmed_to_its_bars() {
        let n = |start, len| Note { start, len, pitch: 40, vel: 0.8 };
        let mut notes = vec![n(-0.25, 1.0), n(2.0, 1.0), n(7.5, 2.0), n(8.0, 1.0)];
        fit(&mut notes, (0.0, 8.0));
        assert_eq!(notes, vec![n(0.0, 1.0), n(2.0, 1.0), n(7.5, 0.5)]);
        assert!(problems(&notes, "bass", (0.0, 8.0), None).is_empty());
    }
}
