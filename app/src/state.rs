//! Editor state shared between the App (inspector, transport) and the Timeline
//! widget through Scope data.

use crate::audio::{self, MixParams, Shared, Stem, Stems, TakeKind};
use crate::fx::{self, FxControls, FxRack};
use crate::plugins::PluginParam;
use crate::model::{default_gain_for, Analysis, Fx, Project, Region, Take, Track};
use makepad_script_audio_aot::AudioShaderRef;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub mod arrange {
    include!("arrange.rs");
}

pub mod record {
    include!("record.rs");
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Selection {
    None,
    Track(usize),
    Region(usize),
    Section(usize),
    /// A chord of the analysis (its name can be corrected).
    Chord(usize),
    /// A lyric line of the analysis (its words can be corrected).
    Lyric(usize),
    /// An effect: its lane (None: the master) and place in the chain.
    Fx(Option<usize>, usize),
}

/// A loaded plugin as it plays (see `EditorState::plugin_info`).
pub struct PluginInfo {
    /// Frames late (compensated), with the bridge's for one in its own process.
    pub latency: u32,
    pub isolated: bool,
    /// poing-plughost's process id.
    pub pid: Option<u32>,
    pub recoveries: u32,
    /// Its bridge's (blocks sent, slowest round trip in µs).
    pub bridge: Option<(u64, u64)>,
}

/// A plugin's second instance for measuring the song.
#[cfg(windows)]
struct MeasurePlugin {
    path: String,
    instrument: bool,
    state: Option<Vec<u8>>,
    handle: crate::plugins::Handle,
    shared: crate::plugins::Shared,
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
    /// Song states before each edit (Ctrl+Z) and after each undo (Ctrl+Shift+Z);
    /// audio long replaced by arrangement edits is read back from its files.
    undo: Vec<(Project, Option<Stems>)>,
    redo: Vec<(Project, Option<Stems>)>,
    /// The chord or lyric line whose words are being corrected (one undo step).
    pub editing_words: Option<Selection>,
    /// A message for the status line from an edit made in the timeline.
    pub notice: Option<String>,
    /// The timeline's area: key focus there (or nowhere) means shortcuts apply.
    pub timeline_area: makepad_widgets::Area,
    /// Meter levels as shown (they fall back slowly): lanes, then master.
    pub meter_show: Vec<f32>,
    /// The range playback loops over (seconds), if any.
    pub loop_range: Option<(f64, f64)>,
    /// The selected note: (MIDI lane, index in its notes).
    pub note_sel: Option<(usize, usize)>,
    /// The length new notes get (the last one drawn), in beats.
    pub note_len: f64,
    /// MIDI lanes' instruments by track id: compiled shader and knobs.
    inst_shaders: HashMap<String, AudioShaderRef>,
    pub inst_controls: HashMap<String, Arc<FxControls>>,
    /// Installed VST3 plugins (name, bundle path), found on disk.
    pub plugin_library: Vec<(String, PathBuf)>,
    /// Each plugin effect's parameters (by effect id).
    pub plugin_params: HashMap<String, Vec<PluginParam>>,
    /// The UI's handle to each loaded plugin, effects by effect id and MIDI
    /// lane instruments by track id: knobs, its editor window, its state.
    #[cfg(windows)]
    plugins: HashMap<String, crate::plugins::Handle>,
    /// Plugin effects loaded but not yet in a rack.
    #[cfg(windows)]
    pending_runners: Vec<(String, crate::plugins::Shared)>,
    /// Plugins on their way out: destroyed here, on the UI thread, once no
    /// rack holds them any more.
    #[cfg(windows)]
    retiring: Vec<crate::plugins::Handle>,
    /// Second instances of the song's plugins for `listen` and `compare`, by
    /// effect or track id, kept between measurements (a sample player loads once).
    #[cfg(windows)]
    measure_plugins: HashMap<String, MeasurePlugin>,
    /// MIDI lanes played by a plugin: track id -> the plugin path loaded for it.
    pub inst_plugins: HashMap<String, String>,
    /// Plugin instruments loaded but not yet in a rack.
    #[cfg(windows)]
    pending_insts: Vec<(String, crate::plugins::Shared)>,
    /// A plugin to play the MIDI part Claude is writing (named in the Describe).
    pub next_instrument: Option<(String, PathBuf)>,
    /// The named sound the next written MIDI part is played by (wins over `next_instrument`).
    pub next_sound: Option<crate::sounds::SoundInfo>,
    /// A state a lane's plugin instrument loads with next (a chosen sound), by track id.
    next_state: HashMap<String, Vec<u8>>,
    /// A remix asked for when the song was dropped: made once it is understood.
    pub pending_remix: Option<String>,
    /// Describe from the bar (not from an agent's tool): a request with nothing
    /// selected may go to Claude, which then sets `ask_agent`.
    pub agent_ok: bool,
    pub ask_agent: Option<String>,
    /// An AI feature was asked for that needs a download first: the app offers it.
    pub needs_download: Option<(&'static str, crate::model_view::Resume)>,
    /// The song in front (its tab is selected): only it talks to the audio thread.
    is_current: bool,
    /// Where a song in a background tab was, to come back to.
    saved_pos: f64,
    /// Tags this song's effect racks: a rack only carries effect state and
    /// plugin instances over from a rack of the same song.
    pub song_tag: u64,
    /// A MIDI keyboard being recorded into a lane, how its notes land on the
    /// grid, and whether they go on top of the lane's notes or replace them.
    pub rec: Option<record::Recording>,
    pub quantise: poing_model::record::Quantise,
    pub overdub: bool,
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
                s.loop_on.store(false, Ordering::Relaxed);
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
        let song_tag = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            path.hash(&mut h);
            h.finish() | 1
        };
        // past the ids takes still name too, so a new mark never inherits old versions
        let next_region = project
            .regions()
            .iter()
            .map(|r| r.id.as_str())
            .chain(project.takes().iter().filter_map(|t| t.region.as_deref()))
            .filter_map(|id| id.trim_start_matches('r').parse::<u64>().ok())
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
            editing_words: None,
            notice: None,
            timeline_area: makepad_widgets::Area::Empty,
            meter_show: vec![0.0; audio::METER_LANES + 1],
            loop_range: None,
            note_sel: None,
            note_len: 1.0,
            inst_shaders: HashMap::new(),
            inst_controls: HashMap::new(),
            plugin_library: {
                let lib = crate::plugins::library();
                crate::sounds::warm(lib.clone());
                lib
            },
            plugin_params: HashMap::new(),
            #[cfg(windows)]
            plugins: HashMap::new(),
            #[cfg(windows)]
            pending_runners: Vec::new(),
            #[cfg(windows)]
            retiring: Vec::new(),
            #[cfg(windows)]
            measure_plugins: HashMap::new(),
            inst_plugins: HashMap::new(),
            #[cfg(windows)]
            pending_insts: Vec::new(),
            next_instrument: None,
            next_sound: None,
            next_state: HashMap::new(),
            pending_remix: None,
            agent_ok: false,
            ask_agent: None,
            needs_download: None,
            is_current: true,
            saved_pos: 0.0,
            song_tag,
            rec: None,
            quantise: Default::default(),
            overdub: true,
        }
        .with_fx()
    }

    fn with_fx(mut self) -> Self {
        let ids: Vec<String> = self.all_fx().iter().map(|f| f.id.clone()).collect();
        for id in ids {
            self.compile_fx(&id);
        }
        self.compile_instruments();
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
        if fx.plugin.is_some() {
            return self.load_plugin_fx(&fx);
        }
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
        let mut rack = fx::build_rack(&self.project, &|id| self.fx_shaders.get(id).cloned(), &|id| self.fx_controls.get(id).cloned());
        rack.song = self.song_tag;
        // MIDI lanes: their notes on the song's beat grid, played by their instruments
        let grid = poing_model::BeatGrid::of(&self.project);
        let sr = self.sr();
        // the metronome's beats, over the song
        let song_beats = grid.beat_at(self.project.duration).ceil().max(0.0) as u64;
        rack.clicks = (0..=song_beats)
            .map(|b| ((grid.time_of(b as f64).max(0.0) * sr as f64) as u64, (b as f64 - grid.bar_offset).rem_euclid(grid.per_bar as f64) == 0.0))
            .collect();
        for (i, t) in self.project.tracks.iter().enumerate() {
            let Some(notes) = &t.notes else { continue };
            let events = crate::synth::events(notes, t.cc.as_deref().unwrap_or_default(), &grid, sr);
            if t.instrument.as_ref().is_some_and(|f| f.plugin.is_some()) {
                // its instance arrives by install or adopt
                #[cfg(windows)]
                rack.midi.push(crate::synth::MidiLane::new(i, events, crate::synth::Sound::Plugin(crate::synth::PluginSound::new(&t.id, sr))));
                continue;
            }
            let (Some(shader), Some(c)) = (self.inst_shaders.get(&t.id), self.inst_controls.get(&t.id)) else { continue };
            let synth = crate::synth::Synth::new(shader.clone(), c.clone(), sr);
            rack.midi.push(crate::synth::MidiLane::new(i, events, crate::synth::Sound::Splash(synth)));
        }
        rack
    }

    /// Compile every MIDI lane's instrument from the bundle.
    fn compile_instruments(&mut self) {
        let dir = self.dir();
        let tracks: Vec<Track> = self.project.tracks.iter().filter(|t| t.is_midi()).cloned().collect();
        // plugin instruments no longer wanted (an undo) go with the next rack swap
        let wanted: Vec<(String, String)> = tracks.iter().filter_map(|t| Some((t.id.clone(), t.instrument.as_ref()?.plugin.clone()?))).collect();
        let gone: Vec<String> = self.inst_plugins.iter().filter(|(id, path)| !wanted.iter().any(|(i, p)| i == *id && p == *path)).map(|(id, _)| id.clone()).collect();
        for id in gone {
            self.inst_plugins.remove(&id);
            self.retire_plugin(&id);
        }
        for t in tracks {
            let Some(inst) = &t.instrument else { continue };
            if let Some(path) = &inst.plugin {
                self.load_plugin_instrument(&t.id, path, inst);
                continue;
            }
            let code = std::fs::read_to_string(dir.join(&inst.file)).map_err(|e| e.to_string());
            match code.and_then(|c| fx::compile_instrument(&c)) {
                Ok(shader) => {
                    let controls = self.inst_controls.get(&t.id).cloned().filter(|c| c.params.len() == shader.params().len());
                    let controls = controls.unwrap_or_else(|| FxControls::new(&shader, inst, None));
                    self.inst_controls.insert(t.id.clone(), controls);
                    self.inst_shaders.insert(t.id.clone(), shader);
                    self.fx_errors.remove(&t.id);
                }
                Err(e) => {
                    self.fx_errors.insert(t.id.clone(), e);
                }
            }
        }
    }

    /// A new MIDI lane: notes played by an instrument written as code.
    pub fn add_midi_track(&mut self, name: &str, code: &str, notes: Vec<poing_model::Note>, cc: Vec<poing_model::Cc>, prompt: Option<String>) -> Result<usize, String> {
        fx::compile_instrument(code)?;
        self.checkpoint();
        let n = (1..).find(|n| self.project.track_index(&format!("midi{n}")).is_none()).unwrap_or(1);
        let id = format!("midi{n}");
        let file = format!("instruments/{id}.splash");
        let dir = self.dir();
        std::fs::create_dir_all(dir.join("instruments")).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(&file), code).map_err(|e| e.to_string())?;
        let instrument = Fx { id: id.clone(), name: name.to_string(), file, params: None, bypass: None, prompt, plugin: None, isolate: None };
        let tracks = self.project.tracks.len();
        // heard at once: with other lanes soloed, the new one joins them
        let solo = self.project.tracks.iter().any(|t| t.solo == Some(true)).then_some(true);
        self.project.tracks.push(Track {
            id,
            name: name.to_string(),
            file: String::new(),
            gain_db: None,
            mute: None,
            solo,
            fx: None,
            notes: Some(notes),
            instrument: Some(instrument),
            cc: (!cc.is_empty()).then_some(cc),
            automation: None,
        });
        // the stem set is tracks then takes: a silent placeholder goes in between
        let mut lanes: Vec<Arc<Stem>> = self.stems[..tracks].to_vec();
        lanes.push(Arc::new(Stem::new(Vec::new(), Vec::new())));
        lanes.extend(self.stems[tracks..].iter().cloned());
        self.publish_stems(Arc::new(lanes));
        self.compile_instruments();
        self.push_mix();
        self.publish_fx();
        self.sel_changed = true;
        self.save()?;
        Ok(tracks)
    }

    /// Replace a MIDI lane's notes (an edit): heard from the next rack.
    pub fn set_notes(&mut self, lane: usize, notes: Vec<poing_model::Note>) {
        if let Some(t) = self.project.tracks.get_mut(lane).filter(|t| t.is_midi()) {
            t.notes = Some(notes);
            self.dirty = true;
            self.publish_fx();
        }
    }

    /// Hand the audio thread a rack for the current effects (it carries
    /// over the state of every effect it keeps).
    pub fn publish_fx(&mut self) {
        if !self.is_current {
            return; // built when its tab comes to the front
        }
        #[cfg(windows)]
        {
            // A rack the audio thread never picked up gives its plugins back
            // (only to its own song: another song's ids mean other plugins).
            if let Some(mut stale) = self.shared.fx_in.lock().ok().and_then(|mut s| s.take()).filter(|r| r.song == self.song_tag) {
                self.pending_runners.extend(stale.take_runners());
                self.pending_insts.extend(stale.take_instruments());
            }
        }
        #[allow(unused_mut)]
        let mut rack = self.make_rack();
        #[cfg(windows)]
        for (id, runner) in std::mem::take(&mut self.pending_runners) {
            // a plugin no longer in the song is dropped here (its control destroys it)
            let _ = rack.install(&id, runner);
        }
        #[cfg(windows)]
        for (id, hosted) in std::mem::take(&mut self.pending_insts) {
            let _ = rack.install_instrument(&id, hosted);
        }
        // lanes with latent plugins and the rest lined up
        #[cfg(windows)]
        rack.compensate(&|id| self.plugins.get(id).map_or(0, |h| h.latency()));
        if let Ok(mut slot) = self.shared.fx_in.lock() {
            *slot = Some(Box::new(rack));
        }
    }

    /// Load plugin effect x (UI thread) and keep it ready for the next rack.
    fn load_plugin_fx(&mut self, fx: &Fx) -> bool {
        #[cfg(windows)]
        {
            match self.plugins.get(&fx.id) {
                // already playing: the rack keeps its instance
                Some(h) if h.isolated == fx.isolated() => return true,
                // in the other place (an undo past "Run in its own process"): moved, with its state
                Some(_) => {
                    self.save_plugin_states();
                    self.retire_plugin(&fx.id);
                }
                None => {}
            }
            let path = PathBuf::from(fx.plugin.clone().unwrap_or_default());
            let state = self.plugin_state(&fx.id);
            let how = crate::plugins::LoadAs { instrument: false, isolated: fx.isolated(), realtime: true };
            match crate::plugins::load(&path, self.sr(), Some(fx), state.as_deref(), how) {
                Ok((handle, shared)) => {
                    self.fx_controls.insert(fx.id.clone(), FxControls::for_plugin(fx));
                    self.plugin_params.insert(fx.id.clone(), handle.params.clone());
                    self.plugins.insert(fx.id.clone(), handle);
                    self.pending_runners.push((fx.id.clone(), shared));
                    self.fx_errors.remove(&fx.id);
                    true
                }
                Err(e) => {
                    self.fx_errors.insert(fx.id.clone(), e);
                    false
                }
            }
        }
        #[cfg(not(windows))]
        {
            self.fx_errors.insert(fx.id.clone(), "VST3 plugins play on Windows for now".into());
            false
        }
    }

    /// Let a plugin effect go: its instance leaves with the next rack swap.
    fn retire_plugin(&mut self, id: &str) {
        self.plugin_params.remove(id);
        #[cfg(windows)]
        if let Some(mut handle) = self.plugins.remove(id) {
            handle.close_editor();
            self.retiring.push(handle);
        }
    }

    /// Where a plugin's saved state lives in the bundle.
    fn plugin_state_file(&self, id: &str) -> PathBuf {
        self.dir().join("plugins").join(format!("{id}.state"))
    }

    /// A plugin's saved state (presets, wavetables, all its parameters), if any.
    fn plugin_state(&self, id: &str) -> Option<Vec<u8>> {
        std::fs::read(self.plugin_state_file(id)).ok()
    }

    /// Write every loaded plugin's state next to the project (on each save):
    /// what is done in a plugin's own window survives reloads, undo, tabs and export.
    fn save_plugin_states(&mut self) {
        #[cfg(windows)]
        let dir = self.dir().join("plugins");
        #[cfg(windows)]
        for (id, h) in self.plugins.iter_mut() {
            // (kept on the handle too: a crashed plugin comes back with it)
            if let Some(bytes) = h.snapshot() {
                let file = dir.join(format!("{id}.state"));
                if let Some(dir) = file.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(file, bytes);
            }
        }
    }

    /// Close plugin `id`'s own window, if open (what was changed there is kept).
    pub fn close_editor(&mut self, id: &str) {
        #[cfg(windows)]
        if let Some(h) = self.plugins.get_mut(id) {
            h.close_editor();
            self.save_plugin_states();
        }
        let _ = id;
    }

    /// Open the window of plugin `id` (an effect id, or a MIDI lane's track id).
    pub fn open_editor(&mut self, id: &str) -> Result<String, String> {
        #[cfg(windows)]
        {
            let h = self.plugins.get_mut(id).ok_or("that plugin is not loaded")?;
            h.open_editor()?;
            return Ok(format!("{}'s own window is open; what you change there is saved with the song.", h.name));
        }
        #[cfg(not(windows))]
        {
            let _ = id;
            Err("plugin windows are Windows-only for now".into())
        }
    }

    pub fn editors_open(&self) -> bool {
        #[cfg(windows)]
        return self.plugins.values().any(|h| h.editor_open());
        #[cfg(not(windows))]
        false
    }

    /// Keep plugin windows and queued knob moves going (UI thread, often).
    /// A window the user closed saves the plugin's state; a plugin whose
    /// latency changed gets the lanes lined up again; one whose process died
    /// comes back with its last state. Returns what is worth saying.
    pub fn pump_plugins(&mut self) -> Option<String> {
        #[cfg(windows)]
        {
            crate::plugins::pump_window_messages();
            let (mut closed, mut relatch) = (false, false);
            let mut crashed = Vec::new();
            for (id, h) in self.plugins.iter_mut() {
                let p = h.pump();
                closed |= p.closed;
                relatch |= p.latency;
                if p.crashed {
                    crashed.push(id.clone());
                }
            }
            let mut said = None;
            for id in crashed {
                let saved = self.plugin_state(&id);
                let Some(h) = self.plugins.get_mut(&id) else { continue };
                let state = h.last_state.clone().or(saved);
                said = Some(match h.recover(state.as_deref()) {
                    Ok(()) => {
                        relatch = true;
                        format!("{} crashed in its own process; Poing reloaded it as it was.", h.name)
                    }
                    Err(e) => e,
                });
            }
            if closed {
                self.save_plugin_states();
            }
            if relatch {
                self.publish_fx();
            }
            said
        }
        #[cfg(not(windows))]
        None
    }

    /// A loaded plugin as it plays: its latency (compensated), whether it runs
    /// in its own process (and that process's id), how often it came back.
    pub fn plugin_info(&self, id: &str) -> Option<PluginInfo> {
        #[cfg(windows)]
        return self.plugins.get(id).map(|h| PluginInfo { latency: h.latency(), isolated: h.isolated, pid: h.helper_pid(), recoveries: h.recoveries, bridge: h.bridge_stats() });
        #[cfg(not(windows))]
        {
            let _ = id;
            None
        }
    }

    /// The inspector's note on a plugin: how late it is, where it runs.
    pub fn plugin_note(&self, id: &str) -> String {
        let Some(i) = self.plugin_info(id) else { return String::new() };
        let mut s = String::new();
        if i.latency > 0 {
            s += &format!("   ·   {:.0} ms latency, the other lanes wait for it", i.latency as f64 * 1000.0 / self.sr() as f64);
        }
        if i.isolated {
            s += "   ·   in its own process";
            if i.recoveries > 0 {
                s += &format!(", reloaded after {} crash{}", i.recoveries, if i.recoveries == 1 { "" } else { "es" });
            }
        }
        s
    }

    /// Any plugins loaded (they are watched for crashes and latency changes).
    pub fn has_plugins(&self) -> bool {
        #[cfg(windows)]
        return !self.plugins.is_empty();
        #[cfg(not(windows))]
        false
    }

    /// Run plugin effect or instrument `id` in a process of its own, or back
    /// in Poing: it is loaded again there with its current state.
    pub fn set_isolated(&mut self, id: &str, on: bool) -> Result<String, String> {
        let lane_inst = self.project.track_index(id).filter(|&l| self.project.tracks[l].instrument.as_ref().is_some_and(|i| i.plugin.is_some()));
        let fx = match lane_inst {
            Some(l) => self.project.tracks[l].instrument.clone(),
            None => self.fx(id).cloned(),
        };
        let Some(mut fx) = fx.filter(|f| f.plugin.is_some()) else { return Err("that is not a plugin".into()) };
        if fx.isolated() == on {
            return Ok(format!("{} already runs {}.", fx.name, if on { "in its own process" } else { "in Poing" }));
        }
        // its state as it sounds now goes with it (a setting: not an undo step)
        self.save_plugin_states();
        fx.isolate = on.then_some(true);
        match lane_inst {
            Some(l) => self.project.tracks[l].instrument = Some(fx.clone()),
            None => {
                if let Some((lane, i)) = self.project.find_fx(id) {
                    self.project.chain_mut(lane)[i] = fx.clone();
                }
            }
        }
        // the loaders see it in the other place and move it
        let loaded = match lane_inst {
            Some(_) => self.load_plugin_instrument(id, fx.plugin.as_deref().unwrap_or_default(), &fx),
            None => self.load_plugin_fx(&fx),
        };
        if !loaded {
            return Err(self.fx_errors.get(id).cloned().unwrap_or_default());
        }
        // the new instance wins over the old one in the playing rack (adopt keeps an installed one)
        self.publish_fx();
        self.save()?;
        Ok(format!("{} runs {}.", fx.name, if on { "in its own process: a crash there leaves Poing playing" } else { "in Poing again" }))
    }

    /// Plugins in the song are loaded, plugins no longer in it let go (after undo).
    fn sync_plugins(&mut self) {
        let wanted: Vec<Fx> = self.all_fx().into_iter().filter(|f| f.plugin.is_some()).collect();
        let gone: Vec<String> = self.plugin_params.keys().filter(|id| !wanted.iter().any(|f| &&f.id == id)).cloned().collect();
        for id in gone {
            self.retire_plugin(&id);
        }
        for fx in wanted {
            self.load_plugin_fx(&fx);
        }
    }

    /// Put an installed plugin on a lane (or the master).
    pub fn add_plugin(&mut self, lane: Option<usize>, name: &str, path: &std::path::Path, prompt: Option<String>) -> Result<String, String> {
        self.checkpoint();
        let id = self.project.next_fx();
        let fx = Fx {
            id: id.clone(),
            name: name.to_string(),
            file: String::new(),
            params: None,
            bypass: None,
            prompt,
            plugin: Some(path.to_string_lossy().into_owned()),
            isolate: None,
        };
        self.project.chain_mut(lane).push(fx.clone());
        if !self.load_plugin_fx(&fx) {
            let why = self.fx_errors.remove(&id).unwrap_or_default();
            self.project.chain_mut(lane).pop();
            return Err(why);
        }
        self.publish_fx();
        self.sel_changed = true;
        self.save()?;
        Ok(id)
    }

    /// A rack for an offline render: plugins get a second instance each
    /// (one that will not load is left out, and named).
    pub fn make_render_rack(&mut self) -> (FxRack, Vec<String>) {
        #[allow(unused_mut)]
        let mut rack = self.make_rack();
        #[allow(unused_mut)]
        let mut missing = Vec::new();
        // each render instance's latency, to line the lanes up with
        #[cfg(windows)]
        let mut late: HashMap<String, u32> = HashMap::new();
        #[cfg(windows)]
        for fx in self.all_fx().into_iter().filter(|f| f.plugin.is_some()) {
            let path = PathBuf::from(fx.plugin.clone().unwrap_or_default());
            // the state as it sounds now (unsaved edits too), else as saved
            let state = self.plugins.get(&fx.id).and_then(|h| h.save_state()).or_else(|| self.plugin_state(&fx.id));
            let how = crate::plugins::LoadAs { instrument: false, isolated: fx.isolated(), realtime: false };
            match crate::plugins::load(&path, self.sr(), Some(&fx), state.as_deref(), how) {
                Ok((handle, shared)) => {
                    late.insert(fx.id.clone(), handle.latency());
                    let _ = rack.install(&fx.id, shared);
                    // the render lets go of it on the worker; it is destroyed here after
                    self.retiring.push(handle);
                }
                Err(_) => missing.push(fx.name.clone()),
            }
        }
        #[cfg(windows)]
        for t in self.project.tracks.iter().filter(|t| t.is_midi()) {
            let Some(inst) = t.instrument.as_ref().filter(|f| f.plugin.is_some()) else { continue };
            let state = self.plugins.get(&t.id).and_then(|h| h.save_state()).or_else(|| self.plugin_state(&t.id));
            let how = crate::plugins::LoadAs { instrument: true, isolated: inst.isolated(), realtime: false };
            match crate::plugins::load(std::path::Path::new(inst.plugin.as_deref().unwrap_or_default()), self.sr(), Some(inst), state.as_deref(), how) {
                Ok((handle, shared)) => {
                    late.insert(t.id.clone(), handle.latency());
                    let _ = rack.install_instrument(&t.id, shared);
                    self.retiring.push(handle);
                }
                Err(_) => missing.push(inst.name.clone()),
            }
        }
        #[cfg(windows)]
        rack.compensate(&|id| late.get(id).copied().unwrap_or(0));
        (rack, missing)
    }

    /// A rack to measure the song with (`listen`, `compare`): the song's plugins
    /// played by second instances with the state they have now, kept for the
    /// next measurement. Names the plugins that would not load.
    pub fn measure_rack(&mut self) -> (FxRack, Vec<String>) {
        #[allow(unused_mut)]
        let mut rack = self.make_rack();
        #[allow(unused_mut)]
        let mut missing = Vec::new();
        #[cfg(windows)]
        {
            let mut wanted: Vec<(String, Fx, bool)> = self.all_fx().into_iter().filter(|f| f.plugin.is_some()).map(|f| (f.id.clone(), f, false)).collect();
            for t in self.project.tracks.iter().filter(|t| t.is_midi()) {
                if let Some(inst) = t.instrument.clone().filter(|f| f.plugin.is_some()) {
                    wanted.push((t.id.clone(), inst, true));
                }
            }
            let keep: Vec<&String> = wanted.iter().map(|(id, _, _)| id).collect();
            let gone: Vec<String> = self.measure_plugins.keys().filter(|id| !keep.contains(id)).cloned().collect();
            for id in gone {
                if let Some(m) = self.measure_plugins.remove(&id) {
                    self.retiring.push(m.handle);
                }
            }
            let mut late: HashMap<String, u32> = HashMap::new();
            for (id, fx, instrument) in wanted {
                let path = fx.plugin.clone().unwrap_or_default();
                let state = self.plugins.get(&id).and_then(|h| h.save_state()).or_else(|| self.plugin_state(&id));
                let reuse = self.measure_plugins.get_mut(&id).filter(|m| m.path == path && m.instrument == instrument);
                let shared = match reuse {
                    Some(m) => {
                        // the last measurement may have ended inside a note: a new
                        // lane does not know what this instance still holds
                        if instrument {
                            crate::plugins::all_notes_off(&m.shared);
                        }
                        if state.is_some() && state != m.state {
                            if let Ok(mut p) = m.shared.plugin.lock() {
                                let _ = p.load_state(state.as_deref().unwrap_or_default());
                            }
                            m.state = state;
                        }
                        late.insert(id.clone(), m.handle.latency());
                        m.shared.clone()
                    }
                    None => {
                        if let Some(old) = self.measure_plugins.remove(&id) {
                            self.retiring.push(old.handle);
                        }
                        let how = crate::plugins::LoadAs { instrument, isolated: fx.isolated(), realtime: false };
                        match crate::plugins::load(std::path::Path::new(&path), self.sr(), Some(&fx), state.as_deref(), how) {
                            Ok((handle, shared)) => {
                                late.insert(id.clone(), handle.latency());
                                self.measure_plugins.insert(id.clone(), MeasurePlugin { path, instrument, state, handle, shared: shared.clone() });
                                shared
                            }
                            Err(_) => {
                                missing.push(fx.name.clone());
                                continue;
                            }
                        }
                    }
                };
                let _ = if instrument { rack.install_instrument(&id, shared) } else { rack.install(&id, shared) };
            }
            rack.compensate(&|id| late.get(id).copied().unwrap_or(0));
        }
        (rack, missing)
    }

    /// Any of the song's plugins (measurements then render them one at a time).
    pub fn has_plugin_sound(&self) -> bool {
        self.all_fx().iter().any(|f| f.plugin.is_some()) || self.project.tracks.iter().any(|t| t.instrument.as_ref().is_some_and(|i| i.plugin.is_some()))
    }

    /// Load the plugin at `path` to play MIDI lane `id` (UI thread), ready for the next rack.
    fn load_plugin_instrument(&mut self, id: &str, path: &str, inst: &Fx) -> bool {
        #[cfg(windows)]
        let same_place = self.plugins.get(id).map_or(true, |h| h.isolated == inst.isolated());
        #[cfg(not(windows))]
        let same_place = true;
        if same_place && self.inst_plugins.get(id).is_some_and(|p| p == path) && !self.next_state.contains_key(id) {
            return true; // already playing: the rack keeps its instance
        }
        #[cfg(windows)]
        {
            // its state goes with it when it moves (an undo past "Run in its own process")
            if !same_place {
                self.save_plugin_states();
            }
            // another plugin was playing this lane: it goes
            self.retire_plugin(id);
            let chosen = self.next_state.remove(id);
            let state = chosen.or_else(|| self.plugin_state(id).filter(|_| self.project.track_index(id).and_then(|l| self.project.tracks[l].instrument.as_ref()).and_then(|i| i.plugin.as_deref()) == Some(path)));
            let how = crate::plugins::LoadAs { instrument: true, isolated: inst.isolated(), realtime: true };
            match crate::plugins::load(std::path::Path::new(path), self.sr(), Some(inst), state.as_deref(), how) {
                Ok((handle, shared)) => {
                    self.plugin_params.insert(id.to_string(), handle.params.clone());
                    self.inst_plugins.insert(id.to_string(), path.to_string());
                    self.plugins.insert(id.to_string(), handle);
                    self.pending_insts.retain(|(i, _)| i != id);
                    self.pending_insts.push((id.to_string(), shared));
                    self.fx_errors.remove(id);
                    true
                }
                Err(e) => {
                    self.fx_errors.insert(id.to_string(), e);
                    false
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = inst;
            self.fx_errors.insert(id.to_string(), "VST3 plugins play on Windows for now".into());
            false
        }
    }

    /// Play MIDI lane `lane` with a named sound: its plugin, loaded with its state.
    pub fn set_instrument_sound(&mut self, lane: usize, sound: &crate::sounds::SoundInfo) -> Result<(), String> {
        let path = self.plugin_library.iter().find(|(n, _)| *n == sound.plugin).map(|(_, p)| p.clone()).ok_or_else(|| format!("{} is not installed", sound.plugin))?;
        let Some(id) = self.project.tracks.get(lane).filter(|t| t.is_midi()).map(|t| t.id.clone()) else { return Err("that lane has no notes".into()) };
        // a sound built on the plugin's own state (a Serum 2 preset): load it plain first
        let template = if crate::sounds::needs_template(sound) {
            self.set_instrument_plugin(lane, &sound.plugin, &path, None)?;
            #[cfg(windows)]
            let t = self.plugins.get(&id).and_then(|h| h.save_state());
            #[cfg(not(windows))]
            let t: Option<Vec<u8>> = None;
            Some(t.ok_or("the plugin gave no state to build the sound on")?)
        } else {
            None
        };
        let state = crate::sounds::state_of(sound, template.as_deref())?;
        self.next_state.insert(id.clone(), state);
        let done = self.set_instrument_plugin(lane, &sound.name, &path, Some(format!("sound: {}", sound.name)));
        self.next_state.remove(&id);
        done?;
        // the lane keeps the sound as it is now (its state file), not only the preset
        self.save_plugin_states();
        Ok(())
    }

    /// Keep lane `lane`'s plugin instrument, as it is now, as a named sound.
    pub fn save_sound(&mut self, lane: usize, name: &str) -> Result<crate::sounds::SoundInfo, String> {
        let t = self.project.tracks.get(lane).ok_or("no such lane")?;
        let inst = t.instrument.as_ref().filter(|i| i.plugin.is_some()).ok_or("that lane is not played by a plugin")?;
        let plugin = std::path::Path::new(inst.plugin.as_deref().unwrap_or_default()).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        #[cfg(windows)]
        let state = self.plugins.get(&t.id).and_then(|h| h.save_state()).or_else(|| self.plugin_state(&t.id));
        #[cfg(not(windows))]
        let state = self.plugin_state(&t.id);
        let state = state.ok_or("the plugin gave no state to keep")?;
        let saved = crate::sounds::save(name, &plugin, "", &state)?;
        crate::sounds::forget();
        Ok(saved)
    }

    /// Humanise MIDI lane `lane` (timing, legato, velocity, dynamics swells): one undo step.
    pub fn humanize_lane(&mut self, lane: usize, amount: f64, seed: u64) -> Result<usize, String> {
        let spb = poing_model::BeatGrid::of(&self.project).seconds_per_beat();
        let t = self.project.tracks.get(lane).filter(|t| t.is_midi()).ok_or("that lane has no notes")?;
        let (mut notes, mut cc) = (t.notes.clone().unwrap_or_default(), t.cc.clone().unwrap_or_default());
        poing_model::humanize::humanize(&mut notes, &mut cc, poing_model::humanize::Humanize { amount, seconds_per_beat: spb, seed });
        self.checkpoint();
        let n = notes.len();
        self.project.tracks[lane].cc = (!cc.is_empty()).then_some(cc);
        self.set_notes(lane, notes);
        self.sel_changed = true;
        Ok(n)
    }

    /// Replace MIDI lane `lane`'s curve for controller `num` (1 dynamics, 11
    /// expression) with `points` (grid beat, 0..1); empty clears it. One undo step.
    pub fn set_cc(&mut self, lane: usize, num: u8, mut points: Vec<poing_model::Cc>) -> Result<(), String> {
        let t = self.project.tracks.get(lane).filter(|t| t.is_midi()).ok_or("that lane has no notes")?;
        let mut cc: Vec<poing_model::Cc> = t.cc.clone().unwrap_or_default().into_iter().filter(|c| c.num != num).collect();
        for p in &mut points {
            p.num = num;
            p.value = p.value.clamp(0.0, 1.0);
        }
        cc.extend(points);
        cc.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        self.checkpoint();
        self.project.tracks[lane].cc = (!cc.is_empty()).then_some(cc);
        self.dirty = true;
        self.publish_fx();
        self.sel_changed = true;
        Ok(())
    }

    /// Set lane `lane`'s level automation (dB over seconds; empty clears it).
    pub fn set_automation(&mut self, lane: usize, mut points: Vec<poing_model::GainPoint>) -> Result<(), String> {
        let t = self.project.tracks.get_mut(lane).ok_or("no such lane")?;
        points.retain(|p| p.t.is_finite() && p.db.is_finite());
        points.sort_by(|a, b| a.t.total_cmp(&b.t));
        for p in &mut points {
            p.db = p.db.clamp(-60.0, 12.0);
        }
        t.automation = (!points.is_empty()).then_some(points);
        self.dirty = true;
        self.push_mix();
        self.sel_changed = true;
        Ok(())
    }

    /// The articulations lane `lane`'s plugin instrument switches by key (name, key), if it says.
    pub fn keyswitches(&self, lane: usize) -> Vec<(String, u8)> {
        #[cfg(windows)]
        if let Some(h) = self.project.tracks.get(lane).and_then(|t| self.plugins.get(&t.id)) {
            return h.keyswitches();
        }
        let _ = lane;
        Vec::new()
    }

    /// Switch lane `lane`'s articulation at grid beat `beat`: a short note on the
    /// library's keyswitch `key`, a sixteenth before the notes it is for.
    pub fn add_keyswitch(&mut self, lane: usize, beat: f64, key: u8) -> Result<(), String> {
        let mut notes = self.project.tracks.get(lane).and_then(|t| t.notes.clone()).ok_or("that lane has no notes")?;
        let at = (beat - 0.25).max(0.0);
        notes.retain(|n| !(n.pitch == key && (n.start - at).abs() < 1e-6));
        notes.push(poing_model::Note { start: at, len: 0.125, pitch: key.min(127), vel: 0.8 });
        notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
        self.checkpoint();
        self.set_notes(lane, notes);
        self.sel_changed = true;
        Ok(())
    }

    /// Play MIDI lane `lane` with the installed plugin `name` at `path`.
    pub fn set_instrument_plugin(&mut self, lane: usize, name: &str, path: &std::path::Path, prompt: Option<String>) -> Result<(), String> {
        let Some(t) = self.project.tracks.get(lane).filter(|t| t.is_midi()).cloned() else { return Err("that lane has no notes".into()) };
        let inst = Fx {
            id: t.id.clone(),
            name: name.to_string(),
            file: String::new(),
            params: None,
            bypass: None,
            prompt,
            plugin: Some(path.to_string_lossy().into_owned()),
            isolate: None,
        };
        if !self.load_plugin_instrument(&t.id, &inst.plugin.clone().unwrap_or_default(), &inst) {
            return Err(self.fx_errors.remove(&t.id).unwrap_or_default());
        }
        self.checkpoint();
        self.project.tracks[lane].instrument = Some(inst);
        self.publish_fx();
        self.sel_changed = true;
        self.save()
    }

    /// The plugins a song switch must keep alive until the audio thread lets
    /// go of this song's rack (their windows close, their state is saved).
    #[cfg(windows)]
    pub fn take_plugin_controls(&mut self) -> Vec<crate::plugins::Handle> {
        self.save_plugin_states();
        let mut out: Vec<_> = self.plugins.drain().map(|(_, h)| h).collect();
        for h in &mut out {
            h.close_editor();
        }
        out.extend(self.retiring.drain(..));
        out
    }

    #[cfg(windows)]
    pub fn adopt_plugin_controls(&mut self, handles: Vec<crate::plugins::Handle>) {
        self.retiring.extend(handles);
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
        let fx = Fx { id: id.clone(), name: name.to_string(), file, params: None, bypass: None, prompt, plugin: None, isolate: None };
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
        self.retire_plugin(id);
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
        if let Some(params) = self.plugin_params.get(id) {
            // a plugin knob: normalized 0..1, through the plugin's control queue
            let Some(p) = crate::plugins::knob_params(params).get(index).cloned() else { return };
            #[cfg(windows)]
            if let Some(h) = self.plugins.get_mut(id) {
                h.set_param(p.id, value);
            }
            if let Some((lane, i)) = self.project.find_fx(id) {
                self.project.chain_mut(lane)[i].set_param(&p.name, value);
                self.dirty = true;
            }
            return;
        }
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
        if self.plugin_params.contains_key(id) {
            return Some(knob);
        }
        let shader = self.fx_shaders.get(id)?;
        shader.params().iter().enumerate().filter(|(_, p)| p.name != "bpm").nth(knob).map(|(i, _)| i)
    }

    /// The knobs of effect `id`: (name, value, min, max).
    pub fn fx_knobs(&self, id: &str) -> Vec<(String, f64, f64, f64)> {
        if let (Some(params), Some(fx)) = (self.plugin_params.get(id), self.fx(id)) {
            return crate::plugins::knob_params(params).into_iter().map(|p| (p.name.clone(), fx.param(&p.name).unwrap_or(p.default), 0.0, 1.0)).collect();
        }
        let (Some(shader), Some(fx)) = (self.fx_shaders.get(id), self.fx(id)) else { return Vec::new() };
        shader
            .params()
            .iter()
            .filter(|p| p.name != "bpm")
            .map(|p| (p.name.clone(), fx.param(&p.name).unwrap_or(p.default as f64), p.min as f64, p.max as f64))
            .collect()
    }

    /// The knobs of MIDI lane `lane`'s built-in instrument: (name, value, min, max).
    pub fn inst_knobs(&self, lane: usize) -> Vec<(String, f64, f64, f64)> {
        let Some(t) = self.project.tracks.get(lane) else { return Vec::new() };
        let (Some(shader), Some(inst)) = (self.inst_shaders.get(&t.id), t.instrument.as_ref().filter(|i| i.plugin.is_none())) else { return Vec::new() };
        shader
            .params()
            .iter()
            .filter(|p| p.name != "bpm")
            .take(6)
            .map(|p| (p.name.clone(), inst.param(&p.name).unwrap_or(p.default as f64), p.min as f64, p.max as f64))
            .collect()
    }

    /// Turn knob `knob` of lane `lane`'s instrument: heard at once, saved with the song.
    pub fn set_inst_knob(&mut self, lane: usize, knob: usize, value: f64) {
        let Some(id) = self.project.tracks.get(lane).map(|t| t.id.clone()) else { return };
        let Some(shader) = self.inst_shaders.get(&id) else { return };
        let Some((index, name)) = shader.params().iter().enumerate().filter(|(_, p)| p.name != "bpm").nth(knob).map(|(i, p)| (i, p.name.clone())) else { return };
        if let Some(c) = self.inst_controls.get(&id) {
            c.set(index, value as f32);
        }
        if let Some(inst) = self.project.tracks[lane].instrument.as_mut() {
            inst.set_param(&name, value);
            self.dirty = true;
        }
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
            .filter(|f| f.plugin.is_none())
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


    pub fn is_current(&self) -> bool {
        self.is_current
    }

    /// Its tab goes to the back: remember where it was, stop talking to the
    /// audio thread, and hand over what the next song must keep alive (the
    /// plugin handles, the stem sets the audio thread may still hold).
    /// Its plugins are loaded again when it comes back.
    #[cfg(windows)]
    pub fn deactivate(&mut self) -> (Vec<crate::plugins::Handle>, Vec<Stems>) {
        let controls = self.take_plugin_controls();
        (controls, self.go_back())
    }
    #[cfg(not(windows))]
    pub fn deactivate(&mut self) -> ((), Vec<Stems>) {
        ((), self.go_back())
    }

    fn go_back(&mut self) -> Vec<Stems> {
        // a recording ends with its song going to the back (what was played lands)
        if self.recording() {
            self.notice = Some(self.finish_recording());
        }
        self.saved_pos = self.playhead();
        self.is_current = false;
        self.plugin_params.retain(|_, _| false);
        self.inst_plugins.clear();
        #[cfg(windows)]
        {
            self.pending_runners.clear();
            self.pending_insts.clear();
        }
        let mut sets = std::mem::take(&mut self.retired);
        sets.push(self.stems.clone());
        sets
    }

    /// Its tab comes to the front: its stems, mix, effects (plugins load
    /// again) and loop go to the audio thread. Playing on, it keeps the time
    /// (`at`, clamped to its length) so songs compare at the same moment;
    /// stopped, it goes back to where it was.
    pub fn activate(&mut self, at: Option<f64>) {
        self.is_current = true;
        // past the version the audio thread has, so this mix is taken
        let shared_version = self.shared.mix.lock().ok().map(|m| m.version).unwrap_or(0);
        self.mix_version = self.mix_version.max(shared_version) + 1;
        let stems = self.stems.clone();
        if let Ok(mut s) = self.shared.stems.lock() {
            *s = stems;
        }
        self.shared.stems_gen.fetch_add(1, Ordering::AcqRel);
        self.push_mix();
        self.sync_plugins();
        self.compile_instruments();
        self.publish_fx();
        self.set_loop(self.loop_range);
        let t = at.unwrap_or(self.saved_pos).min(self.duration());
        self.seek(t);
        self.sel_changed = true;
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
        if !self.is_current {
            // the audio thread never saw this song's stems: nothing to retire
            self.stems = stems;
            return;
        }
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
        // Plugins whose rack was just dropped are destroyed here; a retired
        // handle stays a while for the swap to come round.
        #[cfg(windows)]
        {
            // destroyed here once no rack (live or render) holds them
            let (gone, keep): (Vec<_>, Vec<_>) = self.retiring.drain(..).partition(|h| h.released());
            self.retiring = keep;
            for h in gone {
                h.shut_down();
            }
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
        self.grow_for(&take, &stem);
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
                    notes: None,
                    instrument: None,
                    cc: None,
                    automation: None,
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
    /// Pick the version that is playing (a song take, else version 1) as the
    /// song of a draft: its audio becomes the original (the one it replaces
    /// becomes that take) and the draft ends. The caller starts the analysis.
    pub fn use_version(&mut self) -> Result<String, String> {
        if !self.project.is_draft() {
            return Err("this song is already chosen".into());
        }
        let mut name = "Version 1".to_string();
        if let Some((stem_i, take)) = self.song_take().map(|(i, t)| (i, t.clone())) {
            let dir = self.dir();
            let orig_track = self.project.track_index("original").ok_or("the song has no original")?;
            let (a, b, tmp) = (dir.join(&self.project.tracks[orig_track].file), dir.join(&take.file), dir.join("swap.tmp"));
            std::fs::rename(&a, &tmp).and_then(|_| std::fs::rename(&b, &a)).and_then(|_| std::fs::rename(&tmp, &b)).map_err(|e| format!("swap the versions: {e}"))?;
            let mut lanes: Vec<Arc<Stem>> = self.stems.iter().cloned().collect();
            lanes.swap(orig_track, stem_i);
            let len = lanes[orig_track].l.len() as f64 / self.sr() as f64;
            let other = lanes[stem_i].l.len() as f64 / self.sr() as f64;
            self.publish_stems(Arc::new(lanes));
            if let Some(k) = self.project.take_index(&take.id) {
                let t = &mut self.project.takes_mut()[k];
                name = std::mem::replace(&mut t.prompt, "Version 1".into());
                t.end = other;
            }
            self.project.duration = len;
            self.active.clear();
            self.focus = None;
        }
        self.project.draft = None;
        // the files moved: an undo from before would point at the wrong audio
        self.undo.clear();
        self.redo.clear();
        self.push_mix();
        self.sel_changed = true;
        self.save()?;
        Ok(name)
    }

    /// Is any section combined from a remix playing?
    pub fn combined_on(&self) -> bool {
        self.project.takes().iter().any(|t| t.engine == "copy" && self.take_on(&t.id))
    }

    /// The Combined chip: every combined section off, or all back on.
    pub fn toggle_combined(&mut self) {
        if self.combined_on() {
            let on: Vec<String> = self.project.takes().iter().filter(|t| t.engine == "copy" && self.take_on(&t.id)).map(|t| t.id.clone()).collect();
            for id in on {
                self.toggle_take(&id);
            }
        } else {
            self.restore_combined();
        }
    }

    /// The versions made for region `id` (see `regenerate_region`), oldest first.
    pub fn region_versions(&self, id: &str) -> Vec<String> {
        self.project.takes().iter().filter(|t| t.region.as_deref() == Some(id)).map(|t| t.id.clone()).collect()
    }

    /// Keep take `id`, one version of a marked region, in one undo step: its
    /// other versions go, and so does the mark (it is done). A section version
    /// stays on and plays whenever the song opens; a lane version goes into
    /// its lane, like Keep in the lane.
    pub fn keep_version(&mut self, id: &str) -> Result<String, String> {
        let i = self.project.take_index(id).ok_or("no such take")?;
        let take = self.project.takes()[i].clone();
        let region = take.region.clone().ok_or("this take was not made for a marked region")?;
        let n = take.id.trim_start_matches('t').to_string();
        let message = if take.scope == "section" {
            self.checkpoint();
            self.project.takes_mut()[i].kept = Some(true);
            if !self.take_on(id) {
                self.toggle_take(id);
            }
            format!("Take {n} is kept: it plays there from now on (Ctrl+Z undoes it)")
        } else {
            self.keep_take(id)?
        };
        for other in self.region_versions(&region).into_iter().filter(|o| o != id) {
            self.delete_take(&other)?;
        }
        if let Some(r) = self.project.regions().iter().position(|r| r.id == region) {
            self.project.regions_mut().remove(r);
            self.push_mix();
        }
        if let Some(t) = self.project.take_index(id) {
            self.project.takes_mut()[t].region = None;
        }
        self.sel_changed = true;
        self.save()?;
        Ok(message)
    }

    /// Kept versions of marked regions play again when the song is reopened.
    pub fn restore_kept(&mut self) {
        let kept: Vec<String> = self.project.takes().iter().filter(|t| t.is_kept()).map(|t| t.id.clone()).collect();
        for id in kept {
            if !self.take_on(&id) {
                self.toggle_take(&id);
            }
        }
    }

    /// Sections combined from remixes play again when the song is reopened
    /// (the newest one per section wins, as when they were made).
    pub fn restore_combined(&mut self) {
        let picked: Vec<String> = self.project.takes().iter().filter(|t| t.engine == "copy").map(|t| t.id.clone()).collect();
        for id in picked {
            if !self.take_on(&id) {
                self.toggle_take(&id);
            }
        }
    }

    /// Copy the remix that is playing over section `i` into a section take:
    /// remixes line up with the song, so each section can come from another.
    pub fn use_remix_here(&mut self, i: usize) -> Result<String, String> {
        let s = self.project.sections().get(i).cloned().ok_or("no such section")?;
        let (stem_i, remix) = self.song_take().map(|(k, t)| (k, t.clone())).ok_or("play a remix first (click it above the lanes)")?;
        if remix.engine != "acestep-cover" {
            return Err(format!("{} does not line up with the song in time, so it cannot fill one section", remix.prompt));
        }
        // a little either side: the take fades in and out across the section's edges
        let m = audio::TAKE_RAMP_S * 0.5;
        let (a, b) = ((s.start - m).max(0.0), (s.end + m).min(self.project.duration));
        // (through how it lines up now, if the song was rearranged since)
        let (a, b, part) = self.remix_range(stem_i, &remix, a, b).filter(|(lo, hi, _)| *lo <= s.start + 0.1 && *hi >= s.end - 0.1).ok_or("the remix does not reach that section")?;
        let prompt = format!("{} in the {}", remix.prompt, s.label);
        // the same remix in the same section again: that take, not a copy of it
        let same = self.project.takes().iter().rev().find(|t| t.engine == "copy" && t.prompt == prompt && (t.start - a).abs() < 0.01 && (t.end - b).abs() < 0.01).map(|t| t.id.clone());
        if let Some(id) = same {
            self.restore_combined();
            if !self.take_on(&id) {
                self.toggle_take(&id);
            }
            self.sel = Selection::Section(i);
            return Ok(format!("{prompt}: on again (it was combined before)"));
        }
        let (id, seed) = self.project.next_take();
        let take = Take {
            file: format!("takes/{id}.wav"),
            id,
            scope: "section".into(),
            start: a,
            end: b,
            engine: "copy".into(),
            prompt,
            seed,
            track: None,
            blend: None,
            region: None,
            kept: None,
        };
        audio::write_wav(&self.dir().join(&take.file), &part.l, &part.r, self.sr(), 1.0).map_err(|e| e.to_string())?;
        let stem = part;
        let what = take.prompt.clone();
        self.add_take(take, stem)?;
        // every section picked so far plays together (a song take would hide them)
        self.restore_combined();

        self.sel = Selection::Section(i);
        Ok(format!("{what}: the rest of the song plays as before. Pick another remix and section to combine more."))
    }

    pub fn song_take(&self) -> Option<(usize, &Take)> {
        self.active.iter().filter_map(|id| self.project.take_index(id)).find_map(|i| {
            let t = &self.project.takes()[i];
            (t.scope == "song").then_some((self.project.tracks.len() + i, t))
        })
    }

    /// Loop playback over a range (None: play through).
    pub fn set_loop(&mut self, range: Option<(f64, f64)>) {
        let sr = self.sr() as f64;
        self.loop_range = range;
        if !self.is_current {
            return;
        }
        if let Some((a, b)) = range {
            self.shared.loop_a.store((a.max(0.0) * sr) as u64, Ordering::Relaxed);
            self.shared.loop_b.store((b * sr) as u64, Ordering::Relaxed);
            if !(a..b).contains(&self.playhead()) {
                self.seek(a);
            }
        }
        self.shared.loop_on.store(range.is_some(), Ordering::Relaxed);
    }

    /// L: loop the selected section or region, or stop looping.
    pub fn toggle_loop(&mut self) -> String {
        let range = match self.sel {
            Selection::Section(i) => self.project.sections().get(i).map(|s| (s.start, s.end, s.label.clone())),
            Selection::Region(i) => self.project.regions().get(i).map(|r| (r.start, r.end, "region".to_string())),
            _ => None,
        };
        match range {
            Some((a, b, what)) if self.loop_range != Some((a, b)) => {
                self.set_loop(Some((a, b)));
                format!("Looping the {what} ({a:.1}s - {b:.1}s)  ·  L again to stop")
            }
            _ if self.loop_range.is_some() => {
                self.set_loop(None);
                "Loop off".into()
            }
            _ => "Select a section or region, then press L to loop it".into(),
        }
    }

    pub fn duration(&self) -> f64 {
        match self.song_take() {
            Some((_, t)) => t.end,
            None => self.project.duration,
        }
    }
    pub fn playhead(&self) -> f64 {
        if !self.is_current {
            return self.saved_pos;
        }
        self.shared.pos() as f64 / self.sr() as f64
    }
    pub fn seek(&self, t: f64) {
        if !self.is_current {
            return;
        }
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
        if !self.is_current {
            self.dirty = true;
            return;
        }
        if let Ok(mut m) = self.shared.mix.lock() {
            *m = mix;
        }
        self.dirty = true;
    }

    pub fn mix_snapshot(&self) -> MixParams {
        MixParams::from_project(&self.project, self.mix_version, &self.active)
    }

    pub fn select(&mut self, sel: Selection) {
        if self.note_sel.is_some_and(|(l, _)| sel != Selection::Track(l)) {
            self.note_sel = None;
        }
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
        // a selected note goes first (its lane stays selected)
        if let Some((lane, idx)) = self.note_sel.take() {
            let mut notes = self.project.tracks.get(lane)?.notes.clone()?;
            if idx < notes.len() {
                self.checkpoint();
                notes.remove(idx);
                self.set_notes(lane, notes);
                return Some("Removed a note  ·  Ctrl+Z to undo".into());
            }
        }
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
            // a section is cut from the song, with everything in it
            Selection::Section(i) if i < self.project.sections().len() => {
                return Some(self.arrange(&poing_model::arrange::Op::Delete { section: i }).unwrap_or_else(|e| format!("Could not cut it: {e}")));
            }
            // a misheard line or a chord that is not there
            Selection::Lyric(i) if self.project.analysis.as_ref().and_then(|a| a.lyrics.as_ref()).is_some_and(|l| i < l.len()) => {
                self.checkpoint();
                let l = self.project.analysis.as_mut().and_then(|a| a.lyrics.as_mut()).map(|l| l.remove(i))?;
                self.dirty = true;
                self.write_lyrics();
                format!("Removed the line \"{}\"", l.text.trim())
            }
            Selection::Chord(i) if self.project.analysis.as_ref().and_then(|a| a.chords.as_ref()).is_some_and(|c| i < c.len()) => {
                self.checkpoint();
                let c = self.project.analysis.as_mut().and_then(|a| a.chords.as_mut()).map(|c| c.remove(i))?;
                self.dirty = true;
                format!("Removed the {} chord", c.label)
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
        self.undo.push((self.project.clone(), Some(self.stems.clone())));
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Step back one edit (or forward again with `redo`).
    pub fn undo(&mut self, redo: bool) -> Option<String> {
        if self.recording() {
            return Some("Stop recording first (R)".into());
        }
        let (project, stems) = if redo { self.redo.pop()? } else { self.undo.pop()? };
        let stems = match stems {
            Some(s) => s,
            None => match self.load_stems(&project) {
                Ok(s) => s,
                Err(e) => {
                    // put it back: nothing changed
                    if redo { self.redo.push((project, None)) } else { self.undo.push((project, None)) }
                    return Some(format!("Could not undo: {e}"));
                }
            },
        };
        // a selected lane stays selected (a note edit undone keeps its piano roll open)
        let lane = match self.sel {
            Selection::Track(i) => self.project.tracks.get(i).map(|t| t.id.clone()),
            _ => None,
        };
        let before = self.project.duration;
        let now = (std::mem::replace(&mut self.project, project), Some(self.stems.clone()));
        // the words or their sections changed back: so do the sung lyrics
        let sung = |p: &Project| (p.sections.as_ref().map(|s| s.iter().map(|s| (s.start, s.end, s.label.clone())).collect::<Vec<_>>()), p.analysis.as_ref().and_then(|a| a.lyrics.as_ref()).map(|l| l.iter().map(|l| (l.start, l.end, l.text.clone())).collect::<Vec<_>>()));
        let words_changed = sung(&self.project) != sung(&now.0);
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
        // effects come back from their files (kept on disk for this), plugins reload
        let ids: Vec<String> = self.all_fx().iter().filter(|f| f.plugin.is_none()).map(|f| f.id.clone()).collect();
        for id in ids {
            self.compile_fx(&id);
        }
        self.sync_plugins();
        self.compile_instruments();
        self.publish_fx();
        self.push_mix();
        let keep = lane.and_then(|id| self.project.track_index(&id));
        self.note_sel = None;
        self.select(keep.map_or(Selection::None, Selection::Track));
        if (self.project.duration - before).abs() > 1e-6 {
            // an arrangement edit undone: the song has its old length again
            self.pps = 0.0;
            self.set_loop(None);
            if self.playhead() > self.duration() {
                self.seek(0.0);
            }
        }
        if words_changed {
            self.write_lyrics();
        }
        self.forget_old_audio();
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
        self.save_plugin_states();
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

/// Takes shown by their own name (song versions, remixes, combined sections)
/// rather than their number.
pub fn named_take(t: &Take) -> bool {
    matches!(t.engine.as_str(), "acestep-song" | "acestep-cover" | "copy")
}
/// The take chip that stands for every section combined from remixes.
pub const COMBINED: &str = "@combined";
