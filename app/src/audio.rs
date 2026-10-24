//! Stem loading (makepad-audio-decode: wav/flac/mp3/ogg), waveform peaks, the
//! mixer (shared by live playback and the offline render) and the job worker.
use makepad_micro_serde::*;

use crate::fx::{FxRack, LANE_FRAMES};
use crate::model::{Project, Take};
use makepad_widgets::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Frames per waveform peak block.
pub const PEAK_BLOCK: usize = 256;
/// Region edges fade over this many seconds, in playback and render alike.
const REGION_RAMP_S: f64 = 0.010;
/// A section take fades in and out over this long, inside its margin (where it
/// is still the original), so the seam is inaudible.
pub const TAKE_RAMP_S: f64 = 0.5;
/// Seconds of original kept on each side of a repainted range.
pub const TAKE_MARGIN_S: f64 = 1.0;

/// A song's lanes, in track order. Each stem is shared, so adding lanes copies
/// pointers, not audio.
pub type Stems = Arc<Vec<Arc<Stem>>>;

pub struct Stem {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    /// (min, max) of the mono sum per PEAK_BLOCK frames
    pub peaks: Vec<(f32, f32)>,
    /// largest |peak|, so quiet stems (reverb) still draw a visible waveform
    pub peak_abs: f32,
}

pub fn load_stem(path: &Path, target_sr: u32) -> Result<Stem, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let audio = makepad_audio_decode::decode_any(&bytes).map_err(|e| format!("{}: {e:?}", path.display()))?;
    let ch = audio.channels.max(1) as usize;
    let frames = audio.frames();
    let pcm = &audio.pcm_interleaved_f32;
    let mut l = Vec::with_capacity(frames);
    let mut r = Vec::with_capacity(frames);
    for f in 0..frames {
        l.push(pcm[f * ch]);
        r.push(pcm[f * ch + if ch > 1 { 1 } else { 0 }]);
    }
    if audio.rate != target_sr && audio.rate > 0 {
        l = resample_linear(&l, audio.rate, target_sr);
        r = resample_linear(&r, audio.rate, target_sr);
    }
    Ok(Stem::new(l, r))
}

impl Stem {
    /// A stem from its samples, with its waveform peaks.
    pub fn new(l: Vec<f32>, r: Vec<f32>) -> Stem {
        let peaks: Vec<(f32, f32)> = l
        .chunks(PEAK_BLOCK)
        .zip(r.chunks(PEAK_BLOCK))
        .map(|(a, b)| {
            let (mut mn, mut mx) = (f32::MAX, f32::MIN);
            for (x, y) in a.iter().zip(b) {
                let m = 0.5 * (x + y);
                mn = mn.min(m);
                mx = mx.max(m);
            }
            (mn, mx)
        })
        .collect();
        let peak_abs = peaks.iter().fold(1e-6f32, |acc, (a, b)| acc.max(a.abs()).max(b.abs()));
        Stem { l, r, peaks, peak_abs }
    }

    /// Sample of the take at song frame f when the take starts at `offset`.
    #[inline]
    fn at(&self, f: usize, offset: usize) -> (f32, f32) {
        match f.checked_sub(offset) {
            Some(j) if j < self.l.len() => (self.l[j], self.r[j]),
            _ => (0.0, 0.0),
        }
    }
}

/// Keep a lane take: the lane as heard with the take in at its blend, as one stem.
pub fn bake_lane(lane: &Stem, take: &Stem, k: &MixTake, sr: u32) -> Stem {
    let offset = (k.start.max(0.0) * sr as f64) as usize;
    let (mut l, mut r) = (lane.l.clone(), lane.r.clone());
    for f in 0..l.len().min(r.len()) {
        let w = k.blend * k.weight(f as f64 / sr as f64);
        if w > 0.0 {
            let (tl, tr) = take.at(f, offset);
            l[f] = l[f] * (1.0 - w) + tl * w;
            r[f] = r[f] * (1.0 - w) + tr * w;
        }
    }
    Stem::new(l, r)
}

/// Keep a layer: in place at its blend, as a lane `frames` long.
pub fn place_layer(take: &Stem, k: &MixTake, sr: u32, frames: usize) -> Stem {
    let offset = (k.start.max(0.0) * sr as f64) as usize;
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    for f in 0..frames {
        let w = k.blend * k.weight(f as f64 / sr as f64);
        let (tl, tr) = take.at(f, offset);
        l[f] = tl * w;
        r[f] = tr * w;
    }
    Stem::new(l, r)
}

fn resample_linear(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    let n = (x.len() as f64 * to as f64 / from as f64) as usize;
    let step = from as f64 / to as f64;
    (0..n)
        .map(|i| {
            let p = i as f64 * step;
            let j = p as usize;
            let f = (p - j as f64) as f32;
            let a = x.get(j).copied().unwrap_or(0.0);
            let b = x.get(j + 1).copied().unwrap_or(a);
            a + (b - a) * f
        })
        .collect()
}

#[derive(Clone, Default)]
pub struct MixRegion {
    pub track: usize,
    pub start: f64,
    pub end: f64,
    pub factor: f32,
}

/// How a take sits in the mix (see `poing_model::Take`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TakeKind {
    /// Instead of every lane, for the whole song.
    #[default]
    Song,
    /// Instead of every lane over its range.
    Section,
    /// Instead of lane `track`, by `blend`.
    Lane(usize),
    /// On top, at `blend`, following lane `track`'s level, mute and solo
    /// (`None`: a layer of its own).
    Layer(Option<usize>),
}

/// A take being heard: stem `stem` (after the tracks in the stem set) starts
/// at `start`. Ranges fade over TAKE_RAMP_S where they do not meet the song's
/// edges.
#[derive(Clone, Copy, Debug, Default)]
pub struct MixTake {
    pub stem: usize,
    pub start: f64,
    pub end: f64,
    pub kind: TakeKind,
    pub blend: f32,
    pub fade_in: bool,
    pub fade_out: bool,
}

/// At most this many takes play at once (a fixed array: the audio thread's
/// copy of the mix never allocates for them).
pub const MAX_ACTIVE_TAKES: usize = 8;

/// Everything the mixer needs; rebuilt from the Project whenever it changes.
#[derive(Clone, Default)]
pub struct MixParams {
    pub version: u64,
    pub gains: Vec<f32>,
    pub regions: Vec<MixRegion>,
    pub takes: [Option<MixTake>; MAX_ACTIVE_TAKES],
    /// Only the lanes (with their lane takes and layers) are heard: section
    /// takes still duck the lanes under them but are not added (a stem export).
    pub lanes_only: bool,
}

impl MixParams {
    pub fn from_project(p: &Project, version: u64, active: &[String]) -> Self {
        let regions = p
            .regions()
            .iter()
            .filter_map(|r| {
                let track = p.track_index(&r.track)?;
                let factor = r.factor();
                (factor != 1.0).then(|| MixRegion { track, start: r.start, end: r.end, factor })
            })
            .collect();
        let mut takes = [None; MAX_ACTIVE_TAKES];
        let active = active.iter().filter_map(|id| p.take_index(id)).take(MAX_ACTIVE_TAKES);
        for (slot, i) in takes.iter_mut().zip(active) {
            let t = &p.takes()[i];
            let kind = match t.scope.as_str() {
                "song" => TakeKind::Song,
                "section" => TakeKind::Section,
                "lane" => match t.track.as_deref().and_then(|id| p.track_index(id)) {
                    Some(track) => TakeKind::Lane(track),
                    None => continue,
                },
                _ => TakeKind::Layer(t.track.as_deref().and_then(|id| p.track_index(id))),
            };
            *slot = Some(MixTake {
                stem: p.tracks.len() + i,
                start: t.start,
                end: t.end,
                kind,
                blend: t.blend() as f32,
                fade_in: t.start > 0.01,
                fade_out: t.end < p.duration - 0.01,
            });
        }
        Self { version, gains: p.track_gains(), regions, takes, lanes_only: false }
    }

    pub fn song_take(&self) -> Option<MixTake> {
        self.takes.iter().flatten().find(|k| k.kind == TakeKind::Song).copied()
    }

    /// The ranges of the section takes heard in [t0, t1) seconds.
    pub fn section_takes_between(&self, t0: f64, t1: f64) -> Vec<(f64, f64)> {
        self.takes.iter().flatten().filter(|k| k.kind == TakeKind::Section && k.touches(t0, t1)).map(|k| (k.start, k.end)).collect()
    }

    /// Lane `lane` alone as it sounds in this mix: its level (mute and solo
    /// included), its lane takes and layers; no other lane, no section or
    /// song take.
    pub fn lane_alone(&self, lane: usize) -> MixParams {
        let mut m = self.clone();
        for (i, g) in m.gains.iter_mut().enumerate() {
            if i != lane {
                *g = 0.0;
            }
        }
        for k in m.takes.iter_mut() {
            if k.is_some_and(|k| take_lane(&k) != Some(lane)) {
                *k = None;
            }
        }
        m
    }

    /// Gain of one track at time t (seconds), with short linear ramps at region edges.
    #[inline]
    pub fn gain_at(&self, track: usize, t: f64) -> f32 {
        let mut g = self.gains.get(track).copied().unwrap_or(0.0);
        if g == 0.0 {
            return 0.0;
        }
        for r in &self.regions {
            if r.track != track || t < r.start - REGION_RAMP_S || t > r.end + REGION_RAMP_S {
                continue;
            }
            let a = ((t - r.start) / REGION_RAMP_S + 0.5).clamp(0.0, 1.0);
            let b = ((r.end - t) / REGION_RAMP_S + 0.5).clamp(0.0, 1.0);
            g *= 1.0 + (a * b) as f32 * (r.factor - 1.0);
        }
        g
    }

    /// Gain of lane `track` at t, under the takes that replace it there.
    #[inline]
    fn lane_gain_at(&self, track: usize, t: f64) -> f32 {
        let mut g = self.gain_at(track, t);
        for k in self.takes.iter().flatten() {
            match k.kind {
                TakeKind::Section => g *= 1.0 - k.weight(t),
                TakeKind::Lane(lane) if lane == track => g *= 1.0 - k.blend * k.weight(t),
                _ => {}
            }
        }
        g
    }
}

impl MixTake {
    /// 0..1: how present the take's range is at t (ramps inside its edges).
    #[inline]
    fn weight(&self, t: f64) -> f32 {
        if t < self.start || t > self.end {
            return 0.0;
        }
        let a = if self.fade_in { ((t - self.start) / TAKE_RAMP_S).min(1.0) } else { 1.0 };
        let b = if self.fade_out { ((self.end - t) / TAKE_RAMP_S).min(1.0) } else { 1.0 };
        (a * b) as f32
    }

    fn touches(&self, t0: f64, t1: f64) -> bool {
        self.end > t0 && self.start < t1
    }
}

/// Gain of take `k` at t, following the lane it belongs to.
#[inline]
fn take_gain(mix: &MixParams, k: &MixTake, t: f64) -> f32 {
    match k.kind {
        TakeKind::Section => k.weight(t),
        // A lane take or a lane's layer follows its lane's level, mute and solo.
        TakeKind::Lane(lane) | TakeKind::Layer(Some(lane)) => k.blend * k.weight(t) * mix.gain_at(lane, t),
        TakeKind::Layer(None) => k.blend * k.weight(t),
        TakeKind::Song => 1.0,
    }
}

/// The lane a take plays on, if it belongs to one.
pub(crate) fn take_lane(k: &MixTake) -> Option<usize> {
    match k.kind {
        TakeKind::Lane(lane) | TakeKind::Layer(Some(lane)) => Some(lane),
        _ => None,
    }
}

/// Mix frames [start, start + out_l.len()) into out_l / out_r (overwrites):
/// each lane with its lane takes and layers, through the lane's effects, then
/// the master effects. Section takes join after the lane effects (they stand
/// in for the whole mix). Resampling to the device rate is the caller's business.
/// Meters: one per lane (by track index) and the master, last.
pub const METER_LANES: usize = 32;
pub const MASTER_METER: usize = METER_LANES;

/// Raise meter `i` to `peak` (positive floats order like their bits).
#[inline]
fn meter(meters: Option<&[AtomicU32]>, i: usize, peak: f32) {
    if let Some(m) = meters.and_then(|m| m.get(i)) {
        m.fetch_max(peak.to_bits(), Ordering::Relaxed);
    }
}

fn peak_of(l: &[f32], r: &[f32]) -> f32 {
    l.iter().chain(r.iter()).fold(0.0f32, |p, x| p.max(x.abs()))
}

#[allow(clippy::too_many_arguments)]
pub fn mix_into(
    stems: &[Arc<Stem>],
    mix: &MixParams,
    sr: u32,
    start: usize,
    out_l: &mut [f32],
    out_r: &mut [f32],
    rack: Option<&mut FxRack>,
) {
    mix_metered(stems, mix, sr, start, out_l, out_r, rack, None)
}

/// Frames [0, frames) of the mix through `rack` (an offline render or export)
/// with the plugins' latency taken out: rendered that much longer and the
/// front dropped, so it lines up with the song (and every stem with the others).
pub fn render_aligned(stems: &[Arc<Stem>], mix: &MixParams, sr: u32, frames: usize, rack: &mut FxRack) -> (Vec<f32>, Vec<f32>) {
    // a song take plays through the master only
    let lanes = if mix.song_take().is_some() { 0 } else { rack.latency };
    let late = (lanes + rack.master_latency()) as usize;
    rack.clear_delays();
    let (mut l, mut r) = (vec![0f32; frames + late], vec![0f32; frames + late]);
    mix_into(stems, mix, sr, 0, &mut l, &mut r, Some(rack));
    l.drain(..late);
    r.drain(..late);
    (l, r)
}

/// [`mix_into`], raising `meters` (lanes then master) to this block's peaks.
#[allow(clippy::too_many_arguments)]
pub fn mix_metered(
    stems: &[Arc<Stem>],
    mix: &MixParams,
    sr: u32,
    start: usize,
    out_l: &mut [f32],
    out_r: &mut [f32],
    mut rack: Option<&mut FxRack>,
    meters: Option<&[AtomicU32]>,
) {
    let n = out_l.len().min(out_r.len());
    if n > LANE_FRAMES && rack.is_some() {
        // in blocks the lane buffers hold
        let mut done = 0;
        while done < n {
            let k = (n - done).min(LANE_FRAMES);
            mix_metered(stems, mix, sr, start + done, &mut out_l[done..done + k], &mut out_r[done..done + k], rack.as_deref_mut(), meters);
            done += k;
        }
        return;
    }
    out_l[..n].fill(0.0);
    out_r[..n].fill(0.0);
    let inv_sr = 1.0 / sr as f64;
    let t0 = start as f64 * inv_sr;
    let t1 = (start + n) as f64 * inv_sr;
    // Adds a stem at a gain; returns the peak it added (for the meters).
    let add = |out_l: &mut [f32], out_r: &mut [f32], s: &Stem, offset: usize, gain: &dyn Fn(f64) -> f32| -> f32 {
        let mut peak = 0.0f32;
        for i in 0..out_l.len() {
            let f = start + i;
            if f < offset || f - offset >= s.l.len() {
                continue;
            }
            let g = gain(f as f64 * inv_sr);
            let (a, b) = (s.l[f - offset] * g, s.r[f - offset] * g);
            out_l[i] += a;
            out_r[i] += b;
            peak = peak.max(a.abs()).max(b.abs());
        }
        peak
    };
    let offset_of = |k: &MixTake| (k.start.max(0.0) * sr as f64) as usize;
    // A song take is the whole song: only it plays (through the master).
    if let Some(k) = mix.song_take() {
        if let Some(s) = stems.get(k.stem) {
            let _ = add(&mut out_l[..n], &mut out_r[..n], s, 0, &|_| 1.0);
        }
        if let Some(rack) = rack {
            rack.process_master(&mut out_l[..n], &mut out_r[..n]);
        }
        meter(meters, MASTER_METER, peak_of(&out_l[..n], &out_r[..n]));
        return;
    }
    // latent plugins: the lanes through the rack gather in its wet sum, the rest waits in out
    if let Some(rk) = rack.as_deref_mut().filter(|r| r.latency > 0) {
        rk.wet_l[..n].fill(0.0);
        rk.wet_r[..n].fill(0.0);
    }
    let tracks = mix.gains.len().min(stems.len());
    for (ti, s) in stems[..tracks].iter().enumerate() {
        if mix.gains[ti] == 0.0 {
            continue;
        }
        let varies = mix.regions.iter().any(|r| r.track == ti)
            || mix.takes.iter().flatten().any(|k| matches!(k.kind, TakeKind::Section) || k.kind == TakeKind::Lane(ti));
        let flat = mix.gains[ti];
        let lane_gain = |t: f64| if varies { mix.lane_gain_at(ti, t) } else { flat };
        // A MIDI lane: its instrument plays the notes into the lane buffer.
        if let Some(rk) = rack.as_deref_mut().filter(|r| r.has_midi(ti)) {
            rk.render_midi(ti, start as u64, n);
            for i in 0..n {
                let g = lane_gain((start + i) as f64 * inv_sr);
                rk.lane_l[i] *= g;
                rk.lane_r[i] *= g;
            }
            rk.process_lane(ti, n);
            meter(meters, ti, peak_of(&rk.lane_l[..n], &rk.lane_r[..n]));
            rk.lane_out(ti, n, out_l, out_r);
            continue;
        }
        match rack.as_deref_mut().filter(|r| r.lane_has_fx(ti)) {
            Some(rk) => {
                // The lane and its takes, through the lane's effects.
                let (ll, lr) = (&mut rk.lane_l[..n], &mut rk.lane_r[..n]);
                ll.fill(0.0);
                lr.fill(0.0);
                let _ = add(ll, lr, s, 0, &lane_gain);
                for k in mix.takes.iter().flatten().filter(|k| take_lane(k) == Some(ti) && k.touches(t0, t1)) {
                    if let Some(ts) = stems.get(k.stem) {
                        let _ = add(ll, lr, ts, offset_of(k), &|t| take_gain(mix, k, t));
                    }
                }
                rk.process_lane(ti, n);
                meter(meters, ti, peak_of(&rk.lane_l[..n], &rk.lane_r[..n]));
                rk.lane_out(ti, n, out_l, out_r);
            }
            None => {
                let peak = add(&mut out_l[..n], &mut out_r[..n], s, 0, &lane_gain);
                meter(meters, ti, peak);
            }
        }
    }
    let routed = |k: &MixTake| take_lane(k).is_some_and(|lane| rack.as_ref().is_some_and(|r| r.lane_has_fx(lane)));
    for k in mix.takes.iter().flatten().filter(|k| k.touches(t0, t1) && !routed(k)) {
        if k.kind == TakeKind::Song || (mix.lanes_only && take_lane(k).is_none()) {
            continue;
        }
        if let Some(s) = stems.get(k.stem) {
            let peak = add(&mut out_l[..n], &mut out_r[..n], s, offset_of(k), &|t| take_gain(mix, k, t));
            if let Some(lane) = take_lane(k) {
                meter(meters, lane, peak);
            }
        }
    }
    if let Some(rack) = rack {
        rack.join_wet(&mut out_l[..n], &mut out_r[..n]);
        rack.process_master(&mut out_l[..n], &mut out_r[..n]);
    }
    meter(meters, MASTER_METER, peak_of(&out_l[..n], &out_r[..n]));
}
/// The song stopped: only MIDI lane `lane`'s instrument (what the keys play),
/// at the lane's level, through its effects and the master, into l/r (overwrites).
pub fn mix_live(rack: &mut FxRack, mix: &MixParams, lane: usize, out_l: &mut [f32], out_r: &mut [f32], meters: Option<&[AtomicU32]>) {
    let n = out_l.len().min(out_r.len());
    let gain = mix.gains.get(lane).copied().unwrap_or(1.0);
    let mut done = 0;
    while done < n {
        let k = (n - done).min(LANE_FRAMES);
        rack.render_live(lane, k);
        for i in 0..k {
            rack.lane_l[i] *= gain;
            rack.lane_r[i] *= gain;
        }
        rack.process_lane(lane, k);
        out_l[done..done + k].copy_from_slice(&rack.lane_l[..k]);
        out_r[done..done + k].copy_from_slice(&rack.lane_r[..k]);
        meter(meters, lane, peak_of(&rack.lane_l[..k], &rack.lane_r[..k]));
        rack.process_master(&mut out_l[done..done + k], &mut out_r[done..done + k]);
        meter(meters, MASTER_METER, peak_of(&out_l[done..done + k], &out_r[done..done + k]));
        done += k;
    }
}

/// State shared with the audio callback: transport flags and the published mix.
/// The callback only `try_lock`s `mix` and keeps its own copy when the UI holds it.
pub struct Shared {
    pub playing: AtomicBool,
    pub pos: AtomicU64,
    pub mix: Mutex<MixParams>,
    /// Swapped by generation: the UI publishes a new set and keeps the old one
    /// until `stems_seen` shows the callback switched, so a stem set's final
    /// drop never happens on the audio thread.
    pub stems: Mutex<Stems>,
    pub stems_gen: AtomicU64,
    pub stems_seen: AtomicU64,
    /// A new effects rack for the callback to adopt.
    pub fx_in: Mutex<Option<Box<FxRack>>>,
    /// The rack it replaced, for the UI to drop.
    pub fx_out: Mutex<Option<Box<FxRack>>>,
    /// Peak levels since the UI last read them (f32 bits): lanes, then master.
    pub meters: [AtomicU32; METER_LANES + 1],
    /// Loop playback over [loop_a, loop_b) frames while loop_on.
    pub loop_on: AtomicBool,
    pub loop_a: AtomicU64,
    pub loop_b: AtomicU64,
    /// Keys played live into a MIDI lane, and recording them.
    pub live: crate::live::Live,
}

impl Shared {
    pub fn new(mix: MixParams, stems: Stems) -> Self {
        Self {
            playing: AtomicBool::new(false),
            pos: AtomicU64::new(0),
            mix: Mutex::new(mix),
            stems: Mutex::new(stems),
            stems_gen: AtomicU64::new(0),
            stems_seen: AtomicU64::new(0),
            fx_in: Mutex::new(None),
            fx_out: Mutex::new(None),
            meters: std::array::from_fn(|_| AtomicU32::new(0)),
            loop_on: AtomicBool::new(false),
            loop_a: AtomicU64::new(0),
            loop_b: AtomicU64::new(0),
            live: crate::live::Live::default(),
        }
    }
    pub fn pos(&self) -> usize {
        self.pos.load(Ordering::Relaxed) as usize
    }
    pub fn seek(&self, frame: usize) {
        self.pos.store(frame as u64, Ordering::Relaxed)
    }
    pub fn playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }
    pub fn set_playing(&self, v: bool) {
        self.playing.store(v, Ordering::Relaxed)
    }
}

/// Install the stem player on audio output 0. Owns its scratch buffers; the hot
/// path neither allocates (after the first callbacks size them) nor blocks.
/// `POING_MUTE` keeps agent/test instances silent.
pub fn start_output(cx: &mut Cx, shared: Arc<Shared>, sr: u32) {
    let muted = std::env::var_os("POING_MUTE").is_some();
    let mut cache = MixParams::default();
    let mut stems: Stems = shared.stems.lock().map(|s| s.clone()).unwrap_or_default();
    let mut stems_gen = shared.stems_gen.load(Ordering::Acquire);
    let (mut l, mut r) = (Vec::<f32>::with_capacity(8192), Vec::<f32>::with_capacity(8192));
    let mut rack = Box::new(FxRack::empty());
    let mut was_playing = false;
    let mut last_live: Option<usize> = None;
    let mut click = crate::live::Click::new(sr);
    // where the last block ended: playback from anywhere else is a jump
    let mut expected = usize::MAX;
    cx.audio_output(0, move |info, out| {
        out.zero();
        // A new effects rack: adopt the old one's state, hand the old one back
        // to the UI to drop (only when the return slot is free: no allocation,
        // no drop here).
        if let Ok(mut back) = shared.fx_out.try_lock() {
            if back.is_none() {
                if let Some(mut new) = shared.fx_in.try_lock().ok().and_then(|mut s| s.take()) {
                    new.adopt(&mut rack);
                    *back = Some(std::mem::replace(&mut rack, new));
                }
            }
        }
        if let Ok(m) = shared.mix.try_lock() {
            if m.version != cache.version || cache.gains.len() != m.gains.len() {
                cache.clone_from(&m);
            }
        }
        let gen = shared.stems_gen.load(Ordering::Acquire);
        if gen != stems_gen {
            if let Ok(s) = shared.stems.try_lock() {
                // The UI still holds the previous set: never its last reference here.
                stems = s.clone();
                stems_gen = gen;
                shared.stems_seen.store(gen, Ordering::Release);
            }
        }
        // The song ends with its lanes, or with the song take being heard.
        let lanes = stems.iter().take(cache.gains.len()).map(|s| s.l.len()).max().unwrap_or(0);
        let total = match cache.song_take() {
            Some(k) => stems.get(k.stem).map_or(lanes, |s| s.l.len()),
            None => lanes,
        };
        let ratio = sr as f64 / info.sample_rate.max(1.0);
        let dev_frames = out.frame_count();
        // Keys played live go to the selected MIDI lane's instrument; a lane
        // that loses them lets go of what they hold.
        let live_lane = shared.live.lane().filter(|l| rack.has_midi(*l));
        if live_lane != last_live {
            if let Some(old) = last_live {
                rack.live_release(old);
            }
            last_live = live_lane;
        }
        let counting = shared.playing() && shared.live.counting_in();
        let n_src = if !shared.playing() || counting {
            was_playing = false;
            // Stopped, or counting in: only what the keys play, on their lane.
            let pos = shared.pos() as u64;
            while let Some(k) = shared.live.keys.pop() {
                if let Some(lane) = live_lane {
                    rack.live_key(lane, k.pitch, k.vel, k.on);
                }
                shared.live.heard(pos, k);
            }
            if live_lane.is_none() && !counting {
                return;
            }
            let n_src = ((dev_frames as f64 * ratio).ceil() as usize).max(1);
            l.resize(n_src, 0.0);
            r.resize(n_src, 0.0);
            match live_lane {
                Some(lane) => mix_live(&mut rack, &cache, lane, &mut l, &mut r, Some(&shared.meters)),
                None => {
                    l.fill(0.0);
                    r.fill(0.0);
                }
            }
            if counting {
                shared.live.count(&mut click, &mut l, &mut r);
            }
            n_src
        } else {
            if !was_playing {
                // resumed: notes held when it paused are let go
                rack.midi_jumped();
                was_playing = true;
            }
            let mut start = shared.pos();
            // A loop: wrap back to its start; the block is cut at its end below.
            let looping = shared.loop_on.load(Ordering::Relaxed);
            let (loop_a, loop_b) = (shared.loop_a.load(Ordering::Relaxed) as usize, shared.loop_b.load(Ordering::Relaxed) as usize);
            let looping = looping && loop_b > loop_a + 64 && loop_b <= total;
            if looping && (start >= loop_b || start < loop_a) {
                start = loop_a;
            }
            if start >= total {
                shared.set_playing(false);
                return;
            }
            if start != expected {
                // a seek: what the latency delay lines hold is from elsewhere
                rack.clear_delays();
            }
            // the keys are heard from this block's first frame
            while let Some(k) = shared.live.keys.pop() {
                if let Some(lane) = live_lane {
                    rack.live_key(lane, k.pitch, k.vel, k.on);
                }
                shared.live.heard(start as u64, k);
            }
            let clicking = shared.live.state() == crate::live::RECORDING;
            // Device frames map 1:1 to stem frames only at the stem rate; at another
            // device rate step through the stems by the ratio (nearest frame, fine
            // for monitoring; the render is exact).
            let n_src = ((dev_frames as f64 * ratio).ceil() as usize).min(total - start);
            l.resize(n_src, 0.0);
            r.resize(n_src, 0.0);
            let mut next = start + n_src;
            if looping && start + n_src > loop_b {
                // up to the loop end, then on from its start
                let first = loop_b - start;
                mix_metered(&stems, &cache, sr, start, &mut l[..first], &mut r[..first], Some(&mut rack), Some(&shared.meters));
                mix_metered(&stems, &cache, sr, loop_a, &mut l[first..], &mut r[first..], Some(&mut rack), Some(&shared.meters));
                if clicking {
                    click.beats(&rack.clicks, start as u64, &mut l[..first], &mut r[..first]);
                    click.beats(&rack.clicks, loop_a as u64, &mut l[first..], &mut r[first..]);
                }
                next = loop_a + (n_src - first);
            } else {
                mix_metered(&stems, &cache, sr, start, &mut l, &mut r, Some(&mut rack), Some(&shared.meters));
                if clicking {
                    click.beats(&rack.clicks, start as u64, &mut l, &mut r);
                }
            }
            shared.seek(next);
            expected = next;
            n_src
        };
        if !muted {
            let chans = out.channel_count();
            for i in 0..dev_frames {
                let j = ((i as f64 * ratio) as usize).min(n_src.saturating_sub(1));
                if n_src == 0 {
                    break;
                }
                if chans >= 2 {
                    out.channel_mut(0)[i] = l[j];
                    out.channel_mut(1)[i] = r[j];
                } else {
                    out.channel_mut(0)[i] = 0.5 * (l[j] + r[j]);
                }
            }
        }
    });
}

// ---------------------------------------------------------------- jobs worker

pub enum Job {
    Render { stems: Stems, mix: MixParams, rack: Box<FxRack>, sr: u32, out_dir: PathBuf, name: String },
    /// Understand a song: run the analysis sidecar on `audio`, writing into the
    /// `bundle`, then decode the new stems at `sr`.
    Analyze { audio: PathBuf, bundle: PathBuf, sr: u32 },
    /// Render a take with native YuE2 from `fixture` into `bundle/<take.file>`.
    Generate { bundle: PathBuf, take: Take, fixture: Yue2Fixture, sr: u32 },
    /// Repaint [start, end] of `src` with ACE-Step, one version per take (its
    /// seed, into `bundle/<take.file>`), the model loaded once.
    Repaint { bundle: PathBuf, takes: Vec<Take>, src: PathBuf, job: RepaintJob, sr: u32 },
    /// Split `lane` into dry voice and echo; the dry voice is the take.
    Dereverb { bundle: PathBuf, take: Take, lane: PathBuf, sr: u32 },
    /// The language model writes a MIDI part (its role) over a range of grid beats of the song.
    WriteMidi { bundle: PathBuf, request: String, role: String, sheet: String, bars: crate::midi_writer::Bars, range: (f64, f64) },
    /// The language model writes an effect for a lane (None: the master).
    WriteFx { bundle: PathBuf, lane: Option<usize>, request: String, context: String },
    /// A new part: lego plays it, then it is cleaned and fitted into the gaps.
    Layer { bundle: PathBuf, take: Take, lego: LegoJob, clean: LayerJob, sr: u32 },
    /// A new song from a prompt: the language model writes its sheet, ACE-Step sings
    /// `count` versions into a new bundle in `dir`.
    Song { prompt: String, dir: PathBuf, count: usize, sr: u32 },
    /// The singing half of `Song`: ACE-Step sings `count` versions of the
    /// sheet.json already in `bundle` (what poing-node runs for a new song).
    Sing { bundle: PathBuf, count: usize, seeds: Vec<u64>, sr: u32 },
    /// Remixes of the song in `bundle` (ACE-Step cover), one song take each.
    Remix { bundle: PathBuf, takes: Vec<Take>, job: SongJob, sr: u32 },
    /// Stems, MIDI files, a DAWproject or the DJ export (see `export`).
    Export(Box<crate::export::ExportJob>),
}

/// What `sidecar/acestep_song.py` reads.
#[derive(SerJson, DeJson, Clone, Default)]
pub struct SongJob {
    pub task: String,
    pub out_dir: String,
    pub count: usize,
    pub caption: String,
    pub lyrics: String,
    pub language: Option<String>,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub duration: Option<f64>,
    pub src: Option<String>,
    pub strengths: Option<Vec<f64>>,
    pub seeds: Option<Vec<u64>>,
    pub thinking: bool,
}

/// What `sidecar/acestep_repaint.py` reads (src, out, start and end are filled in by the job).
#[derive(SerJson, DeJson, Clone, Default)]
pub struct RepaintJob {
    pub src: String,
    pub out: String,
    pub start: f64,
    pub end: f64,
    pub margin: f64,
    pub caption: String,
    pub lyrics: String,
    pub seed: u64,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    /// Several versions: one seed and output file each (filled in by the job).
    pub seeds: Option<Vec<u64>>,
    pub outs: Option<Vec<String>>,
}

/// What `yue2_render --fixture` reads: style, section-tagged lyrics, seed,
/// chain of thought ("full" keeps the score's chords) and the ABC score.
#[derive(SerJson, DeJson, Clone)]
pub struct Yue2Fixture {
    pub style: String,
    pub lyrics: String,
    pub seed: u64,
    pub cot: String,
    pub abc: Option<String>,
}

/// Progress and results the worker sends to the UI (after each send it raises
/// the UI signal; the App drains the channel on `Event::Signal`).
pub enum JobUpdate {
    Status(String),
    Done(String),
    /// Progress of the job for `bundle`, 0..1 over the whole job.
    Progress { bundle: PathBuf, message: String, progress: f32 },
    /// The analysis or take for `bundle` failed.
    Failed { bundle: PathBuf, message: String },
    /// analysis.json is written and its stems decoded: (track id, file, stem).
    Analyzed { bundle: PathBuf, stems: Vec<(String, String, Stem)> },
    /// A take finished rendering and is decoded at the project rate.
    TakeReady { bundle: PathBuf, take: Take, stem: Stem },
    /// A MIDI part Claude wrote, checked and snapped to the grid.
    MidiReady { bundle: PathBuf, name: String, role: String, notes: Vec<poing_model::Note>, request: String, note: String },
    /// A job for `bundle` is done (its status line says what came of it).
    Finished { bundle: PathBuf, message: String },
    /// The worker finished job number `seq` (whatever it was): sent after its last update.
    JobDone { seq: u64 },
    /// A song made from a prompt has its first version: open it.
    SongReady { project: PathBuf },
    /// Progress of a job that has no song open yet (a new song being made).
    Starting { message: String, progress: f32 },
    /// An effect Claude wrote and Poing checked, for lane `lane` (None: master).
    FxReady { bundle: PathBuf, lane: Option<usize>, name: String, code: String, request: String, note: String },
}

pub struct Jobs {
    tx: Sender<Job>,
    pub rx: Receiver<JobUpdate>,
    /// Jobs submitted so far: job n is the n-th, and the worker answers each
    /// with `JobDone { seq: n }` when it is finished.
    submitted: AtomicU64,
}

impl Jobs {
    /// One long-lived worker for the app's lifetime, fed over a channel.
    pub fn start(cx: &Cx) -> Jobs {
        let (tx, job_rx) = std::sync::mpsc::channel::<Job>();
        let (up_tx, rx) = std::sync::mpsc::channel::<JobUpdate>();
        let _ = cx
            .thread_spawner()
            .spawn_worker(ThreadOptions { name: Some("poing-jobs".into()), ..Default::default() }, move || {
                let mut seq = 0u64;
                while let Ok(job) = job_rx.recv() {
                    seq += 1;
                    // GPU jobs go to the Windows PC when one is chosen (crate::remote)
                    if let Some(job) = crate::remote::route(job, &up_tx) {
                        run_job(job, &up_tx);
                    }
                    send(&up_tx, JobUpdate::JobDone { seq });
                }
            })
            .map(|h| h.detach());
        Jobs { tx, rx, submitted: AtomicU64::new(0) }
    }

    pub fn submit(&self, job: Job) -> bool {
        let ok = self.tx.send(job).is_ok();
        if ok {
            self.submitted.fetch_add(1, Ordering::SeqCst);
        }
        ok
    }

    /// The sequence number of the last job submitted.
    pub fn last_seq(&self) -> u64 {
        self.submitted.load(Ordering::SeqCst)
    }
}

/// Run one job here, sending its updates to `tx` (the app's worker, or poing-node).
pub fn run_job(job: Job, tx: &Sender<JobUpdate>) {
    // ACE-Step, YuE2 and the analysis want the whole GPU: the language model
    // is loaded again when it is next asked (Song releases it after its sheet)
    if !matches!(job, Job::Render { .. } | Job::WriteFx { .. } | Job::WriteMidi { .. } | Job::Song { .. } | Job::Export(_)) {
        crate::llm::release();
    }
    match job {
        Job::Render { stems, mix, rack, sr, out_dir, name } => render(&stems, &mix, rack, sr, &out_dir, &name, tx),
        Job::Analyze { audio, bundle, sr } => analyze(&audio, &bundle, sr, tx),
        Job::Generate { bundle, take, fixture, sr } => generate(&bundle, take, &fixture, sr, tx),
        Job::Repaint { bundle, takes, src, job, sr } => repaint(&bundle, takes, &src, job, sr, tx),
        Job::Dereverb { bundle, take, lane, sr } => dereverb(&bundle, take, &lane, sr, tx),
        Job::Layer { bundle, take, lego, clean, sr } => layer(&bundle, take, lego, clean, sr, tx),
        Job::WriteFx { bundle, lane, request, context } => write_fx(&bundle, lane, request, &context, tx),
        Job::WriteMidi { bundle, request, role, sheet, bars, range } => write_midi(&bundle, request, &role, &sheet, bars, range, tx),
        Job::Song { prompt, dir, count, sr } => make_song(&prompt, &dir, count, sr, tx),
        Job::Sing { bundle, count, seeds, sr } => sing_song(&bundle, count, seeds, sr, tx),
        Job::Remix { bundle, takes, job, sr } => remix(&bundle, takes, job, sr, tx),
        Job::Export(job) => crate::export::run(*job, tx),
    }
}

pub(crate) fn send(tx: &Sender<JobUpdate>, u: JobUpdate) {
    let _ = tx.send(u);
    SignalToUI::set_ui_signal();
}

pub(crate) fn ffmpeg() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from("ffmpeg")];
    if let Ok(p) = std::env::var("FFMPEG") {
        candidates.insert(0, PathBuf::from(p));
    }
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        candidates.push(Path::new(&la).join("Microsoft").join("WinGet").join("Links").join("ffmpeg.exe"));
    }
    candidates.into_iter().find(|c| Command::new(c).arg("-version").output().map(|o| o.status.success()).unwrap_or(false))
}

/// 24-bit stereo PCM WAV.
pub fn write_wav(path: &Path, l: &[f32], r: &[f32], sr: u32, gain: f32) -> std::io::Result<()> {
    std::fs::write(path, wav_bytes(l, r, sr, gain))
}

/// A 24-bit stereo PCM WAV file's bytes.
pub fn wav_bytes(l: &[f32], r: &[f32], sr: u32, gain: f32) -> Vec<u8> {
    let frames = l.len().min(r.len());
    let data_len = (frames * 2 * 3) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&sr.to_le_bytes());
    b.extend_from_slice(&(sr * 6).to_le_bytes());
    b.extend_from_slice(&6u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        for x in [l[i], r[i]] {
            let v = ((x * gain).clamp(-1.0, 1.0) * 8_388_607.0) as i32;
            b.extend_from_slice(&v.to_le_bytes()[..3]);
        }
    }
    b
}

/// Full mix -> 24-bit WAV, then (when ffmpeg exists) two-pass loudnorm to
/// -14 LUFS / -1 dBTP as a 320k MP3.
pub fn render(stems: &[Arc<Stem>], mix: &MixParams, mut rack: Box<FxRack>, sr: u32, out_dir: &Path, name: &str, tx: &Sender<JobUpdate>) {
    send(tx, JobUpdate::Status("Rendering mix...".into()));
    // The song as it plays: its lanes, or the song take being heard.
    let lanes = stems.iter().take(mix.gains.len()).map(|s| s.l.len()).max().unwrap_or(0);
    // a take past the lanes' end (one that made the song longer) is heard to its end
    let takes = mix.takes.iter().flatten().filter(|k| k.kind != TakeKind::Song).map(|k| (k.end * sr as f64) as usize).max().unwrap_or(0);
    let lanes = lanes.max(takes);
    let frames = match mix.song_take() {
        Some(k) => stems.get(k.stem).map_or(lanes, |s| s.l.len()),
        None => lanes,
    };
    let (l, r) = render_aligned(stems, mix, sr, frames, &mut rack);
    let peak = l.iter().chain(r.iter()).fold(0f32, |a, x| a.max(x.abs()));
    let norm = if peak > 0.99 { 0.99 / peak } else { 1.0 };
    let _ = std::fs::create_dir_all(out_dir);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let wav = out_dir.join(format!("mix-{stamp}.wav"));
    if let Err(e) = write_wav(&wav, &l, &r, sr, norm) {
        return send(tx, JobUpdate::Done(format!("Render failed: {e}")));
    }
    let Some(ff) = ffmpeg() else {
        return send(tx, JobUpdate::Done(format!("Rendered {} (no ffmpeg for MP3)", wav.display())));
    };
    send(tx, JobUpdate::Status("Mastering MP3 (loudness pass 1/2)...".into()));
    let target = "I=-14:TP=-1:LRA=11";
    let probe = Command::new(&ff)
        .args(["-hide_banner", "-i"])
        .arg(&wav)
        .args(["-af", &format!("loudnorm={target}:print_format=json"), "-f", "null", "-"])
        .output();
    let measured = probe.ok().and_then(|o| {
        let err = String::from_utf8_lossy(&o.stderr).to_string();
        let s = err.rfind('{')?;
        let e = err[s..].find('}')? + s + 1;
        Some(err[s..e].to_string())
    });
    let Some(m) = measured else {
        return send(tx, JobUpdate::Done(format!("Rendered {} (loudness measurement failed)", wav.display())));
    };
    // ffmpeg prints `"input_i" : "-13.10",` lines; pick the values out without a JSON dependency.
    let field = |k: &str| -> String {
        m.find(&format!("\"{k}\""))
            .and_then(|i| m[i..].split('"').nth(3).map(|v| v.to_string()))
            .unwrap_or_else(|| "0".into())
    };
    let af = format!(
        "loudnorm={target}:measured_I={}:measured_TP={}:measured_LRA={}:measured_thresh={}:offset={}:linear=true",
        field("input_i"),
        field("input_tp"),
        field("input_lra"),
        field("input_thresh"),
        field("target_offset")
    );
    send(tx, JobUpdate::Status("Mastering MP3 (pass 2/2)...".into()));
    let mp3 = wav.with_extension("mp3");
    let ok = Command::new(&ff)
        .args(["-v", "error", "-y", "-i"])
        .arg(&wav)
        .args(["-af", &af, "-ar", "44100", "-codec:a", "libmp3lame", "-b:a", "320k", "-metadata"])
        .arg(format!("title={name}"))
        .arg(&mp3)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    send(
        tx,
        JobUpdate::Done(if ok {
            format!("Rendered {} (input {} LUFS -> -14)", mp3.display(), field("input_i"))
        } else {
            format!("Rendered {} (MP3 encode failed)", wav.display())
        }),
    );
}

// ------------------------------------------------------------ analysis sidecar

use crate::paths::{self, sidecar_dir, Runtime};

/// One progress line the sidecar prints on stdout.
#[derive(makepad_micro_serde::DeJson)]
struct SidecarLine {
    stage: String,
    progress: Option<f64>,
    message: Option<String>,
    /// Repaint: where the written range starts and ends in the song.
    start: Option<f64>,
    end: Option<f64>,
    /// A finished version (acestep_song.py): its number and file.
    index: Option<usize>,
    path: Option<String>,
}

/// Understand `audio` into `bundle`, then decode the stems. Stems, beats and
/// lyrics come from the native `poing-engine` when it can run here (its models
/// installed, a GPU store); the Python sidecar then adds only key, chords and
/// sections. Without the engine, the sidecar does it all. Both write their
/// stderr to `bundle/analysis.log`.
fn analyze(audio: &Path, bundle: &Path, sr: u32, tx: &Sender<JobUpdate>) {
    let progress =
        |message: &str, progress: f32| send(tx, JobUpdate::Progress { bundle: bundle.to_path_buf(), message: message.into(), progress });
    let fail = |message: String| send(tx, JobUpdate::Failed { bundle: bundle.to_path_buf(), message });
    progress("Starting analysis", 0.0);
    let log_path = bundle.join("analysis.log");
    let log = match std::fs::File::create(&log_path) {
        Ok(f) => f,
        Err(e) => return fail(format!("Analysis failed: {e}")),
    };
    let note = |text: &str| {
        use std::io::Write;
        let _ = writeln!(&log, "poing: {text}");
    };
    // Native stems, beats and lyrics take the first 70% of the bar.
    let native = match engine_command() {
        Some(mut cmd) => {
            cmd.arg("analyze").arg(audio).arg(bundle);
            run_lines(cmd, &log, (0.0, 0.7), &progress)
        }
        None => Ok(LinesOutcome::Unavailable("poing-engine not found".into())),
    };
    let full_sidecar = |why: String| {
        note(&format!("native analysis: {why}; using the Python sidecar for all of it"));
        run_lines(sidecar_command(audio, bundle)?, &log, (0.0, 1.0), &progress).and_then(LinesOutcome::done)
    };
    let result = match native {
        Ok(LinesOutcome::Done) => {
            // The native half stands on its own: a song with stems, beats and
            // lyrics but no sections is better than no analysis.
            let structure = sidecar_command(audio, bundle).and_then(|mut cmd| {
                cmd.arg("--only").arg("structure");
                run_lines(cmd, &log, (0.7, 1.0), &progress).and_then(LinesOutcome::done)
            });
            if let Err(why) = structure {
                note(&format!("sections, key and chords skipped: {why}"));
            }
            Ok(())
        }
        Ok(LinesOutcome::Unavailable(why)) => full_sidecar(why),
        Err(why) => full_sidecar(format!("failed ({why})")),
    };
    if let Err(why) = result {
        return fail(format!("Analysis failed: {why} (see {})", log_path.display()));
    }
    progress("Loading stems", 1.0);
    let mut stems = Vec::new();
    for (id, _) in poing_model::STEMS {
        let file = format!("stems/{id}.wav");
        match load_stem(&bundle.join(&file), sr) {
            Ok(stem) => stems.push((id.to_string(), file, stem)),
            Err(e) => return fail(format!("Analysis failed: {e}")),
        }
    }
    send(tx, JobUpdate::Analyzed { bundle: bundle.to_path_buf(), stems });
}

/// `python poing_analyze.py <audio> <bundle>` in the analysis runtime.
fn sidecar_command(audio: &Path, bundle: &Path) -> Result<Command, String> {
    let mut cmd = paths::python(Runtime::Analysis)?;
    cmd.arg(sidecar_dir().join("poing_analyze.py")).arg(audio).arg(bundle)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUNBUFFERED", "1");
    // SheetSage2 decodes audio with the ffmpeg CLI; put the one we found on PATH.
    if let Some(ff) = ffmpeg().and_then(|f| f.parent().map(Path::to_path_buf)).filter(|p| !p.as_os_str().is_empty()) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![ff];
        paths.extend(std::env::split_paths(&path));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    Ok(cmd)
}

/// The native engine for analysis (see `engine_exe`), with the model
/// manager's analysis models when that pack is in (else the engine's own
/// `~/.makepad/weights`). None when it isn't there or `POING_NATIVE=0`.
fn engine_command() -> Option<Command> {
    if std::env::var("POING_NATIVE").is_ok_and(|v| v == "0") {
        return None;
    }
    let mut cmd = engine_exe()?;
    if let Some(dir) = crate::models::installed_pack(crate::models::NATIVE_PACK) {
        cmd.env("MAKEPAD_HOME", dir).env_remove("MAKEPAD_ASSET_AI_CACHE");
    }
    Some(cmd)
}

/// `poing-engine` whatever `POING_NATIVE` says (the language model has no
/// Python fallback): `POING_ENGINE`, else the GPU engine pack's (with its own
/// CUDA DLLs beside it), else the one beside poing (the installer's is
/// CPU-only on Windows).
pub(crate) fn engine_exe() -> Option<Command> {
    let name = if cfg!(windows) { "poing-engine.exe" } else { "poing-engine" };
    let pack = crate::models::installed_pack(crate::models::ENGINE_PACK).map(|d| d.join("bin").join(name)).filter(|p| p.is_file());
    let exe = std::env::var_os("POING_ENGINE")
        .map(PathBuf::from)
        .or_else(|| pack.clone())
        .or_else(|| Some(std::env::current_exe().ok()?.parent()?.join(name)))?;
    if !exe.is_file() {
        return None;
    }
    let mut cmd = Command::new(&exe);
    // A development engine finds the CUDA runtime DLLs in the toolkit's
    // bin\x64 (since CUDA 13). The pack's sit beside its engine, where
    // Windows looks first.
    #[cfg(windows)]
    if let Some(cuda) = cuda_root().filter(|_| pack.as_ref() != Some(&exe)) {
        let mut paths = vec![cuda.join("bin").join("x64"), cuda.join("bin")];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    Some(cmd)
}

/// How a progress-line process ended, when it didn't fail.
enum LinesOutcome {
    Done,
    /// It can't run here (the engine's `unavailable` stage, or it didn't start).
    Unavailable(String),
}

impl LinesOutcome {
    fn done(self) -> Result<(), String> {
        match self {
            LinesOutcome::Done => Ok(()),
            LinesOutcome::Unavailable(why) => Err(why),
        }
    }
}

/// Run a process that prints sidecar progress lines, relaying them scaled into
/// `band` of the job's bar, with its stderr appended to `log`. It has exited
/// when this returns, so the next process gets the whole GPU.
fn run_lines(mut cmd: Command, log: &std::fs::File, band: (f32, f32), progress: &dyn Fn(&str, f32)) -> Result<LinesOutcome, String> {
    use makepad_micro_serde::DeJson;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let program = cmd.get_program().to_string_lossy().into_owned();
    let stderr = log.try_clone().map_err(|e| e.to_string())?;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::from(stderr));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Ok(LinesOutcome::Unavailable(format!("{program} could not start ({e})"))),
    };
    let (mut error, mut unavailable) = (None, None);
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(|l| l.ok()) {
            let Ok(l) = SidecarLine::deserialize_json_lenient(&line) else { continue };
            let message = l.message.unwrap_or_default();
            match l.stage.as_str() {
                "error" => error = Some(message),
                "unavailable" => unavailable = Some(message),
                _ => progress(&message, band.0 + (band.1 - band.0) * l.progress.unwrap_or(0.0) as f32),
            }
        }
    }
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    if let Some(why) = unavailable {
        return Ok(LinesOutcome::Unavailable(why));
    }
    if !ok || error.is_some() {
        return Err(error.unwrap_or_else(|| format!("{program} exited with an error")));
    }
    Ok(LinesOutcome::Done)
}

/// A Hugging Face model's local snapshot: `HF_HUB_CACHE`, `HF_HOME/hub`, else
/// `~/.cache/huggingface/hub`, newest snapshot first.
fn hf_snapshot(repo: &str) -> Option<PathBuf> {
    let hub = std::env::var_os("HF_HUB_CACHE").map(PathBuf::from).or_else(|| std::env::var_os("HF_HOME").map(|h| PathBuf::from(h).join("hub"))).or_else(|| {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        Some(PathBuf::from(home).join(".cache").join("huggingface").join("hub"))
    })?;
    let snaps = hub.join(format!("models--{}", repo.replace('/', "--"))).join("snapshots");
    let mut dirs: Vec<_> = std::fs::read_dir(snaps).ok()?.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).collect();
    dirs.sort_by_key(|e| std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()));
    dirs.first().map(|e| e.path())
}

/// Can whole-song YuE2 covers run here: is `yue2_render` there (the named
/// file, or on PATH) and are its weights in place? Otherwise Describe on the
/// whole song uses ACE-Step.
pub fn yue2_available() -> bool {
    let exe_ok = match std::env::var_os("POING_YUE2_RENDER") {
        Some(p) => Path::new(&p).is_file(),
        None => {
            let names: &[&str] = if cfg!(windows) { &["yue2_render.exe"] } else { &["yue2_render"] };
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|d| names.iter().any(|n| d.join(n).is_file()))
        }
    };
    let weights = |var: &str, repo: &str| std::env::var_os(var).is_some() || hf_snapshot(repo).is_some();
    exe_ok && weights("POING_YUE2_MODEL", "m-a-p/YuE2-3B") && weights("POING_YUE2_VAE", "m-a-p/YuE2-Vae")
}

/// Native YuE2 (Makepad's `yue2_render`): `POING_YUE2_RENDER`, else on PATH.
/// Weights: `POING_YUE2_MODEL` / `POING_YUE2_VAE`, else the Hugging Face cache.
fn yue2_command() -> Result<(Command, PathBuf, PathBuf), String> {
    let exe = std::env::var_os("POING_YUE2_RENDER").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("yue2_render"));
    let model = std::env::var_os("POING_YUE2_MODEL").map(PathBuf::from).or_else(|| hf_snapshot("m-a-p/YuE2-3B"))
        .ok_or("YuE2-3B weights not found (set POING_YUE2_MODEL)")?;
    let vae = std::env::var_os("POING_YUE2_VAE").map(PathBuf::from).or_else(|| hf_snapshot("m-a-p/YuE2-Vae"))
        .ok_or("YuE2-Vae weights not found (set POING_YUE2_VAE)")?;
    let mut cmd = Command::new(exe);
    // The CUDA runtime DLLs live in the toolkit's bin\x64 since CUDA 13.
    #[cfg(windows)]
    if let Some(cuda) = cuda_root() {
        let mut paths = vec![cuda.join("bin").join("x64"), cuda.join("bin")];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    Ok((cmd, model, vae))
}

/// The CUDA toolkit: `CUDA_PATH` (set by its installer, but missing in
/// processes started before the install), else the newest default install.
#[cfg(windows)]
fn cuda_root() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CUDA_PATH") {
        return Some(PathBuf::from(p));
    }
    let base = PathBuf::from(std::env::var_os("ProgramFiles")?).join("NVIDIA GPU Computing Toolkit").join("CUDA");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(base).ok()?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
    versions.sort_by_key(|p| {
        let v = p.file_name().and_then(|n| n.to_str()).unwrap_or("").trim_start_matches('v').to_string();
        v.split('.').map(|n| n.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>()
    });
    versions.pop()
}

/// Map a `yue2_render` progress line ("Codec { done: 812, max: 6000 }") to 0..1
/// over the whole render.
fn yue2_progress(line: &str) -> Option<(&'static str, f32)> {
    let (stage, rest) = line.split_once(" {")?;
    let nums: Vec<f32> = rest.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect();
    let frac = match nums.as_slice() {
        [done, total, ..] if *total > 0.0 => (done / total).clamp(0.0, 1.0),
        _ => return None,
    };
    let (label, a, b) = match stage.trim() {
        "Score" => ("Planning", 0.0, 0.05),
        "Codec" => ("Singing", 0.05, 0.5),
        "Flow" => ("Shaping the sound", 0.5, 0.95),
        "Decode" => ("Decoding", 0.95, 1.0),
        _ => return None,
    };
    Some((label, a + (b - a) * frac))
}

/// Render one take with native YuE2, relaying its progress, then decode it.
/// The fixture, the render log and the raw output stay in `takes/<id>/`.
fn generate(bundle: &Path, take: Take, fixture: &Yue2Fixture, sr: u32, tx: &Sender<JobUpdate>) {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let fail = |why: String| {
        let message = format!("Take {} failed: {why}", take.id.trim_start_matches('t'));
        send(tx, JobUpdate::Failed { bundle: bundle.to_path_buf(), message })
    };
    let progress =
        |message: &str, progress: f32| send(tx, JobUpdate::Progress { bundle: bundle.to_path_buf(), message: message.into(), progress });
    progress(&format!("Take {}  ·  Starting YuE2", take.id.trim_start_matches('t')), 0.0);
    let work = bundle.join("takes").join(&take.id);
    if let Err(e) = std::fs::create_dir_all(&work) {
        return fail(e.to_string());
    }
    let fixture_path = work.join("fixture.json");
    if let Err(e) = std::fs::write(&fixture_path, fixture.serialize_json()) {
        return fail(e.to_string());
    }
    let (mut cmd, model, vae) = match yue2_command() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    cmd.arg("--model").arg(model).arg("--vae").arg(vae).arg("--fixture").arg(&fixture_path).arg("--out").arg(&work)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("yue2_render did not start ({e}); set POING_YUE2_RENDER")),
    };
    let mut log = std::fs::File::create(work.join("render.log")).ok();
    if let Some(err) = child.stderr.take() {
        for line in BufReader::new(err).lines().map_while(|l| l.ok()) {
            if let Some(f) = log.as_mut() {
                let _ = writeln!(f, "{line}");
            }
            if let Some((label, p)) = yue2_progress(&line) {
                progress(&format!("Take {}  ·  {label}", take.id.trim_start_matches('t')), p);
            }
        }
    }
    match child.wait() {
        Ok(s) if s.success() => {}
        Ok(s) => {
            // 0xC0000135 on Windows: a DLL (the CUDA runtime) was not found.
            let code = s.code().map(|c| format!("{:#X}", c as u32)).unwrap_or_default();
            return fail(format!("yue2_render exited with {code} (see {})", work.join("render.log").display()));
        }
        Err(e) => return fail(e.to_string()),
    }
    let wav = work.join("song.wav");
    let dest = bundle.join(&take.file);
    if let Err(e) = std::fs::rename(&wav, &dest).or_else(|_| std::fs::copy(&wav, &dest).map(|_| ())) {
        return fail(format!("move {}: {e}", wav.display()));
    }
    match load_stem(&dest, sr) {
        Ok(stem) => send(tx, JobUpdate::TakeReady { bundle: bundle.to_path_buf(), take, stem }),
        Err(e) => fail(e),
    }
}

/// One run of a Poing script in one of its Python runtimes.
struct UvScript<'a> {
    runtime: Runtime,
    script: PathBuf,
    args: Vec<std::ffi::OsString>,
    /// stderr goes here
    log: PathBuf,
    /// Sees every progress line (e.g. a finished version) as it comes.
    on_line: Option<&'a dyn Fn(&SidecarLine)>,
}

/// Run a script that prints JSON progress lines, mapping its 0..1 into
/// [from, to] of the job (`progress`). Returns the closing "done" line's
/// range (start, end) when it gives one.
fn run_uv_script(run: UvScript, progress: &dyn Fn(&str, f32), from: f32, to: f32) -> Result<Option<(f64, f64)>, String> {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let log = std::fs::File::create(&run.log).map_err(|e| e.to_string())?;
    let mut cmd = paths::python(run.runtime)?;
    cmd.arg(&run.script).args(&run.args)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log));
    // Demucs and SheetSage2 decode with the ffmpeg CLI; put the one we found on PATH.
    if let Some(ff) = ffmpeg().and_then(|f| f.parent().map(Path::to_path_buf)).filter(|p| !p.as_os_str().is_empty()) {
        let mut paths = vec![ff];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("{} did not start ({e})", cmd.get_program().to_string_lossy()))?;
    let (mut range, mut error) = (None, None);
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(|l| l.ok()) {
            let Ok(l) = SidecarLine::deserialize_json_lenient(&line) else { continue };
            if let Some(f) = run.on_line {
                f(&l);
            }
            let message = l.message.unwrap_or_default();
            match l.stage.as_str() {
                "error" => error = Some(message),
                "done" => range = l.start.zip(l.end),
                _ => progress(&message, from + (to - from) * l.progress.unwrap_or(0.0) as f32),
            }
        }
    }
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    match error {
        Some(e) => Err(format!("{e} (see {})", run.log.display())),
        None if !ok => Err(format!("the script exited with an error (see {})", run.log.display())),
        None => Ok(range),
    }
}

/// Split lane `lane` into dry voice and echo (`sidecar/dereverb`); the dry
/// voice over the take's range is the take. Its blend sets how much echo is
/// left: lane * (1 - b) + dry * b = dry + (1 - b) * echo.
fn dereverb(bundle: &Path, take: Take, lane: &Path, sr: u32, tx: &Sender<JobUpdate>) {
    let n = take.id.trim_start_matches('t').to_string();
    let fail = |why: String| send(tx, JobUpdate::Failed { bundle: bundle.to_path_buf(), message: format!("Take {n} failed: {why}") });
    let progress = |message: &str, progress: f32| {
        send(tx, JobUpdate::Progress { bundle: bundle.to_path_buf(), message: format!("Take {n}  ·  {message}"), progress })
    };
    progress("Starting the de-reverb", 0.0);
    let work = bundle.join("takes").join(&take.id);
    if let Err(e) = std::fs::create_dir_all(&work) {
        return fail(e.to_string());
    }
    let run = UvScript {
        runtime: Runtime::Dereverb,
        script: sidecar_dir().join("dereverb").join("poing_dereverb.py"),
        args: vec![lane.into(), work.clone().into(), take.start.to_string().into(), take.end.to_string().into()],
        log: work.join("render.log"),
        on_line: None,
    };
    if let Err(e) = run_uv_script(run, &progress, 0.0, 1.0) {
        return fail(e);
    }
    finish_take(bundle, take, &work.join("dry.wav"), sr, tx, &fail);
}

/// What `sidecar/acestep_lego.py` reads.
#[derive(SerJson, DeJson, Clone, Default)]
pub struct LegoJob {
    pub src: String,
    pub out: String,
    pub track: String,
    pub caption: String,
    pub seed: u64,
    pub bpm: Option<f64>,
    pub key: Option<String>,
}

/// What `sidecar/poing_layer.py` reads.
#[derive(SerJson, DeJson, Clone, Default)]
pub struct LayerJob {
    pub src: String,
    pub out: String,
    pub stem: String,
    /// The lane whose gaps the layer fills (full level where it is thin).
    pub reference: Option<String>,
    /// The lane a replacement takes over: matched to its loudness.
    pub match_lane: Option<String>,
    pub start: f64,
    pub end: f64,
}

/// A new part: ACE-Step lego plays it against the song, then Demucs keeps
/// just that instrument and it is fitted into the reference lane's gaps.
fn layer(bundle: &Path, take: Take, mut lego: LegoJob, mut clean: LayerJob, sr: u32, tx: &Sender<JobUpdate>) {
    let n = take.id.trim_start_matches('t').to_string();
    let fail = |why: String| send(tx, JobUpdate::Failed { bundle: bundle.to_path_buf(), message: format!("Take {n} failed: {why}") });
    let progress = |message: &str, progress: f32| {
        send(tx, JobUpdate::Progress { bundle: bundle.to_path_buf(), message: format!("Take {n}  ·  {message}"), progress })
    };
    progress("Starting ACE-Step", 0.0);
    let work = bundle.join("takes").join(&take.id);
    if let Err(e) = std::fs::create_dir_all(&work) {
        return fail(e.to_string());
    }
    let played = work.join("lego.wav");
    let cleaned = work.join("layer.wav");
    lego.out = played.to_string_lossy().into_owned();
    clean.src = lego.out.clone();
    clean.out = cleaned.to_string_lossy().into_owned();
    let (lego_json, layer_json) = (work.join("lego.json"), work.join("layer.json"));
    if let Err(e) = std::fs::write(&lego_json, lego.serialize_json()).and_then(|_| std::fs::write(&layer_json, clean.serialize_json())) {
        return fail(e.to_string());
    }
    let run = UvScript {
        runtime: Runtime::AceStep,
        script: sidecar_dir().join("acestep_lego.py"),
        args: vec![lego_json.into()],
        log: work.join("lego.log"),
        on_line: None,
    };
    if let Err(e) = run_uv_script(run, &progress, 0.0, 0.75) {
        return fail(e);
    }
    let run = UvScript {
        runtime: Runtime::Analysis,
        script: sidecar_dir().join("poing_layer.py"),
        args: vec![layer_json.into()],
        log: work.join("layer.log"),
        on_line: None,
    };
    if let Err(e) = run_uv_script(run, &progress, 0.75, 1.0) {
        return fail(e);
    }
    let _ = std::fs::remove_file(&played);
    finish_take(bundle, take, &cleaned, sr, tx, &fail);
}

/// Have Claude write a MIDI part, check it, and hand it to the UI.
fn write_midi(bundle: &Path, request: String, role: &str, sheet: &str, bars: crate::midi_writer::Bars, range: (f64, f64), tx: &Sender<JobUpdate>) {
    let progress = |message: &str, progress: f32| {
        send(tx, JobUpdate::Progress { bundle: bundle.to_path_buf(), message: format!("Part  ·  {message}"), progress })
    };
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let work = bundle.join("instruments").join("drafts").join(stamp.to_string());
    match crate::midi_writer::write_part(&request, role, sheet, bars, range, &work, &progress) {
        Ok(w) if !w.notes.is_empty() => {
            let note = if w.problems.is_empty() {
                format!("{} notes, checked in {} round{}", w.notes.len(), w.rounds, if w.rounds == 1 { "" } else { "s" })
            } else {
                format!("{} notes; the check still says: {}", w.notes.len(), w.problems.join("; "))
            };
            send(tx, JobUpdate::MidiReady { bundle: bundle.to_path_buf(), name: w.name, role: role.to_string(), notes: w.notes, request, note });
        }
        Ok(w) => send(tx, JobUpdate::Failed { bundle: bundle.to_path_buf(), message: format!("The part could not be made: {}", w.problems.join("; ")) }),
        Err(e) => send(tx, JobUpdate::Failed { bundle: bundle.to_path_buf(), message: format!("The part could not be made: {e}") }),
    }
}

/// Have Claude write an effect, check it, and hand it to the UI.
fn write_fx(bundle: &Path, lane: Option<usize>, request: String, context: &str, tx: &Sender<JobUpdate>) {
    let progress = |message: &str, progress: f32| {
        send(tx, JobUpdate::Progress { bundle: bundle.to_path_buf(), message: format!("Effect  ·  {message}"), progress })
    };
    let fail = |why: String| send(tx, JobUpdate::Failed { bundle: bundle.to_path_buf(), message: format!("The effect could not be made: {why}") });
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let work = bundle.join("fx").join("drafts").join(stamp.to_string());
    match crate::fx_writer::write_effect(&request, context, &work, &progress) {
        Ok(w) if crate::fx::compile_source(&w.code).is_ok() => {
            let note = if w.problems.is_empty() {
                format!("checked in {} round{}", w.rounds, if w.rounds == 1 { "" } else { "s" })
            } else {
                format!("plays, but the check still says: {}", w.problems.join("; "))
            };
            send(tx, JobUpdate::FxReady { bundle: bundle.to_path_buf(), lane, name: w.name, code: w.code, request, note });
        }
        Ok(w) => fail(w.problems.join("; ")),
        Err(e) => fail(e),
    }
}

/// Where new songs go: `POING_SONGS`, else `Music/Poing` in the home folder.
pub fn songs_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("POING_SONGS") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default();
    home.join("Music").join("Poing")
}

/// Run `sidecar/acestep_song.py` on `job` (written next to its versions),
/// calling `version(i, file)` as each one lands.
fn run_song_job(job: &SongJob, progress: &dyn Fn(&str, f32), version: &dyn Fn(usize, PathBuf)) -> Result<(), String> {
    let out = PathBuf::from(&job.out_dir);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let job_path = out.join("job.json");
    std::fs::write(&job_path, job.serialize_json()).map_err(|e| e.to_string())?;
    let on_line = |l: &SidecarLine| {
        if let (true, Some(i), Some(p)) = (l.stage == "version", l.index, l.path.as_ref()) {
            version(i, PathBuf::from(p));
        }
    };
    let run = UvScript {
        runtime: Runtime::AceStep,
        script: sidecar_dir().join("acestep_song.py"),
        args: vec![job_path.into()],
        log: out.join("song.log"),
        on_line: Some(&on_line),
    };
    run_uv_script(run, progress, 0.0, 1.0).map(|_| ())
}

/// A new song from a prompt: the language model writes the sheet, ACE-Step sings
/// `count` versions. The first becomes the song (a draft: nothing is
/// analysed until a version is picked), the others its song takes.
fn make_song(prompt: &str, dir: &Path, count: usize, sr: u32, tx: &Sender<JobUpdate>) {
    if let Some((bundle, seeds)) = prepare_song(prompt, dir, count, tx) {
        crate::llm::release();
        sing_song(&bundle, count, seeds, sr, tx);
    }
}

/// The writing half of a new song: the language model writes the sheet into a new bundle
/// in `dir` (sheet.json, lyrics.txt, prompt.txt). Returns the bundle and the
/// versions' seeds; a failure is sent.
pub fn prepare_song(prompt: &str, dir: &Path, count: usize, tx: &Sender<JobUpdate>) -> Option<(PathBuf, Vec<u64>)> {
    let fail = |why: String| send(tx, JobUpdate::Failed { bundle: PathBuf::new(), message: format!("The song could not be made: {why}") });
    send(tx, JobUpdate::Starting { message: format!("{} is writing the song", crate::llm::name()), progress: 0.0 });
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let sheet = match crate::song_writer::write_sheet(prompt, &dir.join(".drafts").join(stamp.to_string())) {
        Ok(s) => s,
        Err(e) => {
            fail(e);
            return None;
        }
    };
    let base = crate::song_writer::folder_name(&sheet.title);
    let Some(bundle) = (1..1000)
        .map(|n| dir.join(if n == 1 { format!("{base}.poing") } else { format!("{base} {n}.poing") }))
        .find(|b| !b.exists())
    else {
        fail("no free folder name".into());
        return None;
    };
    if let Err(e) = std::fs::create_dir_all(bundle.join("takes")) {
        fail(format!("create {}: {e}", bundle.display()));
        return None;
    }
    // the words are known: no transcription needed to describe the song later
    if !sheet.lyrics.trim().is_empty() {
        let _ = std::fs::write(bundle.join("lyrics.txt"), &sheet.lyrics);
    }
    let _ = std::fs::write(bundle.join("sheet.json"), sheet.serialize_json());
    let _ = std::fs::write(bundle.join("prompt.txt"), prompt);
    Some((bundle, (0..count as u64).map(|i| (stamp % 100_000) * 13 + i * 7919).collect()))
}

/// The singing half of a new song: ACE-Step sings `count` versions of the
/// bundle's sheet.json. Version 1 becomes the song (SongReady), the others
/// its song takes.
fn sing_song(bundle: &Path, count: usize, seeds: Vec<u64>, sr: u32, tx: &Sender<JobUpdate>) {
    use std::cell::RefCell;
    let bundle_cell: RefCell<Option<PathBuf>> = RefCell::new(None);
    // once version 1 has opened the song, a failure belongs to it (it clears its busy state)
    let fail = |why: String| {
        let bundle = bundle_cell.borrow().clone().unwrap_or_default();
        send(tx, JobUpdate::Failed { bundle, message: format!("The song could not be made: {why}") })
    };
    let progress = |message: &str, progress: f32| match bundle_cell.borrow().clone() {
        Some(bundle) => send(tx, JobUpdate::Progress { bundle, message: format!("Song  ·  {message}"), progress }),
        None => send(tx, JobUpdate::Starting { message: message.to_string(), progress }),
    };
    let sheet = match std::fs::read_to_string(bundle.join("sheet.json")).map_err(|e| e.to_string()).and_then(|t| {
        crate::song_writer::Sheet::deserialize_json_lenient(&t).map_err(|e| format!("sheet.json: {e:?}"))
    }) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let bundle = bundle.to_path_buf();
    // a mirrored bundle (poing-node) may come without the empty takes folder
    if let Err(e) = std::fs::create_dir_all(bundle.join("takes")) {
        return fail(e.to_string());
    }
    let job = SongJob {
        task: "text2music".into(),
        out_dir: bundle.join("versions").to_string_lossy().into_owned(),
        count,
        caption: sheet.caption.clone(),
        lyrics: sheet.lyrics.clone(),
        language: sheet.language.clone(),
        bpm: sheet.bpm.map(f64::round),
        key: sheet.key.clone(),
        duration: sheet.duration,
        seeds: Some(seeds.clone()),
        thinking: true,
        ..Default::default()
    };
    progress(&format!("ACE-Step is singing \"{}\" ({count} versions, about {} minutes)", sheet.title, 1 + count * 2 / 3), 0.05);
    let version = |i: usize, file: PathBuf| {
        if i == 1 {
            let original = bundle.join("original.wav");
            let made = std::fs::rename(&file, &original).map_err(|e| e.to_string()).and_then(|_| load_stem(&original, sr)).and_then(|stem| {
                let mut p = Project::import(&sheet.title, "original.wav", stem.l.len() as f64 / sr as f64, sr);
                p.draft = Some(true);
                p.takes_made = Some(count.saturating_sub(1) as u64);
                p.save(&bundle.join("project.json"))
            });
            match made {
                Ok(()) => {
                    *bundle_cell.borrow_mut() = Some(bundle.clone());
                    send(tx, JobUpdate::SongReady { project: bundle.join("project.json") });
                }
                Err(e) => fail(e),
            }
            return;
        }
        let id = format!("t{}", i - 1);
        let take = Take {
            file: format!("takes/{id}.wav"),
            id,
            scope: "song".into(),
            start: 0.0,
            end: 0.0,
            engine: "acestep-song".into(),
            prompt: format!("Version {i}"),
            seed: seeds.get(i - 1).copied().unwrap_or(0),
            track: None,
            blend: None,
            region: None,
            kept: None,
        };
        let dest = bundle.join(&take.file);
        match std::fs::rename(&file, &dest).map_err(|e| e.to_string()).and_then(|_| load_stem(&dest, sr)) {
            Ok(stem) => send(tx, JobUpdate::TakeReady { bundle: bundle.clone(), take, stem }),
            Err(e) => fail(e),
        }
    };
    match run_song_job(&job, &progress, &version) {
        Ok(()) => send(tx, JobUpdate::Finished { bundle: bundle.clone(), message: format!("\"{}\": {count} versions. Click one above the lanes to hear it; Use this version starts working on it.", sheet.title) }),
        Err(e) => fail(e),
    }
}

/// Remix the song in `bundle` with ACE-Step cover: one song take per strength.
fn remix(bundle: &Path, takes: Vec<Take>, mut job: SongJob, sr: u32, tx: &Sender<JobUpdate>) {
    let fail = |why: String| send(tx, JobUpdate::Failed { bundle: bundle.to_path_buf(), message: format!("The remix could not be made: {why}") });
    let progress = |message: &str, progress: f32| {
        send(tx, JobUpdate::Progress { bundle: bundle.to_path_buf(), message: format!("Remix  ·  {message}"), progress })
    };
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    job.out_dir = bundle.join("takes").join(format!("remix-{stamp}")).to_string_lossy().into_owned();
    job.count = takes.len();
    let version = |i: usize, file: PathBuf| {
        let Some(take) = takes.get(i - 1).cloned() else { return };
        let dest = bundle.join(&take.file);
        match std::fs::rename(&file, &dest).map_err(|e| e.to_string()).and_then(|_| load_stem(&dest, sr)) {
            Ok(stem) => send(tx, JobUpdate::TakeReady { bundle: bundle.to_path_buf(), take, stem }),
            Err(e) => fail(e),
        }
    };
    match run_song_job(&job, &progress, &version) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&job.out_dir);
            send(tx, JobUpdate::Finished { bundle: bundle.to_path_buf(), message: format!("{} remixes are ready: click them above the lanes to compare. Select a section and Use this remix here to combine them.", takes.len()) });
        }
        Err(e) => fail(e),
    }
}

/// Move a rendered take into place, decode it and hand it to the UI.
fn finish_take(bundle: &Path, take: Take, rendered: &Path, sr: u32, tx: &Sender<JobUpdate>, fail: &dyn Fn(String)) {
    let dest = bundle.join(&take.file);
    if let Err(e) = std::fs::rename(rendered, &dest).or_else(|_| std::fs::copy(rendered, &dest).map(|_| ())) {
        return fail(format!("move {}: {e}", rendered.display()));
    }
    match load_stem(&dest, sr) {
        Ok(stem) => send(tx, JobUpdate::TakeReady { bundle: bundle.to_path_buf(), take, stem }),
        Err(e) => fail(e),
    }
}

/// Repaint one range with ACE-Step (`sidecar/acestep_repaint.py` in its uv
/// project), relaying progress; each take covers the range plus its margins
/// and is handed over as soon as its version is written.
fn repaint(bundle: &Path, takes: Vec<Take>, src: &Path, mut job: RepaintJob, sr: u32, tx: &Sender<JobUpdate>) {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let Some(first) = takes.first().cloned() else { return };
    let n = takes.iter().map(|t| t.id.trim_start_matches('t')).collect::<Vec<_>>().join(", ");
    let what = if takes.len() > 1 { format!("Takes {n}") } else { format!("Take {n}") };
    let fail = |why: String| send(tx, JobUpdate::Failed { bundle: bundle.to_path_buf(), message: format!("{what} failed: {why}") });
    let progress = |message: &str, progress: f32| {
        send(tx, JobUpdate::Progress { bundle: bundle.to_path_buf(), message: format!("{what}  ·  {message}"), progress })
    };
    progress("Starting ACE-Step", 0.0);
    let work = bundle.join("takes").join(&first.id);
    if let Err(e) = std::fs::create_dir_all(&work) {
        return fail(e.to_string());
    }
    job.src = src.to_string_lossy().into_owned();
    job.out = bundle.join(&first.file).to_string_lossy().into_owned();
    job.seed = first.seed;
    if takes.len() > 1 {
        job.seeds = Some(takes.iter().map(|t| t.seed).collect());
        job.outs = Some(takes.iter().map(|t| bundle.join(&t.file).to_string_lossy().into_owned()).collect());
    }
    let job_path = work.join("job.json");
    if let Err(e) = std::fs::write(&job_path, job.serialize_json()) {
        return fail(e.to_string());
    }
    let log = match std::fs::File::create(work.join("render.log")) {
        Ok(f) => f,
        Err(e) => return fail(e.to_string()),
    };
    let mut cmd = match paths::python(Runtime::AceStep) {
        Ok(cmd) => cmd,
        Err(e) => return fail(e),
    };
    cmd.arg(sidecar_dir().join("acestep_repaint.py")).arg(&job_path)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("{} did not start ({e})", cmd.get_program().to_string_lossy())),
    };
    let (mut range, mut error, mut sent) = (None, None, 0);
    // a finished version: decoded and handed over while the next one renders
    let hand_over = |i: usize, (start, end): (f64, f64)| {
        let Some(mut take) = takes.get(i).cloned() else { return };
        take.start = start;
        take.end = end;
        match load_stem(&bundle.join(&take.file), sr) {
            Ok(stem) => send(tx, JobUpdate::TakeReady { bundle: bundle.to_path_buf(), take, stem }),
            Err(e) => fail(e),
        }
    };
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(|l| l.ok()) {
            let Ok(l) = SidecarLine::deserialize_json_lenient(&line) else { continue };
            let message = l.message.unwrap_or_default();
            match l.stage.as_str() {
                "error" => error = Some(message),
                "version" => {
                    progress(&message, l.progress.unwrap_or(0.0) as f32);
                    if let (Some(i), Some(r)) = (l.index, l.start.zip(l.end)) {
                        hand_over(i - 1, r);
                        sent = sent.max(i);
                    }
                }
                "done" => range = l.start.zip(l.end),
                _ => progress(&message, l.progress.unwrap_or(0.0) as f32),
            }
        }
    }
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    let Some(range) = range.filter(|_| ok && error.is_none()) else {
        let why = error.unwrap_or_else(|| "ACE-Step exited with an error".into());
        return fail(format!("{why} (see {})", work.join("render.log").display()));
    };
    // an older sidecar says only "done": its one version is the first take
    if sent == 0 {
        hand_over(0, range);
    }
    if takes.len() > 1 {
        let (a, b) = (job.start, job.end);
        let message = format!("{} versions of the mix over {a:.1}s - {b:.1}s are ready (takes {n}): click them above the lanes to compare, then Keep this version.", takes.len());
        send(tx, JobUpdate::Finished { bundle: bundle.to_path_buf(), message });
    }
}

#[cfg(test)]
mod tests {
    use super::{mix_into, yue2_progress, MixParams, MixTake, Stem, TakeKind, MAX_ACTIVE_TAKES};
    use std::sync::Arc;

    fn constant(v: f32, frames: usize) -> Arc<Stem> {
        Arc::new(Stem { l: vec![v; frames], r: vec![v; frames], peaks: Vec::new(), peak_abs: v })
    }

    #[test]
    fn a_song_take_replaces_the_lanes() {
        let stems = vec![constant(0.25, 1000), constant(1.0, 1500)];
        let mix = with_takes(&[MixTake { stem: 1, start: 0.0, end: 1.5, ..Default::default() }]);
        let (mut l, mut r) = (vec![0.0; 100], vec![0.0; 100]);
        mix_into(&stems, &mix, 1000, 1200, &mut l, &mut r, None);
        assert!(l.iter().chain(&r).all(|&x| x == 1.0), "past the lanes' end the take still plays alone");
    }

    #[test]
    fn a_section_take_crossfades_in_its_range() {
        // lanes 0.25 everywhere for 4 s; a 2 s take placed at 1 s (rate 1000)
        let stems = vec![constant(0.25, 4000), constant(1.0, 2000)];
        let section = MixTake { stem: 1, start: 1.0, end: 3.0, kind: TakeKind::Section, blend: 1.0, fade_in: true, fade_out: true };
        let mix = with_takes(&[section]);
        let (mut l, mut r) = (vec![0.0; 4000], vec![0.0; 4000]);
        mix_into(&stems, &mix, 1000, 0, &mut l, &mut r, None);
        assert_eq!(l[500], 0.25, "before the take: lanes");
        assert_eq!(l[2000], 1.0, "inside: the take alone");
        assert_eq!(l[3500], 0.25, "after: lanes again");
        assert!((l[1250] - 0.625).abs() < 1e-6, "halfway up the ramp: an even blend");
        // a block that starts inside the take plays it from the right offset
        let (mut l2, mut r2) = (vec![0.0; 10], vec![0.0; 10]);
        mix_into(&stems, &mix, 1000, 2000, &mut l2, &mut r2, None);
        assert_eq!(l2[0], 1.0);
    }

    /// One lane at unity plus these takes.
    fn with_takes(takes: &[MixTake]) -> MixParams {
        let mut mix = MixParams { gains: vec![1.0], ..Default::default() };
        for (slot, k) in mix.takes.iter_mut().zip(takes) {
            *slot = Some(*k);
        }
        mix
    }

    #[test]
    fn a_lane_take_blends_into_its_lane_like_dry_plus_some_echo() {
        // vocals = dry 0.6 + wet 0.4; the take is the dry voice; blend 0.65
        // must sound like dry + 0.35 * wet (the disco v4 vocal).
        let stems = vec![constant(1.0, 1000), constant(0.6, 1000)];
        let lane = MixTake { stem: 1, start: 0.0, end: 1.0, kind: TakeKind::Lane(0), blend: 0.65, ..Default::default() };
        let mix = with_takes(&[lane]);
        let (mut l, mut r) = (vec![0.0; 100], vec![0.0; 100]);
        mix_into(&stems, &mix, 1000, 0, &mut l, &mut r, None);
        assert!((l[50] - (0.6 + 0.35 * 0.4)).abs() < 1e-6, "{}", l[50]);
    }

    #[test]
    fn layers_and_lane_takes_stack() {
        let stems = vec![constant(0.5, 1000), constant(0.25, 1000), constant(0.2, 1000), constant(0.1, 1000)];
        let mut mix = with_takes(&[
            MixTake { stem: 2, start: 0.0, end: 1.0, kind: TakeKind::Lane(0), blend: 1.0, ..Default::default() },
            MixTake { stem: 3, start: 0.0, end: 1.0, kind: TakeKind::Layer(Some(1)), blend: 0.5, ..Default::default() },
        ]);
        mix.gains = vec![1.0, 1.0];
        let (mut l, mut r) = (vec![0.0; 10], vec![0.0; 10]);
        mix_into(&stems, &mix, 1000, 0, &mut l, &mut r, None);
        // lane 0 replaced by 0.2, lane 1 kept at 0.25, layer adds 0.05
        assert!((l[5] - 0.5).abs() < 1e-6, "{}", l[5]);
        // muting lane 0 mutes its lane take too
        mix.gains[0] = 0.0;
        mix_into(&stems, &mix, 1000, 0, &mut l, &mut r, None);
        assert!((l[5] - 0.3).abs() < 1e-6, "{}", l[5]);
        // and a layer follows the lane it was added to (solo another lane: it goes quiet)
        mix.gains = vec![1.0, 0.0];
        mix_into(&stems, &mix, 1000, 0, &mut l, &mut r, None);
        assert!((l[5] - 0.2).abs() < 1e-6, "{}", l[5]);
        assert_eq!(MAX_ACTIVE_TAKES, mix.takes.len());
    }

    /// Plugin slots without an instance pass audio through untouched, so with
    /// a claimed latency each lane comes out (most - its own) late: what a
    /// real plugin of that latency would add up to `most`, the same for every lane.
    #[cfg(windows)]
    #[test]
    fn lanes_are_delayed_to_meet_the_latest_one() {
        use crate::fx::{FxControls, FxRack, FxUnit};
        use std::sync::atomic::Ordering;
        let fx = poing_model::Fx { id: String::new(), name: String::new(), file: String::new(), params: None, bypass: None, prompt: None, plugin: Some("x".into()), isolate: None };
        let slot = |id: &str| FxUnit::plugin(id, FxControls::for_plugin(&fx), 1000);
        let mut rack = FxRack::new(vec![vec![slot("a")], vec![slot("b")], Vec::new()], Vec::new());
        rack.compensate(&|id| match id {
            "a" => 300,
            "b" => 100,
            _ => 0,
        });
        assert_eq!((rack.latency, rack.lane_latency.clone()), (300, vec![300, 100, 0]));
        let click = |at: usize| {
            let mut v = vec![0.0f32; 4000];
            v[at] = 1.0;
            Arc::new(Stem::new(v.clone(), v))
        };
        let stems = vec![click(1000), click(1000), click(1000)];
        let heard = |rack: &mut FxRack, lane: usize| -> usize {
            let mut mix = with_takes(&[]);
            mix.gains = vec![0.0; 3];
            mix.gains[lane] = 1.0;
            rack.clear_delays();
            let (mut l, mut r) = (vec![0.0; 3000], vec![0.0; 3000]);
            // in odd blocks, as the audio callback does
            for (a, b) in [(0, 700), (700, 1301), (1301, 3000)] {
                mix_into(&stems, &mix, 1000, a, &mut l[a..b], &mut r[a..b], Some(rack));
            }
            l.iter().position(|x| *x > 0.5).unwrap()
        };
        assert_eq!(heard(&mut rack, 0) + 300, 1300, "lane a, with its own 300");
        assert_eq!(heard(&mut rack, 1) + 100, 1300, "lane b, with its own 100");
        assert_eq!(heard(&mut rack, 2), 1300, "the lane without effects waits for them");
        // bypassed, a latent effect still delays its lane by its latency
        rack.lanes[0][0].controls.bypass.store(true, Ordering::Relaxed);
        assert_eq!(heard(&mut rack, 0), 1300);
        // an offline render takes the 300 out again
        let mut mix = with_takes(&[]);
        mix.gains = vec![0.0, 0.0, 1.0];
        let (l, _) = super::render_aligned(&stems, &mix, 1000, 3000, &mut rack);
        assert_eq!((l.len(), l.iter().position(|x| *x > 0.5)), (3000, Some(1000)));
    }

    #[test]
    fn a_lane_effect_also_processes_that_lane_s_takes() {
        use crate::fx::{compile_source, FxControls, FxRack, FxUnit};
        let code = "let gain = param(0.5, 0, 1)\nfn effect(l, r) { vec2(l * gain, r * gain) }\n";
        let shader = compile_source(code).unwrap();
        let fx = poing_model::Fx { id: "fx1".into(), name: "half".into(), file: String::new(), params: None, bypass: None, prompt: None, plugin: None, isolate: None };
        let unit = FxUnit::new("fx1", shader.clone(), FxControls::new(&shader, &fx, None), 1000);
        let mut rack = FxRack::new(vec![vec![unit], Vec::new()], Vec::new());
        // lane 0 (1.0) has the effect and a layer (1.0); lane 1 (0.25) has none
        let stems = vec![constant(1.0, 1000), constant(0.25, 1000), constant(1.0, 1000)];
        let mut mix = with_takes(&[MixTake { stem: 2, start: 0.0, end: 1.0, kind: TakeKind::Layer(Some(0)), blend: 1.0, ..Default::default() }]);
        mix.gains = vec![1.0, 1.0];
        let (mut l, mut r) = (vec![0.0; 300], vec![0.0; 300]);
        mix_into(&stems, &mix, 1000, 0, &mut l, &mut r, Some(&mut rack));
        assert!((l[200] - (2.0 * 0.5 + 0.25)).abs() < 1e-3, "{}", l[200]);
        // without a rack the same mix is the plain sum
        mix_into(&stems, &mix, 1000, 0, &mut l, &mut r, None);
        assert!((l[200] - 2.25).abs() < 1e-6, "{}", l[200]);
    }

    #[test]
    fn meters_hold_each_lane_s_and_the_master_s_peak() {
        use super::{mix_metered, MASTER_METER, METER_LANES};
        use std::sync::atomic::{AtomicU32, Ordering};
        let stems = vec![constant(0.25, 100), constant(0.5, 100)];
        let mut mix = with_takes(&[]);
        mix.gains = vec![1.0, 0.5];
        let meters: Vec<AtomicU32> = (0..=METER_LANES).map(|_| AtomicU32::new(0)).collect();
        let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
        mix_metered(&stems, &mix, 1000, 0, &mut l, &mut r, None, Some(&meters));
        let read = |i: usize| f32::from_bits(meters[i].load(Ordering::Relaxed));
        assert_eq!(read(0), 0.25);
        assert_eq!(read(1), 0.25, "after the lane's gain");
        assert_eq!(read(MASTER_METER), 0.5);
    }

    #[test]
    fn yue2_progress_lines() {
        assert_eq!(yue2_progress("Flow { done: 16, total: 32 }"), Some(("Shaping the sound", 0.725)));
        assert_eq!(yue2_progress("Codec { done: 0, max: 6000 }"), Some(("Singing", 0.05)));
        assert_eq!(yue2_progress("codec: 3306 tokens 18.34s (196.3 tok/s) = 132.2s audio"), None);
    }
}
