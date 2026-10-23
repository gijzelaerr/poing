//! Lane and master effects: Splash audio shaders (`effect(l, r)`), run on
//! each lane after its takes and on the master after the lanes.
//!
//! The UI thread compiles shaders and builds an [`FxRack`] (all allocation
//! happens there) and hands it to the audio thread, which adopts the old
//! rack's state for every effect it keeps (reverb tails and knob smoothing
//! carry on through a recompile) and hands the old rack back to be dropped.
//! Knobs and bypass are atomics the audio thread reads every block.

use makepad_script_audio_aot as aot;
use poing_model::{Fx, Project};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// An effect's controls: written by the UI, read by the audio thread.
pub struct FxControls {
    /// Param targets as f32 bits, in the shader's param order.
    pub params: Box<[AtomicU32]>,
    pub bypass: AtomicBool,
}

impl FxControls {
    /// Controls for `shader` at the project's settings for `fx`.
    pub fn new(shader: &aot::AudioShaderRef, fx: &Fx, bpm: Option<f64>) -> Arc<Self> {
        let params = shader
            .params()
            .iter()
            .map(|p| {
                let v = if p.name == "bpm" { bpm.map(|b| b as f32) } else { None };
                let v = v.or(fx.param(&p.name).map(|v| v as f32)).unwrap_or(p.default).clamp(p.min, p.max);
                AtomicU32::new(v.to_bits())
            })
            .collect();
        Arc::new(Self { params, bypass: AtomicBool::new(fx.bypassed()) })
    }
    /// A plugin's: bypass only (its knobs go through the plugin's own queue).
    pub fn for_plugin(fx: &Fx) -> Arc<Self> {
        Arc::new(Self { params: Box::new([]), bypass: AtomicBool::new(fx.bypassed()) })
    }

    pub fn set(&self, index: usize, value: f32) {
        if let Some(p) = self.params.get(index) {
            p.store(value.to_bits(), Ordering::Relaxed);
        }
    }
}

/// The block size plugins are set up for; lane audio is fed to them in
/// blocks this long.
pub const PLUGIN_BLOCK: usize = 1024;

/// Size a plugin's buffers to `k` frames: vst3-host processes as many frames
/// as the vectors hold (not `block_size`). Within their capacity
/// (PLUGIN_BLOCK): no allocation.
#[cfg(windows)]
pub fn fit(buffers: &mut vst3_host::audio::AudioBuffers, k: usize) {
    for v in buffers.inputs.iter_mut().chain(buffers.outputs.iter_mut()) {
        v.resize(k, 0.0);
    }
    buffers.block_size = k;
}

/// A fixed stereo delay, its line allocated up front (on the UI thread).
pub struct Delay {
    l: Box<[f32]>,
    r: Box<[f32]>,
    pos: usize,
}

impl Delay {
    pub fn new(frames: usize) -> Self {
        Delay { l: vec![0.0; frames].into(), r: vec![0.0; frames].into(), pos: 0 }
    }

    pub fn frames(&self) -> usize {
        self.l.len()
    }

    /// Delay l/r in place.
    pub fn run(&mut self, l: &mut [f32], r: &mut [f32]) {
        let len = self.l.len();
        if len == 0 {
            return;
        }
        for i in 0..l.len().min(r.len()) {
            std::mem::swap(&mut l[i], &mut self.l[self.pos]);
            std::mem::swap(&mut r[i], &mut self.r[self.pos]);
            self.pos = if self.pos + 1 == len { 0 } else { self.pos + 1 };
        }
    }

    /// Take l/r in without giving anything back (the line stays current).
    pub fn feed(&mut self, l: &[f32], r: &[f32]) {
        let len = self.l.len();
        if len == 0 {
            return;
        }
        for i in 0..l.len().min(r.len()) {
            self.l[self.pos] = l[i];
            self.r[self.pos] = r[i];
            self.pos = if self.pos + 1 == len { 0 } else { self.pos + 1 };
        }
    }

    pub fn clear(&mut self) {
        self.l.fill(0.0);
        self.r.fill(0.0);
    }

    /// Take over `old`'s contents when it is as long (a rebuilt rack plays on). No allocation.
    pub fn carry_from(&mut self, old: &mut Delay) {
        if self.frames() == old.frames() {
            std::mem::swap(self, old);
        }
    }
}

/// What makes a unit's sound.
enum Engine {
    /// A Splash audio shader, with the state the audio thread owns.
    Shader {
        shader: aot::AudioShaderRef,
        ctx: Box<[u32]>,
        state: Box<[u32]>,
        scratch: Box<[u32]>,
        /// Param bits last handed to the shader.
        applied: Box<[u32]>,
        frame: u32,
    },
    /// A VST3 plugin (shared with the UI, which holds its editor). A rebuilt
    /// rack starts without one and takes it over from the rack it replaces
    /// (`adopt`): never a reload.
    #[cfg(windows)]
    Plugin { plugin: Option<crate::plugins::Shared>, buffers: vst3_host::audio::AudioBuffers },
}

/// One playing effect.
pub struct FxUnit {
    pub id: String,
    pub controls: Arc<FxControls>,
    engine: Engine,
    /// Frames its output comes late (a plugin's latency; see `FxRack::compensate`).
    pub latency: u32,
    /// Bypassed, a latent effect still delays its lane by its latency: the dry signal through this.
    bypass_delay: Delay,
}

impl FxUnit {
    pub fn new(id: &str, shader: aot::AudioShaderRef, controls: Arc<FxControls>, sample_rate: u32) -> Self {
        let mut ctx = vec![0u32; shader.ctx_words()].into_boxed_slice();
        shader.init_ctx(&mut ctx, sample_rate as f32);
        let applied = shader.params().iter().map(|p| p.default.to_bits()).collect();
        FxUnit {
            id: id.to_string(),
            controls,
            engine: Engine::Shader {
                state: shader.state_init().into(),
                scratch: vec![0u32; shader.scratch_words()].into_boxed_slice(),
                ctx,
                applied,
                frame: 0,
                shader,
            },
            latency: 0,
            bypass_delay: Delay::new(0),
        }
    }

    /// A plugin slot (its instance arrives by `install` or `adopt`).
    #[cfg(windows)]
    pub fn plugin(id: &str, controls: Arc<FxControls>, sample_rate: u32) -> Self {
        let buffers = vst3_host::audio::AudioBuffers::new(2, 2, PLUGIN_BLOCK, sample_rate as f64);
        FxUnit { id: id.to_string(), controls, engine: Engine::Plugin { plugin: None, buffers }, latency: 0, bypass_delay: Delay::new(0) }
    }

    pub fn is_plugin(&self) -> bool {
        #[cfg(windows)]
        return matches!(self.engine, Engine::Plugin { .. });
        #[cfg(not(windows))]
        false
    }

    /// Take over from the unit this one replaces: a shader's tails, delay lines
    /// and knob smoothing carry on; a plugin instance moves over. No allocation.
    pub fn carry_from(&mut self, old: &mut FxUnit) {
        self.bypass_delay.carry_from(&mut old.bypass_delay);
        match (&mut self.engine, &mut old.engine) {
            (Engine::Shader { shader, state, frame, .. }, Engine::Shader { shader: old_shader, state: old_state, frame: old_frame, .. }) => {
                shader.carry_state(state, old_shader, old_state);
                *frame = *old_frame;
            }
            #[cfg(windows)]
            (Engine::Plugin { plugin, .. }, Engine::Plugin { plugin: old_plugin, .. }) => {
                if plugin.is_none() {
                    *plugin = old_plugin.take();
                }
            }
            #[cfg(windows)]
            _ => {}
        }
    }

    /// Process l/r in place; `tmp` is scratch of at least MAX_FRAMES frames.
    fn process(&mut self, l: &mut [f32], r: &mut [f32], tmp_l: &mut [f32], tmp_r: &mut [f32]) {
        if self.controls.bypass.load(Ordering::Relaxed) {
            // an isolated plugin keeps its place in time; the lane stays as late
            #[cfg(windows)]
            if let Engine::Plugin { plugin: Some(h), .. } = &self.engine {
                if let Some(b) = &h.bridge {
                    b.run(l, r, true);
                }
            }
            self.bypass_delay.run(l, r);
            return;
        }
        // (a latent plugin's bypass line is fed below, once it is known whether it plays)
        match &mut self.engine {
            Engine::Shader { shader, ctx, state, scratch, applied, frame } => {
                for (k, target) in self.controls.params.iter().enumerate() {
                    let bits = target.load(Ordering::Relaxed);
                    if bits != applied[k] {
                        shader.set_param(ctx, k, f32::from_bits(bits));
                        applied[k] = bits;
                    }
                }
                let n = l.len().min(r.len());
                let mut done = 0;
                while done < n {
                    let k = (n - done).min(aot::MAX_FRAMES as usize).min(tmp_l.len());
                    ctx[aot::CTX_FRAME as usize] = *frame;
                    tmp_l[..k].fill(0.0);
                    tmp_r[..k].fill(0.0);
                    shader.run(ctx, state, scratch, &l[done..done + k], &r[done..done + k], &mut tmp_l[..k], &mut tmp_r[..k], k);
                    // A shader that blows up goes quiet instead of deafening anyone.
                    for j in 0..k {
                        l[done + j] = if tmp_l[j].is_finite() { tmp_l[j].clamp(-4.0, 4.0) } else { 0.0 };
                        r[done + j] = if tmp_r[j].is_finite() { tmp_r[j].clamp(-4.0, 4.0) } else { 0.0 };
                    }
                    *frame = frame.wrapping_add(k as u32);
                    done += k;
                }
            }
            #[cfg(windows)]
            Engine::Plugin { plugin, buffers } => {
                let Some(hosted) = plugin.as_ref() else { return };
                // in its own process: its worker has the answer, BRIDGE_DELAY late
                if let Some(b) = &hosted.bridge {
                    self.bypass_delay.feed(l, r);
                    b.run(l, r, false);
                    return;
                }
                // the UI has it (its editor, a knob): this block passes through, as late as the plugin would be
                let Ok(mut runner) = hosted.plugin.try_lock() else {
                    crate::plugins::MISSED_BLOCKS.fetch_add(1, Ordering::Relaxed);
                    self.bypass_delay.run(l, r);
                    return;
                };
                self.bypass_delay.feed(l, r);
                let n = l.len().min(r.len());
                let mut done = 0;
                while done < n {
                    let k = (n - done).min(PLUGIN_BLOCK);
                    fit(buffers, k);
                    buffers.inputs[0][..k].copy_from_slice(&l[done..done + k]);
                    buffers.inputs[1][..k].copy_from_slice(&r[done..done + k]);
                    if runner.process_audio(buffers).is_ok() {
                        let (out_l, out_r) = (&buffers.outputs[0], &buffers.outputs[buffers.outputs.len().min(2) - 1]);
                        for j in 0..k {
                            l[done + j] = if out_l[j].is_finite() { out_l[j].clamp(-4.0, 4.0) } else { 0.0 };
                            r[done + j] = if out_r[j].is_finite() { out_r[j].clamp(-4.0, 4.0) } else { 0.0 };
                        }
                    }
                    done += k;
                }
            }
        }
    }
}
/// Frames a lane buffer holds; mix blocks are cut to this.
pub const LANE_FRAMES: usize = 1 << 14;

/// Every effect in the song, ready to play: one chain per lane (in track
/// order) and the master chain, plus the buffers they process in.
pub struct FxRack {
    pub lanes: Vec<Vec<FxUnit>>,
    pub master: Vec<FxUnit>,
    /// MIDI lanes' instruments and notes.
    pub midi: Vec<crate::synth::MidiLane>,
    /// The song's beats (frame, starts a bar) for the metronome.
    pub clicks: Vec<(u64, bool)>,
    /// The song this rack plays (0: none): state and plugin instances only
    /// carry over between racks of one song.
    pub song: u64,
    pub(crate) lane_l: Vec<f32>,
    pub(crate) lane_r: Vec<f32>,
    tmp_l: Vec<f32>,
    tmp_r: Vec<f32>,
    /// Latency compensation (set by `compensate`): each lane's latency (its
    /// instrument and effects), and the most of any lane, which every other
    /// path is delayed to meet: lanes through the rack by the difference,
    /// everything mixed straight in (lanes without effects, section takes) by
    /// all of it.
    pub lane_latency: Vec<u32>,
    pub latency: u32,
    lane_delay: Vec<Delay>,
    dry_delay: Delay,
    /// Lanes through the rack, summed, while the dry mix waits (latency > 0).
    pub(crate) wet_l: Vec<f32>,
    pub(crate) wet_r: Vec<f32>,
}

impl FxRack {
    pub fn new(lanes: Vec<Vec<FxUnit>>, master: Vec<FxUnit>) -> Self {
        let tmp = aot::MAX_FRAMES as usize;
        FxRack {
            lanes,
            master,
            midi: Vec::new(),
            clicks: Vec::new(),
            song: 0,
            lane_l: vec![0.0; LANE_FRAMES],
            lane_r: vec![0.0; LANE_FRAMES],
            tmp_l: vec![0.0; tmp],
            tmp_r: vec![0.0; tmp],
            lane_latency: Vec::new(),
            latency: 0,
            lane_delay: Vec::new(),
            dry_delay: Delay::new(0),
            wet_l: Vec::new(),
            wet_r: Vec::new(),
        }
    }

    /// Set every plugin's latency (`latency` by effect id, or a MIDI lane's
    /// track id) and line the lanes up: UI thread, before the rack plays (all
    /// the delay lines are allocated here).
    pub fn compensate(&mut self, latency: &dyn Fn(&str) -> u32) {
        for unit in self.lanes.iter_mut().flatten().chain(self.master.iter_mut()) {
            unit.latency = if unit.is_plugin() { latency(&unit.id) } else { 0 };
            unit.bypass_delay = Delay::new(unit.latency as usize);
        }
        for m in &mut self.midi {
            m.latency = m.plugin_id().map_or(0, latency);
        }
        let n = self.lanes.len().max(self.midi.iter().map(|m| m.track + 1).max().unwrap_or(0));
        self.lane_latency = (0..n)
            .map(|i| {
                let fx: u32 = self.lanes.get(i).map_or(0, |c| c.iter().map(|u| u.latency).sum());
                fx + self.midi.iter().filter(|m| m.track == i).map(|m| m.latency).sum::<u32>()
            })
            .collect();
        self.latency = self.lane_latency.iter().copied().max().unwrap_or(0);
        let most = self.latency;
        self.lane_delay = self.lane_latency.iter().map(|&l| Delay::new((most - l) as usize)).collect();
        self.dry_delay = Delay::new(most as usize);
        let wet = if most > 0 { LANE_FRAMES } else { 0 };
        self.wet_l = vec![0.0; wet];
        self.wet_r = vec![0.0; wet];
    }

    /// The master chain's latency (an offline render drops it from the front).
    pub fn master_latency(&self) -> u32 {
        self.master.iter().map(|u| u.latency).sum()
    }

    /// Playback jumped: what the delay lines hold is from elsewhere in the song.
    pub fn clear_delays(&mut self) {
        for d in self.lane_delay.iter_mut().chain(std::iter::once(&mut self.dry_delay)) {
            d.clear();
        }
        for unit in self.lanes.iter_mut().flatten().chain(self.master.iter_mut()) {
            unit.bypass_delay.clear();
        }
    }

    /// Lane `lane`'s first `n` frames (through its effects) into the mix: as
    /// late as the latest lane, into the wet sum, or straight into out.
    pub(crate) fn lane_out(&mut self, lane: usize, n: usize, out_l: &mut [f32], out_r: &mut [f32]) {
        let (ll, lr) = (&mut self.lane_l[..n], &mut self.lane_r[..n]);
        let (to_l, to_r) = if self.latency > 0 {
            if let Some(d) = self.lane_delay.get_mut(lane) {
                d.run(ll, lr);
            }
            (&mut self.wet_l[..n], &mut self.wet_r[..n])
        } else {
            (&mut out_l[..n], &mut out_r[..n])
        };
        for i in 0..n {
            to_l[i] += ll[i];
            to_r[i] += lr[i];
        }
    }

    /// The dry mix in out, delayed to meet the lanes through the rack, plus them.
    pub(crate) fn join_wet(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        if self.latency == 0 {
            return;
        }
        let n = out_l.len().min(out_r.len());
        self.dry_delay.run(&mut out_l[..n], &mut out_r[..n]);
        for i in 0..n {
            out_l[i] += self.wet_l[i];
            out_r[i] += self.wet_r[i];
        }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new(), Vec::new())
    }

    pub fn has_midi(&self, lane: usize) -> bool {
        self.midi.iter().any(|m| m.track == lane)
    }

    /// Render MIDI lane lane over [start, start + n) into the lane buffer.
    pub(crate) fn render_midi(&mut self, lane: usize, start: u64, n: usize) {
        if let Some(m) = self.midi.iter_mut().find(|m| m.track == lane) {
            m.render(start, &mut self.lane_l[..n], &mut self.lane_r[..n]);
        }
    }

    /// A key played live on MIDI lane `lane`.
    pub fn live_key(&mut self, lane: usize, pitch: u8, vel: u8, on: bool) {
        if let Some(m) = self.midi.iter_mut().find(|m| m.track == lane) {
            m.live_key(pitch, vel, on);
        }
    }

    /// Let go of the keys held live on `lane`.
    pub fn live_release(&mut self, lane: usize) {
        if let Some(m) = self.midi.iter_mut().find(|m| m.track == lane) {
            m.live_release();
        }
    }

    /// Only MIDI lane `lane`'s instrument (what the keys play) into the lane buffer.
    pub(crate) fn render_live(&mut self, lane: usize, n: usize) {
        if let Some(m) = self.midi.iter_mut().find(|m| m.track == lane) {
            m.render_live(&mut self.lane_l[..n], &mut self.lane_r[..n]);
        }
    }

    /// Playback jumped (resumed, seeked): MIDI lanes release what sounds.
    pub fn midi_jumped(&mut self) {
        for m in &mut self.midi {
            m.jumped();
        }
    }

    pub fn lane_has_fx(&self, lane: usize) -> bool {
        self.lanes.get(lane).is_some_and(|c| !c.is_empty())
    }

    /// Carry every kept effect over from the rack this one replaces: shader
    /// state, and plugin instances (moved, not reloaded). No allocation.
    pub fn adopt(&mut self, old: &mut FxRack) {
        // another song's `fx1` or `midi1` is a different effect or instrument
        if self.song != old.song {
            return;
        }
        for unit in self.lanes.iter_mut().flatten().chain(self.master.iter_mut()) {
            if let Some(o) = old.lanes.iter_mut().flatten().chain(old.master.iter_mut()).find(|o| o.id == unit.id) {
                unit.carry_from(o);
            }
        }
        // the lanes stay lined up through a rebuild (a note edit): no gap
        for (d, o) in self.lane_delay.iter_mut().zip(old.lane_delay.iter_mut()) {
            d.carry_from(o);
        }
        self.dry_delay.carry_from(&mut old.dry_delay);
        // plugin instruments move over with the notes they hold (the new lane releases them)
        #[cfg(windows)]
        for m in &mut self.midi {
            let crate::synth::Sound::Plugin(p) = &mut m.sound else { continue };
            if p.hosted.is_some() {
                continue;
            }
            for o in &mut old.midi {
                if let crate::synth::Sound::Plugin(op) = &mut o.sound {
                    if op.id == p.id && op.hosted.is_some() {
                        p.take_over(op);
                        break;
                    }
                }
            }
        }
    }

    /// Give MIDI lane instrument `id` its plugin instance.
    #[cfg(windows)]
    pub fn install_instrument(&mut self, id: &str, new: crate::plugins::Shared) -> Result<(), crate::plugins::Shared> {
        for m in &mut self.midi {
            if let crate::synth::Sound::Plugin(p) = &mut m.sound {
                if p.id == id {
                    p.hosted = Some(new);
                    return Ok(());
                }
            }
        }
        Err(new)
    }

    /// Take every plugin instrument out (see `take_runners`).
    #[cfg(windows)]
    pub fn take_instruments(&mut self) -> Vec<(String, crate::plugins::Shared)> {
        let mut out = Vec::new();
        for m in &mut self.midi {
            if let crate::synth::Sound::Plugin(p) = &mut m.sound {
                if let Some(h) = p.hosted.take() {
                    out.push((p.id.clone(), h));
                }
            }
        }
        out
    }

    /// Give plugin unit `id` its instance (a newly loaded plugin).
    #[cfg(windows)]
    pub fn install(&mut self, id: &str, new: crate::plugins::Shared) -> Result<(), crate::plugins::Shared> {
        for unit in self.lanes.iter_mut().flatten().chain(self.master.iter_mut()) {
            if let (true, Engine::Plugin { plugin, .. }) = (unit.id == id, &mut unit.engine) {
                *plugin = Some(new);
                return Ok(());
            }
        }
        Err(new)
    }

    /// Take every plugin instance out (a rack that never reached the audio
    /// thread hands them back instead of destroying them).
    #[cfg(windows)]
    pub fn take_runners(&mut self) -> Vec<(String, crate::plugins::Shared)> {
        let mut out = Vec::new();
        for unit in self.lanes.iter_mut().flatten().chain(self.master.iter_mut()) {
            if let Engine::Plugin { plugin, .. } = &mut unit.engine {
                if let Some(r) = plugin.take() {
                    out.push((unit.id.clone(), r));
                }
            }
        }
        out
    }

    /// Run lane `lane`'s chain over the first `n` frames of the lane buffer.
    pub(crate) fn process_lane(&mut self, lane: usize, n: usize) {
        let Some(chain) = self.lanes.get_mut(lane) else { return };
        for unit in chain {
            unit.process(&mut self.lane_l[..n], &mut self.lane_r[..n], &mut self.tmp_l, &mut self.tmp_r);
        }
    }

    pub(crate) fn process_master(&mut self, l: &mut [f32], r: &mut [f32]) {
        for unit in &mut self.master {
            unit.process(l, r, &mut self.tmp_l, &mut self.tmp_r);
        }
    }
}

/// Compile one effect's shader from its file. Effects must be `effect(l, r)`.
pub fn compile_fx(dir: &Path, fx: &Fx) -> Result<aot::AudioShaderRef, String> {
    let path = dir.join(&fx.file);
    let code = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    compile_source(&code)
}

/// Compile an instrument (oice()) for a MIDI lane.
pub fn compile_instrument(code: &str) -> Result<aot::AudioShaderRef, String> {
    let shader = aot::compile(code).map_err(|errors| errors.iter().map(|e| format!("{e:?}")).collect::<Vec<_>>().join("; "))?;
    if shader.kind() != aot::Kind::Instrument {
        return Err("this shader is an effect (effect(l, r)); a MIDI lane plays an instrument with voice()".into());
    }
    Ok(shader)
}

pub fn compile_source(code: &str) -> Result<aot::AudioShaderRef, String> {
    let shader = aot::compile(code).map_err(|errors| errors.iter().map(|e| format!("{e:?}")).collect::<Vec<_>>().join("; "))?;
    if shader.kind() != aot::Kind::Effect {
        return Err("this shader is an instrument (voice()); a lane effect needs effect(l, r)".into());
    }
    Ok(shader)
}

/// A rack for the project: compiled shaders are looked up in `compiled` (by
/// effect id) and controls come from `controls` (shared with the UI), so a
/// rebuild keeps the knobs the UI is turning. Effects that do not compile
/// are left out (the caller reports them).
pub fn build_rack(
    p: &Project,
    compiled: &dyn Fn(&str) -> Option<aot::AudioShaderRef>,
    controls: &dyn Fn(&str) -> Option<Arc<FxControls>>,
) -> FxRack {
    let sr = p.sr();
    let units = |chain: &[Fx]| -> Vec<FxUnit> {
        chain
            .iter()
            .filter_map(|fx| {
                let c = controls(&fx.id)?;
                if fx.plugin.is_some() {
                    #[cfg(windows)]
                    return Some(FxUnit::plugin(&fx.id, c, sr));
                    #[cfg(not(windows))]
                    return None;
                }
                let shader = compiled(&fx.id)?;
                Some(FxUnit::new(&fx.id, shader, c, sr))
            })
            .collect()
    };
    let lanes = (0..p.tracks.len()).map(|i| units(p.chain(Some(i)))).collect();
    FxRack::new(lanes, units(p.chain(None)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HALF: &str = "let gain = param(0.5, 0, 1)\nfn effect(l, r) { vec2(l * gain, r * gain) }\n";

    fn unit(code: &str) -> FxUnit {
        let shader = compile_source(code).unwrap();
        let fx = Fx { id: "fx1".into(), name: "half".into(), file: String::new(), params: None, bypass: None, prompt: None, plugin: None, isolate: None };
        let controls = FxControls::new(&shader, &fx, None);
        FxUnit::new("fx1", shader, controls, 48000)
    }

    #[test]
    fn an_effect_processes_in_place_and_its_knob_moves() {
        let mut rack = FxRack::new(vec![vec![unit(HALF)]], Vec::new());
        rack.lane_l[..256].fill(1.0);
        rack.lane_r[..256].fill(1.0);
        rack.process_lane(0, 256);
        assert!((rack.lane_l[200] - 0.5).abs() < 1e-3, "{}", rack.lane_l[200]);
        rack.lanes[0][0].controls.set(0, 1.0);
        for _ in 0..20 {
            rack.lane_l[..256].fill(1.0);
            rack.lane_r[..256].fill(1.0);
            rack.process_lane(0, 256);
        }
        assert!((rack.lane_l[255] - 1.0).abs() < 1e-2, "the knob ramps to its new value: {}", rack.lane_l[255]);
        rack.lanes[0][0].controls.bypass.store(true, Ordering::Relaxed);
        rack.lane_l[..8].fill(0.25);
        rack.process_lane(0, 8);
        assert_eq!(rack.lane_l[0], 0.25, "bypassed: untouched");
    }

    #[test]
    fn instruments_are_not_lane_effects() {
        assert!(compile_source("fn voice() { 0.0 }").is_err());
    }
}
