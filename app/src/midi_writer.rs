//! Claude writes a MIDI part: "add a funky bassline" becomes notes on the
//! song's own grid, fitted to its key, chords and sections. Claude Code runs
//! headless (as for effects); its answer is checked and snapped to 1/16, and
//! problems go back for another round, at most two.

use poing_model::{BeatGrid, Note, Project};
use std::path::Path;

pub const MAX_ROUNDS: usize = 2;
pub const MAX_NOTES: usize = 3000;

/// The pitch range a part should stay in.
pub fn range_of(role: &str) -> (u8, u8) {
    match role {
        "bass" => (28, 60),
        "keys" => (45, 84),
        "drums" => (35, 59),
        _ => (55, 91),
    }
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

/// What Claude is told about the song: key, tempo, bars, sections, chords.
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
42 closed hi-hat, 46 open hi-hat, 49 crash, 45/47/50 toms."#;

/// Read Claude's answer: the name and the notes (in grid beats), snapped to 1/16.
pub fn parse_answer(text: &str, bars: Bars) -> Result<(String, Vec<Note>), String> {
    use makepad_micro_serde::DeJson;
    let name = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("NAME:").map(|n| n.trim().trim_matches('`').to_string()))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "MIDI part".into());
    let start = text.find('[').ok_or("no JSON array in the answer")?;
    let end = text.rfind(']').ok_or("no JSON array in the answer")?;
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

/// What is wrong with a part, in words Claude can act on (empty: good).
pub fn problems(notes: &[Note], role: &str, range: (f64, f64)) -> Vec<String> {
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
    out
}

/// The result of writing a part.
pub struct Written {
    pub name: String,
    pub notes: Vec<Note>,
    pub problems: Vec<String>,
    pub rounds: usize,
}

/// Write `request` as a `role` part over grid beats `range` of a song described by `sheet`.
pub fn write_part(request: &str, role: &str, sheet: &str, bars: Bars, range: (f64, f64), work: &Path, progress: &dyn Fn(&str, f32)) -> Result<Written, String> {
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let system_file = work.join("system.md");
    std::fs::write(&system_file, BRIEF).map_err(|e| e.to_string())?;
    let (lo, hi) = range_of(role);
    let (b0, _) = bars.bar_beat(range.0);
    let (b1, beat1) = bars.bar_beat(range.1);
    let last = if beat1 <= 1.01 { b1.saturating_sub(1).max(b0) } else { b1 };
    let ask_for = format!(
        "The song:\n{sheet}\nWrite a {role} part for bars {b0}-{last}, pitch range {lo}-{hi}: \"{request}\""
    );
    let mut prompt = ask_for.clone();
    let mut last_written: Option<Written> = None;
    for round in 1..=MAX_ROUNDS {
        progress(if round == 1 { "Claude is writing the part" } else { "Claude is fixing the part" }, (round - 1) as f32 / MAX_ROUNDS as f32);
        let answer = crate::fx_writer::ask(&system_file, &prompt, work)?;
        let _ = std::fs::write(work.join(format!("answer-{round}.md")), &answer);
        match parse_answer(&answer, bars) {
            Ok((name, mut notes)) => {
                fit(&mut notes, range);
                let found = problems(&notes, role, range);
                let w = Written { name, notes, problems: found, rounds: round };
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
    last_written.ok_or_else(|| "Claude did not return notes".into())
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
        assert!(problems(&[n(40)], "bass", (0.0, 8.0)).is_empty());
        assert!(!problems(&[n(100)], "bass", (0.0, 8.0)).is_empty(), "out of range");
        assert!(!problems(&[], "bass", (0.0, 8.0)).is_empty(), "empty");
    }

    #[test]
    fn a_part_is_trimmed_to_its_bars() {
        let n = |start, len| Note { start, len, pitch: 40, vel: 0.8 };
        let mut notes = vec![n(-0.25, 1.0), n(2.0, 1.0), n(7.5, 2.0), n(8.0, 1.0)];
        fit(&mut notes, (0.0, 8.0));
        assert_eq!(notes, vec![n(0.0, 1.0), n(2.0, 1.0), n(7.5, 0.5)]);
        assert!(problems(&notes, "bass", (0.0, 8.0)).is_empty());
    }
}
