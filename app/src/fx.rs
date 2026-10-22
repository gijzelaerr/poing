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
    pub fn set(&self, index: usize, value: f32) {
        if let Some(p) = self.params.get(index) {
            p.store(value.to_bits(), Ordering::Relaxed);
        }
    }
}

/// One playing effect: its shader and the state the audio thread owns.
pub struct FxUnit {
    pub id: String,
    pub shader: aot::AudioShaderRef,
    pub controls: Arc<FxControls>,
    ctx: Box<[u32]>,
    state: Box<[u32]>,
    scratch: Box<[u32]>,
    /// Param bits last handed to the shader.
    applied: Box<[u32]>,
    frame: u32,
}

impl FxUnit {
    pub fn new(id: &str, shader: aot::AudioShaderRef, controls: Arc<FxControls>, sample_rate: u32) -> Self {
        let mut ctx = vec![0u32; shader.ctx_words()].into_boxed_slice();
        shader.init_ctx(&mut ctx, sample_rate as f32);
        let applied = shader.params().iter().map(|p| p.default.to_bits()).collect();
        FxUnit {
            id: id.to_string(),
            state: shader.state_init().into(),
            scratch: vec![0u32; shader.scratch_words()].into_boxed_slice(),
            ctx,
            applied,
            frame: 0,
            controls,
            shader,
        }
    }

    /// Take over the state of the unit this one replaces (same effect, new
    /// build): tails, delay lines and knob smoothing carry on. No allocation.
    pub fn carry_from(&mut self, old: &FxUnit) {
        self.shader.carry_state(&mut self.state, &old.shader, &old.state);
        self.frame = old.frame;
    }

    /// Process l/r in place; `tmp` is scratch of at least MAX_FRAMES frames.
    fn process(&mut self, l: &mut [f32], r: &mut [f32], tmp_l: &mut [f32], tmp_r: &mut [f32]) {
        if self.controls.bypass.load(Ordering::Relaxed) {
            return;
        }
        for (k, target) in self.controls.params.iter().enumerate() {
            let bits = target.load(Ordering::Relaxed);
            if bits != self.applied[k] {
                self.shader.set_param(&mut self.ctx, k, f32::from_bits(bits));
                self.applied[k] = bits;
            }
        }
        let n = l.len().min(r.len());
        let mut done = 0;
        while done < n {
            let k = (n - done).min(aot::MAX_FRAMES as usize).min(tmp_l.len());
            self.ctx[aot::CTX_FRAME as usize] = self.frame;
            tmp_l[..k].fill(0.0);
            tmp_r[..k].fill(0.0);
            self.shader.run(
                &mut self.ctx,
                &mut self.state,
                &mut self.scratch,
                &l[done..done + k],
                &r[done..done + k],
                &mut tmp_l[..k],
                &mut tmp_r[..k],
                k,
            );
            // A shader that blows up goes quiet instead of deafening anyone.
            for j in 0..k {
                l[done + j] = if tmp_l[j].is_finite() { tmp_l[j].clamp(-4.0, 4.0) } else { 0.0 };
                r[done + j] = if tmp_r[j].is_finite() { tmp_r[j].clamp(-4.0, 4.0) } else { 0.0 };
            }
            self.frame = self.frame.wrapping_add(k as u32);
            done += k;
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
    pub(crate) lane_l: Vec<f32>,
    pub(crate) lane_r: Vec<f32>,
    tmp_l: Vec<f32>,
    tmp_r: Vec<f32>,
}

impl FxRack {
    pub fn new(lanes: Vec<Vec<FxUnit>>, master: Vec<FxUnit>) -> Self {
        let tmp = aot::MAX_FRAMES as usize;
        FxRack { lanes, master, lane_l: vec![0.0; LANE_FRAMES], lane_r: vec![0.0; LANE_FRAMES], tmp_l: vec![0.0; tmp], tmp_r: vec![0.0; tmp] }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new(), Vec::new())
    }

    pub fn lane_has_fx(&self, lane: usize) -> bool {
        self.lanes.get(lane).is_some_and(|c| !c.is_empty())
    }

    /// Carry every kept effect's state over from the rack this one replaces.
    pub fn adopt(&mut self, old: &FxRack) {
        let olds = || old.lanes.iter().flatten().chain(old.master.iter());
        for unit in self.lanes.iter_mut().flatten().chain(self.master.iter_mut()) {
            if let Some(o) = olds().find(|o| o.id == unit.id) {
                unit.carry_from(o);
            }
        }
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
                let shader = compiled(&fx.id)?;
                let c = controls(&fx.id)?;
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
        let fx = Fx { id: "fx1".into(), name: "half".into(), file: String::new(), params: None, bypass: None, prompt: None };
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
