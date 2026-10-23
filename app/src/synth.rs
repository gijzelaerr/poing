//! MIDI lanes: notes played by a Splash `voice()` instrument with a small
//! voice pool, scheduled sample-accurately from the song's beat grid.
//!
//! The UI turns a lane's notes into sorted frame events and builds the
//! instrument (all allocation there); the audio thread walks the events
//! block by block. Any jump (seek, loop wrap, resume, a new rack) releases
//! whatever is sounding, so no note hangs.

use crate::fx::FxControls;
use makepad_script_audio_aot as aot;
use poing_model::{BeatGrid, Note};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Voices per instrument; the oldest is taken when all are busy.
pub const VOICES: usize = 16;

/// A note starting or ending at a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoteEvent {
    pub frame: u64,
    pub pitch: u8,
    pub vel: f32,
    pub on: bool,
}

/// A lane's notes as frame events, note-offs before note-ons at the same frame.
pub fn events(notes: &[Note], grid: &BeatGrid, sample_rate: u32) -> Vec<NoteEvent> {
    let sr = sample_rate as f64;
    let mut out = Vec::with_capacity(notes.len() * 2);
    for n in notes {
        let a = (grid.time_of(n.start).max(0.0) * sr) as u64;
        let b = ((grid.time_of(n.start + n.len.max(1.0 / 64.0)) * sr) as u64).max(a + 1);
        out.push(NoteEvent { frame: a, pitch: n.pitch.min(127), vel: n.vel.clamp(0.0, 1.0) as f32, on: true });
        out.push(NoteEvent { frame: b, pitch: n.pitch.min(127), vel: 0.0, on: false });
    }
    out.sort_by(|x, y| x.frame.cmp(&y.frame).then(x.on.cmp(&y.on)));
    out
}

struct Voice {
    ctx: Box<[u32]>,
    state: Box<[u32]>,
    scratch: Box<[u32]>,
    pitch: i32,
    gate: bool,
    busy: bool,
    age: u64,
}

/// A polyphonic Splash instrument.
pub struct Synth {
    shader: aot::AudioShaderRef,
    controls: Arc<FxControls>,
    applied: Box<[u32]>,
    voices: Vec<Voice>,
    zeros: Box<[f32]>,
    frame: u32,
    clock: u64,
    seed: u32,
}

impl Synth {
    pub fn new(shader: aot::AudioShaderRef, controls: Arc<FxControls>, sample_rate: u32) -> Self {
        let voices = (0..VOICES)
            .map(|_| {
                let mut ctx = vec![0u32; shader.ctx_words()].into_boxed_slice();
                shader.init_ctx(&mut ctx, sample_rate as f32);
                Voice {
                    ctx,
                    state: shader.state_init().into(),
                    scratch: vec![0u32; shader.scratch_words()].into_boxed_slice(),
                    pitch: -1,
                    gate: false,
                    busy: false,
                    age: 0,
                }
            })
            .collect();
        let applied = shader.params().iter().map(|p| p.default.to_bits()).collect();
        Synth { shader, controls, applied, voices, zeros: vec![0.0; aot::MAX_FRAMES as usize].into_boxed_slice(), frame: 0, clock: 0, seed: 1 }
    }

    pub fn note_on(&mut self, pitch: u8, vel: f32) {
        self.clock += 1;
        // the voice already on this pitch, else a free one, else the oldest
        let i = self
            .voices
            .iter()
            .position(|v| v.busy && v.pitch == pitch as i32)
            .or_else(|| self.voices.iter().position(|v| !v.busy))
            .unwrap_or_else(|| self.voices.iter().enumerate().min_by_key(|(_, v)| v.age).map(|(i, _)| i).unwrap_or(0));
        self.seed = self.seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let v = &mut self.voices[i];
        self.shader.note_on(&mut v.state, pitch as f32, vel, self.frame, self.seed);
        v.pitch = pitch as i32;
        v.gate = true;
        v.busy = true;
        v.age = self.clock;
    }

    pub fn note_off(&mut self, pitch: u8) {
        for v in self.voices.iter_mut().filter(|v| v.gate && v.pitch == pitch as i32) {
            self.shader.note_off(&mut v.state);
            v.gate = false;
        }
    }

    pub fn all_off(&mut self) {
        for v in self.voices.iter_mut().filter(|v| v.gate) {
            self.shader.note_off(&mut v.state);
            v.gate = false;
        }
    }

    /// Add `l.len()` frames of every sounding voice into l/r.
    pub fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (k, target) in self.controls.params.iter().enumerate() {
            let bits = target.load(Ordering::Relaxed);
            if bits != self.applied[k] {
                for v in &mut self.voices {
                    self.shader.set_param(&mut v.ctx, k, f32::from_bits(bits));
                }
                self.applied[k] = bits;
            }
        }
        let n = l.len().min(r.len());
        let mut done = 0;
        while done < n {
            let k = (n - done).min(aot::MAX_FRAMES as usize);
            for v in self.voices.iter_mut().filter(|v| v.busy) {
                v.ctx[aot::CTX_FRAME as usize] = self.frame;
                self.shader.run(&mut v.ctx, &mut v.state, &mut v.scratch, &self.zeros[..k], &self.zeros[..k], &mut l[done..done + k], &mut r[done..done + k], k);
                if self.shader.stopped(&v.state) {
                    v.busy = false;
                    v.gate = false;
                }
            }
            self.frame = self.frame.wrapping_add(k as u32);
            done += k;
        }
        // a voice that blows up goes quiet
        for x in l[..n].iter_mut().chain(r[..n].iter_mut()) {
            *x = if x.is_finite() { x.clamp(-4.0, 4.0) } else { 0.0 };
        }
    }
}

/// A VST3 instrument in a MIDI lane (Windows for now), shared with the UI
/// (its editor). A rebuilt rack starts without it and takes it over from the
/// rack it replaces.
#[cfg(windows)]
pub struct PluginSound {
    pub id: String,
    pub hosted: Option<crate::plugins::Shared>,
    buffers: vst3_host::audio::AudioBuffers,
    /// Notes the plugin holds, to release on a jump.
    held: [bool; 128],
    /// Note events not yet given to the plugin (it was busy): (pitch, velocity, on).
    /// Room made up front, so the audio thread never allocates.
    queued: Vec<(u8, u8, bool)>,
}

/// Note events a plugin instrument can have waiting while it is busy.
#[cfg(windows)]
const QUEUE: usize = 1024;

#[cfg(windows)]
impl PluginSound {
    pub fn new(id: &str, sample_rate: u32) -> Self {
        let buffers = vst3_host::audio::AudioBuffers::new(0, 2, crate::fx::PLUGIN_BLOCK, sample_rate as f64);
        PluginSound { id: id.to_string(), hosted: None, buffers, held: [false; 128], queued: Vec::with_capacity(QUEUE) }
    }

    /// Take over the instance (and the notes it holds) of the lane this one replaces.
    pub fn take_over(&mut self, old: &mut PluginSound) {
        self.hosted = old.hosted.take();
        self.held = old.held;
        for e in old.queued.drain(..) {
            if self.queued.len() < QUEUE {
                self.queued.push(e);
            }
        }
    }

    fn send(&mut self, pitch: u8, vel: f32, on: bool) {
        let note = pitch.min(127);
        if self.queued.len() < QUEUE {
            self.queued.push((note, (vel * 127.0).round().clamp(1.0, 127.0) as u8, on));
            self.held[note as usize] = on;
        }
    }

    /// Add `l.len()` frames of the plugin's output into l/r. The plugin busy
    /// (the UI has it): silence for this stretch, its notes wait for the next.
    fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
        use vst3_host::midi::{MidiChannel, MidiEvent};
        let Some(shared) = self.hosted.as_ref() else { return };
        let Ok(mut plugin) = shared.try_lock() else {
            crate::plugins::MISSED_BLOCKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return;
        };
        for (note, velocity, on) in self.queued.drain(..) {
            let event = if on {
                MidiEvent::NoteOn { channel: MidiChannel::Ch1, note, velocity }
            } else {
                MidiEvent::NoteOff { channel: MidiChannel::Ch1, note, velocity: 0 }
            };
            let _ = plugin.send_midi_event_at(event, 0);
        }
        let n = l.len().min(r.len());
        let mut done = 0;
        while done < n {
            let k = (n - done).min(crate::fx::PLUGIN_BLOCK);
            self.buffers.block_size = k;
            if plugin.process_audio(&mut self.buffers).is_ok() {
                let outs = &self.buffers.outputs;
                let (out_l, out_r) = (&outs[0], &outs[outs.len().min(2) - 1]);
                for j in 0..k {
                    l[done + j] += if out_l[j].is_finite() { out_l[j].clamp(-4.0, 4.0) } else { 0.0 };
                    r[done + j] += if out_r[j].is_finite() { out_r[j].clamp(-4.0, 4.0) } else { 0.0 };
                }
            }
            done += k;
        }
    }
}
/// What plays a MIDI lane: a Splash instrument or a plugin.
pub enum Sound {
    Splash(Synth),
    #[cfg(windows)]
    Plugin(PluginSound),
}

impl Sound {
    fn note_on(&mut self, pitch: u8, vel: f32) {
        match self {
            Sound::Splash(s) => s.note_on(pitch, vel),
            #[cfg(windows)]
            Sound::Plugin(p) => p.send(pitch, vel, true),
        }
    }
    fn note_off(&mut self, pitch: u8) {
        match self {
            Sound::Splash(s) => s.note_off(pitch),
            #[cfg(windows)]
            Sound::Plugin(p) => p.send(pitch, 0.0, false),
        }
    }
    fn all_off(&mut self) {
        match self {
            Sound::Splash(s) => s.all_off(),
            #[cfg(windows)]
            Sound::Plugin(p) => {
                for pitch in 0..128u8 {
                    if p.held[pitch as usize] {
                        p.send(pitch, 0.0, false);
                    }
                }
            }
        }
    }
    fn render(&mut self, l: &mut [f32], r: &mut [f32]) {
        match self {
            Sound::Splash(s) => s.render(l, r),
            #[cfg(windows)]
            Sound::Plugin(p) => p.render(l, r),
        }
    }
}

/// One MIDI lane in the rack: its events, its instrument, where playback is.
pub struct MidiLane {
    pub track: usize,
    pub events: Vec<NoteEvent>,
    pub sound: Sound,
    /// The frame the last block ended at (u64::MAX: nothing played yet).
    last_end: u64,
    cursor: usize,
}

impl MidiLane {
    pub fn new(track: usize, events: Vec<NoteEvent>, sound: Sound) -> Self {
        MidiLane { track, events, sound, last_end: u64::MAX, cursor: 0 }
    }

    /// Something other than the next block follows (a seek, a loop, a pause).
    pub fn jumped(&mut self) {
        self.last_end = u64::MAX;
    }

    /// Render frames [start, start + l.len()) of this lane into l/r (overwrites).
    pub fn render(&mut self, start: u64, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        l[..n].fill(0.0);
        r[..n].fill(0.0);
        if start != self.last_end {
            // a jump: release what sounds, carry on from the new place
            self.sound.all_off();
            self.cursor = self.events.partition_point(|e| e.frame < start);
        }
        let end = start + n as u64;
        let mut at = 0usize;
        while let Some(e) = self.events.get(self.cursor).copied() {
            if e.frame >= end {
                break;
            }
            let off = (e.frame.saturating_sub(start) as usize).min(n);
            if off > at {
                self.sound.render(&mut l[at..off], &mut r[at..off]);
                at = off;
            }
            if e.on {
                self.sound.note_on(e.pitch, e.vel);
            } else {
                self.sound.note_off(e.pitch);
            }
            self.cursor += 1;
        }
        if at < n {
            self.sound.render(&mut l[at..n], &mut r[at..n]);
        }
        self.last_end = end;
    }
}

/// The built-in instruments: (name, what it is for, source).
pub const BUILTIN: [(&str, &str, &str); 4] = [
    ("bass", "bass lines", include_str!("instruments/bass.splash")),
    ("keys", "chords, pads and keys", include_str!("instruments/keys.splash")),
    ("lead", "melodies and hooks", include_str!("instruments/lead.splash")),
    ("drums", "drums (General MIDI notes)", include_str!("instruments/drums.splash")),
];

#[cfg(test)]
mod tests {
    use super::*;
    use poing_model::Fx;

    fn synth(code: &str) -> Synth {
        let shader = crate::fx::compile_instrument(code).unwrap();
        let fx = Fx { id: "i".into(), name: "i".into(), file: String::new(), params: None, bypass: None, prompt: None, plugin: None };
        Synth::new(shader.clone(), FxControls::new(&shader, &fx, None), 48000)
    }

    #[test]
    fn the_built_in_instruments_pass_the_check() {
        for (name, _, code) in BUILTIN {
            let r = aot::check(code).unwrap_or_else(|e| panic!("{name} does not compile: {e:?}"));
            assert!(r.warnings.is_empty(), "{name}: {:?}", r.warnings);
            assert!(r.ops_per_frame < 2000.0, "{name}: {} ops", r.ops_per_frame);
        }
    }

    #[test]
    fn a_note_sounds_from_its_offset_and_a_jump_releases_it() {
        let grid = BeatGrid::new(&[], &[], Some(120.0)); // a beat is 0.5 s
        let notes = [Note { start: 0.25, len: 0.5, pitch: 48, vel: 0.9 }];
        let ev = events(&notes, &grid, 48000);
        assert_eq!(ev[0].frame, 6000);
        let mut lane = MidiLane::new(0, ev, Sound::Splash(synth(BUILTIN[0].2)));
        let (mut l, mut r) = (vec![0.0f32; 8000], vec![0.0f32; 8000]);
        lane.render(0, &mut l, &mut r);
        assert!(l[..6000].iter().all(|x| *x == 0.0), "silent before the note");
        assert!(l[6200..8000].iter().any(|x| x.abs() > 0.01), "sounding after it");
        // jump back to the start (a loop): the note is released and ends
        for _ in 0..20 {
            lane.render(0, &mut l, &mut r);
        }
        let (mut l2, mut r2) = (vec![0.0f32; 4000], vec![0.0f32; 4000]);
        lane.jumped();
        lane.render(0, &mut l2, &mut r2);
        // (the 80 ms release tail fades; then silence until the note's time again)
        assert!(l2[3800..].iter().all(|x| x.abs() < 1e-2), "released on the jump: {}", l2[3800..].iter().fold(0.0f32, |m, x| m.max(x.abs())));
    }
}
