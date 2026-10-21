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

use audio::{Job, JobUpdate, Jobs, Stem};
use makepad_widgets::*;
use makepad_micro_serde::DeJson;
use model::{default_gain_for, Analysis, Project, REGION_KINDS};
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

const HELP: &str =
    "Click a track, section or region. Drag on a waveform to mark a region. Space = play/pause, Del = delete, Ctrl+wheel = zoom.";

impl App {
    fn load(&mut self, cx: &mut Cx) {
        let result = (|| -> Result<(EditorState, bool), String> {
            let (path, analyse) = match open_target() {
                Open::Project(path) => (path, false),
                Open::Audio(audio) => import(&audio, 48000)?,
            };
            let project = Project::load(&path)?;
            let mut stems: Vec<Stem> = Vec::new();
            for i in 0..project.tracks.len() {
                stems.push(audio::load_stem(&project.track_path(&path, i), project.sr())?);
            }
            Ok((EditorState::new(project, path, stems), analyse))
        })();
        match result {
            Ok((mut st, analyse)) => {
                self.ui.label(cx, ids!(project_label)).set_text(cx, &song_title(&st.project));
                audio::start_output(cx, st.shared.clone(), st.sr());
                if analyse {
                    self.start_analysis(&mut st);
                }
                self.state = Some(st);
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
        let (mut show_region, mut show_track, mut show_section) = (false, false, false);
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
                ui.label(cx, ids!(insp_title)).set_text(cx, &format!("Section {}   {:.2}s - {:.2}s", i + 1, s.start, s.end));
                ui.text_input(cx, ids!(section_input)).set_text(cx, &s.label);
                show_section = true;
            }
            _ => ui.label(cx, ids!(insp_title)).set_text(cx, HELP),
        }
        ui.view(cx, ids!(region_row)).set_visible(cx, show_region);
        ui.view(cx, ids!(track_row)).set_visible(cx, show_track);
        ui.view(cx, ids!(section_row)).set_visible(cx, show_section);
        ui.widget(cx, ids!(timeline)).redraw(cx);
        ui.view(cx, ids!(inspector)).redraw(cx);
    }

    fn set_status(&self, cx: &mut Cx, s: &str) {
        self.ui.label(cx, ids!(status_label)).set_text(cx, s);
    }

    fn drain_jobs(&mut self, cx: &mut Cx) {
        let mut updates = Vec::new();
        if let Some(jobs) = &self.jobs {
            while let Ok(u) = jobs.rx.try_recv() {
                updates.push(u);
            }
        }
        for u in updates {
            let status = match u {
                JobUpdate::Status(s) => s,
                JobUpdate::Done(s) => {
                    if let Some(st) = &mut self.state {
                        st.analysing = None;
                    }
                    s
                }
                JobUpdate::Progress { message, progress } => {
                    let line = format!("Analysing  ·  {message}  {:.0}%", progress * 100.0);
                    if let Some(st) = &mut self.state {
                        st.analysing = Some((message, progress));
                    }
                    line
                }
                JobUpdate::Analyzed { bundle, stems } => {
                    let Some(st) = &mut self.state else { continue };
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

        if st.dirty && status.is_none() {
            status = Some("Unsaved changes".into());
        }
        if let Some(s) = status {
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
            Some(st) => self.ui.handle_event(cx, event, &mut Scope::with_data(st)),
            None => self.ui.handle_event(cx, event, &mut Scope::empty()),
        }
        self.refresh_inspector(cx);
    }
}
