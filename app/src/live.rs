//! Playing a MIDI lane live from a MIDI keyboard (or the computer keyboard)
//! and recording it: keys go straight to the audio thread over a lock-free
//! queue (no rack rebuild), which plays them on the lane's instrument and,
//! while recording, hands each back with the song frame it was heard at.
//! Also the count-in and the metronome click.

use crossbeam_queue::ArrayQueue;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

/// A key pressed (vel 1..=127) or let go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub pitch: u8,
    pub vel: u8,
    pub on: bool,
}

/// Recording state.
pub const IDLE: u8 = 0;
pub const COUNT_IN: u8 = 1;
pub const RECORDING: u8 = 2;

/// No lane plays the keys.
pub const NO_LANE: usize = usize::MAX;

/// Room in each queue: far more than a block's worth of playing.
const QUEUE: usize = 1024;

/// What the UI, the MIDI input thread and the audio thread share.
pub struct Live {
    /// Keys for the audio thread (from the MIDI thread and the UI).
    pub keys: ArrayQueue<Key>,
    /// Keys heard while recording, with the song frame of their block (audio → UI).
    pub played: ArrayQueue<(u64, Key)>,
    /// The MIDI lane the keys play (track index), or [`NO_LANE`].
    pub lane: AtomicUsize,
    pub state: AtomicU8,
    /// Count-in: frames still to go, the beat length in frames and beats in it.
    pub count_left: AtomicU64,
    pub beat_frames: AtomicU64,
    pub count_beats: AtomicU64,
}

impl Default for Live {
    fn default() -> Self {
        Live {
            keys: ArrayQueue::new(QUEUE),
            played: ArrayQueue::new(QUEUE),
            lane: AtomicUsize::new(NO_LANE),
            state: AtomicU8::new(IDLE),
            count_left: AtomicU64::new(0),
            beat_frames: AtomicU64::new(0),
            count_beats: AtomicU64::new(0),
        }
    }
}

impl Live {
    pub fn lane(&self) -> Option<usize> {
        Some(self.lane.load(Ordering::Relaxed)).filter(|l| *l != NO_LANE)
    }

    pub fn set_lane(&self, lane: Option<usize>) {
        self.lane.store(lane.unwrap_or(NO_LANE), Ordering::Relaxed);
    }

    pub fn state(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }

    /// A key from a device or the computer keyboard (a full queue drops it).
    pub fn key(&self, pitch: u8, vel: u8, on: bool) {
        let _ = self.keys.push(Key { pitch: pitch.min(127), vel: vel.min(127), on: on && vel > 0 });
    }

    /// Count in `beats` beats of `beat_frames` each, then record.
    pub fn count_in(&self, beats: u64, beat_frames: u64) {
        self.beat_frames.store(beat_frames.max(1), Ordering::Relaxed);
        self.count_beats.store(beats, Ordering::Relaxed);
        self.count_left.store(beats * beat_frames.max(1), Ordering::Relaxed);
        self.state.store(if beats == 0 { RECORDING } else { COUNT_IN }, Ordering::Release);
    }

    pub fn counting_in(&self) -> bool {
        self.count_left.load(Ordering::Relaxed) > 0
    }

    /// Audio thread: a key played at song frame `frame`; kept when recording,
    /// and while counting in only in the last half beat (early for the downbeat).
    pub fn heard(&self, frame: u64, key: Key) {
        let keep = match self.state() {
            RECORDING => true,
            COUNT_IN => !key.on || self.count_left.load(Ordering::Relaxed) <= self.beat_frames.load(Ordering::Relaxed) / 2,
            _ => false,
        };
        if keep {
            let _ = self.played.push((frame, key));
        }
    }

    /// Audio thread: `n` frames of count-in went by, with a click on each of
    /// its beats into l/r. At its end the recording starts.
    pub fn count(&self, click: &mut Click, l: &mut [f32], r: &mut [f32]) {
        let left = self.count_left.load(Ordering::Relaxed);
        let (beat, beats) = (self.beat_frames.load(Ordering::Relaxed).max(1), self.count_beats.load(Ordering::Relaxed));
        let n = l.len().min(r.len()) as u64;
        let done = (beats * beat).saturating_sub(left);
        // the beats that start in [done, done + n)
        let mut k = done.div_ceil(beat);
        while k * beat < (done + n).min(beats * beat) {
            click.at((k * beat - done) as usize, k == 0);
            k += 1;
        }
        click.render(l, r);
        let now = left.saturating_sub(n);
        self.count_left.store(now, Ordering::Relaxed);
        if now == 0 {
            let _ = self.state.compare_exchange(COUNT_IN, RECORDING, Ordering::AcqRel, Ordering::Relaxed);
        }
    }
}

/// The metronome: a short decaying sine, higher on the bar. Renders into a
/// block from a start offset; no allocation.
pub struct Click {
    sr: f32,
    /// Frames since the click started (None: quiet).
    age: Option<u32>,
    /// Where in the next rendered block a click starts.
    pending: Option<(usize, bool)>,
    freq: f32,
}

/// Click length and level.
const CLICK_S: f32 = 0.035;
const CLICK_GAIN: f32 = 0.35;

impl Click {
    pub fn new(sr: u32) -> Self {
        Click { sr: sr as f32, age: None, pending: None, freq: 1000.0 }
    }

    /// A click `offset` frames into the next [`Self::render`] (`bar`: the high one).
    pub fn at(&mut self, offset: usize, bar: bool) {
        self.pending = Some((offset, bar));
    }

    /// Add the sounding click into l/r.
    pub fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        let len = (CLICK_S * self.sr) as u32;
        let pending = self.pending.take();
        for i in 0..n {
            if let Some((_, bar)) = pending.filter(|(at, _)| *at == i) {
                self.age = Some(0);
                self.freq = if bar { 1760.0 } else { 1320.0 };
            }
            let Some(age) = self.age else { continue };
            if age >= len {
                self.age = None;
                continue;
            }
            let t = age as f32 / self.sr;
            let s = CLICK_GAIN * (std::f32::consts::TAU * self.freq * t).sin() * (-t / (CLICK_S * 0.25)).exp();
            l[i] += s;
            r[i] += s;
            self.age = Some(age + 1);
        }
    }

    /// Clicks on the song's beats (frames, bar flag) that fall in [start, start + l.len()).
    pub fn beats(&mut self, beats: &[(u64, bool)], start: u64, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len()) as u64;
        let i = beats.partition_point(|b| b.0 < start);
        // one click per block is plenty (blocks are far shorter than a beat)
        if let Some(&(f, bar)) = beats.get(i).filter(|b| b.0 < start + n) {
            self.at((f - start) as usize, bar);
        }
        self.render(l, r);
    }
}

/// The computer keyboard as a piano: A W S E D F T G Y H U J K are the
/// semitones from C (A is C). Returns the semitone.
pub fn piano_key(code: makepad_widgets::KeyCode) -> Option<u8> {
    use makepad_widgets::KeyCode::*;
    Some(match code {
        KeyA => 0,
        KeyW => 1,
        KeyS => 2,
        KeyE => 3,
        KeyD => 4,
        KeyF => 5,
        KeyT => 6,
        KeyG => 7,
        KeyY => 8,
        KeyH => 9,
        KeyU => 10,
        KeyJ => 11,
        KeyK => 12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_in_clicks_each_beat_then_records() {
        let live = Live::default();
        live.count_in(4, 4000); // a click is 1680 frames at 48 kHz
        assert_eq!(live.state(), COUNT_IN);
        let mut click = Click::new(48000);
        let (mut l, mut r) = (vec![0.0f32; 500], vec![0.0f32; 500]);
        let mut loud = Vec::new();
        for block in 0..34 {
            l.fill(0.0);
            r.fill(0.0);
            live.count(&mut click, &mut l, &mut r);
            if l.iter().any(|x| x.abs() > 0.01) {
                loud.push(block);
            }
            assert_eq!(live.state() == RECORDING, block >= 31, "block {block}");
        }
        // each beat's click sounds in its first 4 blocks, then the rest of the beat is quiet
        let starts: Vec<i32> = loud.iter().copied().filter(|b| b % 8 == 0).collect();
        assert_eq!(starts, [0, 8, 16, 24]);
        assert!(loud.iter().all(|b| b % 8 < 4 && *b < 32), "{loud:?}");
    }

    #[test]
    fn keys_heard_while_counting_in_count_only_near_the_downbeat() {
        let live = Live::default();
        live.count_in(4, 100);
        live.heard(500, Key { pitch: 60, vel: 90, on: true });
        assert!(live.played.pop().is_none(), "too early: dropped");
        live.count_left.store(40, Ordering::Relaxed);
        live.heard(500, Key { pitch: 62, vel: 90, on: true });
        assert_eq!(live.played.pop().map(|(f, k)| (f, k.pitch)), Some((500, 62)));
        live.state.store(IDLE, Ordering::Relaxed);
        live.heard(600, Key { pitch: 64, vel: 90, on: true });
        assert!(live.played.pop().is_none(), "not recording: not kept");
    }

    #[test]
    fn a_metronome_click_sounds_from_its_beat_and_fades() {
        let mut click = Click::new(48000);
        let beats = [(1000u64, true), (25000, false)];
        let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        click.beats(&beats, 800, &mut l, &mut r);
        assert!(l[..200].iter().all(|x| *x == 0.0), "quiet before the beat");
        assert!(l[200..512].iter().any(|x| x.abs() > 0.05), "clicks on it");
        for k in 0..6 {
            l.fill(0.0);
            r.fill(0.0);
            click.beats(&beats, 1312 + k * 512, &mut l, &mut r);
        }
        assert!(l.iter().all(|x| *x == 0.0), "over after 35 ms");
    }
}
