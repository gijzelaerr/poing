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
use model::{default_gain_for, Project, REGION_KINDS};
use state::{EditorState, Selection};
use std::path::PathBuf;
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

fn project_path() -> PathBuf {
    std::env::args()
        .skip(1)
        .find(|a| a.ends_with(".json"))
        .or_else(|| std::env::var("POING_PROJECT").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("project.json"))
}

const HELP: &str =
    "Click a track, section or region. Drag on a waveform to mark a region. Space = play/pause, Del = delete, Ctrl+wheel = zoom.";

impl App {
    fn load(&mut self, cx: &mut Cx) {
        let path = project_path();
        let result = (|| -> Result<EditorState, String> {
            let project = Project::load(&path)?;
            let mut stems: Vec<Stem> = Vec::new();
            for i in 0..project.tracks.len() {
                stems.push(audio::load_stem(&project.track_path(&path, i), project.sr())?);
            }
            Ok(EditorState::new(project, path.clone(), stems))
        })();
        match result {
            Ok(st) => {
                self.ui.label(cx, ids!(project_label)).set_text(cx, &format!("{}   ({})", st.project.name, st.path.display()));
                audio::start_output(cx, st.stems.clone(), st.shared.clone(), st.sr());
                self.state = Some(st);
            }
            Err(e) => {
                self.ui.label(cx, ids!(insp_title)).set_text(cx, &format!("Could not open project: {e}"));
                log!("daw: load failed: {e}");
            }
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
        let mut last = None;
        if let Some(jobs) = &self.jobs {
            while let Ok(u) = jobs.rx.try_recv() {
                last = Some(match u {
                    JobUpdate::Status(s) | JobUpdate::Done(s) => s,
                });
            }
        }
        if let Some(s) = last {
            self.set_status(cx, &s);
        }
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
