// Recording a MIDI keyboard into a MIDI lane. R arms the selected lane: from
// a stop it counts in a bar (clicks on the song's beats) and plays, while
// playing it punches in at once. The audio thread hands back every key it
// played with its song frame; stopping turns them into notes on the beat grid
// (`poing_model::record`: quantised, velocities kept) and puts them in the
// lane over the recorded range, replacing or on top: one undo step.
//
// Included into state.rs as its child module `state::record` (it works on
// EditorState's own fields), so poing and poing-node find it alike.

use super::{EditorState, Selection};
use crate::live;
use crate::model::record::{self as rec, Played, Quantise};
use crate::model::{BeatGrid, Note};
use std::sync::atomic::Ordering;

/// A recording in progress.
pub struct Recording {
    pub lane: usize,
    /// Where it punched in (seconds of the song).
    pub from: f64,
    /// Every key heard, in the order played.
    pub played: Vec<Played>,
    /// It went round the loop at least once.
    pub wrapped: bool,
    last_t: f64,
}

impl EditorState {
    /// The MIDI lane the keys play: the one recording, else the selected MIDI lane.
    pub fn keys_lane(&self) -> Option<usize> {
        if let Some(r) = &self.rec {
            return Some(r.lane);
        }
        match self.sel {
            Selection::Track(i) if self.project.tracks.get(i).is_some_and(|t| t.is_midi()) => Some(i),
            _ => None,
        }
    }

    /// Tell the audio thread which lane the keys play (the song in front only).
    pub fn sync_keys(&self) {
        if self.is_current {
            self.shared.live.set_lane(self.keys_lane());
        }
    }

    pub fn recording(&self) -> bool {
        self.rec.is_some()
    }

    /// R: record the selected MIDI lane (a bar's count-in first when stopped),
    /// or stop and keep what was played.
    pub fn toggle_record(&mut self) -> String {
        if self.rec.is_some() {
            self.shared.set_playing(false);
            return self.finish_recording();
        }
        if !self.is_current {
            return String::new();
        }
        let Some(lane) = self.keys_lane() else { return "Select a MIDI lane to record into".into() };
        let live = &self.shared.live;
        while live.played.pop().is_some() {}
        let grid = BeatGrid::of(&self.project);
        let counted = !self.shared.playing();
        if counted {
            if self.playhead() >= self.duration() - 0.01 {
                self.seek(0.0);
            }
            let b = grid.beat_at(self.playhead());
            let beat_s = (grid.time_of(b + 1.0) - grid.time_of(b)).max(0.05);
            live.count_in(grid.per_bar as u64, (beat_s * self.sr() as f64) as u64);
            self.shared.set_playing(true);
        } else {
            live.count_in(0, 1);
        }
        let from = self.playhead();
        self.rec = Some(Recording { lane, from, played: Vec::new(), wrapped: false, last_t: from });
        self.sel_changed = true;
        let name = &self.project.tracks[lane].name;
        if counted {
            format!("Recording {name} after a bar's count-in  ·  R or Space stops")
        } else {
            format!("Recording {name}  ·  R or Space stops")
        }
    }

    /// Take in what the audio thread heard (UI timer, while recording); the
    /// song stopped (Space, or its end), the recording lands.
    pub fn pump_recording(&mut self) -> Option<String> {
        self.take_played();
        self.rec.as_ref()?;
        if self.shared.playing() {
            return None;
        }
        Some(self.finish_recording())
    }

    fn take_played(&mut self) {
        let sr = self.sr() as f64;
        let Some(r) = self.rec.as_mut() else { return };
        while let Some((frame, k)) = self.shared.live.played.pop() {
            let t = frame as f64 / sr;
            if t + 1e-6 < r.last_t {
                r.wrapped = true;
            }
            r.last_t = t;
            r.played.push(Played { t, pitch: k.pitch, vel: k.vel, on: k.on });
        }
    }

    /// The loop playback goes round (seconds), if it is on.
    fn loop_secs(&self) -> Option<(f64, f64)> {
        let s = &self.shared;
        let sr = self.sr() as f64;
        s.loop_on.load(Ordering::Relaxed).then(|| (s.loop_a.load(Ordering::Relaxed) as f64 / sr, s.loop_b.load(Ordering::Relaxed) as f64 / sr))
    }

    /// What has been recorded so far, as it will land (for the piano roll).
    pub fn recorded_notes(&self) -> Option<(usize, Vec<Note>)> {
        let r = self.rec.as_ref()?;
        let grid = BeatGrid::of(&self.project);
        let wrap = r.wrapped.then(|| self.loop_secs()).flatten();
        let notes = rec::notes_from(&r.played, &grid, self.playhead(), wrap);
        Some((r.lane, rec::quantise(&notes, self.quantise)))
    }

    fn finish_recording(&mut self) -> String {
        self.take_played();
        let live = &self.shared.live;
        live.state.store(live::IDLE, Ordering::Release);
        live.count_left.store(0, Ordering::Relaxed);
        let Some(r) = self.rec.take() else { return String::new() };
        self.sel_changed = true;
        let end = self.playhead();
        let grid = BeatGrid::of(&self.project);
        let wrap = r.wrapped.then(|| self.loop_secs()).flatten();
        let notes = rec::quantise(&rec::notes_from(&r.played, &grid, end, wrap), self.quantise);
        let Some(track) = self.project.tracks.get(r.lane).filter(|t| t.is_midi()) else { return String::new() };
        let name = track.name.clone();
        if notes.is_empty() {
            return format!("Nothing played: {name} is as it was");
        }
        // replace clears the punch range: from punch in to stop, or the whole loop once round it
        let (lo, hi) = wrap.unwrap_or((r.from, end.max(r.last_t)));
        let existing = track.notes.clone().unwrap_or_default();
        let merged = rec::merge(&existing, &notes, (grid.beat_at(lo), grid.beat_at(hi)), !self.overdub);
        self.checkpoint();
        self.set_notes(r.lane, merged);
        self.select(Selection::Track(r.lane));
        let how = match self.quantise {
            Quantise::Off => "as played".to_string(),
            q => format!("quantised to {}", q.label()),
        };
        let n = notes.len();
        format!("Recorded {n} note{} into {name}, {how}  ·  Ctrl+Z takes it out", if n == 1 { "" } else { "s" })
    }
}
