//! Editor state shared between the App (inspector, transport) and the Timeline
//! widget through Scope data.

use crate::audio::{MixParams, Shared, Stem};
use crate::model::{default_gain_for, Project, Region};
use std::path::PathBuf;
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
    pub stems: Arc<Vec<Stem>>,
    pub shared: Arc<Shared>,
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
    pub fn new(project: Project, path: PathBuf, stems: Vec<Stem>) -> Self {
        let mix = MixParams::from_project(&project, 0);
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
            stems: Arc::new(stems),
            shared: Arc::new(Shared::new(mix)),
            sel: Selection::None,
            pps: 0.0,
            scroll_t: 0.0,
            sel_changed: true,
            dirty: false,
            mix_version: 0,
            next_region,
        }
    }

    pub fn sr(&self) -> u32 {
        self.project.sr()
    }
    pub fn duration(&self) -> f64 {
        self.project.duration
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
        let mix = MixParams::from_project(&self.project, self.mix_version);
        if let Ok(mut m) = self.shared.mix.lock() {
            *m = mix;
        }
        self.dirty = true;
    }

    pub fn mix_snapshot(&self) -> MixParams {
        MixParams::from_project(&self.project, self.mix_version)
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
