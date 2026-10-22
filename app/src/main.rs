//! Poing: a song's stems as lanes, its sections, and marked regions (bad /
//! regenerate / level fixes with notes for the AI). Edits play live; Save writes
//! project.json for the AI pipeline, Render writes a WAV and a mastered MP3.
//!
//!   poing path/to/project.json      (or POING_PROJECT)
//!   POING_MUTE=1 keeps agent/test instances silent.

pub use makepad_widgets;

mod audio;
pub(crate) use poing_model as model;
mod state;
mod timeline;

use audio::{Job, JobUpdate, Jobs, LayerJob, LegoJob, RepaintJob, Stem, Yue2Fixture};
use makepad_widgets::*;
use makepad_micro_serde::DeJson;
use model::{default_gain_for, demucs_stem_for, lane_intent, snap_to_downbeats, Analysis, LaneIntent, Project, Take, LEGO_TRACKS, REGION_KINDS};
use state::{EditorState, Selection};
use std::path::{Path, PathBuf};
use timeline::kind_index;

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Poing"
                window.inner_size: vec2(1500, 900)
                body +: {
                    flow: Down
                    show_bg: true
                    draw_bg +: {color: #x121418}

                    transport := View{
                        width: Fill height: Fit flow: Right spacing: 8
                        padding: Inset{left: 10 right: 10 top: 8 bottom: 8}
                        align: Align{y: 0.5}
                        play_btn := Button{text: "Play"}
                        start_btn := Button{text: "|<"}
                        zoom_out_btn := Button{text: "Zoom -"}
                        zoom_in_btn := Button{text: "Zoom +"}
                        fit_btn := Button{text: "Fit"}
                        split_btn := Button{text: "Split section at playhead"}
                        save_btn := Button{text: "Save"}
                        render_btn := Button{text: "Render mix"}
                        project_label := Label{text: ""}
                        describe_input := TextInput{width: Fill empty_text: "Describe a new version and press Enter, e.g. 1960s Beatles beat pop, warm tape"}
                    }

                    timeline := mod.widgets.DawTimeline{}

                    inspector := View{
                        width: Fill height: 124 flow: Down spacing: 8
                        padding: Inset{left: 14 right: 14 top: 10 bottom: 10}
                        show_bg: true
                        draw_bg +: {color: #x1e2128}
                        insp_title := Label{text: "Loading..."}
                        region_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            kind_dd := DropDown{width: 140 labels: ["bad" "regenerate" "too_loud" "too_quiet" "mute" "gain"]}
                            region_gain := Slider{width: 260 text: "Gain dB" min: -24.0 max: 12.0 step: 0.5 precision: 1 default: 0.0}
                            note_input := TextInput{width: 380 empty_text: "Note for the AI: what is wrong, what to change"}
                            delete_btn := Button{text: "Delete region"}
                        }
                        track_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            track_gain := Slider{width: 300 text: "Track gain dB" min: -36.0 max: 12.0 step: 0.5 precision: 1 default: 0.0}
                            mute_cb := CheckBox{text: "Mute"}
                            solo_cb := CheckBox{text: "Solo"}
                        }
                        section_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            section_input := TextInput{width: 200 empty_text: "section name"}
                            merge_btn := Button{text: "Remove (merge into previous)"}
                        }
                        take_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            take_blend := Slider{width: 300 text: "Blend %" min: 0.0 max: 100.0 step: 1.0 precision: 0 default: 100.0}
                            keep_btn := Button{text: "Keep as a new song"}
                            delete_take_btn := Button{text: "Delete take"}
                        }
                        status_label := Label{text: ""}
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    state: Option<EditorState>,
    /// When the last status message was shown: "Unsaved changes" waits its turn.
    #[rust]
    status_at: Option<std::time::Instant>,
    #[rust]
    jobs: Option<Jobs>,
}

/// Audio Poing can import (what makepad-audio-decode reads).
const AUDIO_EXT: [&str; 4] = ["mp3", "wav", "flac", "ogg"];

/// What the command line asks to open: a project, or a song to import.
enum Open {
    Project(PathBuf),
    Audio(PathBuf),
}

fn open_target() -> Open {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).collect();
    let is_audio = |a: &String| {
        Path::new(a).extension().and_then(|e| e.to_str()).is_some_and(|e| AUDIO_EXT.contains(&e.to_ascii_lowercase().as_str()))
    };
    if let Some(a) = args.iter().find(|a| is_audio(a)) {
        return Open::Audio(PathBuf::from(a));
    }
    let project = args
        .iter()
        .find(|a| a.ends_with(".json") || a.ends_with(".poing"))
        .cloned()
        .or_else(|| std::env::var("POING_PROJECT").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("project.json"));
    // A bundle folder opens its project.json.
    Open::Project(if project.extension().is_some_and(|e| e == "poing") { project.join("project.json") } else { project })
}

/// Import a song: `<song>.poing/` beside it holds a copy of the original and the
/// project; an existing bundle is reopened. Returns the project file and
/// whether the song still needs analysing.
fn import(audio: &Path, sr: u32) -> Result<(PathBuf, bool), String> {
    let name = audio.file_stem().and_then(|s| s.to_str()).ok_or("bad file name")?.to_string();
    let bundle = audio.with_file_name(format!("{name}.poing"));
    let project_file = bundle.join("project.json");
    if project_file.exists() {
        let p = Project::load(&project_file)?;
        return Ok((project_file, !p.is_analysed()));
    }
    std::fs::create_dir_all(&bundle).map_err(|e| format!("create {}: {e}", bundle.display()))?;
    let ext = audio.extension().and_then(|e| e.to_str()).unwrap_or("wav").to_ascii_lowercase();
    let source = format!("original.{ext}");
    std::fs::copy(audio, bundle.join(&source)).map_err(|e| format!("copy {}: {e}", audio.display()))?;
    let original = audio::load_stem(&bundle.join(&source), sr)?;
    let duration = original.l.len() as f64 / sr as f64;
    Project::import(&name, &source, duration, sr).save(&project_file)?;
    Ok((project_file, true))
}

/// "Song name · E minor · 125 bpm"
fn song_title(p: &Project) -> String {
    let mut t = p.name.clone();
    if let Some(a) = &p.analysis {
        if let Some(k) = &a.key {
            t.push_str(&format!("  ·  {k}"));
        }
        if let Some(bpm) = a.tempo {
            t.push_str(&format!("  ·  {bpm:.0} bpm"));
        }
    }
    t
}

/// Describe on the whole song: queue a YuE2 cover take rendered from the
/// bundle's score (chords and melody kept) and its sung lyrics. A
/// `lyrics.txt` in the bundle (hand-corrected) wins over the transcription.
fn describe(st: &mut EditorState, jobs: Option<&Jobs>, text: &str) -> String {
    let Some(jobs) = jobs else { return "The job worker is not running".into() };
    if !st.project.is_analysed() {
        return "Poing is still listening to the song. Describe it again when the lanes are in.".into();
    }
    if st.analysing.is_some() {
        return "A job is still running; describe the next take when it is done.".into();
    }
    let bundle = st.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let abc = std::fs::read_to_string(bundle.join("score").join("score.abc")).ok().filter(|s| !s.trim().is_empty());
    let lyrics = std::fs::read_to_string(bundle.join("lyrics.txt")).unwrap_or_else(|_| st.project.sung_lyrics()).replace("\r\n", "\n");
    if let Selection::Section(i) = st.sel {
        return repaint_section(st, jobs, bundle, i, text, lyrics);
    }
    // A lane, or a region on one: work on that part.
    let lane = match st.sel {
        Selection::Track(i) => Some((i, None)),
        Selection::Region(r) => st
            .project
            .regions()
            .get(r)
            .and_then(|reg| st.project.track_index(&reg.track).map(|i| (i, Some((reg.start, reg.end))))),
        _ => None,
    };
    if let Some((i, range)) = lane {
        return describe_lane(st, jobs, bundle, i, range, text);
    }
    // Key and tempo anchor the style, as in the cover that worked.
    let mut style = text.to_string();
    if let Some(a) = &st.project.analysis {
        if let Some(k) = a.key.as_ref().filter(|k| !text.contains(k.as_str())) {
            style.push_str(&format!(", {k}"));
        }
        if let Some(bpm) = a.tempo.filter(|_| !text.contains("bpm")) {
            style.push_str(&format!(", {bpm:.0} bpm"));
        }
    }
    let (id, seed) = st.project.next_take();
    let take = Take {
        file: format!("takes/{id}.wav"),
        id,
        scope: "song".into(),
        start: 0.0,
        end: 0.0,
        engine: "yue2".into(),
        prompt: text.to_string(),
        seed,
        track: None,
        blend: None,
    };
    let n = take.id.trim_start_matches('t').to_string();
    let with_score = abc.is_some();
    let fixture = Yue2Fixture { style, lyrics, seed, cot: "full".into(), abc };
    if !jobs.submit(Job::Generate { bundle, take, fixture, sr: st.sr() }) {
        return "The job worker is not running".into();
    }
    st.analysing = Some((format!("Take {n}"), 0.0));
    // The take number is spent even if the render fails: seeds never repeat.
    let _ = st.save();
    if with_score {
        format!("Making take {n} from the song's score and lyrics (about a minute)...")
    } else {
        format!("Making take {n}; no score was found, so YuE2 writes its own (slower)...")
    }
}

/// Describe on a selected section: ACE-Step repaints that range of the song
/// (the source audio), keeping a second of original each side for the seam.
fn repaint_section(st: &mut EditorState, jobs: &Jobs, bundle: PathBuf, i: usize, text: &str, lyrics: String) -> String {
    let Some(section) = st.project.sections().get(i).cloned() else { return "No such section".into() };
    let Some(source) = st.project.source.clone() else { return "This project has no source audio to repaint".into() };
    let (id, seed) = st.project.next_take();
    let n = id.trim_start_matches('t').to_string();
    let take = Take {
        file: format!("takes/{id}.wav"),
        id,
        scope: "section".into(),
        start: section.start,
        end: section.end,
        engine: "acestep".into(),
        prompt: text.to_string(),
        seed,
        track: None,
        blend: None,
    };
    let a = st.project.analysis.clone().unwrap_or_default();
    let job = RepaintJob {
        start: section.start,
        end: section.end,
        margin: audio::TAKE_MARGIN_S,
        caption: text.to_string(),
        lyrics,
        seed,
        bpm: a.tempo.map(f64::round),
        key: a.key,
        ..Default::default()
    };
    if !jobs.submit(Job::Repaint { src: bundle.join(source), bundle, take, job, sr: st.sr() }) {
        return "The job worker is not running".into();
    }
    st.analysing = Some((format!("Take {n}"), 0.0));
    let _ = st.save();
    format!("Repainting the {} ({:.0}s - {:.0}s) as take {n} (under a minute)...", section.label, section.start, section.end)
}

/// Describe on a lane (or a region of it): less echo, or a new part added.
fn describe_lane(st: &mut EditorState, jobs: &Jobs, bundle: PathBuf, lane: usize, range: Option<(f64, f64)>, text: &str) -> String {
    let track = st.project.tracks[lane].clone();
    let lane_file = st.project.track_path(&st.path, lane);
    let intent = lane_intent(text, &track.id);
    // A replaced part comes in on the one: its range snaps to downbeats.
    let range = match (&intent, range) {
        (LaneIntent::Replace(_), Some((a, b))) => {
            let downbeats = st.project.analysis.as_ref().and_then(|a| a.downbeats.clone()).unwrap_or_default();
            Some(snap_to_downbeats(a, b, &downbeats))
        }
        _ => range,
    };
    let (start, end) = range.unwrap_or((0.0, st.project.duration));
    let place = range.map(|(a, b)| format!(" from {a:.1}s to {b:.1}s")).unwrap_or_default();
    match &intent {
        LaneIntent::AddWhat => {
            return format!("Which instrument should Poing add? Say it, e.g. \"add warm strings\" (it can play: {}).", LEGO_TRACKS.join(", "))
        }
        LaneIntent::Replace(None) => {
            return format!("Which instrument should {} become? Say it, e.g. \"warm rhodes keys\" (it can play: {}).", track.name, LEGO_TRACKS.join(", "))
        }
        LaneIntent::Replace(Some(i)) if i == "vocals" || i == "backing_vocals" => {
            return "Singing a part again needs its words: select the section and Describe it to repaint it.".into()
        }
        _ => {}
    }
    let (id, seed) = st.project.next_take();
    let n = id.trim_start_matches('t').to_string();
    let mut take = Take {
        file: format!("takes/{id}.wav"),
        id,
        scope: "lane".into(),
        start,
        end,
        engine: "dereverb".into(),
        prompt: text.to_string(),
        seed,
        track: Some(track.id.clone()),
        // dry + 35% of the echo: what worked for the disco v4 vocal
        blend: Some(0.65),
    };
    let (job, status) = match intent {
        LaneIntent::Replace(Some(instrument)) => {
            let Some(source) = st.project.source.clone() else { return "This song has no source audio to play along to".into() };
            take.engine = "lego".into();
            take.blend = Some(1.0);
            let a = st.project.analysis.clone().unwrap_or_default();
            let lego = LegoJob {
                src: bundle.join(source).to_string_lossy().into_owned(),
                track: instrument.clone(),
                caption: text.to_string(),
                seed,
                bpm: a.tempo.map(f64::round),
                key: a.key,
                ..Default::default()
            };
            let (a, b) = range.unwrap_or((0.0, 0.0));
            let clean = LayerJob {
                stem: demucs_stem_for(&instrument).to_string(),
                match_lane: Some(lane_file.to_string_lossy().into_owned()),
                start: a,
                end: b,
                ..Default::default()
            };
            let status = format!("Playing {} again as {instrument}{place}, take {n} (about a minute and a half)...", track.name);
            (Job::Layer { bundle, take, lego, clean, sr: st.sr() }, status)
        }
        LaneIntent::AddLayer(instrument) => {
            let Some(source) = st.project.source.clone() else { return "This song has no source audio to play along to".into() };
            take.scope = "layer".into();
            take.engine = "lego".into();
            take.blend = Some(1.0);
            let a = st.project.analysis.clone().unwrap_or_default();
            let stem = demucs_stem_for(&instrument).to_string();
            // A part of the lane's own kind fills that lane's gaps; anything else plays as is.
            let reference = (stem == track.id).then(|| lane_file.to_string_lossy().into_owned());
            let fills = reference.is_some();
            let lego = LegoJob {
                src: bundle.join(source).to_string_lossy().into_owned(),
                track: instrument.clone(),
                caption: text.to_string(),
                seed,
                bpm: a.tempo.map(f64::round),
                key: a.key,
                ..Default::default()
            };
            let (a, b) = range.unwrap_or((0.0, 0.0));
            let clean = LayerJob { stem, reference, start: a, end: b, ..Default::default() };
            let how = if fills { format!(", filling the gaps in {}", track.name) } else { String::new() };
            let status = format!("Adding a {instrument} part{place} as take {n}{how} (about a minute and a half)...");
            (Job::Layer { bundle, take, lego, clean, sr: st.sr() }, status)
        }
        _ => {
            let status =
                format!("Taking the echo out of {}{place} as take {n} (about 2 minutes). Then set how much echo stays with Blend.", track.name);
            (Job::Dereverb { bundle, take, lane: lane_file, sr: st.sr() }, status)
        }
    };
    if !jobs.submit(job) {
        return "The job worker is not running".into();
    }
    st.analysing = Some((format!("Take {n}"), 0.0));
    let _ = st.save();
    status
}

/// The rate stems are decoded at (the open song's; all songs use 48 kHz).
fn stem_sr(state: &Option<EditorState>) -> u32 {
    state.as_ref().map_or(48000, |st| st.sr())
}

/// Add a finished take to a song that is no longer open.
fn record_take(bundle: &Path, take: Take) -> Result<String, String> {
    let file = bundle.join("project.json");
    let mut p = Project::load(&file)?;
    p.takes_made = Some(p.takes_made.unwrap_or(0).max(take_number(&take.id)));
    p.takes_mut().push(take);
    p.save(&file)?;
    Ok(p.name)
}

fn take_number(id: &str) -> u64 {
    id.trim_start_matches('t').parse().unwrap_or(0)
}

/// Keep a song take: it becomes a new song next to this one
/// (`<name> take N.poing`), which is then analysed like any import.
fn promote_take(st: &EditorState) -> Result<PathBuf, String> {
    let (_, take) = st.song_take().ok_or("no song take is playing")?;
    let dir = st.path.parent().ok_or("project has no folder")?;
    let name = format!("{} take {}", st.project.name, take.id.trim_start_matches('t'));
    let bundle = dir.parent().unwrap_or(dir).join(format!("{name}.poing"));
    let project_file = bundle.join("project.json");
    if project_file.exists() {
        return Ok(project_file);
    }
    std::fs::create_dir_all(&bundle).map_err(|e| format!("create {}: {e}", bundle.display()))?;
    std::fs::copy(dir.join(&take.file), bundle.join("original.wav")).map_err(|e| e.to_string())?;
    // Hand-corrected lyrics still describe the new song.
    let _ = std::fs::copy(dir.join("lyrics.txt"), bundle.join("lyrics.txt"));
    Project::import(&name, "original.wav", take.end - take.start, st.sr()).save(&project_file)?;
    Ok(project_file)
}

const HELP: &str =
    "Click a track, section or region. Drag on a waveform to mark a region. Space = play/pause, Del = delete, Ctrl+wheel = zoom.";

impl App {
    fn load(&mut self, cx: &mut Cx) {
        self.open(cx, open_target());
    }

    /// Open a project or import a song. The audio output is installed once;
    /// later songs reuse its Shared and swap their stems in by generation.
    fn open(&mut self, cx: &mut Cx, target: Open) {
        let shared = self.state.as_ref().map(|st| st.shared.clone());
        let result = (|| -> Result<(EditorState, bool), String> {
            let path = match target {
                Open::Project(path) => path,
                Open::Audio(audio) => import(&audio, 48000)?.0,
            };
            let mut project = Project::load(&path)?;
            let analyse = project.source.is_some() && !project.is_analysed();
            let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
            // A take whose audio is gone is dropped rather than failing the song.
            project.takes_mut().retain(|t| dir.join(&t.file).exists());
            let mut stems: Vec<Stem> = Vec::new();
            for i in 0..project.tracks.len() {
                stems.push(audio::load_stem(&project.track_path(&path, i), project.sr())?);
            }
            for t in project.takes() {
                stems.push(audio::load_stem(&dir.join(&t.file), project.sr())?);
            }
            Ok((EditorState::new(project, path, stems, shared.clone()), analyse))
        })();
        match result {
            Ok((mut st, analyse)) => {
                self.ui.label(cx, ids!(project_label)).set_text(cx, &song_title(&st.project));
                if shared.is_none() {
                    audio::start_output(cx, st.shared.clone(), st.sr());
                }
                if analyse {
                    self.start_analysis(&mut st);
                }
                if let Some(old) = self.state.replace(st) {
                    // Its stem sets may still be in the audio thread's hands for a callback.
                    if let Some(st) = &mut self.state {
                        for set in old.into_retired() {
                            st.retire(set);
                        }
                    }
                }
            }
            Err(e) => {
                self.ui.label(cx, ids!(insp_title)).set_text(cx, &format!("Could not open: {e}"));
                log!("poing: load failed: {e}");
            }
        }
    }

    /// Understand the song in the background; lanes fill in when it lands.
    fn start_analysis(&self, st: &mut EditorState) {
        let (Some(jobs), Some(source)) = (&self.jobs, st.project.source.clone()) else { return };
        let bundle = st.path.parent().map(Path::to_path_buf).unwrap_or_default();
        if jobs.submit(Job::Analyze { audio: bundle.join(source), bundle, sr: st.sr() }) {
            st.analysing = Some(("Starting analysis".into(), 0.0));
        }
    }

    /// Show the inspector row for the current selection and fill its widgets.
    fn refresh_inspector(&mut self, cx: &mut Cx) {
        let Some(st) = &mut self.state else { return };
        if !st.sel_changed {
            return;
        }
        st.sel_changed = false;
        let ui = self.ui.clone();
        ui.button(cx, ids!(play_btn)).set_text(cx, if st.shared.playing() { "Pause" } else { "Play" });
        let (mut show_region, mut show_track, mut show_section, mut show_take) = (false, false, false, false);
        match st.sel {
            Selection::Region(i) if i < st.project.regions().len() => {
                let r = st.project.regions()[i].clone();
                let track = st.project.track_index(&r.track).map(|t| st.project.tracks[t].name.clone()).unwrap_or_default();
                ui.label(cx, ids!(insp_title)).set_text(
                    cx,
                    &format!("Region {} on {}   {:.2}s - {:.2}s ({:.2}s)", r.id, track, r.start, r.end, r.end - r.start),
                );
                ui.drop_down(cx, ids!(kind_dd)).set_selected_item(cx, kind_index(&r.kind));
                ui.slider(cx, ids!(region_gain)).set_value(cx, r.gain());
                ui.text_input(cx, ids!(note_input)).set_text(cx, r.note.as_deref().unwrap_or(""));
                show_region = true;
            }
            Selection::Track(i) if i < st.project.tracks.len() => {
                let t = st.project.tracks[i].clone();
                ui.label(cx, ids!(insp_title)).set_text(cx, &format!("Track: {}   ({})", t.name, t.file));
                ui.slider(cx, ids!(track_gain)).set_value(cx, t.gain());
                ui.check_box(cx, ids!(mute_cb)).set_active(cx, t.muted(), Animate::No);
                ui.check_box(cx, ids!(solo_cb)).set_active(cx, t.soloed(), Animate::No);
                show_track = true;
            }
            Selection::Section(i) if i < st.project.sections().len() => {
                let s = st.project.sections()[i].clone();
                ui.label(cx, ids!(insp_title)).set_text(cx, &format!("Section {}   {:.2}s - {:.2}s   ·   Describe above to repaint it", i + 1, s.start, s.end));
                ui.text_input(cx, ids!(section_input)).set_text(cx, &s.label);
                show_section = true;
            }
            _ => match st.focused_take() {
                Some(t) => {
                    let n = t.id.trim_start_matches('t');
                    let span = if t.scope == "song" {
                        let len = t.end - t.start;
                        format!("whole song, {}:{:02}", (len / 60.0) as u32, (len % 60.0) as u32)
                    } else {
                        format!("{:.0}s - {:.0}s", t.start, t.end)
                    };
                    let on = match (t.scope.as_str(), t.track.as_deref()) {
                        ("lane", Some(track)) => format!("  ·  replaces {track}"),
                        ("layer", Some(track)) => format!("  ·  added to {track}"),
                        _ => String::new(),
                    };
                    let title = format!("Take {n}  ·  {}{on}  ·  {span}  ·  seed {}", t.prompt, t.seed);
                    ui.label(cx, ids!(insp_title)).set_text(cx, &title);
                    let blends = t.scope == "lane" || t.scope == "layer";
                    ui.slider(cx, ids!(take_blend)).set_visible(cx, blends);
                    ui.slider(cx, ids!(take_blend)).set_value(cx, (t.blend() * 100.0).round());
                    let keep = match t.scope.as_str() {
                        "song" => "Keep as a new song",
                        "layer" => "Keep as a lane",
                        "lane" => "Keep in the lane",
                        _ => "",
                    };
                    ui.button(cx, ids!(keep_btn)).set_visible(cx, !keep.is_empty());
                    ui.button(cx, ids!(keep_btn)).set_text(cx, keep);
                    show_take = true;
                }
                None => ui.label(cx, ids!(insp_title)).set_text(cx, HELP),
            },
        }
        ui.view(cx, ids!(region_row)).set_visible(cx, show_region);
        ui.view(cx, ids!(track_row)).set_visible(cx, show_track);
        ui.view(cx, ids!(section_row)).set_visible(cx, show_section);
        ui.view(cx, ids!(take_row)).set_visible(cx, show_take);
        ui.widget(cx, ids!(timeline)).redraw(cx);
        ui.view(cx, ids!(inspector)).redraw(cx);
    }

    fn set_status(&mut self, cx: &mut Cx, s: &str) {
        self.ui.label(cx, ids!(status_label)).set_text(cx, s);
        self.status_at = Some(std::time::Instant::now());
    }

    fn drain_jobs(&mut self, cx: &mut Cx) {
        let mut updates = Vec::new();
        if let Some(jobs) = &self.jobs {
            while let Ok(u) = jobs.rx.try_recv() {
                updates.push(u);
            }
        }
        // Jobs belong to the song they were started for; the open song may have changed since.
        let open_bundle = self.state.as_ref().and_then(|st| st.path.parent().map(Path::to_path_buf));
        let mine = |b: &Path| open_bundle.as_deref() == Some(b);
        for u in updates {
            let status = match u {
                JobUpdate::Status(s) | JobUpdate::Done(s) => s,
                JobUpdate::Failed { bundle, message } => {
                    if let Some(st) = self.state.as_mut().filter(|_| mine(&bundle)) {
                        st.analysing = None;
                    }
                    message
                }
                JobUpdate::Progress { bundle, message, progress } => {
                    let Some(st) = self.state.as_mut().filter(|_| mine(&bundle)) else { continue };
                    let what = if message.starts_with("Take") { "" } else { "Analysing  ·  " };
                    let line = format!("{what}{message}  {:.0}%", progress * 100.0);
                    st.analysing = Some((message, progress));
                    line
                }
                JobUpdate::TakeReady { bundle, mut take, stem } => {
                    let n = take.id.trim_start_matches('t').to_string();
                    take.end = take.start + stem.l.len() as f64 / stem_sr(&self.state) as f64;
                    match self.state.as_mut().filter(|_| mine(&bundle)) {
                        Some(st) => match st.add_take(take, stem) {
                            Ok(()) => format!("Take {n} is ready: playing it now. Click Original above the lanes to compare."),
                            Err(e) => format!("Take {n} could not be added: {e}"),
                        },
                        // Its song is closed: record the take there for next time.
                        None => match record_take(&bundle, take) {
                            Ok(name) => format!("Take {n} of {name} is ready; it is there when you open that song."),
                            Err(e) => format!("Take {n} could not be recorded: {e}"),
                        },
                    }
                }
                JobUpdate::Analyzed { bundle, stems } => {
                    let Some(st) = self.state.as_mut().filter(|_| mine(&bundle)) else { continue };
                    let file = bundle.join("analysis.json");
                    let result = std::fs::read_to_string(&file)
                        .map_err(|e| format!("read {}: {e}", file.display()))
                        .and_then(|t| Analysis::deserialize_json_lenient(&t).map_err(|e| format!("parse {}: {e:?}", file.display())))
                        .and_then(|a| st.apply_analysis(a, stems));
                    match result {
                        Ok(()) => {
                            self.ui.label(cx, ids!(project_label)).set_text(cx, &song_title(&st.project));
                            let a = st.project.analysis.as_ref();
                            format!(
                                "Understood: {} sections, {} chords, {} lyric lines",
                                st.project.sections().len(),
                                a.and_then(|a| a.chords.as_ref()).map_or(0, |c| c.len()),
                                a.and_then(|a| a.lyrics.as_ref()).map_or(0, |l| l.len()),
                            )
                        }
                        Err(e) => {
                            st.analysing = None;
                            format!("Analysis could not be applied: {e}")
                        }
                    }
                }
            };
            self.set_status(cx, &status);
        }
        self.ui.widget(cx, ids!(timeline)).redraw(cx);
    }
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        self.jobs = Some(Jobs::start(cx));
        self.load(cx);
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let ui = self.ui.clone();
        let Some(st) = &mut self.state else { return };
        let mut status: Option<String> = None;

        if ui.button(cx, ids!(play_btn)).clicked(actions) {
            st.toggle_play();
        }
        if ui.button(cx, ids!(start_btn)).clicked(actions) {
            st.seek(0.0);
            st.scroll_t = 0.0;
        }
        let zoom = |st: &mut EditorState, f: f64| {
            let center = st.playhead();
            st.pps = (st.pps * f).clamp(1.0, 400.0);
            st.scroll_t = (center - 2.0).max(0.0);
        };
        if ui.button(cx, ids!(zoom_in_btn)).clicked(actions) {
            zoom(st, 1.6);
        }
        if ui.button(cx, ids!(zoom_out_btn)).clicked(actions) {
            zoom(st, 1.0 / 1.6);
        }
        if ui.button(cx, ids!(fit_btn)).clicked(actions) {
            st.pps = 0.0;
            st.scroll_t = 0.0;
        }
        if ui.button(cx, ids!(split_btn)).clicked(actions) {
            let t = st.playhead();
            st.split_section_at(t);
        }
        if ui.button(cx, ids!(save_btn)).clicked(actions) {
            status = Some(match st.save() {
                Ok(()) => format!("Saved {} ({} regions)", st.path.display(), st.project.regions().len()),
                Err(e) => format!("Save failed: {e}"),
            });
        }
        if ui.button(cx, ids!(render_btn)).clicked(actions) {
            let out_dir = st.path.parent().map(|p| p.join("renders")).unwrap_or_else(|| PathBuf::from("renders"));
            let job = Job::Render { stems: st.stems.clone(), mix: st.mix_snapshot(), sr: st.sr(), out_dir, name: st.project.name.clone() };
            let queued = self.jobs.as_ref().map(|j| j.submit(job)).unwrap_or(false);
            status = Some(if queued { "Rendering...".into() } else { "Render worker is not running".into() });
        }

        if let Some((text, _)) = ui.text_input(cx, ids!(describe_input)).returned(actions) {
            if !text.trim().is_empty() {
                status = Some(describe(st, self.jobs.as_ref(), text.trim()));
            }
        }
        if ui.button(cx, ids!(delete_take_btn)).clicked(actions) {
            if let Some(id) = st.focus.clone() {
                status = Some(match st.delete_take(&id) {
                    Ok(()) => "Take deleted".into(),
                    Err(e) => format!("Delete failed: {e}"),
                });
            }
        }
        if ui.button(cx, ids!(keep_btn)).clicked(actions) && st.analysing.is_some() {
            status = Some("A job is still running; keep the take when it is done.".into());
        } else if ui.button(cx, ids!(keep_btn)).clicked(actions) && st.focused_take().is_some_and(|t| t.scope != "song") {
            let id = st.focus.clone().unwrap_or_default();
            status = Some(st.keep_take(&id).unwrap_or_else(|e| format!("Keep failed: {e}")));
        } else if ui.button(cx, ids!(keep_btn)).clicked(actions) {
            match promote_take(st) {
                Ok(project) => {
                    self.set_status(cx, "Opening the take as a new song; Poing is listening to it...");
                    self.open(cx, Open::Project(project));
                    self.refresh_inspector(cx);
                    return;
                }
                Err(e) => status = Some(format!("Keep failed: {e}")),
            }
        }

        if let Some(v) = ui.slider(cx, ids!(take_blend)).slided(actions) {
            if let Some(id) = st.focus.clone() {
                st.set_blend(&id, v / 100.0);
            }
        }

        if let Selection::Region(i) = st.sel {
            if i < st.project.regions().len() {
                if let Some(k) = ui.drop_down(cx, ids!(kind_dd)).selected(actions) {
                    let kind = REGION_KINDS[k].to_string();
                    let r = &mut st.project.regions_mut()[i];
                    r.gain_db = Some(default_gain_for(&kind));
                    r.kind = kind;
                    st.push_mix();
                    st.sel_changed = true;
                }
                if let Some(v) = ui.slider(cx, ids!(region_gain)).slided(actions) {
                    let r = &mut st.project.regions_mut()[i];
                    r.gain_db = Some(v);
                    if r.kind == "bad" || r.kind == "regenerate" {
                        r.kind = "gain".into();
                        st.sel_changed = true;
                    }
                    st.push_mix();
                }
                if let Some(text) = ui.text_input(cx, ids!(note_input)).changed(actions) {
                    st.project.regions_mut()[i].note = Some(text);
                    st.dirty = true;
                }
                if ui.button(cx, ids!(delete_btn)).clicked(actions) {
                    st.delete_selected();
                }
            }
        }
        if let Selection::Track(i) = st.sel {
            if let Some(v) = ui.slider(cx, ids!(track_gain)).slided(actions) {
                st.project.tracks[i].gain_db = Some(v);
                st.push_mix();
            }
            if let Some(v) = ui.check_box(cx, ids!(mute_cb)).changed(actions) {
                st.project.tracks[i].mute = Some(v);
                st.push_mix();
            }
            if let Some(v) = ui.check_box(cx, ids!(solo_cb)).changed(actions) {
                st.project.tracks[i].solo = Some(v);
                st.push_mix();
            }
        }
        if let Selection::Section(i) = st.sel {
            if let Some(text) = ui.text_input(cx, ids!(section_input)).changed(actions) {
                if let Some(s) = st.project.sections_mut().get_mut(i) {
                    s.label = text;
                    st.dirty = true;
                }
            }
            if ui.button(cx, ids!(merge_btn)).clicked(actions) {
                st.delete_selected();
            }
        }

        let recent = self.status_at.is_some_and(|t| t.elapsed().as_secs_f64() < 6.0);
        if st.dirty && status.is_none() && !recent {
            self.ui.label(cx, ids!(status_label)).set_text(cx, "Unsaved changes");
        }
        if let Some(s) = status {
            log!("poing: {s}");
            self.set_status(cx, &s);
        }
        ui.widget(cx, ids!(timeline)).redraw(cx);
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        crate::timeline::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if let Event::Signal = event {
            self.drain_jobs(cx);
        }
        self.match_event(cx, event);
        match &mut self.state {
            Some(st) => {
                st.collect_retired();
                self.ui.handle_event(cx, event, &mut Scope::with_data(st))
            }
            None => self.ui.handle_event(cx, event, &mut Scope::empty()),
        }
        self.refresh_inspector(cx);
    }
}
