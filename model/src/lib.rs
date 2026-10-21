//! Poing's project model. project.json is the interchange file between the app, the AI pipeline and agents.
//! Paths inside are relative to the project file. Optional fields are
//! `Option` so files written by other tools (the Python prep step) load.

use makepad_micro_serde::*;
use std::path::{Path, PathBuf};

#[derive(SerJson, DeJson, Clone, Debug)]
pub struct Project {
    pub name: String,
    pub duration: f64,
    pub tracks: Vec<Track>,
    pub sample_rate: Option<u32>,
    pub sections: Option<Vec<Section>>,
    pub regions: Option<Vec<Region>>,
}

#[derive(SerJson, DeJson, Clone, Debug)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub file: String,
    pub gain_db: Option<f64>,
    pub mute: Option<bool>,
    pub solo: Option<bool>,
}

#[derive(SerJson, DeJson, Clone, Debug)]
pub struct Section {
    pub start: f64,
    pub end: f64,
    pub label: String,
}

#[derive(SerJson, DeJson, Clone, Debug)]
pub struct Region {
    pub id: String,
    pub track: String,
    pub start: f64,
    pub end: f64,
    #[rename(type)]
    pub kind: String,
    pub gain_db: Option<f64>,
    pub note: Option<String>,
}

/// Region types. "bad" and "regenerate" are flags for the AI pipeline and do
/// not change the sound; "mute" silences; the others apply `gain_db`.
pub const REGION_KINDS: [&str; 6] = ["bad", "regenerate", "too_loud", "too_quiet", "mute", "gain"];

pub fn default_gain_for(kind: &str) -> f64 {
    match kind {
        "too_loud" => -6.0,
        "too_quiet" => 4.0,
        _ => 0.0,
    }
}

pub fn db_to_lin(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

impl Track {
    pub fn gain(&self) -> f64 {
        self.gain_db.unwrap_or(0.0)
    }
    pub fn muted(&self) -> bool {
        self.mute.unwrap_or(false)
    }
    pub fn soloed(&self) -> bool {
        self.solo.unwrap_or(false)
    }
}

impl Region {
    pub fn gain(&self) -> f64 {
        self.gain_db.unwrap_or(0.0)
    }
    pub fn factor(&self) -> f32 {
        match self.kind.as_str() {
            "mute" => 0.0,
            "bad" | "regenerate" => 1.0,
            _ => db_to_lin(self.gain()),
        }
    }
}

impl Project {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let mut p = Project::deserialize_json(&text).map_err(|e| format!("parse {}: {e:?}", path.display()))?;
        p.sections.get_or_insert_with(Vec::new);
        p.regions.get_or_insert_with(Vec::new);
        Ok(p)
    }

    /// Temp file then rename, so a crash never leaves a half-written project.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.serialize_json()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    pub fn sr(&self) -> u32 {
        self.sample_rate.unwrap_or(48000)
    }
    pub fn sections(&self) -> &Vec<Section> {
        self.sections.as_ref().expect("filled on load")
    }
    pub fn sections_mut(&mut self) -> &mut Vec<Section> {
        self.sections.get_or_insert_with(Vec::new)
    }
    pub fn regions(&self) -> &Vec<Region> {
        self.regions.as_ref().expect("filled on load")
    }
    pub fn regions_mut(&mut self) -> &mut Vec<Region> {
        self.regions.get_or_insert_with(Vec::new)
    }

    pub fn track_path(&self, project_path: &Path, i: usize) -> PathBuf {
        project_path.parent().unwrap_or(Path::new(".")).join(&self.tracks[i].file)
    }

    pub fn track_index(&self, id: &str) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// Linear gain per track after mute/solo.
    pub fn track_gains(&self) -> Vec<f32> {
        let any_solo = self.tracks.iter().any(|t| t.soloed());
        self.tracks
            .iter()
            .map(|t| if t.muted() || (any_solo && !t.soloed()) { 0.0 } else { db_to_lin(t.gain()) })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(regions: Vec<Region>) -> Project {
        Project {
            name: "t".into(),
            duration: 2.0,
            tracks: vec![
                Track { id: "a".into(), name: "A".into(), file: "stems/a.wav".into(), gain_db: None, mute: None, solo: None },
                Track { id: "b".into(), name: "B".into(), file: "stems/b.wav".into(), gain_db: Some(-6.0), mute: None, solo: None },
            ],
            sample_rate: Some(48000),
            sections: Some(vec![Section { start: 0.0, end: 2.0, label: "verse".into() }]),
            regions: Some(regions),
        }
    }

    fn region(kind: &str, gain_db: f64) -> Region {
        Region { id: "r1".into(), track: "a".into(), start: 0.5, end: 1.0, kind: kind.into(), gain_db: Some(gain_db), note: Some("n".into()) }
    }

    #[test]
    fn roundtrip_keeps_the_type_field() {
        let dir = std::env::temp_dir().join(format!("poing-model-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project.json");
        project(vec![region("too_loud", -6.0)]).save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"type\":\"too_loud\""), "{text}");
        let back = Project::load(&path).unwrap();
        assert_eq!(back.regions()[0].kind, "too_loud");
        assert_eq!(back.regions()[0].note.as_deref(), Some("n"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn files_without_optional_fields_load() {
        // What the Python prep step writes: no solo/mute, no regions yet.
        let dir = std::env::temp_dir().join(format!("poing-model-min-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project.json");
        std::fs::write(&path, r#"{"name":"x","duration":1.5,"tracks":[{"id":"v","name":"Vocals","file":"stems/v.wav","gain_db":0}]}"#).unwrap();
        let p = Project::load(&path).unwrap();
        assert_eq!(p.sr(), 48000);
        assert!(p.sections().is_empty() && p.regions().is_empty());
        assert!(!p.tracks[0].muted() && !p.tracks[0].soloed());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn solo_silences_the_others() {
        let mut p = project(vec![]);
        p.tracks[1].solo = Some(true);
        let g = p.track_gains();
        assert_eq!(g[0], 0.0);
        assert!((g[1] - 0.501).abs() < 0.01);
    }

    #[test]
    fn region_factors() {
        assert_eq!(region("mute", 0.0).factor(), 0.0);
        assert_eq!(region("regenerate", -6.0).factor(), 1.0);
        assert_eq!(region("bad", -6.0).factor(), 1.0);
        assert!((region("gain", -6.0).factor() - 0.501).abs() < 0.01);
    }
}
