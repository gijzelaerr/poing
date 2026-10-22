//! Editor state shared between the App (inspector, transport) and the Timeline
//! widget through Scope data.

use crate::audio::{self, MixParams, Shared, Stem, Stems, TakeKind};
use crate::fx::{self, FxControls, FxRack};
use crate::model::{default_gain_for, Analysis, Fx, Project, Region, Take, Track};
use makepad_script_audio_aot::AudioShaderRef;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Selection {
    None,
    Track(usize),
    Region(usize),
    Section(usize),
    /// An effect: its lane (None: the master) and place in the chain.
    Fx(Option<usize>, usize),
}

pub struct EditorState {
    pub project: Project,
    pub path: PathBuf,
    pub stems: Stems,
    pub shared: Arc<Shared>,
    /// Stem sets the audio thread may still hold; dropped here once it moved on.
    retired: Vec<Stems>,
    /// Background job in progress (analysis or a take): (message, 0..1).
    pub analysing: Option<(String, f32)>,
    /// Takes being heard (A/B), at most one per slot; empty plays the lanes.
    pub active: Vec<String>,
    /// The take the inspector shows: the one last switched on.
    pub focus: Option<String>,
    pub sel: Selection,
    /// pixels per second; 0 asks the timeline to fit the song on its next draw
    pub pps: f64,
    /// time (s) at the left edge of the waveform area
    pub scroll_t: f64,
    /// set when the selection or the selected item changed; the App refreshes the inspector
    pub sel_changed: bool,
    pub dirty: bool,
    mix_version: u64,
    next_region: u64,
    /// Compiled effect shaders by effect id.
    fx_shaders: HashMap<String, AudioShaderRef>,
    /// The file time each effect was last compiled from (good build or not).
    fx_mtimes: HashMap<String, Option<SystemTime>>,
    /// Knob and bypass controls by effect id, shared with the audio thread.
    pub fx_controls: HashMap<String, Arc<FxControls>>,
    /// Effects that do not compile right now, with why.
    pub fx_errors: HashMap<String, String>,
    fx_polled: Instant,
    /// Song states before each edit (Ctrl+Z) and after each undo (Ctrl+Shift+Z).
    undo: Vec<(Project, Stems)>,
    redo: Vec<(Project, Stems)>,
    /// A message for the status line from an edit made in the timeline.
    pub notice: Option<String>,
    /// The timeline's area: key focus there (or nowhere) means shortcuts apply.
    pub timeline_area: makepad_widgets::Area,
    /// Meter levels as shown (they fall back slowly): lanes, then master.
    pub meter_show: Vec<f32>,
}

impl EditorState {
    /// `shared`: the audio output's state when one is already playing another
    /// song; this song's stems and mix are published into it.
    pub fn new(project: Project, path: PathBuf, stems: Vec<Stem>, shared: Option<Arc<Shared>>) -> Self {
        let stems: Stems = Arc::new(stems.into_iter().map(Arc::new).collect());
        // Mix versions keep counting up across songs, so the callback sees the change.
        let mix_version = shared.as_ref().and_then(|s| s.mix.lock().ok().map(|m| m.version + 1)).unwrap_or(0);
        let mix = MixParams::from_project(&project, mix_version, &[]);
        let shared = match shared {
            Some(s) => {
                s.set_playing(false);
                s.seek(0);
                if let Ok(mut m) = s.mix.lock() {
                    *m = mix;
                }
                if let Ok(mut set) = s.stems.lock() {
                    *set = stems.clone();
                }
                s.stems_gen.fetch_add(1, Ordering::AcqRel);
                s
            }
            None => Arc::new(Shared::new(mix, stems.clone())),
        };
        let next_region = project
            .regions()
            .iter()
            .filter_map(|r| r.id.trim_start_matches('r').parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        Self {
            project,
            path,
            shared,
            stems,
            retired: Vec::new(),
            analysing: None,
            active: Vec::new(),
            focus: None,
            sel: Selection::None,
            pps: 0.0,
            scroll_t: 0.0,
            sel_changed: true,
            dirty: false,
            mix_version,
            next_region,
            fx_shaders: HashMap::new(),
            fx_mtimes: HashMap::new(),
            fx_controls: HashMap::new(),
            fx_errors: HashMap::new(),
            fx_polled: Instant::now(),
            undo: Vec::new(),
            redo: Vec::new(),
            notice: None,
            timeline_area: makepad_widgets::Area::Empty,
            meter_show: vec![0.0; audio::METER_LANES + 1],
        }
        .with_fx()
    }

    fn with_fx(mut self) -> Self {
        let ids: Vec<String> = self.all_fx().iter().map(|f| f.id.clone()).collect();
        for id in ids {
            self.compile_fx(&id);
        }
        self.publish_fx();
        self
    }

    fn dir(&self) -> PathBuf {
        self.path.parent().map(|p| p.to_path_buf()).unwrap_or_default()
    }

    /// Every effect, lanes first then the master.
    pub fn all_fx(&self) -> Vec<Fx> {
        let lanes = (0..self.project.tracks.len()).flat_map(|i| self.project.chain(Some(i)).to_vec());
        lanes.chain(self.project.chain(None).to_vec()).collect()
    }

    pub fn fx(&self, id: &str) -> Option<&Fx> {
        let (lane, i) = self.project.find_fx(id)?;
        self.project.chain(lane).get(i)
    }

    /// (Re)compile effect `id` from its file. Knobs it still has keep their
    /// values; it plays from the next rack.
    fn compile_fx(&mut self, id: &str) -> bool {
        let Some(fx) = self.fx(id).cloned() else { return false };
        let file = self.dir().join(&fx.file);
        let mtime = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
        self.fx_mtimes.insert(id.to_string(), mtime);
        match fx::compile_fx(&self.dir(), &fx) {
            Ok(shader) => {
                let bpm = self.project.analysis.as_ref().and_then(|a| a.tempo);
                self.fx_controls.insert(id.to_string(), FxControls::new(&shader, &fx, bpm));
                self.fx_shaders.insert(id.to_string(), shader);
                self.fx_errors.remove(id);
                true
            }
            Err(e) => {
                // the last good build, if any, keeps playing
                self.fx_errors.insert(id.to_string(), e);
                false
            }
        }
    }

    /// A rack for the song as it is now (fresh effect state).
    pub fn make_rack(&self) -> FxRack {
        fx::build_rack(&self.project, &|id| self.fx_shaders.get(id).cloned(), &|id| self.fx_controls.get(id).cloned())
    }

    /// Hand the audio thread a rack for the current effects (it carries
    /// over the state of every effect it keeps).
    pub fn publish_fx(&mut self) {
        let rack = Box::new(self.make_rack());
        if let Ok(mut slot) = self.shared.fx_in.lock() {
            *slot = Some(rack);
        }
    }

    /// Add an effect written as `code` to a lane (or the master).
    pub fn add_fx(&mut self, lane: Option<usize>, name: &str, code: &str, prompt: Option<String>) -> Result<String, String> {
        fx::compile_source(code)?;
        self.checkpoint();
        let id = self.project.next_fx();
        let file = format!("fx/{id}.splash");
        let dir = self.dir();
        std::fs::create_dir_all(dir.join("fx")).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(&file), code).map_err(|e| e.to_string())?;
        let fx = Fx { id: id.clone(), name: name.to_string(), file, params: None, bypass: None, prompt };
        self.project.chain_mut(lane).push(fx);
        self.compile_fx(&id);
        self.publish_fx();
        self.sel_changed = true;
        self.save()?;
        Ok(id)
    }

    pub fn remove_fx(&mut self, id: &str) -> Result<(), String> {
        let Some((lane, i)) = self.project.find_fx(id) else { return Ok(()) };
        self.project.chain_mut(lane).remove(i);
        self.fx_shaders.remove(id);
        self.fx_mtimes.remove(id);
        self.fx_controls.remove(id);
        self.fx_errors.remove(id);
        self.publish_fx();
        self.sel_changed = true;
        self.save()
    }

    /// Turn a knob: heard at once, remembered in the project.
    pub fn set_fx_param(&mut self, id: &str, index: usize, value: f64) {
        let Some(shader) = self.fx_shaders.get(id) else { return };
        let Some(name) = shader.params().get(index).map(|p| p.name.clone()) else { return };
        if let Some(c) = self.fx_controls.get(id) {
            c.set(index, value as f32);
        }
        if let Some((lane, i)) = self.project.find_fx(id) {
            self.project.chain_mut(lane)[i].set_param(&name, value);
            self.dirty = true;
        }
    }

    pub fn set_fx_bypass(&mut self, id: &str, bypass: bool) {
        if let Some(c) = self.fx_controls.get(id) {
            c.bypass.store(bypass, Ordering::Relaxed);
        }
        if let Some((lane, i)) = self.project.find_fx(id) {
            self.project.chain_mut(lane)[i].bypass = Some(bypass);
            self.dirty = true;
        }
    }

    /// The shader param behind knob `knob` (knobs skip `bpm`, set from the song).
    pub fn fx_param_index(&self, id: &str, knob: usize) -> Option<usize> {
        let shader = self.fx_shaders.get(id)?;
        shader.params().iter().enumerate().filter(|(_, p)| p.name != "bpm").nth(knob).map(|(i, _)| i)
    }

    /// The knobs of effect `id`: (name, value, min, max).
    pub fn fx_knobs(&self, id: &str) -> Vec<(String, f64, f64, f64)> {
        let (Some(shader), Some(fx)) = (self.fx_shaders.get(id), self.fx(id)) else { return Vec::new() };
        shader
            .params()
            .iter()
            .filter(|p| p.name != "bpm")
            .map(|p| (p.name.clone(), fx.param(&p.name).unwrap_or(p.default as f64), p.min as f64, p.max as f64))
            .collect()
    }

    /// Live coding: recompile effects whose file changed (at most once a
    /// second). Returns what happened, for the status line.
    pub fn reload_changed_fx(&mut self) -> Option<String> {
        if self.fx_polled.elapsed().as_secs_f64() < 1.0 {
            return None;
        }
        self.fx_polled = Instant::now();
        let dir = self.dir();
        let changed: Vec<Fx> = self
            .all_fx()
            .into_iter()
            .filter(|f| {
                let mtime = std::fs::metadata(dir.join(&f.file)).and_then(|m| m.modified()).ok();
                self.fx_mtimes.get(&f.id) != Some(&mtime)
            })
            .collect();
        if changed.is_empty() {
            return None;
        }
        let mut notes = Vec::new();
        for f in &changed {
            if self.compile_fx(&f.id) {
                notes.push(format!("{} recompiled", f.name));
            } else {
                notes.push(format!("{}: {}", f.name, self.fx_errors.get(&f.id).cloned().unwrap_or_default()));
            }
        }
        self.publish_fx();
        self.sel_changed = true;
        Some(notes.join("  ·  "))
    }

    /// This song's stem sets the audio thread may still hold: its current one
    /// and any not yet collected. For the next song to keep until it moved on.
    pub fn into_retired(self) -> Vec<Stems> {
        let mut sets = self.retired;
        sets.push(self.stems);
        sets
    }

    /// Keep a stem set alive until the audio thread has let go of it.
    pub fn retire(&mut self, stems: Stems) {
        self.retired.push(stems);
    }

    pub fn sr(&self) -> u32 {
        self.project.sr()
    }

    /// Merge a finished analysis: new stem lanes join the song while it keeps
    /// playing, sections and chords appear, and the project is saved.
    pub fn apply_analysis(&mut self, analysis: Analysis, stems: Vec<(String, String, Stem)>) -> Result<(), String> {
        let files: Vec<(String, String)> = stems.iter().map(|(id, file, _)| (id.clone(), file.clone())).collect();
        let before = self.project.tracks.len();
        let added = self.project.apply_analysis(analysis, &files);
        // The stem set is tracks then takes: new lanes go between them.
        let mut lanes: Vec<Arc<Stem>> = self.stems[..before].to_vec();
        for (id, _, stem) in stems {
            if self.project.track_index(&id).is_some_and(|i| added.contains(&i)) {
                lanes.push(Arc::new(stem));
            }
        }
        lanes.extend(self.stems[before..].iter().cloned());
        if lanes.len() != self.project.tracks.len() + self.project.takes().len() {
            return Err(format!("analysis lanes {} != tracks + takes", lanes.len()));
        }
        self.publish_stems(Arc::new(lanes));
        self.push_mix();
        self.analysing = None;
        self.sel_changed = true;
        self.save()
    }

    /// Hand the audio thread a new stem set; keep the old one until it switched.
    fn publish_stems(&mut self, stems: Stems) {
        let old = std::mem::replace(&mut self.stems, stems.clone());
        if let Ok(mut s) = self.shared.stems.lock() {
            *s = stems;
        }
        self.shared.stems_gen.fetch_add(1, Ordering::AcqRel);
        self.retired.push(old);
    }

    /// Drop retired stem sets once the audio thread runs on the current one.
    pub fn collect_retired(&mut self) {
        // a rack the audio thread replaced is dropped here, not there
        if let Ok(mut back) = self.shared.fx_out.try_lock() {
            back.take();
        }
        if !self.retired.is_empty()
            && self.shared.stems_seen.load(Ordering::Acquire) == self.shared.stems_gen.load(Ordering::Acquire)
        {
            self.retired.clear();
        }
    }
    /// A new take joins the stem set and is switched on straight away.
    pub fn add_take(&mut self, take: Take, stem: Stem) -> Result<(), String> {
        self.checkpoint();
        let id = take.id.clone();
        self.project.takes_mut().push(take);
        let mut lanes: Vec<Arc<Stem>> = self.stems.iter().cloned().collect();
        lanes.push(Arc::new(stem));
        self.publish_stems(Arc::new(lanes));
        self.analysing = None;
        // show the new take in the inspector, with its blend
        self.sel = Selection::None;
        if !self.take_on(&id) {
            self.toggle_take(&id);
        }
        self.save()
    }

    /// Delete a take: its audio file goes, the stem set shrinks.
    pub fn delete_take(&mut self, id: &str) -> Result<(), String> {
        let Some(i) = self.project.take_index(id) else { return Ok(()) };
        let take = self.project.takes_mut().remove(i);
        let mut lanes: Vec<Arc<Stem>> = self.stems.iter().cloned().collect();
        lanes.remove(self.project.tracks.len() + i);
        self.publish_stems(Arc::new(lanes));
        self.active.retain(|a| a != id);
        if self.focus.as_deref() == Some(id) {
            self.focus = None;
        }
        self.takes_changed(self.duration());
        // the audio file stays: undo brings the take back
        let _ = take;
        self.save()
    }

    /// Keep a lane take (baked into its lane as a new stem file) or a layer
    /// (a lane of its own). The stem set stays tracks then takes. Song takes
    /// are kept as a new song instead (see `promote_take`).
    pub fn keep_take(&mut self, id: &str) -> Result<String, String> {
        self.checkpoint();
        let i = self.project.take_index(id).ok_or("no such take")?;
        let take = self.project.takes()[i].clone();
        let k = MixParams::from_project(&self.project, 0, &[id.to_string()]).takes[0].ok_or("this take has no lane to go to")?;
        let (sr, tracks) = (self.sr(), self.project.tracks.len());
        let dir = self.path.parent().ok_or("project has no folder")?.to_path_buf();
        let take_stem = self.stems[tracks + i].clone();
        let n = take.id.trim_start_matches('t').to_string();
        let mut lanes: Vec<Arc<Stem>> = self.stems[..tracks].to_vec();
        let message = match k.kind {
            TakeKind::Lane(lane) => {
                let track = self.project.tracks[lane].clone();
                let baked = audio::bake_lane(&self.stems[lane], &take_stem, &k, sr);
                let file = format!("stems/{}-{}.wav", track.id, take.id);
                audio::write_wav(&dir.join(&file), &baked.l, &baked.r, sr, 1.0).map_err(|e| e.to_string())?;
                self.project.tracks[lane].file = file;
                lanes[lane] = Arc::new(baked);
                format!("Take {n} is now part of {}", track.name)
            }
            TakeKind::Layer(_) => {
                let frames = self.stems[..tracks].iter().map(|s| s.l.len()).max().unwrap_or(0);
                let placed = audio::place_layer(&take_stem, &k, sr, frames);
                let what = take.track.clone().unwrap_or_else(|| "layer".into());
                let track_id = format!("layer-{}", take.id);
                let file = format!("stems/{track_id}.wav");
                audio::write_wav(&dir.join(&file), &placed.l, &placed.r, sr, 1.0).map_err(|e| e.to_string())?;
                let mut name = what.clone();
                if let Some(c) = name.get_mut(0..1) {
                    c.make_ascii_uppercase();
                }
                self.project.tracks.push(Track {
                    id: track_id,
                    name: format!("{name} layer {n}"),
                    file,
                    gain_db: None,
                    mute: None,
                    solo: None,
                    fx: None,
                });
                lanes.push(Arc::new(placed));
                format!("Take {n} is now a lane of its own")
            }
            _ => return Err("keep a song or section take as a new song".into()),
        };
        // the remaining takes follow the tracks, in order
        for (j, s) in self.stems[tracks..].iter().enumerate() {
            if j != i {
                lanes.push(s.clone());
            }
        }
        self.project.takes_mut().remove(i);
        self.active.retain(|a| a != id);
        self.focus = None;
        self.publish_stems(Arc::new(lanes));
        self.push_mix();
        self.sel_changed = true;
        // the take's file stays: undo brings it back
        self.save()?;
        Ok(message)
    }

    pub fn take_on(&self, id: &str) -> bool {
        self.active.iter().any(|a| a == id)
    }

    /// Switch a take on or off. On, it replaces the take in its slot; a song
    /// take replaces everything, anything else switches the song take off.
    pub fn toggle_take(&mut self, id: &str) {
        let before = self.duration();
        let Some(i) = self.project.take_index(id) else { return };
        if self.take_on(id) {
            self.active.retain(|a| a != id);
            self.focus = None;
        } else {
            let take = &self.project.takes()[i];
            let (slot, song) = (take.slot(), take.scope == "song");
            let takes = self.project.takes();
            self.active.retain(|a| {
                let other = takes.iter().find(|t| &t.id == a);
                !song && other.is_some_and(|t| t.slot() != slot && t.scope != "song")
            });
            self.active.push(id.to_string());
            self.focus = Some(id.to_string());
        }
        self.takes_changed(before);
    }

    /// Back to the lanes as they are: every take off.
    pub fn all_takes_off(&mut self) {
        let before = self.duration();
        self.active.clear();
        self.focus = None;
        self.takes_changed(before);
    }

    fn takes_changed(&mut self, before_duration: f64) {
        self.push_mix();
        self.sel_changed = true;
        if (self.duration() - before_duration).abs() > 0.5 {
            self.pps = 0.0; // a song take has its own length: fit it
        }
        if self.playhead() > self.duration() {
            self.seek(0.0);
        }
    }

    /// The take shown in the inspector: the one last switched on.
    pub fn focused_take(&self) -> Option<Take> {
        let id = self.focus.as_deref().filter(|id| self.take_on(id))?;
        self.project.take_index(id).map(|i| self.project.takes()[i].clone())
    }

    /// How much of a lane or layer take is heard.
    pub fn set_blend(&mut self, id: &str, blend: f64) {
        if let Some(i) = self.project.take_index(id) {
            self.project.takes_mut()[i].blend = Some(blend.clamp(0.0, 1.0));
            self.push_mix();
        }
    }

    /// The song take being heard, if any.
    pub fn song_take(&self) -> Option<(usize, &Take)> {
        self.active.iter().filter_map(|id| self.project.take_index(id)).find_map(|i| {
            let t = &self.project.takes()[i];
            (t.scope == "song").then_some((self.project.tracks.len() + i, t))
        })
    }

    pub fn duration(&self) -> f64 {
        match self.song_take() {
            Some((_, t)) => t.end,
            None => self.project.duration,
        }
    }
    pub fn playhead(&self) -> f64 {
        self.shared.pos() as f64 / self.sr() as f64
    }
    pub fn seek(&self, t: f64) {
        self.shared.seek((t.clamp(0.0, self.duration()) * self.sr() as f64) as usize)
    }
    pub fn toggle_play(&mut self) {
        let p = !self.shared.playing();
        if p && self.playhead() >= self.duration() - 0.01 {
            self.seek(0.0);
        }
        self.shared.set_playing(p);
        self.sel_changed = true;
    }

    /// After any change that affects the sound (gains, mute/solo, regions).
    /// The lock is held only for the swap; the audio thread only try_locks.
    pub fn push_mix(&mut self) {
        self.mix_version += 1;
        let mix = MixParams::from_project(&self.project, self.mix_version, &self.active);
        if let Ok(mut m) = self.shared.mix.lock() {
            *m = mix;
        }
        self.dirty = true;
    }

    pub fn mix_snapshot(&self) -> MixParams {
        MixParams::from_project(&self.project, self.mix_version, &self.active)
    }

    pub fn select(&mut self, sel: Selection) {
        self.sel = sel;
        self.sel_changed = true;
    }

    pub fn add_region(&mut self, track: usize, start: f64, end: f64) -> usize {
        let kind = "bad".to_string();
        let r = Region {
            id: format!("r{}", self.next_region),
            track: self.project.tracks[track].id.clone(),
            start: start.min(end).max(0.0),
            end: start.max(end).min(self.duration()),
            gain_db: Some(default_gain_for(&kind)),
            kind,
            note: Some(String::new()),
        };
        self.next_region += 1;
        self.project.regions_mut().push(r);
        self.push_mix();
        self.project.regions().len() - 1
    }

    /// Delete what is selected (a region, section, effect, lane) or, with
    /// nothing selected, the take shown in the inspector. Says what it did
    /// (for the status line); everything can be undone.
    pub fn delete_selected(&mut self) -> Option<String> {
        let done = match self.sel {
            Selection::Fx(lane, i) => {
                let fx = self.project.chain(lane).get(i).cloned()?;
                self.checkpoint();
                let _ = self.remove_fx(&fx.id);
                format!("Removed {}", fx.name)
            }
            Selection::Region(i) if i < self.project.regions().len() => {
                self.checkpoint();
                let r = self.project.regions_mut().remove(i);
                self.push_mix();
                format!("Removed the {} region", r.kind)
            }
            Selection::Section(i) if self.project.sections().len() > 1 && i < self.project.sections().len() => {
                self.checkpoint();
                // merge into the previous section (or the next one if it is the first)
                let secs = self.project.sections_mut();
                let removed = secs.remove(i);
                let into = if i > 0 {
                    secs[i - 1].end = removed.end;
                    secs[i - 1].label.clone()
                } else {
                    secs[0].start = removed.start;
                    secs[0].label.clone()
                };
                self.dirty = true;
                format!("Merged the {} into the {into} before it", removed.label)
            }
            Selection::Track(i) if i < self.project.tracks.len() => {
                if self.project.tracks.len() == 1 {
                    return Some("The last lane stays: a song needs one".into());
                }
                self.checkpoint();
                let name = self.remove_track(i);
                format!("Removed the {name} lane")
            }
            Selection::None => {
                let take = self.focused_take()?;
                self.checkpoint();
                let _ = self.delete_take(&take.id);
                format!("Deleted take {}", take.id.trim_start_matches('t'))
            }
            _ => return None,
        };
        self.select(Selection::None);
        Some(format!("{done}  ·  Ctrl+Z to undo"))
    }

    /// Remove lane `i` with what hangs off it: its regions, effects, and the
    /// takes made for it. Its audio file stays in the bundle (undo needs it).
    fn remove_track(&mut self, i: usize) -> String {
        let track = self.project.tracks.remove(i);
        self.project.regions_mut().retain(|r| r.track != track.id);
        let tracks_before = self.project.tracks.len() + 1;
        // the stem set is tracks then takes: drop this lane and its takes
        let takes = self.project.takes().clone();
        let keep: Vec<bool> = takes.iter().map(|t| t.track.as_deref() != Some(track.id.as_str())).collect();
        let mut lanes: Vec<Arc<Stem>> = Vec::new();
        for (j, s) in self.stems.iter().enumerate() {
            let keep_it = if j < tracks_before { j != i } else { keep[j - tracks_before] };
            if keep_it {
                lanes.push(s.clone());
            }
        }
        let gone: Vec<String> = takes.iter().zip(&keep).filter(|(_, k)| !**k).map(|(t, _)| t.id.clone()).collect();
        self.project.takes_mut().retain(|t| !gone.contains(&t.id));
        self.active.retain(|a| !gone.contains(a));
        self.publish_stems(Arc::new(lanes));
        self.push_mix();
        self.publish_fx();
        self.dirty = true;
        track.name
    }

    /// Remember the song as it is, before an edit (for Ctrl+Z).
    pub fn checkpoint(&mut self) {
        self.undo.push((self.project.clone(), self.stems.clone()));
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Step back one edit (or forward again with `redo`).
    pub fn undo(&mut self, redo: bool) -> Option<String> {
        let (project, stems) = if redo { self.redo.pop()? } else { self.undo.pop()? };
        let now = (std::mem::replace(&mut self.project, project), self.stems.clone());
        if redo {
            self.undo.push(now);
        } else {
            self.redo.push(now);
        }
        self.publish_stems(stems);
        let project = &self.project;
        self.active.retain(|id| project.take_index(id).is_some());
        if self.focus.as_deref().is_some_and(|id| self.project.take_index(id).is_none()) {
            self.focus = None;
        }
        // effects come back from their files (kept on disk for this)
        let ids: Vec<String> = self.all_fx().iter().map(|f| f.id.clone()).collect();
        for id in ids {
            self.compile_fx(&id);
        }
        self.publish_fx();
        self.push_mix();
        self.select(Selection::None);
        let _ = self.save();
        Some(if redo { "Redone".into() } else { "Undone  ·  Ctrl+Shift+Z to redo".into() })
    }

    /// Split the section under time t in two at t.
    pub fn split_section_at(&mut self, t: f64) {
        if let Some(i) = self.project.sections().iter().position(|s| t > s.start + 0.2 && t < s.end - 0.2) {
            self.checkpoint();
            let secs = self.project.sections_mut();
            let mut right = secs[i].clone();
            right.start = t;
            secs[i].end = t;
            secs.insert(i + 1, right);
            self.select(Selection::Section(i + 1));
            self.dirty = true;
        }
    }

    pub fn save(&mut self) -> Result<(), String> {
        // Regions are kept in time order; a selected region stays selected.
        let selected = match self.sel {
            Selection::Region(i) => self.project.regions().get(i).map(|r| r.id.clone()),
            _ => None,
        };
        self.project.regions_mut().sort_by(|a, b| a.start.total_cmp(&b.start));
        if let Some(id) = selected {
            if let Some(i) = self.project.regions().iter().position(|r| r.id == id) {
                self.sel = Selection::Region(i);
            }
        }
        self.project.save(&self.path)?;
        self.dirty = false;
        Ok(())
    }
}
