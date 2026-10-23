//! Recording a MIDI keyboard into a lane: played events (seconds of the song)
//! become notes on the song's beat grid, optionally quantised, and land in the
//! lane replacing what was there over the recorded range, or on top of it.

use crate::{BeatGrid, Note};

/// Where recorded notes start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quantise {
    /// As played.
    Off,
    /// To 1/8 notes.
    Eighth,
    /// To 1/16 notes.
    #[default]
    Sixteenth,
    /// To 1/16 notes, every second one late (see [`SWING`]).
    SixteenthSwing,
}

/// With swing, the second 1/16 of each 1/8 starts at this fraction of the 1/8
/// (0.5 is straight).
pub const SWING: f64 = 0.6;

impl Quantise {
    pub const ALL: [Quantise; 4] = [Quantise::Off, Quantise::Eighth, Quantise::Sixteenth, Quantise::SixteenthSwing];

    pub fn label(self) -> &'static str {
        match self {
            Quantise::Off => "Off",
            Quantise::Eighth => "1/8",
            Quantise::Sixteenth => "1/16",
            Quantise::SixteenthSwing => "1/16 swing",
        }
    }

    /// The grid step in beats (None: off).
    pub fn step(self) -> Option<f64> {
        match self {
            Quantise::Off => None,
            Quantise::Eighth => Some(0.5),
            Quantise::Sixteenth | Quantise::SixteenthSwing => Some(0.25),
        }
    }

    /// The grid point nearest `beat`. With swing that is the nearest point of
    /// the swung grid, so a note played on the late 1/16 stays there.
    pub fn snap(self, beat: f64) -> f64 {
        match self {
            Quantise::Off => beat,
            Quantise::Eighth => BeatGrid::snap(beat, 2.0),
            Quantise::Sixteenth => BeatGrid::snap(beat, 4.0),
            Quantise::SixteenthSwing => {
                let eighth = (beat * 2.0).floor() / 2.0;
                [eighth, eighth + 0.5 * SWING, eighth + 0.5]
                    .into_iter()
                    .min_by(|a, b| (a - beat).abs().total_cmp(&(b - beat).abs()))
                    .unwrap_or(beat)
            }
        }
    }
}

/// A key pressed or let go, at a time of the song (seconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Played {
    pub t: f64,
    pub pitch: u8,
    /// MIDI velocity, 1..=127 for a press.
    pub vel: u8,
    pub on: bool,
}

/// Shortest note a recording keeps, in beats.
const MIN_LEN: f64 = 1.0 / 64.0;

/// Pair presses with releases into notes on `grid` (in beats). A key still
/// held at `end` (seconds) ends there. `wrap` is the loop the recording went
/// round, if any: a release before its press came after a wrap, so the note
/// ends at the loop's end.
pub fn notes_from(events: &[Played], grid: &BeatGrid, end: f64, wrap: Option<(f64, f64)>) -> Vec<Note> {
    let mut held: [Option<(f64, u8)>; 128] = [None; 128];
    let mut out = Vec::new();
    let close = |out: &mut Vec<Note>, pitch: u8, (t0, vel): (f64, u8), t1: f64| {
        let t1 = match wrap {
            Some((_, b)) if t1 < t0 => b,
            _ => t1,
        };
        let start = grid.beat_at(t0);
        let len = (grid.beat_at(t1) - start).max(MIN_LEN);
        out.push(Note { start, len, pitch, vel: vel.clamp(1, 127) as f64 / 127.0 });
    };
    for e in events {
        let p = e.pitch.min(127);
        // a second press of a held key ends the first
        if let Some(on) = held[p as usize].take() {
            close(&mut out, p, on, e.t);
        }
        if e.on && e.vel > 0 {
            held[p as usize] = Some((e.t, e.vel));
        }
    }
    for p in 0..128u8 {
        if let Some(on) = held[p as usize].take() {
            close(&mut out, p, on, end);
        }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    out
}

/// Move each note's start to the grid; its length stays (at least one step).
pub fn quantise(notes: &[Note], q: Quantise) -> Vec<Note> {
    let Some(step) = q.step() else { return notes.to_vec() };
    notes.iter().map(|n| Note { start: q.snap(n.start).max(0.0), len: n.len.max(step), ..n.clone() }).collect()
}

/// The lane's notes with a recording in. `replace` clears what started in
/// `range` (beats: punch in to stop) first; else the recording goes on top
/// (a note already there at the same start and pitch is not doubled).
pub fn merge(existing: &[Note], recorded: &[Note], range: (f64, f64), replace: bool) -> Vec<Note> {
    let (lo, hi) = range;
    let mut out: Vec<Note> = existing.iter().filter(|n| !(replace && n.start >= lo - 1e-9 && n.start < hi)).cloned().collect();
    for n in recorded {
        if !out.iter().any(|e| e.pitch == n.pitch && (e.start - n.start).abs() < 1e-6) {
            out.push(n.clone());
        }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid120() -> BeatGrid {
        BeatGrid::new(&[], &[], Some(120.0)) // a beat is 0.5 s
    }

    fn on(t: f64, pitch: u8, vel: u8) -> Played {
        Played { t, pitch, vel, on: true }
    }
    fn off(t: f64, pitch: u8) -> Played {
        Played { t, pitch, vel: 0, on: false }
    }

    #[test]
    fn quantise_snaps_starts_to_eighths_and_sixteenths() {
        assert_eq!(Quantise::Off.snap(1.13), 1.13);
        assert_eq!(Quantise::Eighth.snap(1.13), 1.0);
        assert_eq!(Quantise::Eighth.snap(1.3), 1.5);
        assert_eq!(Quantise::Sixteenth.snap(1.13), 1.25);
        assert_eq!(Quantise::Sixteenth.snap(1.1), 1.0);
        assert_eq!(Quantise::Sixteenth.snap(-0.05), 0.0);
    }

    #[test]
    fn swing_keeps_the_late_sixteenth_late() {
        let q = Quantise::SixteenthSwing;
        // the swung grid in a beat: 0, 0.3, 0.5, 0.8, 1.0
        assert_eq!(q.snap(0.02), 0.0);
        assert!((q.snap(0.31) - 0.3).abs() < 1e-12, "played on the swung 1/16: stays");
        assert!((q.snap(0.26) - 0.3).abs() < 1e-12, "a straight 1/16 goes to the swung place");
        assert_eq!(q.snap(0.45), 0.5);
        assert!((q.snap(0.79) - 0.8).abs() < 1e-12);
        assert_eq!(q.snap(0.93), 1.0);
        assert!((q.snap(3.33) - 3.3).abs() < 1e-12);
    }

    #[test]
    fn presses_and_releases_pair_into_notes_with_their_velocity() {
        let ev = [on(1.0, 60, 100), on(1.26, 64, 64), off(1.5, 60), off(1.74, 64)];
        let notes = notes_from(&ev, &grid120(), 4.0, None);
        assert_eq!(notes.len(), 2);
        assert_eq!((notes[0].pitch, notes[0].start, notes[0].len), (60, 2.0, 1.0));
        assert!((notes[0].vel - 100.0 / 127.0).abs() < 1e-12);
        assert_eq!(notes[1].pitch, 64);
        assert!((notes[1].start - 2.52).abs() < 1e-9 && (notes[1].len - 0.96).abs() < 1e-9);
        assert!((notes[1].vel - 64.0 / 127.0).abs() < 1e-12);
    }

    #[test]
    fn a_key_held_at_stop_ends_there_and_a_wrapped_one_at_the_loop_end() {
        let notes = notes_from(&[on(1.0, 60, 90)], &grid120(), 2.0, None);
        assert_eq!((notes[0].start, notes[0].len), (2.0, 2.0));
        // looping over 0..4 s: pressed at 3.5 s, let go after the wrap at 0.5 s
        let notes = notes_from(&[on(3.5, 62, 90), off(0.5, 62)], &grid120(), 1.0, Some((0.0, 4.0)));
        assert_eq!((notes[0].start, notes[0].len), (7.0, 1.0));
        // a release with no press is ignored; a repeated press ends the first
        let notes = notes_from(&[off(0.1, 50), on(0.5, 50, 80), on(1.0, 50, 70), off(1.5, 50)], &grid120(), 3.0, None);
        assert_eq!(notes.len(), 2);
        assert_eq!((notes[0].start, notes[0].len, notes[1].start, notes[1].len), (1.0, 1.0, 2.0, 1.0));
    }

    #[test]
    fn notes_follow_a_played_in_tempo() {
        // beats at 0, 0.5, 1.1, 1.6: the third beat came late
        let grid = BeatGrid::new(&[0.0, 0.5, 1.1, 1.6], &[], None);
        let notes = notes_from(&[on(1.1, 60, 100), off(1.6, 60)], &grid, 2.0, None);
        assert!((notes[0].start - 2.0).abs() < 1e-9 && (notes[0].len - 1.0).abs() < 1e-9);
    }

    #[test]
    fn quantising_moves_starts_and_keeps_lengths_and_velocities() {
        let played = [Note { start: 2.07, len: 0.9, pitch: 60, vel: 0.8 }, Note { start: 2.48, len: 0.05, pitch: 62, vel: 0.3 }];
        let q = quantise(&played, Quantise::Sixteenth);
        assert_eq!((q[0].start, q[0].len, q[0].vel), (2.0, 0.9, 0.8));
        assert_eq!((q[1].start, q[1].len, q[1].vel), (2.5, 0.25, 0.3), "at least one step long");
        assert_eq!(quantise(&played, Quantise::Off), played.to_vec());
    }

    #[test]
    fn replace_clears_the_punch_range_and_overdub_adds() {
        let n = |start: f64, pitch: u8| Note { start, len: 0.5, pitch, vel: 0.7 };
        let lane = [n(0.0, 40), n(4.0, 40), n(6.0, 43), n(8.0, 40)];
        let rec = [n(4.0, 60), n(5.0, 62)];
        // punched in at beat 4, stopped at 8: everything that started there goes
        let replaced = merge(&lane, &rec, (4.0, 8.0), true);
        assert_eq!(replaced.iter().map(|n| (n.start, n.pitch)).collect::<Vec<_>>(), [(0.0, 40), (4.0, 60), (5.0, 62), (8.0, 40)]);
        let over = merge(&lane, &rec, (4.0, 8.0), false);
        assert_eq!(over.len(), 6);
        // the same note again is not doubled
        assert_eq!(merge(&over, &rec, (4.0, 8.0), false).len(), 6);
    }
}
