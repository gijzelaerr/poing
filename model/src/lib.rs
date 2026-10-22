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
    /// The imported audio file, relative to the project (bundles made by import).
    pub source: Option<String>,
    /// What the analysis found; absent until it has run.
    pub analysis: Option<Analysis>,
    /// Generated versions, newest last. Never overwritten: a take is kept or deleted.
    pub takes: Option<Vec<Take>>,
    /// Takes ever started, deleted ones included, so ids and seeds never repeat.
    pub takes_made: Option<u64>,
    /// Effects on the whole mix, after the lanes.
    pub master_fx: Option<Vec<Fx>>,
    /// Effects ever made, so their ids and files never repeat.
    pub fx_made: Option<u64>,
}

/// One generated version of part of the song, played over [start, end]:
/// - `"song"`: the whole song, instead of every lane
/// - `"section"`: a range of the mix, instead of every lane there
/// - `"lane"`: one lane (`track`), blended in by `blend` (1 = replaces it)
/// - `"layer"`: a new part on top, at `blend`
#[derive(SerJson, DeJson, Clone, Debug)]
pub struct Take {
    pub id: String,
    pub scope: String,
    pub start: f64,
    pub end: f64,
    /// `"yue2"` (cover), `"acestep"` (repaint), `"dereverb"`, `"lego"` (a new part).
    pub engine: String,
    /// The Describe text it was made from.
    pub prompt: String,
    pub seed: u64,
    /// Audio file relative to the project.
    pub file: String,
    /// The lane a `"lane"` or `"layer"` take belongs to.
    pub track: Option<String>,
    /// How much of a lane or layer take is heard, 0..1.
    pub blend: Option<f64>,
}

impl Take {
    /// Takes in one slot replace each other when switched on: one version of
    /// the song, of a range, of a lane. Layers stack.
    pub fn slot(&self) -> String {
        match self.scope.as_str() {
            "song" => "song".into(),
            "section" => format!("section:{:.2}-{:.2}", self.start, self.end),
            "lane" => format!("lane:{}", self.track.as_deref().unwrap_or("")),
            _ => format!("layer:{}", self.id),
        }
    }
    pub fn blend(&self) -> f64 {
        self.blend.unwrap_or(1.0)
    }
}

/// ACE-Step lego's instrument tracks.
pub const LEGO_TRACKS: [&str; 12] =
    ["woodwinds", "brass", "fx", "synth", "strings", "percussion", "keyboard", "guitar", "bass", "drums", "backing_vocals", "vocals"];

/// What Describe on a lane asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum LaneIntent {
    /// Less (or more) echo on this lane: split it into dry voice and reverb.
    Dereverb,
    /// A new part (an instrument lego can play) layered on top.
    AddLayer(String),
    /// "add ..." without an instrument Poing can tell.
    AddWhat,
    /// An audio effect on the lane (saturation, reverb, delay ...): Claude
    /// writes it as a Splash audio shader.
    Effect,
    /// Anything else: play this lane's part again, as the named instrument or
    /// the lane's own kind (None: Poing cannot tell which instrument).
    Replace(Option<String>),
}

/// Words that ask for an audio effect.
pub const EFFECT_WORDS: [&str; 34] = [
    "saturat", "tape", "drive", "overdrive", "distort", "fuzz", "crunch", "reverb", "plate", "hall", "delay", "echo",
    "chorus", "flang", "phaser", "tremolo", "vibrato", "compress", "limit", "eq", "equali", "filter", "lowpass",
    "highpass", "bitcrush", "crush", "lofi", "wah", "widen", "wider", "stereo", "gate", "galm", "vervorm",
];

/// The installed plugin a Describe names, if any: whole words of the plugin's
/// name, also without its vendor's first word or a trailing version number
/// ("pro-q" finds "FabFilter Pro-Q 3"). The longest name named wins.
pub fn plugin_named<'a>(text: &str, plugins: &'a [String]) -> Option<&'a String> {
    let t = format!(" {} ", text.to_lowercase().replace(|c: char| !c.is_alphanumeric() && c != '-', " "));
    let aliases = |name: &str| -> Vec<String> {
        let n = name.to_lowercase().replace(|c: char| !c.is_alphanumeric() && c != '-', " ");
        let words: Vec<&str> = n.split_whitespace().collect();
        let mut out = vec![words.join(" ")];
        let unversioned: Vec<&str> = match words.split_last() {
            Some((last, rest)) if !rest.is_empty() && last.chars().all(|c| c.is_ascii_digit() || c == 'v') => rest.to_vec(),
            _ => words.clone(),
        };
        out.push(unversioned.join(" "));
        // a version glued on ("Serum2") is said with a space ("serum 2")
        let joined = words.join(" ");
        let head = joined.trim_end_matches(|c: char| c.is_ascii_digit());
        if head.len() < joined.len() && head.ends_with(|c: char| c.is_alphabetic()) {
            out.push(format!("{head} {}", &joined[head.len()..]));
        }
        for w in [&words, &unversioned] {
            if w.len() > 1 {
                out.push(w[1..].join(" "));
            }
        }
        out.retain(|a| a.len() >= 4);
        out
    };
    plugins
        .iter()
        .filter_map(|p| aliases(p).into_iter().filter(|a| t.contains(&format!(" {a} "))).map(|a| a.len()).max().map(|l| (l, p)))
        .max_by_key(|(l, _)| *l)
        .map(|(_, p)| p)
}

/// A MIDI part a Describe asks for (with no lane selected): "add a bassline",
/// "a drum beat in midi", "chords on keys". The built-in instrument it gets:
/// bass, keys, lead or drums. None: not a part.
pub fn midi_part(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    let words: Vec<&str> = t.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let has = |ws: &[&str]| ws.iter().any(|w| words.iter().any(|x| x.starts_with(w)));
    let asks = has(&["add", "write", "make", "voeg", "maak", "midi"]);
    let part = if has(&["drum", "beat", "groove", "percussion", "hihat", "kick", "trommel"]) {
        Some("drums")
    } else if has(&["bass"]) {
        Some("bass")
    } else if has(&["chord", "pad", "keys", "piano", "organ", "string", "akkoord"]) {
        Some("keys")
    } else if has(&["melod", "lead", "hook", "riff", "arp", "counter", "solo"]) {
        Some("lead")
    } else {
        None
    };
    match (asks, part) {
        (true, Some(p)) => Some(p),
        (true, None) if has(&["midi"]) => Some("lead"),
        _ => None,
    }
}

/// Does a Describe ask for an audio effect?
pub fn is_effect_text(text: &str) -> bool {
    let t = text.to_lowercase();
    let words: Vec<&str> = t.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    EFFECT_WORDS.iter().any(|e| words.iter().any(|w| if e.len() <= 3 { w == e } else { w.starts_with(e) }))
        || t.contains("lo-fi")
}

/// Does a Describe with nothing selected ask for an effect on the whole mix
/// (rather than a new version of the song)?
pub fn is_master_effect_text(text: &str) -> bool {
    let t = text.to_lowercase();
    is_effect_text(&t) && ["master", "whole mix", "the mix", "everything", "whole song", "hele mix", "alles"].iter().any(|w| t.contains(w))
}

/// The lego instrument a Describe names, if any.
fn instrument_in(t: &str) -> Option<String> {
    let has = |w: &str| t.split(|c: char| !c.is_alphanumeric()).any(|x| x.starts_with(w));
    let synonyms: [(&str, &str); 16] = [
        ("drum", "drums"), ("percussion", "percussion"), ("shaker", "percussion"), ("conga", "percussion"),
        ("piano", "keyboard"), ("keys", "keyboard"), ("rhodes", "keyboard"), ("organ", "keyboard"),
        ("synth", "synth"), ("string", "strings"), ("guitar", "guitar"), ("bass", "bass"), ("horn", "brass"),
        ("choir", "backing_vocals"), ("harmon", "backing_vocals"), ("strijk", "strings"),
    ];
    LEGO_TRACKS
        .iter()
        .find(|track| has(track))
        .map(|t| t.to_string())
        .or_else(|| synonyms.iter().find(|(w, _)| has(w)).map(|(_, t)| t.to_string()))
}

/// Read a Describe on lane `track` (English and Dutch keywords).
pub fn lane_intent(text: &str, track: &str) -> LaneIntent {
    let t = text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| t.split(|c: char| !c.is_alphanumeric()).any(|x| x == *w || x.starts_with(w)));
    // Less echo is taken out of the recording; more of anything is an effect.
    let less = has(&["less", "minder", "remove", "no", "without", "zonder", "weg"]);
    if (less && has(&["echo", "reverb", "galm", "nagalm"])) || has(&["dry", "droog", "drier", "droger"]) {
        return LaneIntent::Dereverb;
    }
    if is_effect_text(&t) {
        return LaneIntent::Effect;
    }
    // The lane's own kind, when it is one lego plays.
    let own = matches!(track, "drums" | "bass" | "vocals").then(|| track.to_string());
    if has(&["add", "extra", "layer", "toevoegen", "voeg", "laag", "more", "meer"]) {
        return match instrument_in(&t).or(own) {
            Some(i) => LaneIntent::AddLayer(i),
            None => LaneIntent::AddWhat,
        };
    }
    LaneIntent::Replace(instrument_in(&t).or(own))
}

/// Where a range edit starts and ends: on the nearest downbeat within half a
/// second, so a replaced part comes in on the one.
pub fn snap_to_downbeats(start: f64, end: f64, downbeats: &[f64]) -> (f64, f64) {
    let snap = |t: f64| {
        downbeats
            .iter()
            .copied()
            .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
            .filter(|d| (d - t).abs() <= 0.5)
            .unwrap_or(t)
    };
    let (a, b) = (snap(start), snap(end));
    if b > a + 0.5 {
        (a, b)
    } else {
        (start, end)
    }
}

/// The Demucs stem that holds a lego track (htdemucs: vocals, drums, bass, other).
pub fn demucs_stem_for(track: &str) -> &'static str {
    match track {
        "drums" | "percussion" => "drums",
        "bass" => "bass",
        "vocals" | "backing_vocals" => "vocals",
        _ => "other",
    }
}

/// Seeds for successive takes: fixed so a take can be made again, varied so
/// the next take sounds different. 1234 made the first liked It's Autumn cover.
pub const TAKE_SEEDS: [u64; 6] = [1234, 1966, 2026, 42, 7, 831001];

/// Song understanding, as written by the analysis sidecar (`analysis.json`).
#[derive(SerJson, DeJson, Clone, Debug, Default)]
pub struct Analysis {
    pub tempo: Option<f64>,
    pub key: Option<String>,
    pub beats: Option<Vec<f64>>,
    pub downbeats: Option<Vec<f64>>,
    /// Chord spans, e.g. label "F#", "Em", "B7".
    pub chords: Option<Vec<Section>>,
    pub sections: Option<Vec<Section>>,
    pub lyrics: Option<Vec<LyricLine>>,
}

#[derive(SerJson, DeJson, Clone, Debug)]
pub struct LyricLine {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// The stems the analysis separates, in lane order: (track id, display name).
pub const STEMS: [(&str, &str); 4] = [("vocals", "Vocals"), ("drums", "Drums"), ("bass", "Bass"), ("other", "Other")];

#[derive(SerJson, DeJson, Clone, Debug)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub file: String,
    pub gain_db: Option<f64>,
    pub mute: Option<bool>,
    pub solo: Option<bool>,
    /// Effects on this lane, in order (Splash audio shaders).
    pub fx: Option<Vec<Fx>>,
    /// A MIDI lane: notes played by `instrument` (then `file` is empty).
    pub notes: Option<Vec<Note>>,
    /// The instrument of a MIDI lane: a Splash `voice()` shader (`file`) or,
    /// later, an instrument plugin.
    pub instrument: Option<Fx>,
}

/// A note on a MIDI lane, in beats of the song's beat grid.
#[derive(SerJson, DeJson, Clone, Debug, PartialEq)]
pub struct Note {
    pub start: f64,
    pub len: f64,
    /// MIDI note number (60 = middle C).
    pub pitch: u8,
    /// 0..1
    pub vel: f64,
}

impl Track {
    pub fn is_midi(&self) -> bool {
        self.notes.is_some()
    }
}

/// The song's beats as times (from the analysis), so MIDI notes in beats
/// follow a played-in tempo: between detected beats it interpolates, past
/// either end it goes on at the median beat length.
#[derive(Clone, Debug)]
pub struct BeatGrid {
    /// Seconds of beat 0, 1, 2 ...
    times: Vec<f64>,
    /// Seconds per beat beyond the detected ones.
    period: f64,
    /// Beats per bar (from the downbeats, else 4).
    pub per_bar: u32,
    /// The beat a bar starts on (the first downbeat's index).
    pub bar_offset: f64,
}

impl BeatGrid {
    pub fn new(beats: &[f64], downbeats: &[f64], tempo: Option<f64>) -> Self {
        let mut times: Vec<f64> = beats.iter().copied().filter(|t| t.is_finite()).collect();
        times.sort_by(|a, b| a.total_cmp(b));
        times.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
        let mut gaps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.05).collect();
        gaps.sort_by(|a, b| a.total_cmp(b));
        let period = gaps.get(gaps.len() / 2).copied().or_else(|| tempo.map(|t| 60.0 / t.max(1.0))).unwrap_or(0.5);
        if times.is_empty() {
            times.push(0.0);
        }
        // beats per bar: beats between consecutive downbeats, most common
        let index_of = |t: f64| times.iter().position(|b| (b - t).abs() < period * 0.3);
        let idx: Vec<usize> = downbeats.iter().filter_map(|&d| index_of(d)).collect();
        let mut counts: Vec<u32> = idx.windows(2).map(|w| (w[1] - w[0]) as u32).filter(|c| (2..=12).contains(c)).collect();
        counts.sort();
        let per_bar = counts.get(counts.len() / 2).copied().unwrap_or(4);
        let bar_offset = idx.first().map(|&i| (i as u32 % per_bar) as f64).unwrap_or(0.0);
        BeatGrid { times, period, per_bar, bar_offset }
    }

    /// From a project's analysis (a plain 120 bpm grid without one).
    pub fn of(p: &Project) -> Self {
        let a = p.analysis.clone().unwrap_or_default();
        Self::new(&a.beats.unwrap_or_default(), &a.downbeats.unwrap_or_default(), a.tempo.or(Some(120.0)))
    }

    pub fn seconds_per_beat(&self) -> f64 {
        self.period
    }

    pub fn time_of(&self, beat: f64) -> f64 {
        let n = self.times.len();
        if beat <= 0.0 {
            return self.times[0] + beat * self.period;
        }
        let i = beat.floor() as usize;
        if i + 1 < n {
            let f = beat - i as f64;
            self.times[i] + (self.times[i + 1] - self.times[i]) * f
        } else {
            self.times[n - 1] + (beat - (n - 1) as f64) * self.period
        }
    }

    pub fn beat_at(&self, t: f64) -> f64 {
        let n = self.times.len();
        if t <= self.times[0] {
            return (t - self.times[0]) / self.period;
        }
        if t >= self.times[n - 1] {
            return (n - 1) as f64 + (t - self.times[n - 1]) / self.period;
        }
        let i = self.times.partition_point(|b| *b <= t) - 1;
        i as f64 + (t - self.times[i]) / (self.times[i + 1] - self.times[i])
    }

    /// Snap a beat position to a grid of `div` steps per beat (4 = 1/16 notes).
    pub fn snap(beat: f64, div: f64) -> f64 {
        (beat * div).round() / div
    }
}

/// An effect: a Splash audio shader (`effect(l, r)`) in the bundle's `fx/`
/// folder, its knob settings and whether it is bypassed. Editing the file
/// recompiles it live.
#[derive(SerJson, DeJson, Clone, Debug)]
pub struct Fx {
    pub id: String,
    pub name: String,
    /// Shader source, relative to the project.
    pub file: String,
    /// Knob values by param name; params not listed play at their default.
    pub params: Option<Vec<FxParam>>,
    pub bypass: Option<bool>,
    /// The Describe it was written from.
    pub prompt: Option<String>,
    /// A VST3 plugin instead of a shader: its bundle path (then `file` is
    /// empty and `params` hold normalized values by parameter name).
    pub plugin: Option<String>,
}

#[derive(SerJson, DeJson, Clone, Debug)]
pub struct FxParam {
    pub name: String,
    pub value: f64,
}

impl Fx {
    pub fn bypassed(&self) -> bool {
        self.bypass.unwrap_or(false)
    }
    pub fn param(&self, name: &str) -> Option<f64> {
        self.params.as_ref()?.iter().find(|p| p.name == name).map(|p| p.value)
    }
    pub fn set_param(&mut self, name: &str, value: f64) {
        let params = self.params.get_or_insert_with(Vec::new);
        match params.iter_mut().find(|p| p.name == name) {
            Some(p) => p.value = value,
            None => params.push(FxParam { name: name.into(), value }),
        }
    }
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
    /// A fresh project for an imported song: one "Original" lane playing the
    /// source file, everything else filled in when the analysis lands.
    pub fn import(name: &str, source_file: &str, duration: f64, sample_rate: u32) -> Self {
        Project {
            name: name.to_string(),
            duration,
            tracks: vec![Track {
                id: "original".into(),
                name: "Original".into(),
                file: source_file.to_string(),
                gain_db: None,
                mute: None,
                solo: None,
                fx: None,
                notes: None,
                instrument: None,
            }],
            sample_rate: Some(sample_rate),
            sections: Some(Vec::new()),
            regions: Some(Vec::new()),
            source: Some(source_file.to_string()),
            analysis: None,
            takes: Some(Vec::new()),
            takes_made: None,
            master_fx: None,
            fx_made: None,
        }
    }

    pub fn is_analysed(&self) -> bool {
        self.analysis.is_some()
    }

    /// Merge an analysis: add the stem lanes (before any existing ones), mute the
    /// original so the stems are what plays, and take its sections when the
    /// project has none yet. Returns the indices of the tracks it added.
    pub fn apply_analysis(&mut self, analysis: Analysis, stem_files: &[(String, String)]) -> Vec<usize> {
        let mut added = Vec::new();
        for (id, file) in stem_files {
            if self.track_index(id).is_some() {
                continue;
            }
            let name = STEMS.iter().find(|(s, _)| s == id).map(|(_, n)| n.to_string()).unwrap_or_else(|| id.clone());
            self.tracks.push(Track { id: id.clone(), name, file: file.clone(), gain_db: None, mute: None, solo: None, fx: None, notes: None, instrument: None });
            added.push(self.tracks.len() - 1);
        }
        if !added.is_empty() {
            if let Some(i) = self.track_index("original") {
                self.tracks[i].mute = Some(true);
            }
        }
        if self.sections().is_empty() {
            if let Some(secs) = &analysis.sections {
                *self.sections_mut() = secs.clone();
            }
        }
        self.analysis = Some(analysis);
        added
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        // Lenient: fields from newer versions are skipped, not an error.
        let mut p = Project::deserialize_json_lenient(&text).map_err(|e| format!("parse {}: {e:?}", path.display()))?;
        p.sections.get_or_insert_with(Vec::new);
        p.regions.get_or_insert_with(Vec::new);
        p.takes.get_or_insert_with(Vec::new);
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

    pub fn takes(&self) -> &Vec<Take> {
        self.takes.as_ref().expect("filled on load")
    }
    pub fn takes_mut(&mut self) -> &mut Vec<Take> {
        self.takes.get_or_insert_with(Vec::new)
    }
    pub fn take_index(&self, id: &str) -> Option<usize> {
        self.takes().iter().position(|t| t.id == id)
    }
    /// Start a take: its id and seed. Counts up past deleted takes.
    pub fn next_take(&mut self) -> (String, u64) {
        let existing = self.takes().iter().filter_map(|t| t.id.trim_start_matches('t').parse::<u64>().ok()).max().unwrap_or(0);
        let n = self.takes_made.unwrap_or(0).max(existing);
        self.takes_made = Some(n + 1);
        let i = n as usize;
        let seed = TAKE_SEEDS[i % TAKE_SEEDS.len()] + 1000 * (i / TAKE_SEEDS.len()) as u64;
        (format!("t{}", n + 1), seed)
    }

    /// A new effect id ("fx1", "fx2", ...): counts up past deleted effects.
    pub fn next_fx(&mut self) -> String {
        let n = self.fx_made.unwrap_or(0) + 1;
        self.fx_made = Some(n);
        format!("fx{n}")
    }

    /// The effect chain of lane `lane`, or the master's (`None`).
    pub fn chain(&self, lane: Option<usize>) -> &[Fx] {
        match lane {
            Some(i) => self.tracks.get(i).and_then(|t| t.fx.as_deref()).unwrap_or(&[]),
            None => self.master_fx.as_deref().unwrap_or(&[]),
        }
    }
    pub fn chain_mut(&mut self, lane: Option<usize>) -> &mut Vec<Fx> {
        match lane {
            Some(i) => self.tracks[i].fx.get_or_insert_with(Vec::new),
            None => self.master_fx.get_or_insert_with(Vec::new),
        }
    }
    /// Where effect `id` sits: (lane or master, index in its chain).
    pub fn find_fx(&self, id: &str) -> Option<(Option<usize>, usize)> {
        let lanes = (0..self.tracks.len()).map(Some).chain(std::iter::once(None));
        lanes.into_iter().find_map(|lane| self.chain(lane).iter().position(|f| f.id == id).map(|i| (lane, i)))
    }

    /// Lyrics tagged by section, the way YuE2 wants them: `[Verse]` then its
    /// lines, a blank line between sections. A line belongs to the section its
    /// middle falls in; sections without words keep their tag.
    pub fn sung_lyrics(&self) -> String {
        let lines = self.analysis.as_ref().and_then(|a| a.lyrics.as_ref());
        let mut out = Vec::new();
        for s in self.sections() {
            let tag: String = s.label.chars().filter(|c| !c.is_ascii_digit()).collect();
            let tag = tag.trim();
            let mut chars = tag.chars();
            let tag = chars.next().map(|c| c.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default();
            let mut block = format!("[{tag}]");
            for l in lines.into_iter().flatten() {
                let mid = 0.5 * (l.start + l.end);
                if mid >= s.start && mid < s.end && !l.text.trim().is_empty() {
                    block.push('\n');
                    block.push_str(l.text.trim());
                }
            }
            out.push(block);
        }
        out.join("\n\n") + "\n"
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
                Track { id: "a".into(), name: "A".into(), file: "stems/a.wav".into(), gain_db: None, mute: None, solo: None, fx: None, notes: None, instrument: None },
                Track { id: "b".into(), name: "B".into(), file: "stems/b.wav".into(), gain_db: Some(-6.0), mute: None, solo: None, fx: None, notes: None, instrument: None },
            ],
            sample_rate: Some(48000),
            sections: Some(vec![Section { start: 0.0, end: 2.0, label: "verse".into() }]),
            regions: Some(regions),
            source: None,
            analysis: None,
            takes: None,
            takes_made: None,
            master_fx: None,
            fx_made: None,
        }
    }

    #[test]
    fn describe_on_a_lane_is_read_in_english_and_dutch() {
        assert_eq!(lane_intent("less echo on the voice", "vocals"), LaneIntent::Dereverb);
        assert_eq!(lane_intent("minder galm", "vocals"), LaneIntent::Dereverb);
        assert_eq!(
            lane_intent("add tight 80s disco house drums, four-on-the-floor kick", "drums"),
            LaneIntent::AddLayer("drums".into())
        );
        assert_eq!(lane_intent("voeg strijkers toe, warme strings", "other"), LaneIntent::AddLayer("strings".into()));
        assert_eq!(lane_intent("add some piano chords", "other"), LaneIntent::AddLayer("keyboard".into()));
        assert_eq!(lane_intent("add more groove", "drums"), LaneIntent::AddLayer("drums".into()));
        assert_eq!(lane_intent("add more groove", "other"), LaneIntent::AddWhat);
        assert_eq!(lane_intent("funkier, slap style", "bass"), LaneIntent::Replace(Some("bass".into())));
        assert_eq!(lane_intent("warm rhodes instead", "other"), LaneIntent::Replace(Some("keyboard".into())));
        assert_eq!(lane_intent("make it nicer", "other"), LaneIntent::Replace(None));
    }

    #[test]
    fn the_beat_grid_follows_played_beats() {
        // uneven beats (a live band), downbeats every 4
        let beats = [1.0, 1.5, 2.02, 2.5, 3.0, 3.48, 4.0, 4.5];
        let g = BeatGrid::new(&beats, &[1.0, 3.0], None);
        assert_eq!(g.per_bar, 4);
        assert!((g.time_of(2.0) - 2.02).abs() < 1e-9, "on a detected beat");
        assert!((g.time_of(2.5) - 2.26).abs() < 1e-9, "between beats: interpolated");
        assert!((g.time_of(9.0) - 5.5).abs() < 1e-9, "past the end: median beat length");
        assert!((g.time_of(-1.0) - 0.5).abs() < 1e-9, "before the first beat");
        for b in [-1.5, 0.0, 0.3, 2.5, 6.9, 11.25] {
            assert!((g.beat_at(g.time_of(b)) - b).abs() < 1e-9, "round trip at {b}");
        }
        assert_eq!(BeatGrid::snap(2.13, 4.0), 2.25);
        // no analysis: a plain grid
        let plain = BeatGrid::new(&[], &[], Some(120.0));
        assert!((plain.time_of(8.0) - 4.0).abs() < 1e-9 && plain.per_bar == 4);
    }

    #[test]
    fn midi_parts_are_read_from_describe() {
        assert_eq!(midi_part("add a funky bassline"), Some("bass"));
        assert_eq!(midi_part("add a four on the floor drum beat"), Some("drums"));
        assert_eq!(midi_part("write warm pad chords under the chorus"), Some("keys"));
        assert_eq!(midi_part("add a catchy melody"), Some("lead"));
        assert_eq!(midi_part("make a midi part for the bridge"), Some("lead"));
        assert_eq!(midi_part("1960s Beatles beat pop, warm tape"), None, "a style is a cover");
        assert_eq!(midi_part("tape saturation on the whole mix"), None);
    }

    #[test]
    fn installed_plugins_are_found_by_name() {
        let plugins: Vec<String> = ["FabFilter Pro-Q 3", "FabFilter Pro-C 2", "Raum", "Replika XT", "Supercharger GT", "Supercharger", "Love", "Serum", "Serum2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let found = |t: &str| plugin_named(t, &plugins).cloned();
        assert_eq!(found("add Raum on the vocals"), Some("Raum".into()));
        assert_eq!(found("pro-q on the bass"), Some("FabFilter Pro-Q 3".into()));
        assert_eq!(found("FabFilter Pro-C please"), Some("FabFilter Pro-C 2".into()));
        assert_eq!(found("supercharger gt, gently"), Some("Supercharger GT".into()), "the longer name wins");
        assert_eq!(found("add some reverb"), None, "a plain effect is a shader");
        assert_eq!(found("add tight disco drums"), None);
        assert_eq!(found("lovely warm tape"), None, "whole words only");
        assert_eq!(found("play it with Serum 2"), Some("Serum2".into()), "a glued version said apart");
        assert_eq!(found("play it with serum"), Some("Serum".into()));
    }

    #[test]
    fn effects_are_told_apart_from_echo_removal_and_new_parts() {
        assert_eq!(lane_intent("tape saturation on the bass", "bass"), LaneIntent::Effect);
        assert_eq!(lane_intent("add some reverb", "vocals"), LaneIntent::Effect);
        assert_eq!(lane_intent("a slapback delay", "vocals"), LaneIntent::Effect);
        assert_eq!(lane_intent("less echo", "vocals"), LaneIntent::Dereverb);
        assert_eq!(lane_intent("minder galm op de stem", "vocals"), LaneIntent::Dereverb);
        assert_eq!(lane_intent("add tight disco drums", "drums"), LaneIntent::AddLayer("drums".into()));
        assert!(!is_master_effect_text("1960s beat pop, warm tape sound"), "a style is a cover, not an effect");
        assert!(is_master_effect_text("gentle tape saturation on the whole mix"));
    }

    #[test]
    fn range_edits_snap_to_downbeats() {
        let downbeats = [0.0, 2.0, 4.0, 6.0];
        assert_eq!(snap_to_downbeats(1.7, 5.8, &downbeats), (2.0, 6.0));
        assert_eq!(snap_to_downbeats(2.9, 4.2, &downbeats), (2.9, 4.0), "too far from a downbeat: stays");
        assert_eq!(snap_to_downbeats(1.8, 2.1, &downbeats), (1.8, 2.1), "would collapse: stays");
    }

    #[test]
    fn take_slots() {
        let mut t = Take {
            id: "t4".into(),
            scope: "lane".into(),
            start: 0.0,
            end: 9.0,
            engine: "dereverb".into(),
            prompt: "less echo".into(),
            seed: 1,
            file: "takes/t4.wav".into(),
            track: Some("vocals".into()),
            blend: Some(0.65),
        };
        assert_eq!(t.slot(), "lane:vocals");
        t.scope = "layer".into();
        assert_eq!(t.slot(), "layer:t4", "layers stack: each has its own slot");
    }

    #[test]
    fn sung_lyrics_are_tagged_by_section() {
        let mut p = project(vec![]);
        *p.sections_mut() = vec![
            Section { start: 0.0, end: 5.0, label: "intro".into() },
            Section { start: 5.0, end: 20.0, label: "verse 2".into() },
        ];
        p.analysis = Some(Analysis {
            lyrics: Some(vec![
                LyricLine { start: 6.0, end: 9.0, text: " Ah, hear the wind ".into() },
                LyricLine { start: 9.0, end: 12.0, text: "See the birds".into() },
            ]),
            ..Default::default()
        });
        assert_eq!(p.sung_lyrics(), "[Intro]\n\n[Verse]\nAh, hear the wind\nSee the birds\n");
    }

    #[test]
    fn next_take_counts_up_with_fixed_seeds() {
        let mut p = project(vec![]);
        p.takes = Some(Vec::new());
        assert_eq!(p.next_take(), ("t1".to_string(), 1234));
        let take = Take {
            id: "t1".into(),
            scope: "song".into(),
            start: 0.0,
            end: 1.0,
            engine: "yue2".into(),
            prompt: "x".into(),
            seed: 1234,
            file: "takes/t1.wav".into(),
            track: None,
            blend: None,
        };
        p.takes_mut().push(take);
        assert_eq!(p.next_take(), ("t2".to_string(), 1966));
        // t2 was deleted (or never finished): the next one is still new
        p.takes_mut().clear();
        assert_eq!(p.next_take(), ("t3".to_string(), 2026));
    }

    #[test]
    fn import_then_analysis_adds_stems_and_sections() {
        let mut p = Project::import("song", "original.mp3", 10.0, 48000);
        assert_eq!(p.tracks.len(), 1);
        let a = Analysis {
            tempo: Some(123.0),
            key: Some("F major".into()),
            sections: Some(vec![Section { start: 0.0, end: 5.0, label: "intro".into() }, Section { start: 5.0, end: 10.0, label: "verse".into() }]),
            ..Default::default()
        };
        let stems: Vec<(String, String)> = STEMS.iter().map(|(id, _)| (id.to_string(), format!("stems/{id}.wav"))).collect();
        let added = p.apply_analysis(a, &stems);
        assert_eq!(added.len(), 4);
        assert_eq!(p.tracks[1].name, "Vocals");
        assert!(p.tracks[0].muted(), "original is muted once stems exist");
        assert_eq!(p.sections().len(), 2);
        assert!(p.is_analysed());
        // applying again does not duplicate lanes
        let again = p.apply_analysis(Analysis::default(), &stems);
        assert!(again.is_empty() && p.tracks.len() == 5);
    }

    #[test]
    fn analysis_json_from_the_sidecar_parses() {
        let json = r#"{"tempo":123.0,"key":"E minor","beats":[0.5,1.0],"downbeats":[0.5],
            "chords":[{"start":0.0,"end":1.9,"label":"Em"}],
            "sections":[{"start":0.0,"end":5.9,"label":"intro"}],
            "lyrics":[{"start":15.4,"end":19.8,"text":"hear the wind"}],
            "stems":{"vocals":"stems/vocals.wav"}}"#;
        let a = Analysis::deserialize_json_lenient(json).unwrap();
        assert_eq!(a.key.as_deref(), Some("E minor"));
        assert_eq!(a.chords.unwrap()[0].label, "Em");
        assert_eq!(a.lyrics.unwrap()[0].text, "hear the wind");
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
