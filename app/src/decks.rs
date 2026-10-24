//! Live mode: two decks, each playing a Poing song with its stems, mixed like a
//! DJ mixer and played from a controller (the OMNIS-DUO; see docs/LIVE.md).
//!
//! The UI thread builds a [`DeckSong`] (the song's stems, its mix and the DJ
//! export's beat grid and cues) and publishes it by generation. The MIDI thread
//! turns the controller's messages into [`Decks`]' atomics and commands. The
//! audio callback's [`Player`] owns the transport: it renders each deck at its
//! speed from the song's own mixer, through its EQ, filter, fader and the
//! crossfader, then the beat FX (Splash shaders in an [`FxRack`]).

use crate::audio::{mix_into, MixParams, Stems};
use crate::deckmap::{Act, Control, Decoder, Map, Msg};
use crate::fx::{FxControls, FxRack, FxUnit};
use crossbeam_queue::ArrayQueue;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

pub const DECKS: usize = 2;
/// The tempo fader's range: ±8%.
pub const TEMPO_RANGE: f32 = 0.08;
/// Loop lengths the loop encoder steps through (beats).
const LOOP_MIN: f32 = 0.25;
const LOOP_MAX: f32 = 32.0;
/// The jog: steps a revolution, and seconds of audio a revolution (33⅓ rpm).
const JOG_STEPS: f64 = 720.0;
const JOG_SECONDS: f64 = 1.8;
/// A nudge step moves the deck this far (seconds), at most this much faster or slower.
const NUDGE_S: f64 = 0.001;
const NUDGE_MAX: f64 = 0.05;
/// Sync: phase is pulled in over this long (seconds), at most this much off tempo.
const SYNC_PULL_S: f64 = 0.25;
const SYNC_MAX: f64 = 0.08;
/// The beat FX echo's lengths (beats), stepped by beat ◀ ▶.
pub const FX_BEATS: [f32; 5] = [0.25, 0.5, 0.75, 1.0, 2.0];
pub const FX_NAMES: [&str; 2] = ["Echo", "Reverb"];
/// Furthest the scratch moves the playhead per output frame.
const MAX_RATE: f64 = 4.0;

// ------------------------------------------------------------------ the grid

/// A song's beats (seconds), from the DJ export's grid: the beat at a time and
/// the time of a (fractional) beat, going on at the end beats' length past
/// either end.
#[derive(Clone, Debug, Default)]
pub struct Grid {
    pub beats: Vec<f64>,
}

impl Grid {
    pub fn new(mut beats: Vec<f64>) -> Grid {
        beats.retain(|b| b.is_finite());
        beats.sort_by(f64::total_cmp);
        beats.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
        if beats.len() < 2 {
            beats = vec![0.0, 0.5];
        }
        Grid { beats }
    }

    /// Beat number (fractional) at t seconds.
    pub fn beat_at(&self, t: f64) -> f64 {
        let b = &self.beats;
        let n = b.len();
        if t <= b[0] {
            return (t - b[0]) / (b[1] - b[0]);
        }
        if t >= b[n - 1] {
            return (n - 1) as f64 + (t - b[n - 1]) / (b[n - 1] - b[n - 2]);
        }
        let i = b.partition_point(|x| *x <= t) - 1;
        i as f64 + (t - b[i]) / (b[i + 1] - b[i])
    }

    /// Seconds at beat number `k` (fractional).
    pub fn time_at(&self, k: f64) -> f64 {
        let b = &self.beats;
        let n = b.len();
        if k <= 0.0 {
            return b[0] + k * (b[1] - b[0]);
        }
        if k >= (n - 1) as f64 {
            return b[n - 1] + (k - (n - 1) as f64) * (b[n - 1] - b[n - 2]);
        }
        let i = k.floor() as usize;
        b[i] + (k - i as f64) * (b[i + 1] - b[i])
    }

    /// The tempo at t (bpm): from the length of the beat there.
    pub fn bpm_at(&self, t: f64) -> f64 {
        let k = self.beat_at(t).floor();
        60.0 / (self.time_at(k + 1.0) - self.time_at(k)).max(1e-3)
    }
}

/// Where a jump to `target` lands so the deck keeps its beat: the target plus
/// how far past its last beat the deck is now (when playing).
pub fn keep_phase(grid: &Grid, now: f64, target: f64) -> f64 {
    let off = grid.beat_at(now).rem_euclid(1.0);
    grid.time_at(grid.beat_at(target).round() + off)
}

/// The speed a deck plays at to follow another one: their tempos matched at
/// where each is, and the beat phase pulled together. `lead` is the other
/// deck's (grid, seconds, speed).
pub fn follow_speed(mine: &Grid, t: f64, lead: (&Grid, f64, f64)) -> f64 {
    let (grid, at, speed) = lead;
    let base = grid.bpm_at(at) * speed / mine.bpm_at(t);
    let err = (grid.beat_at(at) - mine.beat_at(t) + 0.5).rem_euclid(1.0) - 0.5;
    let beat_s = 60.0 / mine.bpm_at(t);
    base * (1.0 + (err * beat_s / SYNC_PULL_S).clamp(-SYNC_MAX, SYNC_MAX))
}

// ------------------------------------------------------------ a deck's song

/// A song on a deck, as the UI thread builds it: the lanes and the mix as the
/// song plays them, its beats and cues (the DJ export's), and which lanes are
/// which stem.
pub struct DeckSong {
    pub name: String,
    pub stems: Stems,
    pub mix: MixParams,
    pub sr: u32,
    /// Song frames.
    pub frames: usize,
    pub grid: Grid,
    pub bpm: f64,
    /// Hot cues A..D: (seconds, name).
    pub hot: [Option<(f64, String)>; 4],
    /// A cue at every section start: (seconds, name).
    pub sections: Vec<(f64, String)>,
    /// Per lane, the stem it mutes with (0..4 as `deckmap::STEMS`).
    pub role: Vec<u8>,
}

/// The stem a lane is (by its id or name); anything else is "other".
pub fn role_of(id: &str, name: &str) -> u8 {
    let s = format!("{id} {name}").to_lowercase();
    let has = |w: &[&str]| w.iter().any(|w| s.contains(w));
    if has(&["vocal", "voice", "vox", "zang"]) {
        0
    } else if has(&["drum", "perc", "beat"]) {
        1
    } else if has(&["bass"]) {
        2
    } else {
        3
    }
}

impl DeckSong {
    /// A deck's copy of the song open in `st`: its stems are shared, not copied.
    pub fn of(st: &crate::state::EditorState) -> DeckSong {
        let p = &st.project;
        let mix = MixParams::from_project(p, 0, &st.active);
        let lanes = st.stems.iter().take(p.tracks.len()).map(|s| s.l.len()).max().unwrap_or(0);
        let frames = match mix.song_take() {
            Some(k) => st.stems.get(k.stem).map_or(lanes, |s| s.l.len()),
            None => lanes,
        };
        let sr = p.sr();
        let seconds = frames as f64 / sr as f64;
        let a = p.analysis.clone().unwrap_or_default();
        let per_bar = poing_model::BeatGrid::of(p).per_bar;
        let (beats, downbeats) = (a.beats.unwrap_or_default(), a.downbeats.unwrap_or_default());
        let (grid, avg) = crate::djexport::dj_grid(&downbeats, &beats, per_bar, seconds);
        let (mut hot, mut sections) = ([None, None, None, None], Vec::new());
        let grid_beats = if grid.is_empty() {
            // no usable grid: the analysis' beats, else 120 bpm
            if beats.len() >= 2 {
                beats.clone()
            } else {
                (0..((seconds / 0.5) as usize + 2)).map(|k| k as f64 * 0.5).collect()
            }
        } else {
            for c in crate::djexport::dj_cues(p.sections(), &grid, per_bar, seconds) {
                match c.num {
                    0..=3 => hot[c.num as usize] = Some((c.start, c.name)),
                    _ => sections.push((c.start, c.name)),
                }
            }
            crate::djexport::grid_lines(&grid, per_bar, seconds).0
        };
        let grid = Grid::new(grid_beats);
        let bpm = avg.or(a.tempo).unwrap_or_else(|| grid.bpm_at(seconds * 0.5));
        DeckSong {
            name: p.name.clone(),
            stems: st.stems.clone(),
            role: p.tracks.iter().map(|t| role_of(&t.id, &t.name)).collect(),
            mix,
            sr,
            frames,
            grid,
            bpm,
            hot,
            sections,
        }
    }

    pub fn seconds(&self) -> f64 {
        self.frames as f64 / self.sr as f64
    }

    /// The section playing at t: (index, name).
    pub fn section_at(&self, t: f64) -> Option<(usize, &str)> {
        self.sections.iter().rposition(|(s, _)| *s <= t + 1e-3).map(|i| (i, self.sections[i].1.as_str()))
    }
}

// ------------------------------------------------------- shared with the UI

/// An f32 in an atomic.
pub struct AF32(AtomicU32);

impl AF32 {
    pub fn new(v: f32) -> Self {
        AF32(AtomicU32::new(v.to_bits()))
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// An f64 in an atomic.
pub struct AF64(AtomicU64);

impl AF64 {
    pub fn new(v: f64) -> Self {
        AF64(AtomicU64::new(v.to_bits()))
    }
    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub fn set(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// What the deck does once, in order (the audio thread owns the transport).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    Play,
    CueDown,
    CueUp,
    Start,
    HotCue(u8),
    Section(i8),
    /// A beat loop of this many beats (the same one again: off).
    Loop(f32),
    LoopToggle,
    LoopSize(i32),
    /// To this many seconds (the tests).
    #[allow(dead_code)]
    Seek(f64),
}

/// One deck's controls (UI and MIDI → audio) and what it is doing (audio → UI).
pub struct Deck {
    pub cmds: ArrayQueue<Cmd>,
    pub touch: AtomicBool,
    /// Jog steps not yet heard: the top while touched, and the side.
    pub scratch: AtomicI32,
    pub nudge: AtomicI32,
    /// The tempo fader, 0..1 (0.5 is the middle).
    pub tempo: AF32,
    pub sync: AtomicBool,
    /// Bit per stem (deckmap::STEMS): muted.
    pub mutes: AtomicU8,
    pub trim: AF32,
    pub eq_high: AF32,
    pub eq_mid: AF32,
    pub eq_low: AF32,
    pub color: AF32,
    pub fader: AF32,
    /// The hardware Load button: the UI loads the song in front.
    pub load: AtomicBool,
    // what it is doing
    pub pos: AF64,
    pub playing: AtomicBool,
    /// Playback speed (1 = as recorded).
    pub speed: AF64,
    pub beat: AF64,
    pub loop_on: AtomicBool,
    pub loop_beats: AF32,
    pub cue: AF64,
    pub meter: AF32,
    /// The song: published by generation; the UI keeps the one it replaced
    /// until `seen` shows the callback moved on.
    pub song: Mutex<Option<Arc<DeckSong>>>,
    pub gen: AtomicU64,
    pub seen: AtomicU64,
}

impl Default for Deck {
    fn default() -> Self {
        Deck {
            cmds: ArrayQueue::new(256),
            touch: AtomicBool::new(false),
            scratch: AtomicI32::new(0),
            nudge: AtomicI32::new(0),
            tempo: AF32::new(0.5),
            sync: AtomicBool::new(false),
            mutes: AtomicU8::new(0),
            trim: AF32::new(0.5),
            eq_high: AF32::new(0.5),
            eq_mid: AF32::new(0.5),
            eq_low: AF32::new(0.5),
            color: AF32::new(0.5),
            fader: AF32::new(1.0),
            load: AtomicBool::new(false),
            pos: AF64::new(0.0),
            playing: AtomicBool::new(false),
            speed: AF64::new(1.0),
            beat: AF64::new(0.0),
            loop_on: AtomicBool::new(false),
            loop_beats: AF32::new(4.0),
            cue: AF64::new(0.0),
            meter: AF32::new(0.0),
            song: Mutex::new(None),
            gen: AtomicU64::new(0),
            seen: AtomicU64::new(0),
        }
    }
}

impl Deck {
    pub fn send(&self, c: Cmd) {
        let _ = self.cmds.push(c);
    }
    pub fn muted(&self, stem: usize) -> bool {
        self.mutes.load(Ordering::Relaxed) & (1 << stem) != 0
    }
}

/// Port ids the controller is on (inputs and outputs), for the MIDI thread.
const PORTS: usize = 8;

/// Live mode's shared state.
pub struct Decks {
    /// Live mode is on: the callback plays the decks instead of the song.
    pub on: AtomicBool,
    pub deck: [Deck; DECKS],
    pub xfader: AF32,
    pub fx_on: AtomicBool,
    /// 0 echo, 1 reverb.
    pub fx_kind: AtomicU8,
    pub fx_depth: AF32,
    /// Index into FX_BEATS.
    pub fx_beats: AtomicU8,
    /// The beat FX rack, handed to the callback once.
    pub fx: Mutex<Option<Box<DjFx>>>,
    /// The tempo the decks play at (the leading deck's), for the UI.
    pub bpm: AF64,
    pub ports_in: [AtomicU64; PORTS],
    pub ports_out: [AtomicU64; PORTS],
    /// Messages from the controller that the map does not know, for the log.
    pub unmapped: ArrayQueue<[u8; 3]>,
    /// Bumped by every control the MIDI thread handles (the UI redraws).
    pub touched: AtomicU64,
}

impl Default for Decks {
    fn default() -> Self {
        Decks {
            on: AtomicBool::new(false),
            deck: [Deck::default(), Deck::default()],
            xfader: AF32::new(0.5),
            fx_on: AtomicBool::new(false),
            fx_kind: AtomicU8::new(0),
            fx_depth: AF32::new(0.5),
            fx_beats: AtomicU8::new(2),
            fx: Mutex::new(None),
            bpm: AF64::new(0.0),
            ports_in: std::array::from_fn(|_| AtomicU64::new(0)),
            ports_out: std::array::from_fn(|_| AtomicU64::new(0)),
            unmapped: ArrayQueue::new(64),
            touched: AtomicU64::new(0),
        }
    }
}

fn set_ports(slots: &[AtomicU64; PORTS], ids: &[u64]) {
    for (i, s) in slots.iter().enumerate() {
        s.store(ids.get(i).copied().unwrap_or(0), Ordering::Relaxed);
    }
}

impl Decks {
    pub fn on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    /// The controller's ports (ids as the MIDI input reports them).
    pub fn set_ports(&self, inputs: &[u64], outputs: &[u64]) {
        set_ports(&self.ports_in, inputs);
        set_ports(&self.ports_out, outputs);
    }
    pub fn is_port(&self, id: u64) -> bool {
        id != 0 && self.ports_in.iter().any(|p| p.load(Ordering::Relaxed) == id)
    }
    pub fn outputs(&self) -> impl Iterator<Item = u64> + '_ {
        self.ports_out.iter().map(|p| p.load(Ordering::Relaxed)).filter(|p| *p != 0)
    }

    /// The song on deck `d` (the UI's reference).
    pub fn song(&self, d: usize) -> Option<Arc<DeckSong>> {
        self.deck[d].song.lock().ok()?.clone()
    }

    /// Put a song on deck `d`; returns the one it replaces, which the caller
    /// keeps until [`Decks::retired`] says the callback let go of it.
    pub fn load(&self, d: usize, song: Arc<DeckSong>) -> Option<Arc<DeckSong>> {
        let deck = &self.deck[d];
        let old = deck.song.lock().ok().and_then(|mut s| s.replace(song));
        deck.gen.fetch_add(1, Ordering::AcqRel);
        deck.mutes.store(0, Ordering::Relaxed);
        old
    }

    /// Has the callback moved past every song published before now?
    pub fn retired(&self) -> bool {
        self.deck.iter().all(|d| d.seen.load(Ordering::Acquire) == d.gen.load(Ordering::Acquire))
    }

    /// A controller message, decoded by `map` (on the MIDI thread).
    pub fn midi(&self, map: &Map, dec: &mut Decoder, d: [u8; 3]) {
        match map.decode(dec, d) {
            Some((c, msg)) => {
                self.control(c, msg);
                self.touched.fetch_add(1, Ordering::Relaxed);
            }
            None => {
                // the meter and the ignored controls are known; only note
                // and CC messages are news (not pressure or clock)
                let status = if d[0] & 0xf0 == 0x80 { d[0] | 0x10 } else { d[0] };
                let known = map.controls.iter().any(|c| c.status == status && c.data1 == d[1]);
                if !known && matches!(d[0] & 0xf0, 0x80 | 0x90 | 0xb0) {
                    let _ = self.unmapped.push(d);
                }
            }
        }
    }

    pub fn control(&self, c: &Control, msg: Msg) {
        let deck = &self.deck[c.deck.min(DECKS - 1)];
        let down = msg == Msg::Down;
        let value = match msg {
            Msg::Value(v) => v,
            _ => 0.0,
        };
        let steps = match msg {
            Msg::Steps(s) => s,
            _ => 0,
        };
        match c.act {
            Act::Play if down => deck.send(Cmd::Play),
            Act::Cue => deck.send(if down { Cmd::CueDown } else { Cmd::CueUp }),
            Act::Start if down => deck.send(Cmd::Start),
            Act::Sync if down => {
                let on = !deck.sync.load(Ordering::Relaxed);
                deck.sync.store(on, Ordering::Relaxed);
                if on {
                    // one deck leads
                    self.deck[1 - c.deck.min(1)].sync.store(false, Ordering::Relaxed);
                }
            }
            Act::Load if down => deck.load.store(true, Ordering::Relaxed),
            Act::HotCue(n) if down => deck.send(Cmd::HotCue(n - 1)),
            Act::Loop(b) if down => deck.send(Cmd::Loop(b)),
            Act::Mute(s) if down => {
                deck.mutes.fetch_xor(1 << s, Ordering::Relaxed);
            }
            Act::Section(dir) if down => deck.send(Cmd::Section(dir)),
            Act::LoopToggle if down => deck.send(Cmd::LoopToggle),
            Act::LoopSize if steps != 0 => deck.send(Cmd::LoopSize(steps)),
            Act::Touch => deck.touch.store(down, Ordering::Relaxed),
            Act::Scratch => {
                // the top turned while not touched (CDJ mode) bends like the side
                let to = if deck.touch.load(Ordering::Relaxed) { &deck.scratch } else { &deck.nudge };
                to.fetch_add(steps, Ordering::Relaxed);
            }
            Act::Nudge => {
                deck.nudge.fetch_add(steps, Ordering::Relaxed);
            }
            Act::Tempo => deck.tempo.set(value),
            Act::Trim => deck.trim.set(value),
            Act::EqHigh => deck.eq_high.set(value),
            Act::EqMid => deck.eq_mid.set(value),
            Act::EqLow => deck.eq_low.set(value),
            Act::Color => deck.color.set(value),
            Act::Fader => deck.fader.set(value),
            Act::XFader => self.xfader.set(value),
            Act::FxOn if down => {
                self.fx_on.fetch_xor(true, Ordering::Relaxed);
            }
            Act::FxSelect if down => {
                let k = self.fx_kind.load(Ordering::Relaxed);
                self.fx_kind.store((k + 1) % FX_NAMES.len() as u8, Ordering::Relaxed);
            }
            Act::FxDepth => self.fx_depth.set(value),
            Act::FxBeats(dir) if down => {
                let i = self.fx_beats.load(Ordering::Relaxed) as i32 + dir as i32;
                self.fx_beats.store(i.clamp(0, FX_BEATS.len() as i32 - 1) as u8, Ordering::Relaxed);
            }
            _ => {}
        }
    }
}

// --------------------------------------------------------------- LED writer

/// What the controller's lights last showed, so only changes are sent.
pub struct Leds {
    sent: std::collections::HashMap<(u8, u8), u8>,
}

impl Default for Leds {
    fn default() -> Self {
        Leds { sent: std::collections::HashMap::new() }
    }
}

impl Leds {
    /// The messages that bring the lights up to date: play, cue, hot cues,
    /// stems, loops, sync, load, the beat FX button and the level meters.
    pub fn update(&mut self, map: &Map, decks: &Decks) -> Vec<[u8; 3]> {
        let mut want: Vec<(&Control, u8)> = Vec::new();
        let lit = |on: bool| if on { 0x7f } else { 0x00 };
        for (d, deck) in decks.deck.iter().enumerate() {
            let song = deck.song.lock().ok().and_then(|s| s.clone());
            let playing = deck.playing.load(Ordering::Relaxed);
            let loop_on = deck.loop_on.load(Ordering::Relaxed);
            let loop_beats = deck.loop_beats.get();
            for c in map.controls.iter().filter(|c| c.deck == d && c.led) {
                let v = match c.act {
                    Act::Play => lit(playing),
                    Act::Cue => lit(song.is_some() && !playing && (deck.pos.get() - deck.cue.get()).abs() < 0.005),
                    Act::Load => lit(song.is_some()),
                    Act::Sync => lit(deck.sync.load(Ordering::Relaxed)),
                    Act::HotCue(n) => lit(song.as_ref().is_some_and(|s| s.hot[n as usize - 1].is_some())),
                    Act::Mute(s) => lit(song.is_some() && !deck.muted(s as usize)),
                    Act::Loop(b) => lit(loop_on && (loop_beats - b).abs() < 1e-3),
                    Act::LoopToggle => lit(loop_on),
                    Act::Level => (deck.meter.get().sqrt() * 127.0).clamp(0.0, 127.0) as u8,
                    _ => continue,
                };
                want.push((c, v));
            }
        }
        if let Some(c) = map.led(0, Act::FxOn) {
            want.push((c, lit(decks.fx_on.load(Ordering::Relaxed))));
        }
        let mut out = Vec::new();
        for (c, v) in want {
            if self.sent.insert((c.status, c.data1), v) != Some(v) {
                out.push([c.status, c.data1, v]);
            }
        }
        out
    }

    /// Forget what was sent (everything is sent again).
    pub fn reset(&mut self) {
        self.sent.clear();
    }
}

// ---------------------------------------------------------------- the DSP

/// A biquad (RBJ cookbook), stereo.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [[f32; 2]; 2],
}

impl Biquad {
    fn set(&mut self, sr: f32, fc: f32, q: f32, high: bool) {
        let w = std::f32::consts::TAU * (fc / sr).clamp(1e-4, 0.49);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1) = if high { ((1.0 + c) / 2.0, -(1.0 + c)) } else { ((1.0 - c) / 2.0, 1.0 - c) };
        self.b = [b0 / a0, b1 / a0, b0 / a0];
        self.a = [-2.0 * c / a0, (1.0 - alpha) / a0];
    }
    #[inline]
    fn run(&mut self, ch: usize, x: f32) -> f32 {
        let z = &mut self.z[ch];
        let y = self.b[0] * x + z[0];
        z[0] = self.b[1] * x - self.a[0] * y + z[1];
        z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
    fn clear(&mut self) {
        self.z = [[0.0; 2]; 2];
    }
    /// A tail this small is gone: zero it before it turns denormal (slow on x86).
    fn flush(&mut self) {
        for z in self.z.iter_mut().flatten() {
            if z.abs() < 1e-15 {
                *z = 0.0;
            }
        }
    }
}

/// A deck's channel strip: a 3-band isolator EQ (Linkwitz-Riley crossovers at
/// 250 Hz and 2.5 kHz, the low band through the upper crossover's allpass so
/// the bands add up flat) and the colour filter.
struct Strip {
    sr: f32,
    /// LR4 sections (two Butterworth biquads each): low, above low; mid,
    /// high (from above low); the low band's allpass (its low and high part).
    lr: [[Biquad; 2]; 6],
    filter: Biquad,
    /// The filter's knob as last set (-1..1, 0 off).
    color: f32,
    gains: [f32; 3],
}

/// A knob 0..1 to a band gain: 0 kills, the middle is flat, the top +6 dB.
fn eq_gain(v: f32) -> f32 {
    if v <= 0.5 {
        (v * 2.0).powi(2)
    } else {
        1.0 + (v - 0.5) * 2.0 * 0.995
    }
}

impl Strip {
    fn new(sr: u32) -> Strip {
        let sr = sr as f32;
        let mut s = Strip { sr, lr: [[Biquad::default(); 2]; 6], filter: Biquad::default(), color: 0.0, gains: [1.0; 3] };
        let q = std::f32::consts::FRAC_1_SQRT_2;
        for (i, (fc, high)) in [(250.0, false), (250.0, true), (2500.0, false), (2500.0, true), (2500.0, false), (2500.0, true)].into_iter().enumerate() {
            for b in &mut s.lr[i] {
                b.set(sr, fc, q, high);
            }
        }
        s
    }

    fn clear(&mut self) {
        self.lr.iter_mut().flatten().for_each(|b| b.clear());
        self.filter.clear();
    }

    fn set(&mut self, deck: &Deck) {
        self.gains = [eq_gain(deck.eq_low.get()), eq_gain(deck.eq_mid.get()), eq_gain(deck.eq_high.get())];
        let c = (deck.color.get() * 2.0 - 1.0).clamp(-1.0, 1.0);
        let c = if c.abs() < 0.04 { 0.0 } else { c };
        if c != self.color {
            if (c < 0.0) != (self.color < 0.0) || c == 0.0 {
                self.filter.clear();
            }
            if c < 0.0 {
                self.filter.set(self.sr, 20000.0 * (80.0f32 / 20000.0).powf(-c), 0.9, false);
            } else if c > 0.0 {
                self.filter.set(self.sr, 20.0 * (6000.0f32 / 20.0).powf(c), 0.9, true);
            }
            self.color = c;
        }
    }

    fn run(&mut self, l: &mut [f32], r: &mut [f32]) {
        let [gl, gm, gh] = self.gains;
        for (ch, buf) in [l, r].into_iter().enumerate() {
            for x in buf.iter_mut() {
                let mut lr4 = |i: usize, v: f32| {
                    let s = &mut self.lr[i];
                    let y = s[0].run(ch, v);
                    s[1].run(ch, y)
                };
                let low = lr4(0, *x);
                let rest = lr4(1, *x);
                let mid = lr4(2, rest);
                let high = lr4(3, rest);
                let low = lr4(4, low) + lr4(5, low);
                let mut y = gl * low + gm * mid + gh * high;
                if self.color != 0.0 {
                    y = self.filter.run(ch, y);
                }
                *x = y;
            }
        }
        self.lr.iter_mut().flatten().for_each(|b| b.flush());
        self.filter.flush();
    }
}

/// The beat FX: a Splash echo and reverb on the master, and where their knobs are.
pub struct DjFx {
    pub rack: Box<FxRack>,
    /// Echo's bpm, beats, mix and on.
    echo: [usize; 4],
    reverb_mix: usize,
}

/// Build the beat FX (on the UI thread: shaders compile and buffers are made here).
pub fn build_fx(sr: u32) -> Result<DjFx, String> {
    let unit = |id: &str, code: &str| -> Result<(FxUnit, Vec<String>), String> {
        let shader = crate::fx::compile_source(code).map_err(|e| format!("{id}: {e}"))?;
        let names = shader.params().iter().map(|p| p.name.to_string()).collect();
        let fx = poing_model::Fx { id: id.into(), name: id.into(), file: String::new(), params: None, bypass: None, prompt: None, plugin: None, isolate: None };
        let controls = FxControls::new(&shader, &fx, None);
        Ok((FxUnit::new(id, shader, controls, sr), names))
    };
    let (echo, echo_names) = unit("live-echo", include_str!("../live/echo.splash"))?;
    let (reverb, reverb_names) = unit("live-reverb", include_str!("fx_examples/allpass_reverb.splash"))?;
    let at = |names: &[String], n: &str| names.iter().position(|x| x == n).ok_or_else(|| format!("no `{n}` knob"));
    let ix = [at(&echo_names, "bpm")?, at(&echo_names, "beats")?, at(&echo_names, "mix")?, at(&echo_names, "on")?];
    let reverb_mix = at(&reverb_names, "mix")?;
    reverb.controls.bypass.store(true, Ordering::Relaxed);
    Ok(DjFx { rack: Box::new(FxRack::new(Vec::new(), vec![echo, reverb])), echo: ix, reverb_mix })
}

impl DjFx {
    fn run(&mut self, decks: &Decks, bpm: f64, l: &mut [f32], r: &mut [f32]) {
        let on = decks.fx_on.load(Ordering::Relaxed);
        let kind = decks.fx_kind.load(Ordering::Relaxed);
        let depth = decks.fx_depth.get();
        let beats = FX_BEATS[(decks.fx_beats.load(Ordering::Relaxed) as usize).min(FX_BEATS.len() - 1)];
        let echo = &self.rack.master[0].controls;
        echo.set(self.echo[0], bpm.clamp(60.0, 200.0) as f32);
        echo.set(self.echo[1], beats);
        echo.set(self.echo[2], depth);
        echo.set(self.echo[3], if on && kind == 0 { 1.0 } else { 0.0 });
        let reverb = &self.rack.master[1].controls;
        reverb.set(self.reverb_mix, depth);
        reverb.bypass.store(!(on && kind == 1), Ordering::Relaxed);
        self.rack.process_master(l, r);
    }
}

// --------------------------------------------------------------- the player

/// One deck on the audio thread: its song, transport and strip.
struct DeckPlayer {
    song: Option<Arc<DeckSong>>,
    gen: u64,
    /// Song frames (fractional).
    pos: f64,
    playing: bool,
    /// Playing while Cue is held (CDJ preview).
    previewing: bool,
    cue: f64,
    loop_on: bool,
    loop_beats: f32,
    /// The loop in beats: [a, a + loop_beats).
    loop_a: f64,
    scratch: f64,
    nudge: f64,
    /// The mix as heard (its lane gains with the mutes), and one to change to.
    mix: MixParams,
    next: MixParams,
    mutes: u8,
    speed: f64,
    strip: Strip,
    win_l: Vec<f32>,
    win_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    alt_l: Vec<f32>,
    alt_r: Vec<f32>,
}

/// Frames a block of the player holds (device blocks are cut to this).
const BLOCK: usize = 4096;

fn reserved_mix() -> MixParams {
    let mut m = MixParams::default();
    m.gains.reserve(64);
    m.regions.reserve(256);
    m
}

impl DeckPlayer {
    fn new(sr: u32) -> DeckPlayer {
        let win = (BLOCK as f64 * MAX_RATE) as usize + 8;
        DeckPlayer {
            song: None,
            gen: 0,
            pos: 0.0,
            playing: false,
            previewing: false,
            cue: 0.0,
            loop_on: false,
            loop_beats: 4.0,
            loop_a: 0.0,
            scratch: 0.0,
            nudge: 0.0,
            mix: reserved_mix(),
            next: reserved_mix(),
            mutes: 0,
            speed: 1.0,
            strip: Strip::new(sr),
            win_l: vec![0.0; win],
            win_r: vec![0.0; win],
            out_l: vec![0.0; BLOCK],
            out_r: vec![0.0; BLOCK],
            alt_l: vec![0.0; BLOCK],
            alt_r: vec![0.0; BLOCK],
        }
    }

    fn t(&self) -> f64 {
        self.song.as_ref().map_or(0.0, |s| self.pos / s.sr as f64)
    }

    /// Adopt a newly published song (it starts at its first hot cue, stopped).
    fn adopt(&mut self, deck: &Deck) {
        let gen = deck.gen.load(Ordering::Acquire);
        if gen == self.gen {
            return;
        }
        let Ok(slot) = deck.song.try_lock() else { return };
        // the UI holds the one this replaces until `seen` moves: never its last drop here
        self.song = slot.clone();
        drop(slot);
        self.gen = gen;
        deck.seen.store(gen, Ordering::Release);
        self.playing = false;
        self.previewing = false;
        self.loop_on = false;
        self.scratch = 0.0;
        self.nudge = 0.0;
        if let Some(s) = &self.song {
            let start = s.hot[0].as_ref().map_or(0.0, |h| h.0);
            self.pos = start * s.sr as f64;
            self.cue = self.pos;
            self.mix.clone_from(&s.mix);
            self.mutes = 0;
        }
        self.strip.clear();
    }

    /// The deck's gains with these stems muted, into `mix`.
    fn apply_mutes(song: &DeckSong, mutes: u8, mix: &mut MixParams) {
        for (i, g) in mix.gains.iter_mut().enumerate() {
            let base = song.mix.gains.get(i).copied().unwrap_or(0.0);
            let role = song.role.get(i).copied().unwrap_or(3);
            *g = if mutes & (1 << role) != 0 { 0.0 } else { base };
        }
    }

    fn command(&mut self, c: Cmd) {
        let Some(song) = self.song.clone() else { return };
        let sr = song.sr as f64;
        let g = &song.grid;
        let t = self.pos / sr;
        let jump = |me: &mut Self, target: f64| {
            let to = if me.playing { keep_phase(g, t, target) } else { target };
            me.pos = to.clamp(0.0, song.seconds()) * sr;
            if me.loop_on {
                // a loop moves with the jump, still on the beat
                me.loop_a = g.beat_at(me.pos / sr).floor();
            }
        };
        match c {
            Cmd::Play => {
                self.previewing = false;
                self.playing = !self.playing;
            }
            Cmd::CueDown => {
                if self.playing && !self.previewing {
                    self.pos = self.cue;
                    self.playing = false;
                } else if (self.pos - self.cue).abs() < 0.005 * sr {
                    self.playing = true;
                    self.previewing = true;
                } else {
                    self.cue = g.time_at(g.beat_at(t).round()).max(0.0) * sr;
                    self.pos = self.cue;
                }
            }
            Cmd::CueUp => {
                if self.previewing {
                    self.previewing = false;
                    self.playing = false;
                    self.pos = self.cue;
                }
            }
            Cmd::Start => {
                self.pos = 0.0;
                self.cue = 0.0;
            }
            Cmd::HotCue(n) => {
                if let Some((at, _)) = song.hot.get(n as usize).and_then(|h| h.as_ref()) {
                    jump(self, *at);
                    self.cue = (*at * sr).round();
                    // a hot cue starts the deck, as on a CDJ
                    self.playing = true;
                    self.previewing = false;
                }
            }
            Cmd::Section(dir) => {
                let now = song.section_at(t).map(|(i, _)| i);
                let target = match (now, dir) {
                    // back: to this section's start, or the one before when just past it
                    (Some(i), -1) => {
                        let i = if t - song.sections[i].0 < 1.0 { i.saturating_sub(1) } else { i };
                        song.sections.get(i)
                    }
                    (Some(i), _) => song.sections.get(i + 1),
                    (None, _) => song.sections.first(),
                };
                if let Some(&(at, _)) = target {
                    jump(self, at);
                }
            }
            Cmd::Loop(beats) => {
                if self.loop_on && (self.loop_beats - beats).abs() < 1e-3 {
                    self.loop_on = false;
                } else {
                    self.start_loop(g, t, beats);
                }
            }
            Cmd::LoopToggle => {
                if self.loop_on {
                    self.loop_on = false;
                } else {
                    self.start_loop(g, t, self.loop_beats);
                }
            }
            Cmd::LoopSize(steps) => {
                let k = if steps > 0 { 2.0 } else { 0.5 };
                self.loop_beats = (self.loop_beats * k).clamp(LOOP_MIN, LOOP_MAX);
            }
            Cmd::Seek(s) => {
                self.pos = s.clamp(0.0, song.seconds()) * sr;
                self.loop_on = false;
            }
        }
    }

    /// A loop of `beats` from the beat (or the part of one) the deck is in.
    fn start_loop(&mut self, g: &Grid, t: f64, beats: f32) {
        let q = (beats as f64).min(1.0);
        self.loop_beats = beats;
        self.loop_a = (g.beat_at(t) / q).floor() * q;
        self.loop_on = true;
    }

    /// Render `n` frames (at the output rate) of the deck at `speed`, into out_l/out_r.
    fn render(&mut self, deck: &Deck, speed: f64, live_sr: u32, n: usize) {
        self.out_l[..n].fill(0.0);
        self.out_r[..n].fill(0.0);
        let Some(song) = self.song.clone() else { return };
        let sr = song.sr as f64;
        // frames of the song a frame of the output moves
        let to_song = sr / live_sr as f64;
        let mut rate = if deck.touch.load(Ordering::Relaxed) {
            // the hand on the platter drives it: it glides to where the jog is
            self.scratch += deck.scratch.swap(0, Ordering::Relaxed) as f64 * JOG_SECONDS / JOG_STEPS * sr;
            let most = MAX_RATE * n as f64;
            let mv = if self.scratch.abs() < 0.5 { self.scratch } else { self.scratch * 0.35 }.clamp(-most, most);
            self.scratch -= mv;
            mv / n as f64
        } else {
            self.scratch = 0.0;
            if self.playing {
                speed * to_song
            } else {
                0.0
            }
        };
        // a nudge: a little faster or slower until the steps are made up
        self.nudge += deck.nudge.swap(0, Ordering::Relaxed) as f64 * NUDGE_S * sr;
        if self.playing && self.nudge != 0.0 {
            let most = NUDGE_MAX * to_song * n as f64;
            let mv = self.nudge.clamp(-most, most);
            self.nudge -= mv;
            rate += mv / n as f64;
        } else if !self.playing {
            self.nudge = 0.0;
        }
        rate = rate.clamp(-MAX_RATE, MAX_RATE);
        // the stems as muted now; a change crossfades over this block
        let mutes = deck.mutes.load(Ordering::Relaxed);
        let fade = mutes != self.mutes;
        if fade {
            self.next.clone_from(&self.mix);
            Self::apply_mutes(&song, mutes, &mut self.next);
        }
        let g = &song.grid;
        // a stopped (or held) record is silent, not its last sample held
        let mut done = if rate == 0.0 { n } else { 0 };
        while done < n {
            // up to the loop's end, then on from its start
            let mut k = n - done;
            let looping = self.loop_on && rate > 0.0;
            let (la, lb) = (g.time_at(self.loop_a) * sr, g.time_at(self.loop_a + self.loop_beats as f64) * sr);
            if looping {
                if self.pos >= lb || self.pos < la - 1.0 {
                    self.pos = la + (self.pos - la).rem_euclid((lb - la).max(1.0));
                }
                k = k.min(((lb - self.pos) / rate).ceil().max(1.0) as usize);
            }
            self.segment(&song, rate, done, k, fade, n);
            self.pos += rate * k as f64;
            if looping && self.pos >= lb {
                self.pos -= lb - la;
            }
            done += k;
        }
        if fade {
            std::mem::swap(&mut self.mix, &mut self.next);
            self.mutes = mutes;
        }
        if self.pos < 0.0 {
            self.pos = 0.0;
        }
        if self.pos >= song.frames as f64 {
            self.pos = song.frames as f64;
            self.playing = false;
        }
        self.speed = if self.playing { rate / to_song } else { 0.0 };
        self.strip.set(deck);
        self.strip.run(&mut self.out_l[..n], &mut self.out_r[..n]);
    }

    /// Frames [at, at + k) of the block: the song from `pos` on at `rate`,
    /// rendered by the song's own mixer over the source window they need and
    /// read between its frames. `fade`: crossfade from `mix` to `next` over the
    /// whole block of `n`.
    fn segment(&mut self, song: &DeckSong, rate: f64, at: usize, k: usize, fade: bool, n: usize) {
        let end = self.pos + rate * (k.max(1) - 1) as f64;
        let lo = self.pos.min(end).floor().max(0.0);
        let len = ((self.pos.max(end).ceil() - lo) as usize + 3).min(self.win_l.len());
        let start = lo as usize;
        let mixes: &[&MixParams] = if fade { &[&self.mix, &self.next] } else { &[&self.mix] };
        for (m, mix) in mixes.iter().enumerate() {
            mix_into(&song.stems, mix, song.sr, start, &mut self.win_l[..len], &mut self.win_r[..len], None);
            let (ol, or) = if m == 0 { (&mut self.out_l, &mut self.out_r) } else { (&mut self.alt_l, &mut self.alt_r) };
            for i in 0..k {
                let p = self.pos + rate * i as f64;
                if p < 0.0 {
                    continue;
                }
                let j = (p - lo) as usize;
                let f = (p - lo - j as f64) as f32;
                let (j, j1) = (j.min(len - 1), (j + 1).min(len - 1));
                ol[at + i] = self.win_l[j] + (self.win_l[j1] - self.win_l[j]) * f;
                or[at + i] = self.win_r[j] + (self.win_r[j1] - self.win_r[j]) * f;
            }
        }
        if fade {
            for i in at..at + k {
                let w = (i as f32 + 0.5) / n as f32;
                self.out_l[i] += (self.alt_l[i] - self.out_l[i]) * w;
                self.out_r[i] += (self.alt_r[i] - self.out_r[i]) * w;
            }
        }
    }

    fn publish(&self, deck: &Deck) {
        let t = self.t();
        deck.pos.set(t);
        deck.playing.store(self.playing, Ordering::Relaxed);
        deck.speed.set(self.speed);
        deck.beat.set(self.song.as_ref().map_or(0.0, |s| s.grid.beat_at(t)));
        deck.loop_on.store(self.loop_on, Ordering::Relaxed);
        deck.loop_beats.set(self.loop_beats);
        deck.cue.set(self.song.as_ref().map_or(0.0, |s| self.cue / s.sr as f64));
    }
}

/// The decks on the audio thread (made on the UI thread, then moved into the callback).
pub struct Player {
    sr: u32,
    decks: [DeckPlayer; DECKS],
    fx: Option<Box<DjFx>>,
}

impl Player {
    pub fn new(sr: u32) -> Player {
        Player { sr, decks: [DeckPlayer::new(sr), DeckPlayer::new(sr)], fx: None }
    }

    /// Frames [0, n) of the live mix into l/r (overwrites).
    pub fn render(&mut self, shared: &Decks, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        if self.fx.is_none() {
            if let Some(fx) = shared.fx.try_lock().ok().and_then(|mut s| s.take()) {
                self.fx = Some(fx);
            }
        }
        let mut done = 0;
        while done < n {
            let k = (n - done).min(BLOCK);
            self.block(shared, &mut l[done..done + k], &mut r[done..done + k]);
            done += k;
        }
    }

    /// The speed each deck plays at: its tempo fader, or following the other.
    fn speeds(&self, shared: &Decks) -> ([f64; DECKS], f64) {
        let own = |d: usize| 1.0 + (shared.deck[d].tempo.get() as f64 - 0.5) * 2.0 * TEMPO_RANGE as f64;
        let mut speed = [own(0), own(1)];
        for d in 0..DECKS {
            let o = 1 - d;
            let (me, lead) = (&self.decks[d], &self.decks[o]);
            if !shared.deck[d].sync.load(Ordering::Relaxed) {
                continue;
            }
            let (Some(mine), Some(theirs)) = (&me.song, &lead.song) else { continue };
            let lead_speed = own(o);
            // a deck being scratched does not lead: the follower holds its tempo
            let leading = lead.playing && !shared.deck[o].touch.load(Ordering::Relaxed);
            speed[d] = if leading && me.playing {
                follow_speed(&mine.grid, me.t(), (&theirs.grid, lead.t(), lead_speed))
            } else {
                // stopped: the tempo matched, the phase is pulled in once both play
                theirs.grid.bpm_at(lead.t()) * lead_speed / mine.grid.bpm_at(me.t())
            };
        }
        // the tempo heard: the leading deck's (a playing one, not synced first)
        let bpm_of = |d: usize| self.decks[d].song.as_ref().map(|s| s.grid.bpm_at(self.decks[d].t()) * speed[d]);
        let lead = (0..DECKS)
            .filter(|d| self.decks[*d].playing && self.decks[*d].song.is_some())
            .min_by_key(|d| shared.deck[*d].sync.load(Ordering::Relaxed))
            .or((0..DECKS).find(|d| self.decks[*d].song.is_some()));
        (speed, lead.and_then(bpm_of).unwrap_or(120.0))
    }

    fn block(&mut self, shared: &Decks, l: &mut [f32], r: &mut [f32]) {
        let n = l.len();
        for (p, d) in self.decks.iter_mut().zip(&shared.deck) {
            p.adopt(d);
            while let Some(c) = d.cmds.pop() {
                p.command(c);
            }
        }
        let (speed, bpm) = self.speeds(shared);
        shared.bpm.set(bpm);
        let x = shared.xfader.get().clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
        let cross = [x.cos(), x.sin()];
        l.fill(0.0);
        r.fill(0.0);
        for d in 0..DECKS {
            let deck = &shared.deck[d];
            let p = &mut self.decks[d];
            p.render(deck, speed[d], self.sr, n);
            let trim = 10f32.powf((deck.trim.get() - 0.5) * 24.0 / 20.0);
            let fader = deck.fader.get().clamp(0.0, 1.0).powi(2);
            let gain = trim * fader * cross[d];
            let mut peak = 0.0f32;
            for i in 0..n {
                let (a, b) = (p.out_l[i] * gain, p.out_r[i] * gain);
                l[i] += a;
                r[i] += b;
                peak = peak.max(a.abs()).max(b.abs());
            }
            // the meter falls back slowly; the UI and the LEDs read it
            deck.meter.set(peak.max(deck.meter.get() * 0.9));
            p.publish(deck);
        }
        if let Some(fx) = self.fx.as_mut() {
            fx.run(shared, bpm, l, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::Stem;

    /// A song of `beats` at 120 bpm (0.5 s beats) whose four lanes are DC at
    /// 0.1, 0.2, 0.4 and 0.8: vocals, drums, bass and other.
    fn song(sr: u32, seconds: f64) -> Arc<DeckSong> {
        let frames = (seconds * sr as f64) as usize;
        let lane = |v: f32| Arc::new(Stem::new(vec![v; frames], vec![v; frames]));
        let stems: Stems = Arc::new(vec![lane(0.1), lane(0.2), lane(0.4), lane(0.8)]);
        let mix = MixParams { version: 1, gains: vec![1.0; 4], ..Default::default() };
        let beats: Vec<f64> = (0..(seconds * 2.0) as usize + 1).map(|k| k as f64 * 0.5).collect();
        Arc::new(DeckSong {
            name: "test".into(),
            stems,
            mix,
            sr,
            frames,
            grid: Grid::new(beats),
            bpm: 120.0,
            hot: [Some((0.0, "Intro".into())), Some((8.0, "Chorus 1".into())), Some((16.0, "Chorus 2".into())), None],
            sections: vec![(0.0, "Intro".into()), (8.0, "Chorus 1".into()), (16.0, "Chorus 2".into())],
            role: vec![0, 1, 2, 3],
        })
    }

    fn run(p: &mut Player, d: &Decks, seconds: f64) -> (Vec<f32>, Vec<f32>) {
        let n = (seconds * p.sr as f64) as usize;
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        for i in (0..n).step_by(512) {
            let k = (n - i).min(512);
            p.render(d, &mut l[i..i + k], &mut r[i..i + k]);
        }
        (l, r)
    }

    #[test]
    fn the_grid_counts_beats_and_goes_on_past_its_ends() {
        let g = Grid::new(vec![1.0, 1.5, 2.0, 3.0]);
        assert_eq!(g.beat_at(1.25), 0.5);
        assert_eq!(g.beat_at(2.5), 2.5);
        assert_eq!(g.beat_at(4.0), 4.0, "the last beat's length goes on");
        assert_eq!(g.beat_at(0.5), -1.0);
        assert_eq!(g.time_at(2.5), 2.5);
        assert_eq!(g.time_at(-2.0), 0.0);
        assert!((g.bpm_at(1.2) - 120.0).abs() < 1e-9 && (g.bpm_at(2.2) - 60.0).abs() < 1e-9);
    }

    #[test]
    fn a_jump_keeps_the_beat() {
        let g = Grid::new((0..100).map(|k| k as f64 * 0.5).collect());
        // 0.1 s past a beat, a jump to the cue at 8 s lands 0.1 s past it
        assert!((keep_phase(&g, 3.1, 8.0) - 8.1).abs() < 1e-9);
        assert!((keep_phase(&g, 3.45, 8.0) - 8.45).abs() < 1e-9);
    }

    #[test]
    fn sync_matches_tempo_and_pulls_the_phase_in() {
        let a = Grid::new((0..400).map(|k| k as f64 * 0.5).collect()); // 120 bpm
        let b = Grid::new((0..400).map(|k| k as f64 * 0.48).collect()); // 125 bpm
        // in phase: b plays at 120/125
        let s = follow_speed(&b, 4.8, (&a, 5.0, 1.0));
        assert!((s - 0.96).abs() < 1e-9, "{s}");
        // b a quarter beat behind: it speeds up, within the limit
        let s = follow_speed(&b, 4.8 - 0.12, (&a, 5.0, 1.0));
        assert!(s > 0.96 && s <= 0.96 * (1.0 + SYNC_MAX) + 1e-9, "{s}");
        // simulated: deck b, 0.3 beat off and 3% slow, is in phase within 2 s
        let (mut ta, mut tb, dt) = (10.0, 10.0 + 0.3 * 0.48, 0.01);
        for _ in 0..200 {
            let s = follow_speed(&b, tb, (&a, ta, 1.03));
            ta += dt * 1.03;
            tb += dt * s;
        }
        let err = (a.beat_at(ta) - b.beat_at(tb) + 0.5).rem_euclid(1.0) - 0.5;
        assert!(err.abs() < 0.02, "phase error {err} beats");
    }

    #[test]
    fn decks_play_cue_mute_and_crossfade() {
        let sr = 48000;
        let decks = Decks::default();
        let mut p = Player::new(sr);
        decks.load(0, song(sr, 30.0));
        decks.load(1, song(sr, 30.0));
        // deck 1 alone on the crossfader
        decks.xfader.set(0.0);
        decks.deck[0].send(Cmd::Play);
        let (l, _) = run(&mut p, &decks, 0.5);
        assert!((l[l.len() - 1] - 1.5).abs() < 1e-3, "all four lanes: {}", l[l.len() - 1]);
        assert!((decks.deck[0].pos.get() - 0.5).abs() < 0.02);
        assert_eq!(decks.deck[1].pos.get(), 0.0, "deck 2 waits");
        // mute the drums: 0.2 goes, with a fade over one block, not a click
        decks.deck[0].mutes.fetch_xor(1 << 1, Ordering::Relaxed);
        let (l, _) = run(&mut p, &decks, 0.1);
        assert!(l[100] > 1.3 && l[100] < 1.5, "fading: {}", l[100]);
        assert!((l[l.len() - 1] - 1.3).abs() < 1e-3, "{}", l[l.len() - 1]);
        // the middle of the crossfader: each deck at -3 dB
        decks.xfader.set(0.5);
        decks.deck[1].send(Cmd::Play);
        let (l, _) = run(&mut p, &decks, 0.1);
        let half = std::f32::consts::FRAC_1_SQRT_2;
        assert!((l[l.len() - 1] - (1.3 + 1.5) * half).abs() < 1e-3, "{}", l[l.len() - 1]);
        // a hot cue jumps, keeping the beat
        let before = decks.deck[0].beat.get().rem_euclid(1.0);
        decks.deck[0].send(Cmd::HotCue(1));
        let _ = run(&mut p, &decks, 0.0107);
        let at = decks.deck[0].pos.get();
        assert!(at > 8.0 && at < 8.5, "{at}");
        assert!((decks.deck[0].beat.get().rem_euclid(1.0) - before).abs() < 0.05, "kept the phase");
        // a 1-beat loop: the deck stays within it
        decks.deck[0].send(Cmd::Loop(1.0));
        let _ = run(&mut p, &decks, 2.0);
        let t = decks.deck[0].pos.get();
        assert!(decks.deck[0].loop_on.load(Ordering::Relaxed) && (8.0..9.0).contains(&t), "{t}");
        decks.deck[0].send(Cmd::Loop(1.0));
        let _ = run(&mut p, &decks, 1.0);
        assert!(!decks.deck[0].loop_on.load(Ordering::Relaxed) && decks.deck[0].pos.get() > 9.0);
    }

    #[test]
    fn the_cue_button_works_like_a_cdj() {
        let sr = 48000;
        let decks = Decks::default();
        let mut p = Player::new(sr);
        decks.load(0, song(sr, 30.0));
        let d = &decks.deck[0];
        d.send(Cmd::Seek(3.1));
        d.send(Cmd::CueDown);
        let _ = run(&mut p, &decks, 0.05);
        assert_eq!(d.cue.get(), 3.0, "stopped: the cue moves here, onto the beat");
        // held on the cue: it plays; let go: back to the cue
        d.send(Cmd::CueDown);
        let _ = run(&mut p, &decks, 0.5);
        assert!(d.playing.load(Ordering::Relaxed) && d.pos.get() > 3.4);
        d.send(Cmd::CueUp);
        let _ = run(&mut p, &decks, 0.05);
        assert!(!d.playing.load(Ordering::Relaxed) && d.pos.get() == 3.0);
    }

    #[test]
    fn the_jog_nudges_and_scratches() {
        let sr = 48000;
        let decks = Decks::default();
        let mut p = Player::new(sr);
        decks.load(0, song(sr, 30.0));
        let d = &decks.deck[0];
        d.send(Cmd::Play);
        let _ = run(&mut p, &decks, 1.0);
        let t0 = d.pos.get();
        // 20 steps on the side: 20 ms ahead of where it would be, made up in a few blocks
        d.nudge.fetch_add(20, Ordering::Relaxed);
        let _ = run(&mut p, &decks, 1.0);
        assert!((d.pos.get() - t0 - 1.0 - 0.020).abs() < 0.002, "{}", d.pos.get() - t0);
        // touched: the platter holds it, a turn back moves it back
        d.touch.store(true, Ordering::Relaxed);
        let _ = run(&mut p, &decks, 0.2);
        let held = d.pos.get();
        let (l, _) = run(&mut p, &decks, 0.2);
        assert!((d.pos.get() - held).abs() < 1e-3, "held still");
        assert!(l.iter().all(|x| x.abs() < 1e-6), "and silent (the EQ's tail long gone)");
        d.scratch.fetch_add(-72, Ordering::Relaxed); // a tenth of a turn back: 0.18 s
        let _ = run(&mut p, &decks, 0.3);
        assert!((held - d.pos.get() - 0.18).abs() < 0.01, "{}", held - d.pos.get());
        d.touch.store(false, Ordering::Relaxed);
        let _ = run(&mut p, &decks, 0.5);
        assert!(d.pos.get() > held, "let go: it plays on");
    }

    #[test]
    fn the_eq_kills_and_is_flat_in_the_middle() {
        let sr = 48000;
        let decks = Decks::default();
        let mut s = Strip::new(sr);
        let tone = |hz: f32| -> Vec<f32> { (0..9600).map(|i| (i as f32 * std::f32::consts::TAU * hz / sr as f32).sin()).collect() };
        let peak = |x: &[f32]| x[4800..].iter().fold(0.0f32, |p, v| p.max(v.abs()));
        let d = &decks.deck[0];
        s.set(d);
        let (mut l, mut r) = (tone(1000.0), tone(1000.0));
        s.run(&mut l, &mut r);
        assert!((peak(&l) - 1.0).abs() < 0.012, "flat within 0.1 dB: {}", peak(&l));
        d.eq_low.set(0.0);
        s.set(d);
        let (mut l, mut r) = (tone(60.0), tone(60.0));
        s.run(&mut l, &mut r);
        assert!(peak(&l) < 0.1, "low killed: {}", peak(&l));
        d.eq_low.set(0.5);
        d.color.set(0.0);
        s.set(d);
        let (mut l, mut r) = (tone(5000.0), tone(5000.0));
        s.run(&mut l, &mut r);
        assert!(peak(&l) < 0.05, "filter fully down: {}", peak(&l));
    }

    #[test]
    fn the_beat_fx_build_and_echo_on_the_beat() {
        let sr = 48000;
        let mut fx = build_fx(sr).expect("the live shaders compile");
        let decks = Decks::default();
        decks.fx_on.store(true, Ordering::Relaxed);
        decks.fx_depth.set(1.0);
        decks.fx_beats.store(3, Ordering::Relaxed); // one beat
        // an impulse at 120 bpm, once the knobs have settled: its echo a beat later
        let (n, at) = (48000, 9600);
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        l[at] = 1.0;
        r[at] = 1.0;
        for i in (0..n).step_by(1024) {
            let k = (n - i).min(1024);
            fx.run(&decks, 120.0, &mut l[i..i + k], &mut r[i..i + k]);
        }
        let (echo, v) = l[at + 100..].iter().enumerate().fold((0, 0.0f32), |(bi, bv), (i, v)| if v.abs() > bv { (i, v.abs()) } else { (bi, bv) });
        assert!((echo as i64 + 100 - 24000).abs() < 3 && v > 0.5, "echo {} later at {v}", echo + 100);
    }

    #[test]
    fn leds_follow_the_decks_and_send_only_changes() {
        let map = Map::parse(crate::deckmap::BUILT_IN).unwrap();
        let decks = Decks::default();
        decks.load(1, song(48000, 30.0));
        let mut leds = Leds::default();
        let first = leds.update(&map, &decks);
        assert!(first.contains(&[0x96, 0x00, 0x7f]) && first.contains(&[0x96, 0x03, 0x00]), "hot cues A..C lit, D dark");
        assert!(first.contains(&[0x96, 0x08, 0x7f]), "the vocals play");
        assert!(leds.update(&map, &decks).is_empty(), "nothing changed");
        decks.deck[1].mutes.fetch_xor(1, Ordering::Relaxed);
        assert_eq!(leds.update(&map, &decks), vec![[0x96, 0x08, 0x00]]);
    }

    #[test]
    fn controller_messages_reach_the_decks() {
        let map = Map::parse(crate::deckmap::BUILT_IN).unwrap();
        let decks = Decks::default();
        let mut dec = Decoder::default();
        decks.midi(&map, &mut dec, [0x96, 0x09, 0x7f]);
        assert!(decks.deck[1].muted(1), "shift + pad 2 mutes deck 2's drums");
        decks.midi(&map, &mut dec, [0x96, 0x09, 0x00]);
        assert!(decks.deck[1].muted(1), "letting go keeps it");
        decks.midi(&map, &mut dec, [0x91, 0x1f, 0x7f]);
        decks.midi(&map, &mut dec, [0x90, 0x1f, 0x7f]);
        assert!(decks.deck[0].sync.load(Ordering::Relaxed) && !decks.deck[1].sync.load(Ordering::Relaxed), "one deck leads");
        decks.midi(&map, &mut dec, [0xb4, 0x0b, 0x00]);
        assert_eq!(decks.xfader.get(), 0.0);
        decks.midi(&map, &mut dec, [0x96, 0x00, 0x7f]);
        assert_eq!(decks.deck[1].cmds.pop(), Some(Cmd::HotCue(0)));
        decks.midi(&map, &mut dec, [0x96, 0x0c, 0x7f]);
        assert_eq!(decks.unmapped.pop(), Some([0x96, 0x0c, 0x7f]), "unknown: logged");
        decks.midi(&map, &mut dec, [0x86, 0x00, 0x00]);
        decks.midi(&map, &mut dec, [0x90, 0x3f, 0x7f]);
        decks.midi(&map, &mut dec, [0xb4, 0x46, 0x7f]);
        decks.midi(&map, &mut dec, [0xa6, 0x00, 0x40]);
        assert!(decks.unmapped.pop().is_none(), "a known pad's note-off, shift, headphone cue and pad pressure are no news");
    }
}
