//! Editor state shared between the App (inspector, transport) and the Timeline
//! widget through Scope data.

use crate::audio::{MixParams, Shared, Stem, Stems};
use crate::model::{default_gain_for, Analysis, Project, Region, Take};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Selection {
    None,
    Track(usize),
    Region(usize),
    Section(usize),
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
    /// The take being heard instead of the lanes (A/B); None plays the lanes.
    pub audition: Option<String>,
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
}

impl EditorState {
    /// `shared`: the audio output's state when one is already playing another
    /// song; this song's stems and mix are published into it.
    pub fn new(project: Project, path: PathBuf, stems: Vec<Stem>, shared: Option<Arc<Shared>>) -> Self {
        let stems: Stems = Arc::new(stems.into_iter().map(Arc::new).collect());
        // Mix versions keep counting up across songs, so the callback sees the change.
        let mix_version = shared.as_ref().and_then(|s| s.mix.lock().ok().map(|m| m.version + 1)).unwrap_or(0);
        let mix = MixParams::from_project(&project, mix_version, None);
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
            audition: None,
            sel: Selection::None,
            pps: 0.0,
            scroll_t: 0.0,
            sel_changed: true,
            dirty: false,
            mix_version,
            next_region,
        }
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
        if !self.retired.is_empty()
            && self.shared.stems_seen.load(Ordering::Acquire) == self.shared.stems_gen.load(Ordering::Acquire)
        {
            self.retired.clear();
        }
    }
    /// A new take joins the stem set and is auditioned straight away.
    pub fn add_take(&mut self, take: Take, stem: Stem) -> Result<(), String> {
        let id = take.id.clone();
        self.project.takes_mut().push(take);
        let mut lanes: Vec<Arc<Stem>> = self.stems.iter().cloned().collect();
        lanes.push(Arc::new(stem));
        self.publish_stems(Arc::new(lanes));
        self.analysing = None;
        self.set_audition(Some(id));
        self.save()
    }

    /// Delete a take: its audio file goes, the stem set shrinks.
    pub fn delete_take(&mut self, id: &str) -> Result<(), String> {
        let Some(i) = self.project.take_index(id) else { return Ok(()) };
        if self.audition.as_deref() == Some(id) {
            self.set_audition(None);
        }
        let take = self.project.takes_mut().remove(i);
        let mut lanes: Vec<Arc<Stem>> = self.stems.iter().cloned().collect();
        lanes.remove(self.project.tracks.len() + i);
        self.publish_stems(Arc::new(lanes));
        self.push_mix();
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_file(dir.join(&take.file));
        }
        self.save()
    }

    /// Hear a take instead of the lanes (None: back to the lanes).
    pub fn set_audition(&mut self, id: Option<String>) {
        let before = self.duration();
        self.audition = id.filter(|id| self.project.take_index(id).is_some());
        self.push_mix();
        self.sel_changed = true;
        if (self.duration() - before).abs() > 0.5 {
            self.pps = 0.0; // a song take has its own length: fit it
        }
        if self.playhead() > self.duration() {
            self.seek(0.0);
        }
    }

    /// The song take being heard, if any.
    pub fn song_take(&self) -> Option<(usize, &Take)> {
        let i = self.project.take_index(self.audition.as_deref()?)?;
        let t = &self.project.takes()[i];
        (t.scope == "song").then_some((self.project.tracks.len() + i, t))
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
        let mix = MixParams::from_project(&self.project, self.mix_version, self.audition.as_deref());
        if let Ok(mut m) = self.shared.mix.lock() {
            *m = mix;
        }
        self.dirty = true;
    }

    pub fn mix_snapshot(&self) -> MixParams {
        MixParams::from_project(&self.project, self.mix_version, self.audition.as_deref())
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

    pub fn delete_selected(&mut self) {
        match self.sel {
            Selection::Region(i) if i < self.project.regions().len() => {
                self.project.regions_mut().remove(i);
                self.select(Selection::None);
                self.push_mix();
            }
            Selection::Section(i) if self.project.sections().len() > 1 && i < self.project.sections().len() => {
                // merge into the previous section (or the next one if it is the first)
                let secs = self.project.sections_mut();
                let removed = secs.remove(i);
                if i > 0 {
                    secs[i - 1].end = removed.end;
                } else {
                    secs[0].start = removed.start;
                }
                self.select(Selection::None);
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// Split the section under time t in two at t.
    pub fn split_section_at(&mut self, t: f64) {
        let secs = self.project.sections_mut();
        if let Some(i) = secs.iter().position(|s| t > s.start + 0.2 && t < s.end - 0.2) {
            let mut right = secs[i].clone();
            right.start = t;
            secs[i].end = t;
            secs.insert(i + 1, right);
            self.select(Selection::Section(i + 1));
            self.dirty = true;
        }
    }

    pub fn save(&mut self) -> Result<(), String> {
        self.project.regions_mut().sort_by(|a, b| a.start.total_cmp(&b.start));
        if let Selection::Region(_) = self.sel {
            self.select(Selection::None);
        }
        self.project.save(&self.path)?;
        self.dirty = false;
        Ok(())
    }
}
