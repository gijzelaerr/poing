//! Live mode: two decks, each playing a Poing song with its stems, mixed like a
//! DJ mixer and played from a controller (the OMNIS-DUO; see docs/LIVE.md).
//!
//! The UI thread builds a [`DeckSong`] (the song's stems, its mix and the DJ
//! export's beat grid and cues) and publishes it by generation. The MIDI thread
//! turns the controller's messages into [`Decks`]' atomics and commands. The
//! audio callback's [`Player`] owns the transport: it renders each deck at its
//! speed from the song's own mixer and effects rack (the lanes' effects and
//! the MIDI lanes' instruments, a rack per deck), through its EQ, filter,
//! fader and the crossfader, then the beat FX (Splash shaders in an [`FxRack`]).
//!
//! A deck renders every frame of its song once, in order, through its rack,
//! and plays that at its speed (so instruments and effects bend with the
//! tempo, like the stems). What it rendered last stays in a short history:
//! the platter going back plays that, as a record would.
//!
//! Key lock: instead of reading the history faster or slower (tempo and pitch
//! together, like vinyl), the deck lays short overlapping pieces of it at
//! their own pitch, each chosen near where the playhead is so it carries on
//! from the one before (WSOLA). The platter in the hand is always vinyl.
//!
//! Headphone cue: the decks whose cue button is on, after their EQ and trim
//! but before their fader and the crossfader, summed into a [`CueRing`] that
//! a second sound output (the headphones) plays.

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
/// The song as heard that a deck keeps behind its playhead (seconds): what the
/// platter can go back over.
const HISTORY_S: f64 = 4.0;
/// A stem mute fades over this long (seconds), in steps of this many frames.
const FADE_S: f64 = 0.02;
const FADE_STEP: usize = 64;
/// The playhead may be this many frames past what was rendered and still carry on from it.
const GAP: u64 = 16;
/// Key lock: a new piece every this long (seconds), each twice as long and
/// looked for up to this far (seconds) either side of the playhead.
const LOCK_HOP_S: f64 = 0.02;
const LOCK_SEARCH_S: f64 = 0.01;
/// Key lock holds between these speeds; outside them the deck plays like vinyl.
const LOCK_SPEEDS: (f64, f64) = (0.5, 2.0);
/// Frames skipped between the ones compared when choosing a piece.
const LOCK_STRIDE: usize = 4;

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
    /// The song's effects and instruments for this deck. The callback takes
    /// it while the song is on the deck and puts it back when it lets go, so
    /// it is dropped with the song, on the UI thread.
    pub rack: Mutex<Option<Box<FxRack>>>,
    /// Plugins of the song that did not load for the deck (by name).
    pub missing: Vec<String>,
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
    /// A deck's copy of the song open in `st`: its stems are shared, not
    /// copied; its effects and instruments are the deck's own (plugins get an
    /// instance for the deck, as they sound now).
    pub fn of(st: &mut crate::state::EditorState) -> DeckSong {
        let (rack, missing) = st.make_deck_rack();
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
            rack: Mutex::new(Some(Box::new(rack))),
            missing,
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
    /// Key lock ("master tempo"): the tempo fader leaves the pitch alone.
    pub keylock: AtomicBool,
    /// Heard in the headphones (the mixer's cue button).
    pub pfl: AtomicBool,
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
            keylock: AtomicBool::new(false),
            pfl: AtomicBool::new(false),
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
    /// What the headphones play, and its level.
    pub cue: CueRing,
    pub cue_meter: AF32,
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
            cue: CueRing::default(),
            cue_meter: AF32::new(0.0),
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
            Act::KeyLock if down => {
                deck.keylock.fetch_xor(true, Ordering::Relaxed);
            }
            Act::HeadCue if down => {
                deck.pfl.fetch_xor(true, Ordering::Relaxed);
            }
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
                    Act::KeyLock => lit(deck.keylock.load(Ordering::Relaxed)),
                    Act::HeadCue => lit(deck.pfl.load(Ordering::Relaxed)),
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

// ------------------------------------------------------- the headphone cue

/// Frames the cue ring holds, and how far behind the writer the headphones
/// start reading (their latency, and the room for the two outputs' blocks to
/// fall differently).
const CUE_RING: usize = 1 << 15;
pub const CUE_BEHIND: u64 = 1024;

/// The headphone mix on its way from the decks' callback to the headphones'
/// output: a ring of stereo frames (f32 bits) by frame number, one writer and
/// one reader, no locks.
pub struct CueRing {
    l: Box<[AtomicU32]>,
    r: Box<[AtomicU32]>,
    written: AtomicU64,
}

impl Default for CueRing {
    fn default() -> Self {
        CueRing { l: (0..CUE_RING).map(|_| AtomicU32::new(0)).collect(), r: (0..CUE_RING).map(|_| AtomicU32::new(0)).collect(), written: AtomicU64::new(0) }
    }
}

impl CueRing {
    /// The decks' callback: the next frames of the headphone mix.
    pub fn push(&self, l: &[f32], r: &[f32]) {
        let base = self.written.load(Ordering::Relaxed);
        let n = l.len().min(r.len());
        for i in 0..n {
            let s = (base as usize + i) % CUE_RING;
            self.l[s].store(l[i].to_bits(), Ordering::Relaxed);
            self.r[s].store(r[i].to_bits(), Ordering::Relaxed);
        }
        self.written.store(base + n as u64, Ordering::Release);
    }
}

/// The headphones' side of the [`CueRing`]: where it reads.
#[derive(Default)]
pub struct CueReader {
    at: u64,
    reading: bool,
}

impl CueReader {
    /// The next frames for the headphones (overwrites l/r). It reads
    /// [`CUE_BEHIND`] frames behind the writer. The two outputs run on their
    /// own clocks: when the headphones catch up with the writer they play
    /// silence for what is missing and step back; when they fall far behind
    /// they skip ahead.
    pub fn pull(&mut self, ring: &CueRing, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        let written = ring.written.load(Ordering::Acquire);
        let behind = written.saturating_sub(self.at);
        if !self.reading || self.at > written || behind > 4 * CUE_BEHIND {
            self.at = written.saturating_sub(CUE_BEHIND);
            self.reading = true;
        }
        for i in 0..n {
            if self.at >= written {
                // caught up: the rest is silence, and the next block starts further back
                l[i..n].fill(0.0);
                r[i..n].fill(0.0);
                self.reading = false;
                return;
            }
            let s = self.at as usize % CUE_RING;
            l[i] = f32::from_bits(ring.l[s].load(Ordering::Relaxed));
            r[i] = f32::from_bits(ring.r[s].load(Ordering::Relaxed));
            self.at += 1;
        }
    }

    /// Nothing is being written (live mode is off): start afresh when it is.
    pub fn stop(&mut self) {
        self.reading = false;
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

/// Key lock's state: the piece being played and the fade out of it that the
/// next one goes over.
struct Lock {
    /// Frames a piece plays alone before the next comes in, and how far either side of the playhead one is looked for.
    hop: usize,
    search: usize,
    /// The fade in over a hop (the fade out is 1 minus it).
    rise: Vec<f32>,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    /// Frames of `buf` played (`hop`: all of it).
    read: usize,
    tail_l: Vec<f32>,
    tail_r: Vec<f32>,
    /// The song frame the last piece would carry on at.
    next: Option<u64>,
    /// The lock is laying pieces (else the deck plays like vinyl).
    on: bool,
    /// Leaving the lock: frames of `tail` already faded out under vinyl (`hop`: done).
    fade: usize,
    /// What would come next, and the history near the playhead (mono), to compare.
    want: Vec<f32>,
    near: Vec<f32>,
}

impl Lock {
    fn new(sr: u32) -> Lock {
        let hop = ((LOCK_HOP_S * sr as f64) as usize).max(64);
        let search = ((LOCK_SEARCH_S * sr as f64) as usize).max(16);
        let rise = (0..hop).map(|i| 0.5 - 0.5 * (std::f32::consts::PI * (i as f32 + 0.5) / hop as f32).cos()).collect();
        Lock {
            hop,
            search,
            rise,
            buf_l: vec![0.0; hop],
            buf_r: vec![0.0; hop],
            read: hop,
            tail_l: vec![0.0; hop],
            tail_r: vec![0.0; hop],
            next: None,
            on: false,
            fade: hop,
            want: vec![0.0; hop],
            near: vec![0.0; 2 * search + hop + 1],
        }
    }

    fn clear(&mut self) {
        self.on = false;
        self.read = self.hop;
        self.fade = self.hop;
        self.next = None;
    }

    /// The deck plays like vinyl again (l/r hold what it plays): what the
    /// lock had still to play fades out under it over a hop, and it fades in.
    fn leave(&mut self, l: &mut [f32], r: &mut [f32]) {
        let hop = self.hop;
        if self.on {
            self.on = false;
            // the rest of the piece, then the fade out that would have followed it
            let left = hop - self.read;
            for i in 0..hop {
                let fall = 1.0 - self.rise[i];
                let (a, b) = if i < left { (self.buf_l[self.read + i], self.buf_r[self.read + i]) } else { (self.tail_l[i - left], self.tail_r[i - left]) };
                self.want[i] = a * fall;
                self.near[i] = b * fall;
            }
            self.tail_l[..hop].copy_from_slice(&self.want[..hop]);
            self.tail_r[..hop].copy_from_slice(&self.near[..hop]);
            self.read = hop;
            self.next = None;
            self.fade = 0;
        }
        for i in 0..l.len().min(r.len()) {
            if self.fade >= hop {
                break;
            }
            let rise = self.rise[self.fade];
            l[i] = l[i] * rise + self.tail_l[self.fade];
            r[i] = r[i] * rise + self.tail_r[self.fade];
            self.fade += 1;
        }
    }
}

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
    /// The mix as heard (its lane gains with the mutes).
    mix: MixParams,
    /// The stems wanted muted (a bit each), and how far each is faded in (0..1).
    mutes: u8,
    mute_f: [f32; 4],
    speed: f64,
    strip: Strip,
    /// The song's rack while it is on this deck, and the frames its output
    /// comes late (read ahead by that much, so the deck stays on its grid).
    rack: Option<Box<FxRack>>,
    late: u64,
    /// The song as rendered, a ring by song frame: frames [h_lo, h_hi).
    hist_l: Vec<f32>,
    hist_r: Vec<f32>,
    h_lo: u64,
    h_hi: u64,
    /// The playhead was moved (a cue, a loop's end): forward play renders afresh.
    jumped: bool,
    lock: Lock,
    win_l: Vec<f32>,
    win_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
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
        let hist = ((HISTORY_S * sr as f64) as usize).max(2 * win);
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
            mutes: 0,
            mute_f: [1.0; 4],
            speed: 1.0,
            strip: Strip::new(sr),
            rack: None,
            late: 0,
            hist_l: vec![0.0; hist],
            hist_r: vec![0.0; hist],
            h_lo: 0,
            h_hi: 0,
            jumped: false,
            lock: Lock::new(sr),
            win_l: vec![0.0; win],
            win_r: vec![0.0; win],
            out_l: vec![0.0; BLOCK],
            out_r: vec![0.0; BLOCK],
        }
    }

    fn t(&self) -> f64 {
        self.song.as_ref().map_or(0.0, |s| self.pos / s.sr as f64)
    }

    /// Adopt a newly published song (it starts at its first hot cue, stopped).
    /// The rack of the song it replaces goes back into that song, which the UI
    /// still holds: nothing is dropped here.
    fn adopt(&mut self, deck: &Deck) {
        let gen = deck.gen.load(Ordering::Acquire);
        if gen == self.gen {
            return;
        }
        let Ok(slot) = deck.song.try_lock() else { return };
        let new = slot.clone();
        drop(slot);
        let same = matches!((&self.song, &new), (Some(a), Some(b)) if Arc::ptr_eq(a, b));
        if !same {
            // both racks' slots, or wait a block: the UI is in one of them
            let mut theirs = match &new {
                Some(s) => match s.rack.try_lock() {
                    Ok(r) => Some(r),
                    Err(_) => return,
                },
                None => None,
            };
            if let Some(old) = &self.song {
                let Ok(mut back) = old.rack.try_lock() else { return };
                if back.is_none() {
                    *back = self.rack.take();
                }
            }
            // (a rack that could not go back stays here, unused, rather than be dropped)
            if self.rack.is_none() {
                self.rack = theirs.as_mut().and_then(|r| r.take());
            }
        }
        // the UI holds the song this replaces until `seen` moves: never its last drop here
        self.song = new;
        self.gen = gen;
        deck.seen.store(gen, Ordering::Release);
        self.playing = false;
        self.previewing = false;
        self.loop_on = false;
        self.scratch = 0.0;
        self.nudge = 0.0;
        self.h_lo = 0;
        self.h_hi = 0;
        self.jumped = true;
        self.lock.clear();
        if let Some(s) = &self.song {
            let start = s.hot[0].as_ref().map_or(0.0, |h| h.0);
            self.pos = start * s.sr as f64;
            self.cue = self.pos;
            self.mix.clone_from(&s.mix);
            self.mutes = 0;
            self.mute_f = [1.0; 4];
            // a song take plays through the master only
            let lanes = |r: &FxRack| if s.mix.song_take().is_some() { 0 } else { r.latency };
            self.late = self.rack.as_ref().map_or(0, |r| (lanes(r) + r.master_latency()) as u64);
        }
        self.strip.clear();
    }

    /// The deck's gains with each stem as far in as `f` says, into `mix`.
    fn apply_mutes(song: &DeckSong, f: &[f32; 4], mix: &mut MixParams) {
        for (i, g) in mix.gains.iter_mut().enumerate() {
            let base = song.mix.gains.get(i).copied().unwrap_or(0.0);
            let role = song.role.get(i).copied().unwrap_or(3) as usize;
            *g = base * f[role.min(3)];
        }
    }

    fn command(&mut self, c: Cmd) {
        let Some(song) = self.song.clone() else { return };
        let sr = song.sr as f64;
        let g = &song.grid;
        let t = self.pos / sr;
        let before = self.pos;
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
        if self.pos != before {
            self.jumped = true;
        }
    }

    /// A loop of `beats` from the beat (or the part of one) the deck is in.
    fn start_loop(&mut self, g: &Grid, t: f64, beats: f32) {
        let q = (beats as f64).min(1.0);
        self.loop_beats = beats;
        self.loop_a = (g.beat_at(t) / q).floor() * q;
        self.loop_on = true;
    }

    /// Render `n` frames (at the output rate) of the deck at `speed`, through
    /// its channel strip, into out_l/out_r.
    fn render(&mut self, deck: &Deck, speed: f64, live_sr: u32, n: usize) {
        self.play(deck, speed, live_sr, n);
        self.strip.set(deck);
        self.strip.run(&mut self.out_l[..n], &mut self.out_r[..n]);
    }

    /// The deck's song at `speed`, before the strip.
    fn play(&mut self, deck: &Deck, speed: f64, live_sr: u32, n: usize) {
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
        // the stems as muted now: what is rendered from here on fades to it
        self.mutes = deck.mutes.load(Ordering::Relaxed);
        let held = deck.touch.load(Ordering::Relaxed);
        // key lock: playing on at a speed it holds, the platter not in the hand
        let locked = deck.keylock.load(Ordering::Relaxed) && self.playing && !held && (LOCK_SPEEDS.0..=LOCK_SPEEDS.1).contains(&rate);
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
                    self.jumped = true;
                }
                k = k.min(((lb - self.pos) / rate).ceil().max(1.0) as usize);
            }
            if locked {
                self.stretch(&song, rate, done, k);
            } else {
                self.segment(&song, rate, done, k, held);
            }
            self.pos += rate * k as f64;
            if looping && self.pos >= lb {
                self.pos -= lb - la;
                self.jumped = true;
            }
            done += k;
        }
        if !locked {
            // out of key lock: its last piece fades under what plays now
            self.lock.leave(&mut self.out_l[..n], &mut self.out_r[..n]);
        }
        if self.pos < 0.0 {
            self.pos = 0.0;
        }
        if self.pos >= song.frames as f64 {
            self.pos = song.frames as f64;
            self.playing = false;
        }
        self.speed = if self.playing { rate / to_song } else { 0.0 };
    }

    /// [`Self::segment`] with key lock: frames [at, at + k) from the pieces
    /// the lock lays, a new one whenever the last is played out.
    fn stretch(&mut self, song: &DeckSong, rate: f64, at: usize, k: usize) {
        let mut p = self.pos;
        for i in 0..k {
            if self.lock.read >= self.lock.hop {
                self.piece(song, p.max(0.0).round() as u64);
            }
            let j = self.lock.read;
            self.out_l[at + i] = self.lock.buf_l[j];
            self.out_r[at + i] = self.lock.buf_r[j];
            self.lock.read += 1;
            p += rate;
        }
    }

    /// The next piece for the key lock, for a playhead at song frame `a`: the
    /// stretch of the history within reach of `a` that best carries on from
    /// the piece before, faded in over that one's fade out.
    fn piece(&mut self, song: &DeckSong, a: u64) {
        let (hop, search) = (self.lock.hop as u64, self.lock.search as u64);
        let cap = self.hist_l.len() as u64;
        // after a jump, or somewhere the history is not: afresh from here
        let jumped = std::mem::take(&mut self.jumped);
        if jumped || a < self.h_lo || a > self.h_hi + GAP {
            self.h_lo = a;
            self.h_hi = a;
            self.lock.next = None;
        }
        self.extend(song, a + search + 2 * hop + 1);
        let read = |hist: &[f32], f: u64| hist[(f % cap) as usize];
        if !self.lock.on {
            // coming from vinyl: as if the piece before was the song from here
            self.lock.on = true;
            self.lock.fade = self.lock.hop;
            self.lock.next = Some(a);
            for i in 0..hop as usize {
                let fall = 1.0 - self.lock.rise[i];
                self.lock.tail_l[i] = fall * read(&self.hist_l, a + i as u64);
                self.lock.tail_r[i] = fall * read(&self.hist_r, a + i as u64);
            }
        }
        // where to look: around the playhead, within the history
        let lo = a.saturating_sub(search).max(self.h_lo);
        let hi = a + search;
        let mut start = a;
        if let Some(next) = self.lock.next.filter(|n| *n >= self.h_lo && *n + hop <= self.h_hi) {
            let stride = LOCK_STRIDE;
            let m = hop as usize / stride;
            for i in 0..m {
                let f = next + (i * stride) as u64;
                self.lock.want[i] = read(&self.hist_l, f) + read(&self.hist_r, f);
            }
            let span = (hi - lo) as usize + hop as usize;
            for i in 0..span {
                let f = lo + i as u64;
                self.lock.near[i] = read(&self.hist_l, f) + read(&self.hist_r, f);
            }
            // how well the stretch from `s` carries on: its likeness to what would have come next
            let score = |near: &[f32], want: &[f32], s: u64| -> f32 {
                let at = (s - lo) as usize;
                let (mut dot, mut energy) = (0.0f32, 1e-9f32);
                for i in 0..m {
                    let y = near[at + i * stride];
                    dot += want[i] * y;
                    energy += y * y;
                }
                dot / energy.sqrt()
            };
            // what would have come next, when it is within reach: then nothing is changed
            let mut best = f32::MIN;
            if (lo..=hi).contains(&next) {
                start = next;
                best = score(&self.lock.near, &self.lock.want, next) + 1e-6;
            }
            for s in lo..=hi {
                let v = score(&self.lock.near, &self.lock.want, s);
                if v > best {
                    best = v;
                    start = s;
                }
            }
        }
        for i in 0..hop as usize {
            let (rise, f) = (self.lock.rise[i], start + i as u64);
            self.lock.buf_l[i] = self.lock.tail_l[i] + rise * read(&self.hist_l, f);
            self.lock.buf_r[i] = self.lock.tail_r[i] + rise * read(&self.hist_r, f);
            self.lock.tail_l[i] = (1.0 - rise) * read(&self.hist_l, f + hop);
            self.lock.tail_r[i] = (1.0 - rise) * read(&self.hist_r, f + hop);
        }
        self.lock.next = Some(start + hop);
        self.lock.read = 0;
    }

    /// Frames [at, at + k) of the block: the song from `pos` on at `rate`,
    /// read between the frames of the history, which is rendered on as far as
    /// they need. Going back past the history's start (the platter turned back
    /// right after a jump) is the song's mixer alone, without the rack.
    fn segment(&mut self, song: &DeckSong, rate: f64, at: usize, k: usize, held: bool) {
        let end = self.pos + rate * (k.max(1) - 1) as f64;
        let (first, last) = (self.pos.min(end), self.pos.max(end));
        if last < 0.0 {
            return;
        }
        let lo = first.floor().max(0.0) as u64;
        let hi = last.floor() as u64 + 2;
        let cap = self.hist_l.len() as u64;
        if rate > 0.0 {
            // playing on from a jump, or from somewhere the history is not: afresh from here
            if (self.jumped && !held) || lo < self.h_lo || lo > self.h_hi + GAP {
                self.h_lo = lo;
                self.h_hi = lo;
            }
            if !held {
                self.jumped = false;
            }
        }
        if lo < self.h_lo || lo > self.h_hi + GAP || hi - lo >= cap {
            return self.dry(song, rate, at, k, lo as usize, (hi - lo) as usize + 1);
        }
        if hi > self.h_hi {
            self.extend(song, hi);
        }
        for i in 0..k {
            let p = self.pos + rate * i as f64;
            if p < 0.0 {
                continue;
            }
            let j = p as u64;
            let f = (p - j as f64) as f32;
            let (a, b) = ((j % cap) as usize, ((j + 1) % cap) as usize);
            self.out_l[at + i] = self.hist_l[a] + (self.hist_l[b] - self.hist_l[a]) * f;
            self.out_r[at + i] = self.hist_r[a] + (self.hist_r[b] - self.hist_r[a]) * f;
        }
    }

    /// Render the song on from where the history ends up to frame `to`: each
    /// frame once, in order, through the rack. A stem being muted or unmuted
    /// fades over the frames rendered here.
    fn extend(&mut self, song: &DeckSong, to: u64) {
        let cap = self.hist_l.len();
        let per_frame = (1.0 / (FADE_S * song.sr as f64)) as f32;
        while self.h_hi < to {
            let mut k = ((to - self.h_hi) as usize).min(self.win_l.len());
            let mutes = self.mutes;
            let want = |s: usize| if mutes & (1 << s) != 0 { 0.0 } else { 1.0 };
            if (0..4).any(|s| self.mute_f[s] != want(s)) {
                k = k.min(FADE_STEP);
                let step = per_frame * k as f32;
                for s in 0..4 {
                    let (f, w) = (self.mute_f[s], want(s));
                    self.mute_f[s] = if w > f { (f + step).min(1.0) } else { (f - step).max(0.0) };
                }
                Self::apply_mutes(song, &self.mute_f, &mut self.mix);
            }
            let start = (self.h_hi + self.late) as usize;
            mix_into(&song.stems, &self.mix, song.sr, start, &mut self.win_l[..k], &mut self.win_r[..k], self.rack.as_deref_mut());
            let at = (self.h_hi % cap as u64) as usize;
            let first = k.min(cap - at);
            self.hist_l[at..at + first].copy_from_slice(&self.win_l[..first]);
            self.hist_r[at..at + first].copy_from_slice(&self.win_r[..first]);
            self.hist_l[..k - first].copy_from_slice(&self.win_l[first..k]);
            self.hist_r[..k - first].copy_from_slice(&self.win_r[first..k]);
            self.h_hi += k as u64;
        }
        self.h_lo = self.h_lo.max(self.h_hi.saturating_sub(cap as u64));
    }

    /// [`Self::segment`] where the history is not: the lanes from the song's
    /// mixer over the window they need, without effects or instruments.
    fn dry(&mut self, song: &DeckSong, rate: f64, at: usize, k: usize, start: usize, len: usize) {
        let len = len.min(self.win_l.len());
        mix_into(&song.stems, &self.mix, song.sr, start, &mut self.win_l[..len], &mut self.win_r[..len], None);
        for i in 0..k {
            let p = self.pos + rate * i as f64 - start as f64;
            if p < 0.0 {
                continue;
            }
            let j = p as usize;
            let f = (p - j as f64) as f32;
            let (j, j1) = (j.min(len - 1), (j + 1).min(len - 1));
            self.out_l[at + i] = self.win_l[j] + (self.win_l[j1] - self.win_l[j]) * f;
            self.out_r[at + i] = self.win_r[j] + (self.win_r[j1] - self.win_r[j]) * f;
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
    /// The headphone mix of the block being rendered.
    cue_l: Vec<f32>,
    cue_r: Vec<f32>,
}

impl Player {
    pub fn new(sr: u32) -> Player {
        Player { sr, decks: [DeckPlayer::new(sr), DeckPlayer::new(sr)], fx: None, cue_l: vec![0.0; BLOCK], cue_r: vec![0.0; BLOCK] }
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
        self.cue_l[..n].fill(0.0);
        self.cue_r[..n].fill(0.0);
        for d in 0..DECKS {
            let deck = &shared.deck[d];
            let p = &mut self.decks[d];
            p.render(deck, speed[d], self.sr, n);
            let trim = 10f32.powf((deck.trim.get() - 0.5) * 24.0 / 20.0);
            if deck.pfl.load(Ordering::Relaxed) {
                // the headphones hear it whatever its fader and the crossfader say
                for i in 0..n {
                    self.cue_l[i] += p.out_l[i] * trim;
                    self.cue_r[i] += p.out_r[i] * trim;
                }
            }
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
        shared.cue.push(&self.cue_l[..n], &self.cue_r[..n]);
        let peak = self.cue_l[..n].iter().chain(&self.cue_r[..n]).fold(0.0f32, |p, x| p.max(x.abs()));
        shared.cue_meter.set(peak.max(shared.cue_meter.get() * 0.9));
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
            rack: Mutex::new(None),
            missing: Vec::new(),
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

    /// A short feedback delay: what it gives depends on every frame before.
    const DELAY: &str = "var b = [0.0; 4096]\nvar w = int(0)\nfn effect(l, r) {\n    let y = tap(b, w, 1000.0)\n    b[w] = (l + r) * 0.5 + y * 0.5\n    w = (w + 1) % 4096\n    vec2(l + y, r - y)\n}\n";

    fn fx(id: &str) -> poing_model::Fx {
        poing_model::Fx { id: id.into(), name: id.into(), file: String::new(), params: None, bypass: None, prompt: None, plugin: None, isolate: None }
    }

    /// Two lanes of noise, the first through [`DELAY`]: (stems, mix, a rack).
    fn song_with_fx(sr: u32, frames: usize) -> (Stems, MixParams, FxRack) {
        let mut seed = 12345u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
        };
        let mut lane = || Arc::new(Stem::new((0..frames).map(|_| noise()).collect(), (0..frames).map(|_| noise()).collect()));
        let stems: Stems = Arc::new(vec![lane(), lane()]);
        let mix = MixParams { version: 1, gains: vec![0.8, 0.5], ..Default::default() };
        let shader = crate::fx::compile_source(DELAY).expect("the delay compiles");
        let unit = FxUnit::new("fx1", shader.clone(), FxControls::new(&shader, &fx("fx1"), None), sr);
        (stems, mix, FxRack::new(vec![vec![unit], Vec::new()], Vec::new()))
    }

    fn deck_song(sr: u32, stems: Stems, mix: MixParams, frames: usize, rack: FxRack) -> Arc<DeckSong> {
        let n = mix.gains.len();
        Arc::new(DeckSong {
            name: "test".into(),
            stems,
            mix,
            sr,
            frames,
            grid: Grid::new((0..=frames / (sr as usize / 2)).map(|k| k as f64 * 0.5).collect()),
            bpm: 120.0,
            hot: [Some((0.0, "Intro".into())), None, None, None],
            sections: vec![(0.0, "Intro".into())],
            role: vec![3; n],
            rack: Mutex::new(Some(Box::new(rack))),
            missing: Vec::new(),
        })
    }

    #[test]
    fn a_deck_plays_the_lane_effects_as_the_song_does() {
        let sr = 48000;
        let frames = sr as usize * 2;
        for block in [512usize, 333] {
            // the song's own mixer, in one go
            let (stems, mix, mut rack) = song_with_fx(sr, frames);
            let (mut want_l, mut want_r) = (vec![0.0f32; sr as usize], vec![0.0f32; sr as usize]);
            mix_into(&stems, &mix, sr, 0, &mut want_l, &mut want_r, Some(&mut rack));
            let dry = {
                let (mut l, mut r) = (vec![0.0f32; 4800], vec![0.0f32; 4800]);
                mix_into(&stems, &mix, sr, 0, &mut l, &mut r, None);
                l
            };
            assert!(want_l[2000..4800].iter().zip(&dry[2000..]).any(|(a, b)| (a - b).abs() > 0.05), "the delay is heard");
            // the deck, at the song's speed, in blocks
            let (stems, mix, rack) = song_with_fx(sr, frames);
            let decks = Decks::default();
            decks.load(0, deck_song(sr, stems, mix, frames, rack));
            let deck = &decks.deck[0];
            let mut p = DeckPlayer::new(sr);
            p.adopt(deck);
            p.command(Cmd::Play);
            let (mut got_l, mut got_r) = (Vec::new(), Vec::new());
            while got_l.len() < want_l.len() {
                let k = (want_l.len() - got_l.len()).min(block);
                p.play(deck, 1.0, sr, k);
                got_l.extend_from_slice(&p.out_l[..k]);
                got_r.extend_from_slice(&p.out_r[..k]);
            }
            let worst = got_l.iter().zip(&want_l).chain(got_r.iter().zip(&want_r)).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(worst < 1e-6, "blocks of {block}: off by {worst}");
        }
    }

    /// A song of one MIDI lane on the built-in bass: (on, off) frames of its one note.
    fn midi_song(sr: u32, seconds: f64, note: (u64, u64)) -> Arc<DeckSong> {
        use crate::synth::{MidiLane, NoteEvent, Sound, Synth, BUILTIN};
        let shader = crate::fx::compile_instrument(BUILTIN[0].2).expect("the bass compiles");
        let synth = Synth::new(shader.clone(), FxControls::new(&shader, &fx("midi1"), None), sr);
        let mut rack = FxRack::new(vec![Vec::new()], Vec::new());
        let events = vec![NoteEvent::note(note.0, 40, 0.9, true), NoteEvent::note(note.1, 40, 0.0, false)];
        rack.midi.push(MidiLane::new(0, events, Sound::Splash(synth)));
        let stems: Stems = Arc::new(vec![Arc::new(Stem::new(Vec::new(), Vec::new()))]);
        let mix = MixParams { version: 1, gains: vec![1.0], ..Default::default() };
        deck_song(sr, stems, mix, (seconds * sr as f64) as usize, rack)
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |p, v| p.max(v.abs()))
    }

    #[test]
    fn a_midi_lane_plays_on_a_deck_at_the_deck_s_speed() {
        let sr = 48000;
        let decks = Decks::default();
        let mut p = Player::new(sr);
        // a note from 0.5 s to 2.5 s of the song
        decks.load(0, midi_song(sr, 10.0, (24000, 120000)));
        decks.xfader.set(0.0);
        decks.deck[0].tempo.set(0.5 + 0.05 / (2.0 * TEMPO_RANGE)); // +5%
        decks.deck[0].send(Cmd::Play);
        let (l, _) = run(&mut p, &decks, 2.0);
        assert!((decks.deck[0].speed.get() - 1.05).abs() < 1e-6);
        // heard from 0.5 s / 1.05
        let on = (24000.0 / 1.05) as usize;
        assert!(peak(&l[..on - 20]) < 1e-5, "silent before the note: {}", peak(&l[..on - 20]));
        assert!(peak(&l[on..on + 480]) > 0.01, "the note starts on time: {}", peak(&l[on..on + 480]));
        // and held all the way: a note let go (a block taken for a jump) would be gone
        for w in l[on + 480..].chunks(2400) {
            assert!(peak(w) > 0.01, "the note sounds on: {}", peak(w));
        }
    }

    #[test]
    fn a_loop_s_end_lets_go_of_the_notes_it_cuts() {
        let sr = 48000;
        let decks = Decks::default();
        let mut p = Player::new(sr);
        // a note from 8.4 s to 8.6 s, and a one-beat loop from 8.0 s: it ends inside the note
        decks.load(0, midi_song(sr, 20.0, (403200, 412800)));
        decks.xfader.set(0.0);
        let d = &decks.deck[0];
        d.send(Cmd::Seek(8.0));
        d.send(Cmd::Loop(1.0));
        d.send(Cmd::Play);
        let (l, _) = run(&mut p, &decks, 2.0);
        assert!(d.loop_on.load(Ordering::Relaxed) && (8.0..8.5).contains(&d.pos.get()));
        // the fourth time round: quiet until the note comes again
        let at = |s: f64| ((1.5 + s) * sr as f64) as usize;
        assert!(peak(&l[at(0.0)..at(0.05)]) > 1e-3, "the cut note's release is heard after the loop's end (rendered again, not replayed)");
        assert!(peak(&l[at(0.2)..at(0.38)]) < 1e-3, "nothing hangs over the loop's end: {}", peak(&l[at(0.2)..at(0.38)]));
        assert!(peak(&l[at(0.41)..at(0.5)]) > 0.01, "the note plays each time round");
    }

    #[test]
    fn the_platter_turned_back_past_what_was_played_still_sounds() {
        let sr = 48000;
        let decks = Decks::default();
        let mut p = Player::new(sr);
        decks.load(0, song(sr, 30.0));
        decks.xfader.set(0.0);
        let d = &decks.deck[0];
        // on a cue, nothing played yet, the hand goes back
        d.send(Cmd::Seek(8.0));
        d.touch.store(true, Ordering::Relaxed);
        d.scratch.fetch_add(-72, Ordering::Relaxed);
        let (l, _) = run(&mut p, &decks, 0.3);
        assert!((8.0 - d.pos.get() - 0.18).abs() < 0.01, "{}", d.pos.get());
        assert!(peak(&l) > 1.0, "the lanes are heard: {}", peak(&l));
    }

    /// A deck with a 440 Hz tone at half scale on it, playing.
    fn tone_deck(sr: u32) -> (Decks, DeckPlayer) {
        let frames = sr as usize * 6;
        let tone: Vec<f32> = (0..frames).map(|i| 0.5 * (i as f32 * std::f32::consts::TAU * 440.0 / sr as f32).sin()).collect();
        let stems: Stems = Arc::new(vec![Arc::new(Stem::new(tone.clone(), tone))]);
        let mix = MixParams { version: 1, gains: vec![1.0], ..Default::default() };
        let decks = Decks::default();
        decks.load(0, deck_song(sr, stems, mix, frames, FxRack::new(vec![Vec::new()], Vec::new())));
        let mut p = DeckPlayer::new(sr);
        p.adopt(&decks.deck[0]);
        p.command(Cmd::Play);
        (decks, p)
    }

    fn play_for(p: &mut DeckPlayer, deck: &Deck, speed: f64, sr: u32, frames: usize) -> Vec<f32> {
        let mut got = Vec::new();
        while got.len() < frames {
            let k = (frames - got.len()).min(441);
            p.play(deck, speed, sr, k);
            got.extend_from_slice(&p.out_l[..k]);
        }
        got
    }

    /// Upward zero crossings: a tone's frequency over a second.
    fn crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    #[test]
    fn key_lock_keeps_the_pitch_at_another_tempo() {
        let sr = 48000;
        // like vinyl: 8% faster is 8% higher
        let (decks, mut p) = tone_deck(sr);
        let got = play_for(&mut p, &decks.deck[0], 1.08, sr, 2 * sr as usize);
        assert!((crossings(&got[sr as usize..]) as i32 - 475).abs() <= 2, "vinyl: {} Hz", crossings(&got[sr as usize..]));
        // locked: the same tempo, the tone where it was
        let (decks, mut p) = tone_deck(sr);
        decks.deck[0].keylock.store(true, Ordering::Relaxed);
        let got = play_for(&mut p, &decks.deck[0], 1.08, sr, 2 * sr as usize);
        assert!((p.pos - 2.0 * 1.08 * sr as f64).abs() < 1.0, "the deck is 8% further: {}", p.pos);
        assert!((crossings(&got[sr as usize..]) as i32 - 440).abs() <= 2, "locked: {} Hz", crossings(&got[sr as usize..]));
        // and as loud all the way: the pieces join without holes or bumps
        for w in got[480..].chunks(480) {
            assert!((0.45..0.56).contains(&peak(w)), "level {}", peak(w));
        }
        // slower too
        let got = play_for(&mut p, &decks.deck[0], 0.92, sr, 2 * sr as usize);
        assert!((crossings(&got[sr as usize..]) as i32 - 440).abs() <= 2, "locked, slower: {} Hz", crossings(&got[sr as usize..]));
    }

    #[test]
    fn key_lock_at_the_song_s_own_tempo_changes_nothing() {
        let sr = 48000;
        let frames = sr as usize * 2;
        let (stems, mix, mut rack) = song_with_fx(sr, frames);
        let (mut want, mut want_r) = (vec![0.0f32; sr as usize], vec![0.0f32; sr as usize]);
        mix_into(&stems, &mix, sr, 0, &mut want, &mut want_r, Some(&mut rack));
        let (stems, mix, rack) = song_with_fx(sr, frames);
        let decks = Decks::default();
        decks.load(0, deck_song(sr, stems, mix, frames, rack));
        decks.deck[0].keylock.store(true, Ordering::Relaxed);
        let mut p = DeckPlayer::new(sr);
        p.adopt(&decks.deck[0]);
        p.command(Cmd::Play);
        let got = play_for(&mut p, &decks.deck[0], 1.0, sr, sr as usize);
        let worst = got.iter().zip(&want).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(worst < 1e-5, "off by {worst}");
    }

    #[test]
    fn key_lock_goes_on_and_off_without_a_click() {
        let sr = 48000;
        let (decks, mut p) = tone_deck(sr);
        let deck = &decks.deck[0];
        let mut got = Vec::new();
        for on in [false, true, false, true, false] {
            deck.keylock.store(on, Ordering::Relaxed);
            got.extend(play_for(&mut p, deck, 1.08, sr, 10007));
        }
        // the hand on the platter takes it out of the lock, and letting go puts it back
        deck.keylock.store(true, Ordering::Relaxed);
        got.extend(play_for(&mut p, deck, 1.08, sr, 10007));
        // a 475 Hz tone at half scale moves at most 0.031 a frame
        let step = got.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(step < 0.06, "a jump of {step} between two frames");
        for w in got[480..].chunks(480) {
            assert!((0.3..0.75).contains(&peak(w)), "level {}", peak(w));
        }
    }

    /// A latent plugin on one lane makes the whole rack late; the deck reads
    /// ahead by that much, so what it plays stays where its position says.
    #[cfg(windows)]
    #[test]
    fn a_deck_stays_on_its_grid_with_a_latent_plugin() {
        let sr = 48000;
        let frames = sr as usize;
        let click = {
            let mut v = vec![0.0f32; frames];
            v[1000] = 1.0;
            Arc::new(Stem::new(v.clone(), v))
        };
        let quiet = Arc::new(Stem::new(vec![0.0; frames], vec![0.0; frames]));
        let stems: Stems = Arc::new(vec![quiet, click]);
        let mix = MixParams { version: 1, gains: vec![1.0, 1.0], ..Default::default() };
        let mut plugin = fx("late");
        plugin.plugin = Some("x".into());
        let slot = FxUnit::plugin("late", FxControls::for_plugin(&plugin), sr);
        let mut rack = FxRack::new(vec![vec![slot], Vec::new()], Vec::new());
        rack.compensate(&|id| if id == "late" { 300 } else { 0 });
        assert_eq!(rack.latency, 300);
        let decks = Decks::default();
        decks.load(0, deck_song(sr, stems, mix, frames, rack));
        let deck = &decks.deck[0];
        let mut p = DeckPlayer::new(sr);
        p.adopt(deck);
        p.command(Cmd::Play);
        let mut got = Vec::new();
        while got.len() < 4000 {
            p.play(deck, 1.0, sr, 500);
            got.extend_from_slice(&p.out_l[..500]);
        }
        assert_eq!(got.iter().position(|x| *x > 0.5), Some(1000), "the click on the lane without the plugin");
    }

    #[test]
    fn a_deck_gives_its_rack_back_with_the_song() {
        let sr = 48000;
        let decks = Decks::default();
        let mut p = Player::new(sr);
        let first = midi_song(sr, 10.0, (0, 4800));
        decks.load(0, first.clone());
        let _ = run(&mut p, &decks, 0.05);
        assert!(first.rack.lock().unwrap().is_none(), "the deck has it");
        let old = decks.load(0, midi_song(sr, 10.0, (0, 4800)));
        assert!(!decks.retired());
        let _ = run(&mut p, &decks, 0.05);
        assert!(decks.retired() && old.is_some());
        assert!(first.rack.lock().unwrap().is_some(), "back in its song, for the UI to drop");
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
    fn the_headphones_hear_a_cued_deck_whatever_the_faders_say() {
        let sr = 48000;
        let decks = Decks::default();
        let mut p = Player::new(sr);
        decks.load(0, song(sr, 30.0));
        decks.load(1, song(sr, 30.0));
        // the crossfader on deck 1, deck 2's fader down, both playing
        decks.xfader.set(0.0);
        decks.deck[1].fader.set(0.0);
        decks.deck[0].send(Cmd::Play);
        decks.deck[1].send(Cmd::Play);
        let mut ears = CueReader::default();
        let mut heard = |p: &mut Player| {
            let (l, _) = run(p, &decks, 0.2);
            // the headphones' next block: from just behind what the decks wrote
            let (mut cl, mut cr) = (vec![0.0f32; 512], vec![0.0f32; 512]);
            ears.pull(&decks.cue, &mut cl, &mut cr);
            (l[l.len() - 1], cl[cl.len() - 1])
        };
        let (master, phones) = heard(&mut p);
        assert!((master - 1.5).abs() < 1e-3 && phones == 0.0, "no cue button on: {master} {phones}");
        // deck 2's cue button: the headphones get it, the master does not
        let map = Map::parse(crate::deckmap::BUILT_IN).unwrap();
        decks.midi(&map, &mut Decoder::default(), [0xb4, 0x47, 0x7f]);
        assert!(decks.deck[1].pfl.load(Ordering::Relaxed));
        let (master, phones) = heard(&mut p);
        assert!((master - 1.5).abs() < 1e-3 && (phones - 1.5).abs() < 1e-3, "deck 2 in the headphones only: {master} {phones}");
        assert!(decks.cue_meter.get() > 1.0);
        // both cue buttons: both decks
        decks.deck[0].pfl.store(true, Ordering::Relaxed);
        let (_, phones) = heard(&mut p);
        assert!((phones - 3.0).abs() < 1e-3, "{phones}");
        assert!(Leds::default().update(&map, &decks).contains(&[0xb4, 0x47, 0x7f]), "the cue button is lit");
    }

    #[test]
    fn the_headphones_read_behind_the_decks_and_survive_their_own_clock() {
        let ring = CueRing::default();
        let mut ears = CueReader::default();
        let ramp = |from: usize, n: usize| -> Vec<f32> { (from..from + n).map(|i| i as f32).collect() };
        let pull = |ears: &mut CueReader, n: usize| {
            let (mut l, mut r) = (vec![9.0f32; n], vec![9.0f32; n]);
            ears.pull(&ring, &mut l, &mut r);
            l
        };
        ring.push(&ramp(0, 4000), &ramp(0, 4000));
        // it starts CUE_BEHIND frames back, and goes on from where it was
        let behind = CUE_BEHIND as usize;
        assert_eq!(pull(&mut ears, 500), ramp(4000 - behind, 500));
        ring.push(&ramp(4000, 500), &ramp(4000, 500));
        assert_eq!(pull(&mut ears, 500), ramp(4500 - behind, 500));
        // faster than the decks: silence for what is not there yet, then back behind them
        let got = pull(&mut ears, 2000);
        assert_eq!(got[..behind - 500], ramp(5000 - behind, behind - 500)[..]);
        assert!(got[behind - 500..].iter().all(|x| *x == 0.0));
        ring.push(&ramp(4500, 3000), &ramp(4500, 3000));
        assert_eq!(pull(&mut ears, 10)[0], (7500 - behind) as f32);
        // slower than the decks: far behind, it skips to its place again
        ring.push(&ramp(7500, 20000), &ramp(7500, 20000));
        assert_eq!(pull(&mut ears, 10)[0], (27500 - behind) as f32);
        // and round the ring's end
        for k in 0..40 {
            ring.push(&ramp(27500 + k * 1000, 1000), &ramp(27500 + k * 1000, 1000));
            assert_eq!(pull(&mut ears, 1000)[999], (27500 + (k + 1) * 1000 - behind + 9) as f32, "block {k}");
        }
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
        decks.midi(&map, &mut dec, [0x91, 0x11, 0x7f]);
        decks.midi(&map, &mut dec, [0x91, 0x11, 0x00]);
        assert!(decks.deck[1].keylock.load(Ordering::Relaxed) && !decks.deck[0].keylock.load(Ordering::Relaxed), "master tempo on the touch panel is key lock");
        decks.midi(&map, &mut dec, [0x96, 0x0c, 0x7f]);
        assert_eq!(decks.unmapped.pop(), Some([0x96, 0x0c, 0x7f]), "unknown: logged");
        decks.midi(&map, &mut dec, [0x86, 0x00, 0x00]);
        decks.midi(&map, &mut dec, [0x90, 0x3f, 0x7f]);
        decks.midi(&map, &mut dec, [0xb4, 0x46, 0x7f]);
        decks.midi(&map, &mut dec, [0xa6, 0x00, 0x40]);
        assert!(decks.unmapped.pop().is_none(), "a known pad's note-off, shift, headphone cue and pad pressure are no news");
    }
}
