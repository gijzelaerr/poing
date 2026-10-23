//! Poing: a song's stems as lanes, its sections, and marked regions (bad /
//! regenerate / level fixes with notes for the AI). Edits play live; Save writes
//! project.json for the AI pipeline, Render writes a WAV and a mastered MP3.
//!
//!   poing path/to/project.json      (or POING_PROJECT)
//!   POING_MUTE=1 keeps agent/test instances silent.

pub use makepad_widgets;

mod audio;
mod fx;
mod fx_writer;
mod plugins;
mod synth;
mod midi_writer;
mod song_writer;
pub(crate) use poing_model as model;
mod state;
mod timeline;

use audio::{Job, JobUpdate, Jobs, LayerJob, LegoJob, RepaintJob, SongJob, Stem, Yue2Fixture};
use makepad_widgets::*;
use makepad_micro_serde::DeJson;
use model::{default_gain_for, demucs_stem_for, is_master_effect_text, lane_intent, snap_to_downbeats, Analysis, LaneIntent, Project, Take, LEGO_TRACKS, REGION_KINDS};
use state::{EditorState, Selection};
use std::path::{Path, PathBuf};
use timeline::kind_index;

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*

    // Poing's look: near-black surfaces, one blue accent, few controls.
    let ACCENT = #x0a84ff
    let TEXT = #xeceef2
    let TEXT_2 = #x8d939e
    let SURFACE = #x15171c
    let FIELD = #x1f2229
    let HAIRLINE = #x2a2e36

    // A round icon button (play/pause).
    let RoundIconButton = ButtonFlatterIcon{
        width: 34 height: 34
        padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
        margin: Inset{left: 0 right: 0 top: 0 bottom: 0}
        align: Align{x: 0.5 y: 0.5}
        icon_walk: Walk{width: 14 height: 14}
        draw_icon +: {color: #xffffff}
        draw_bg +: {
            border_radius: 17.0
            border_size: 0.0
            color: ACCENT
            color_hover: #x3399ff
            color_down: #x0066cc
            color_focus: ACCENT
        }
    }

    // A quiet pill button with a label.
    let PillButton = ButtonFlatter{
        height: 28
        padding: Inset{left: 12 right: 12 top: 0 bottom: 0}
        align: Align{x: 0.5 y: 0.5}
        draw_text +: {
            color: TEXT
            color_hover: #xffffff
            text_style: theme.font_regular{font_size: 9.5}
        }
        draw_bg +: {
            border_radius: 14.0
            border_size: 1.0
            border_color: HAIRLINE
            border_color_hover: #x3a3f49
            color: FIELD
            color_hover: #x272b33
            color_down: #x30353f
            color_focus: FIELD
        }
    }

    let Caption = Label{
        draw_text +: {color: TEXT_2 text_style: theme.font_regular{font_size: 9}}
    }

    // The start screen's three ways in.
    let HomeCard = RoundedView{
        width: 300 height: 240 flow: Down spacing: 10
        padding: Inset{left: 18 right: 18 top: 18 bottom: 18}
        show_bg: true
        draw_bg +: {color: #x181a20 border_radius: 12.0 border_size: 1.0 border_color: HAIRLINE}
    }
    let CardTitle = Label{
        draw_text +: {color: TEXT text_style: theme.font_bold{font_size: 13}}
    }
    let CardText = Label{
        width: Fill
        draw_text +: {color: TEXT_2 text_style: theme.font_regular{font_size: 9.5}}
    }
    let PromptFrame = RoundedView{
        width: Fill height: 34
        padding: Inset{left: 12 right: 6 top: 0 bottom: 0}
        show_bg: true
        draw_bg +: {color: FIELD border_radius: 17.0 border_size: 1.0 border_color: HAIRLINE}
    }
    let PromptInput = TextInputFlat{
        width: Fill height: 34
        padding: Inset{left: 2 right: 4 top: 8.2 bottom: 9.8}
        margin: Inset{left: 0 right: 0 top: 0 bottom: 0}
        draw_bg +: {
            border_radius: 0.0 border_size: 0.0
            color: #00000000 color_hover: #00000000 color_focus: #00000000
            color_down: #00000000 color_empty: #00000000
            border_color: #00000000 border_color_hover: #00000000
            border_color_focus: #00000000 border_color_down: #00000000
            border_color_empty: #00000000
        }
        draw_cursor +: {color: ACCENT}
        draw_text +: {
            color: TEXT color_hover: TEXT color_focus: #xffffff
            color_empty: #x6b717c color_empty_hover: #x7b818c
            text_style: theme.font_regular{font_size: 10}
        }
    }

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Poing"
                window.inner_size: vec2(1500, 900)
                pass +: {clear_color: #x0f1013}
                caption_bar +: {
                    draw_bg.color: #x121317
                    caption_label +: {label +: {draw_text +: {color: #x8d939e}}}
                }
                body +: {
                    flow: Down
                    show_bg: true
                    draw_bg +: {color: #x0f1013}

                    topbar := View{
                        width: Fill height: 56 flow: Right spacing: 14
                        padding: Inset{left: 14 right: 14 top: 11 bottom: 11}
                        show_bg: true
                        draw_bg +: {color: #x121317}
                        play_btn := RoundIconButton{
                            draw_icon.svg: crate_resource("self:resources/icons/play.svg")
                        }
                        pause_btn := RoundIconButton{
                            visible: false
                            draw_icon.svg: crate_resource("self:resources/icons/pause.svg")
                        }
                        title_col := View{
                            width: Fit height: Fill flow: Right spacing: 10
                            padding: Inset{left: 0 right: 6 top: 8 bottom: 0}
                            project_label := Label{
                                text: "Poing"
                                draw_text +: {color: TEXT text_style: theme.font_bold{font_size: 11}}
                            }
                            project_meta := Caption{text: ""}
                        }
                        describe_frame := RoundedView{
                            width: Fill height: 34 flow: Right spacing: 8
                            padding: Inset{left: 14 right: 6 top: 0 bottom: 0}
                            show_bg: true
                            draw_bg +: {color: FIELD border_radius: 17.0 border_size: 1.0 border_color: HAIRLINE}
                            Icon{
                                margin: Inset{top: 10 bottom: 0 left: 0 right: 0}
                                icon_walk: Walk{width: 13 height: 13}
                                draw_icon +: {
                                    svg: crate_resource("self:resources/icons/sparkle.svg")
                                    color: ACCENT
                                }
                            }
                            describe_input := TextInputFlat{
                                width: Fill height: 34
                                empty_text: "Describe a change. Select a lane, section or region first to change just that part."
                                padding: Inset{left: 2 right: 4 top: 8.2 bottom: 9.8}
                                margin: Inset{left: 0 right: 0 top: 0 bottom: 0}
                                draw_bg +: {
                                    border_radius: 0.0 border_size: 0.0
                                    color: #00000000 color_hover: #00000000 color_focus: #00000000
                                    color_down: #00000000 color_empty: #00000000
                                    border_color: #00000000 border_color_hover: #00000000
                                    border_color_focus: #00000000 border_color_down: #00000000
                                    border_color_empty: #00000000
                                }
                                draw_cursor +: {color: ACCENT}
                                draw_text +: {
                                    color: TEXT color_hover: TEXT color_focus: #xffffff
                                    color_empty: #x6b717c color_empty_hover: #x7b818c
                                    text_style: theme.font_regular{font_size: 10.5}
                                }
                            }
                        }
                        songs_btn := PillButton{
                            margin: Inset{top: 3 bottom: 0 left: 0 right: 0}
                            text: "Songs"
                        }
                        render_btn := PillButton{
                            margin: Inset{top: 3 bottom: 0 left: 0 right: 0}
                            text: "Export"
                        }
                    }
                    View{width: Fill height: 1 show_bg: true draw_bg +: {color: HAIRLINE}}

                    // The start screen: shown until a song is open.
                    home := View{
                        visible: false
                        width: Fill height: Fill flow: Down spacing: 14
                        align: Align{x: 0.5 y: 0.3}
                        Label{
                            text: "Start a song"
                            draw_text +: {color: TEXT text_style: theme.font_bold{font_size: 22}}
                        }
                        home_cards := View{
                            width: Fit height: Fit flow: Right spacing: 16
                            margin: Inset{top: 6 bottom: 0 left: 0 right: 0}
                            split_card := HomeCard{
                                CardTitle{text: "Split a song"}
                                CardText{text: "Drop an MP3, WAV, FLAC or OGG on this card. Poing splits it into parts and finds its sections, chords and words."}
                                View{width: Fill height: Fill}
                                Caption{text: "Drop a song here"}
                            }
                            remix_card := HomeCard{
                                CardTitle{text: "Remix a song"}
                                CardText{text: "Say how, then drop the song on this card. Poing makes three remixes to compare, and you can mix them per section."}
                                View{width: Fill height: Fill}
                                PromptFrame{
                                    remix_input := PromptInput{empty_text: "e.g. 80s disco, Dutch female vocals"}
                                }
                            }
                            new_card := HomeCard{
                                CardTitle{text: "Make a new song"}
                                CardText{text: "Say what it is about and how it sounds. Claude writes the words, ACE-Step sings three versions, you pick one."}
                                View{width: Fill height: Fill}
                                PromptFrame{
                                    new_input := PromptInput{empty_text: "e.g. a summer song about the farm"}
                                }
                                make_btn := PillButton{text: "Make it"}
                            }
                        }
                        home_status := Label{
                            text: ""
                            draw_text +: {color: TEXT_2 text_style: theme.font_regular{font_size: 9.5}}
                        }
                        home_error := Label{
                            text: ""
                            draw_text +: {color: #xff6961 text_style: theme.font_regular{font_size: 9.5}}
                        }
                        recent_box := View{
                            width: Fit height: Fit flow: Down spacing: 6
                            margin: Inset{top: 18 bottom: 0 left: 0 right: 0}
                            align: Align{x: 0.5 y: 0.0}
                            Caption{text: "Recent"}
                            rec0 := PillButton{text: ""}
                            rec1 := PillButton{text: ""}
                            rec2 := PillButton{text: ""}
                            rec3 := PillButton{text: ""}
                            rec4 := PillButton{text: ""}
                            rec5 := PillButton{text: ""}
                            rec6 := PillButton{text: ""}
                            rec7 := PillButton{text: ""}
                        }
                    }

                    timeline_box := View{
                        width: Fill height: Fill flow: Down
                        timeline := mod.widgets.DawTimeline{}
                    }

                    inspector_box := View{
                    width: Fill height: Fit flow: Down
                    View{width: Fill height: 1 show_bg: true draw_bg +: {color: HAIRLINE}}
                    inspector := View{
                        width: Fill height: Fit flow: Down spacing: 10
                        padding: Inset{left: 18 right: 18 top: 12 bottom: 12}
                        show_bg: true
                        draw_bg +: {color: #x15171c}
                        insp_title := Label{
                            text: "Loading..."
                            draw_text +: {color: TEXT text_style: theme.font_bold{font_size: 10}}
                        }
                        region_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            kind_dd := DropDown{width: 140 labels: ["bad" "regenerate" "too_loud" "too_quiet" "mute" "gain"]}
                            region_gain := Slider{width: 260 text: "Gain dB" min: -24.0 max: 12.0 step: 0.5 precision: 1 default: 0.0}
                            note_input := TextInput{width: 380 empty_text: "Note for the AI: what is wrong, what to change"}
                            delete_btn := PillButton{text: "Delete region"}
                        }
                        track_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            track_gain := Slider{width: 300 text: "Level dB" min: -36.0 max: 12.0 step: 0.5 precision: 1 default: 0.0}
                            mute_cb := CheckBox{text: "Mute"}
                            solo_cb := CheckBox{text: "Solo"}
                        }
                        section_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            section_input := TextInput{width: 200 empty_text: "section name"}
                            merge_btn := PillButton{text: "Merge into previous"}
                            use_remix_btn := PillButton{text: "Use the playing remix here"}
                        }
                        take_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            take_blend_box := View{width: Fit height: Fit take_blend := Slider{width: 300 text: "Blend %" min: 0.0 max: 100.0 step: 1.0 precision: 0 default: 100.0}}
                            keep_btn := PillButton{text: "Keep as a new song"}
                            delete_take_btn := PillButton{text: "Delete take"}
                        }
                        fx_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 12
                            align: Align{y: 0.5}
                            fx_b0 := View{width: Fit height: Fit fx_k0 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_b1 := View{width: Fit height: Fit fx_k1 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_b2 := View{width: Fit height: Fit fx_k2 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_b3 := View{width: Fit height: Fit fx_k3 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_b4 := View{width: Fit height: Fit fx_k4 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_b5 := View{width: Fit height: Fit fx_k5 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_bypass := CheckBox{text: "Bypass"}
                            fx_remove := PillButton{text: "Remove"}
                        }
                        status_label := Caption{text: ""}
                    }
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
    /// Autosave: fires a moment after the last change (no Save button).
    #[rust]
    autosave: Timer,
    /// The songs the start screen lists, in button order.
    #[rust]
    recent: Vec<PathBuf>,
    /// The start screen is showing (also while a song stays open behind it).
    #[rust]
    home_shown: bool,
}

/// Audio Poing can import (what makepad-audio-decode reads).
const AUDIO_EXT: [&str; 4] = ["mp3", "wav", "flac", "ogg"];

/// What the command line asks to open: a project, or a song to import.
enum Open {
    Project(PathBuf),
    Audio(PathBuf),
}

/// Is a text field (or a slider's typed value) taking the keyboard? Then
/// keys are text; otherwise they are shortcuts (a clicked button keeps the
/// focus but does not want keys).
fn typing(ui: &WidgetRef, cx: &Cx) -> bool {
    let focus = cx.key_focus();
    if focus.is_empty() {
        return false;
    }
    let fields = [ids!(describe_input), ids!(note_input), ids!(section_input), ids!(remix_input), ids!(new_input)];
    if fields.iter().any(|id| ui.widget(cx, *id).area() == focus) {
        return true;
    }
    let sliders = [ids!(track_gain), ids!(region_gain), ids!(take_blend), ids!(fx_k0), ids!(fx_k1), ids!(fx_k2), ids!(fx_k3), ids!(fx_k4), ids!(fx_k5)];
    sliders.iter().any(|id| ui.slider(cx, *id).has_text_focus(cx))
}

/// What a path handed to Poing (an argument or a drop) opens: a song to
/// import, a `.poing` bundle or a project file.
fn target_for(path: &Path) -> Option<Open> {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    if AUDIO_EXT.contains(&ext.as_str()) {
        Some(Open::Audio(path.to_path_buf()))
    } else if ext == "poing" || path.join("project.json").exists() {
        Some(Open::Project(path.join("project.json")))
    } else if ext == "json" {
        Some(Open::Project(path.to_path_buf()))
    } else {
        None
    }
}

/// What the command line (or POING_PROJECT) asks to open; None shows the start screen.
fn open_target() -> Option<Open> {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).collect();
    args.iter()
        .find_map(|a| target_for(Path::new(a)))
        .or_else(|| std::env::var("POING_PROJECT").ok().and_then(|p| target_for(Path::new(&p))))
}

/// Poing's own settings folder (the recent songs list lives there).
fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join(if cfg!(target_os = "linux") { "poing" } else { "Poing" }))
}

/// Songs opened before, newest first (project files that still exist).
fn recent_songs() -> Vec<PathBuf> {
    let Some(file) = config_dir().map(|d| d.join("recent.txt")) else { return Vec::new() };
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .take(8)
        .collect()
}

fn remember_song(project: &Path) {
    let Some(dir) = config_dir() else { return };
    let plain = |p: PathBuf| match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        // Windows canonical paths carry a verbatim prefix; keep the plain form
        Some(s) => PathBuf::from(s),
        None => p,
    };
    let project = plain(std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf()));
    let mut list = vec![project.clone()];
    list.extend(recent_songs().into_iter().map(plain).filter(|p| plain(std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())) != project));
    let text: Vec<String> = list.iter().take(8).map(|p| p.to_string_lossy().into_owned()).collect();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("recent.txt"), text.join("\n"));
}

/// A song's name for the start screen: its project's name, else its folder's.
fn song_name(project: &Path) -> String {
    Project::load(project).map(|p| p.name).unwrap_or_else(|_| {
        project.parent().and_then(|d| d.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    })
}

/// An analysis that finished on disk but never reached the project (the
/// app moved to another song meanwhile): its result and decoded stems.
fn finished_analysis(bundle: &Path, sr: u32) -> Option<(Analysis, Vec<(String, String, Stem)>)> {
    let text = std::fs::read_to_string(bundle.join("analysis.json")).ok()?;
    let analysis = Analysis::deserialize_json_lenient(&text).ok()?;
    let mut stems = Vec::new();
    for (id, _) in model::STEMS {
        let file = format!("stems/{id}.wav");
        stems.push((id.to_string(), file.clone(), audio::load_stem(&bundle.join(&file), sr).ok()?));
    }
    Some((analysis, stems))
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

/// "E minor · 125 bpm · 2:14": what the analysis found, under the title.
fn song_meta(p: &Project) -> String {
    let mut parts = Vec::new();
    if let Some(a) = &p.analysis {
        if let Some(k) = &a.key {
            parts.push(k.clone());
        }
        if let Some(bpm) = a.tempo {
            parts.push(format!("{bpm:.0} bpm"));
        }
    }
    parts.push(format!("{}:{:02}", (p.duration / 60.0) as u32, (p.duration % 60.0) as u32));
    parts.join("  ·  ")
}

fn show_title(ui: &WidgetRef, cx: &mut Cx, p: &Project) {
    ui.label(cx, ids!(project_label)).set_text(cx, &p.name);
    ui.label(cx, ids!(project_meta)).set_text(cx, &song_meta(p));
}

/// The remix asked for when the song was dropped (once it is understood):
/// three ACE-Step covers, close to free, each a song take.
fn start_remix(st: &mut EditorState, jobs: &Jobs) -> String {
    let Some(prompt) = st.pending_remix.take() else { return String::new() };
    let bundle = st.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let Some(source) = st.project.source.clone() else { return "This song has no audio to remix".into() };
    let a = st.project.analysis.clone().unwrap_or_default();
    let lyrics = std::fs::read_to_string(bundle.join("lyrics.txt")).unwrap_or_else(|_| st.project.sung_lyrics()).replace("\r\n", "\n");
    let how = [(0.6, "close"), (0.45, "looser"), (0.3, "free")];
    // numbered on from earlier remixes of this song
    let before = st.project.takes().iter().filter(|t| t.engine == "acestep-cover").count();
    let mut takes = Vec::new();
    for (k, (_, name)) in how.iter().enumerate() {
        let (id, seed) = st.project.next_take();
        takes.push(Take {
            file: format!("takes/{id}.wav"),
            id,
            scope: "song".into(),
            start: 0.0,
            end: 0.0,
            engine: "acestep-cover".into(),
            prompt: format!("Remix {} · {name}", before + k + 1),
            seed,
            track: None,
            blend: None,
        });
    }
    let job = SongJob {
        task: "cover".into(),
        caption: prompt.clone(),
        lyrics,
        bpm: a.tempo.map(f64::round),
        key: a.key,
        src: Some(bundle.join(source).to_string_lossy().into_owned()),
        strengths: Some(how.iter().map(|(s, _)| *s).collect()),
        seeds: Some(takes.iter().map(|t| t.seed).collect()),
        ..Default::default()
    };
    if !jobs.submit(Job::Remix { bundle, takes, job, sr: st.sr() }) {
        return "The job worker is not running".into();
    }
    st.analysing = Some(("Remix".into(), 0.0));
    let _ = st.save();
    format!("Making three remixes as \"{prompt}\" (close, looser, free; a minute or two)...")
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
    // A MIDI part ("add a funky bassline", "... played by Serum"), over the selected section or the song
    if let (Some(role), Selection::None | Selection::Section(_)) = (model::midi_part(text), st.sel) {
        st.next_instrument = named_plugin(st, text);
        return write_midi(st, jobs, bundle, role, text);
    }
    // An installed plugin named with nothing selected: on the master.
    if matches!(st.sel, Selection::None | Selection::Section(_)) {
        if let Some(result) = add_named_plugin(st, None, text) {
            return result;
        }
    }
    // An effect on the whole mix ("... on the master")
    if is_master_effect_text(text) && !matches!(st.sel, Selection::Track(_) | Selection::Region(_)) {
        return write_effect(st, jobs, bundle, None, text);
    }
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
    // Without native YuE2 here, the whole song is remixed with ACE-Step instead.
    if !audio::yue2_available() {
        st.pending_remix = Some(text.to_string());
        return start_remix(st, jobs);
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

/// Describe that names an installed plugin ("Raum on the vocals"): put it on
/// the lane, or the master. None when no plugin is named.
/// An installed plugin the text names: (name, bundle path).
fn named_plugin(st: &EditorState, text: &str) -> Option<(String, PathBuf)> {
    let names: Vec<String> = st.plugin_library.iter().map(|(n, _)| n.clone()).collect();
    let name = model::plugin_named(text, &names)?.clone();
    let path = st.plugin_library.iter().find(|(n, _)| *n == name)?.1.clone();
    Some((name, path))
}

fn add_named_plugin(st: &mut EditorState, lane: Option<usize>, text: &str) -> Option<String> {
    let (name, path) = named_plugin(st, text)?;
    // on a MIDI lane an instrument plays its notes; an effect goes on as usual
    if let Some(l) = lane.filter(|&l| st.project.tracks[l].is_midi()) {
        match st.set_instrument_plugin(l, &name, &path, Some(text.to_string())) {
            Ok(()) => return Some(format!("{name} now plays {}.", st.project.tracks[l].name)),
            Err(e) if !e.contains("not an instrument") => return Some(format!("{name} could not play it: {e}")),
            Err(_) => {}
        }
    }
    let place = lane.map(|l| st.project.tracks[l].name.clone()).unwrap_or_else(|| "the master".into());
    Some(match st.add_plugin(lane, &name, &path, Some(text.to_string())) {
        Ok(_) => {
            let i = st.project.chain(lane).len() - 1;
            st.select(Selection::Fx(lane, i));
            format!("{name} is on {place}. Its main knobs are below; lanes are not delay-compensated yet.")
        }
        Err(e) => format!("{name} could not be added: {e}"),
    })
}

/// Claude writes a MIDI part for the selected section (or the whole song).
fn write_midi(st: &mut EditorState, jobs: &Jobs, bundle: PathBuf, role: &str, text: &str) -> String {
    let bars = midi_writer::Bars::of(&st.project);
    let grid = model::BeatGrid::of(&st.project);
    let (range, place) = match st.sel {
        Selection::Section(i) => {
            let s = st.project.sections()[i].clone();
            ((grid.beat_at(s.start).round(), grid.beat_at(s.end).round()), format!("the {}", s.label))
        }
        _ => ((bars.first_beat, grid.beat_at(st.project.duration).floor()), "the whole song".to_string()),
    };
    let sheet = midi_writer::song_sheet(&st.project, bars);
    let job = Job::WriteMidi { bundle, request: text.to_string(), role: role.to_string(), sheet, bars, range };
    if !jobs.submit(job) {
        return "The job worker is not running".into();
    }
    st.analysing = Some(("Part".into(), 0.0));
    format!("Claude is writing a {role} part for {place} from the song's key, chords and sections (under a minute)...")
}

/// Claude writes an effect for a lane (or the master, `None`) from `text`.
fn write_effect(st: &mut EditorState, jobs: &Jobs, bundle: PathBuf, lane: Option<usize>, text: &str) -> String {
    let a = st.project.analysis.clone().unwrap_or_default();
    let song = match (a.key, a.tempo) {
        (Some(k), Some(bpm)) => format!("a song in {k} at {bpm:.0} bpm"),
        _ => "a song".into(),
    };
    let (context, place) = match lane {
        Some(l) => {
            let t = &st.project.tracks[l];
            // Its level, so the defaults engage on this material.
            let level = st.stems.get(l).map(|s| {
                let rms = (s.l.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>() / s.l.len().max(1) as f64).sqrt();
                let db = |x: f64| 20.0 * x.max(1e-9).log10();
                format!("; its level is about {:.0} dBFS RMS with peaks near {:.0} dBFS", db(rms), db(s.peak_abs as f64))
            });
            (format!("the {} lane (its {} stem) of {song}{}", t.name, t.id, level.unwrap_or_default()), t.name.clone())
        }
        None => (format!("the master bus (the whole mix) of {song}"), "the master".into()),
    };
    if !jobs.submit(Job::WriteFx { bundle, lane, request: text.to_string(), context }) {
        return "The job worker is not running".into();
    }
    st.analysing = Some(("Effect".into(), 0.0));
    format!("Claude is writing \"{text}\" for {place}; Poing checks it before you hear it (under a minute)...")
}

/// Describe on a lane (or a region of it): less echo, or a new part added.
fn describe_lane(st: &mut EditorState, jobs: &Jobs, bundle: PathBuf, lane: usize, range: Option<(f64, f64)>, text: &str) -> String {
    let track = st.project.tracks[lane].clone();
    let lane_file = st.project.track_path(&st.path, lane);
    // An installed plugin by name goes on the lane as it is.
    if let Some(result) = add_named_plugin(st, Some(lane), text) {
        return result;
    }
    let intent = lane_intent(text, &track.id);
    if intent == LaneIntent::Effect {
        return write_effect(st, jobs, bundle, Some(lane), text);
    }
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

const HELP: &str = "Click a lane, section or take  ·  drag across a lane to mark a region  ·  Space play  ·  L loop selection  ·  F fit  ·  B split section  ·  Ctrl+wheel zoom  ·  Del delete  ·  Ctrl+Z undo";

impl App {
    fn load(&mut self, cx: &mut Cx) {
        match open_target() {
            Some(target) => self.open(cx, target),
            None => self.show_home(cx, None),
        }
    }

    /// The start screen: drop a song, or pick one opened before.
    fn show_home(&mut self, cx: &mut Cx, error: Option<&str>) {
        let ui = self.ui.clone();
        let songs = recent_songs();
        let buttons = [ids!(rec0), ids!(rec1), ids!(rec2), ids!(rec3), ids!(rec4), ids!(rec5), ids!(rec6), ids!(rec7)];
        for (i, b) in buttons.iter().enumerate() {
            let btn = ui.button(cx, *b);
            match songs.get(i) {
                Some(p) => {
                    btn.set_text(cx, &song_name(p));
                    btn.set_visible(cx, true);
                }
                None => btn.set_visible(cx, false),
            }
        }
        self.recent = songs;
        ui.view(cx, ids!(recent_box)).set_visible(cx, !self.recent.is_empty());
        ui.label(cx, ids!(home_error)).set_text(cx, error.unwrap_or(""));
        self.show_song_view(cx, false);
    }

    /// Switch between the start screen and the song.
    fn show_song_view(&mut self, cx: &mut Cx, song: bool) {
        let ui = self.ui.clone();
        ui.view(cx, ids!(home)).set_visible(cx, !song);
        self.home_shown = !song;
        ui.view(cx, ids!(describe_frame)).set_visible(cx, song);
        for b in [ids!(render_btn), ids!(songs_btn)] {
            ui.button(cx, b).set_visible(cx, song);
        }
        if !song {
            ui.button(cx, ids!(play_btn)).set_visible(cx, false);
            ui.button(cx, ids!(pause_btn)).set_visible(cx, false);
            ui.label(cx, ids!(project_label)).set_text(cx, "Poing");
            ui.label(cx, ids!(project_meta)).set_text(cx, "");
        }
        for id in [ids!(timeline_box), ids!(inspector_box)] {
            ui.view(cx, id).set_visible(cx, song);
        }
        ui.redraw(cx);
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
            // a draft waits until one of its versions is picked
            let analyse = project.source.is_some() && !project.is_analysed() && !project.is_draft();
            let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
            // A take whose audio is gone is dropped rather than failing the song.
            project.takes_mut().retain(|t| dir.join(&t.file).exists());
            let mut stems: Vec<Stem> = Vec::new();
            for i in 0..project.tracks.len() {
                // a MIDI lane has no audio of its own: a silent placeholder
                if project.tracks[i].is_midi() {
                    stems.push(Stem::new(Vec::new(), Vec::new()));
                    continue;
                }
                stems.push(audio::load_stem(&project.track_path(&path, i), project.sr())?);
            }
            for t in project.takes() {
                stems.push(audio::load_stem(&dir.join(&t.file), project.sr())?);
            }
            let sr = project.sr();
            let mut st = EditorState::new(project, path, stems, shared.clone());
            st.restore_combined();
            // Analysed while another song was open: apply it from disk, no second run.
            let mut analyse = analyse;
            if analyse {
                if let Some((analysis, stems)) = finished_analysis(&dir, sr) {
                    st.apply_analysis(analysis, stems)?;
                    analyse = false;
                }
            }
            Ok((st, analyse))
        })();
        match result {
            Ok((mut st, analyse)) => {
                show_title(&self.ui, cx, &st.project);
                remember_song(&st.path);
                self.show_song_view(cx, true);
                if shared.is_none() {
                    audio::start_output(cx, st.shared.clone(), st.sr());
                }
                if analyse {
                    self.start_analysis(&mut st);
                }
                if let Some(mut old) = self.state.replace(st) {
                    // Its stem sets and plugins may still be in the audio thread's hands.
                    #[cfg(windows)]
                    let controls = old.take_plugin_controls();
                    if let Some(st) = &mut self.state {
                        #[cfg(windows)]
                        st.adopt_plugin_controls(controls);
                        for set in old.into_retired() {
                            st.retire(set);
                        }
                    }
                }
            }
            Err(e) => {
                log!("poing: load failed: {e}");
                if self.state.is_some() {
                    self.set_status(cx, &format!("Could not open that: {e}"));
                } else {
                    self.show_home(cx, Some(&format!("Could not open that: {e}")));
                }
            }
        }
    }

    /// Understand the song in the background; lanes fill in when it lands.
    fn start_analysis(&self, st: &mut EditorState) {
        analyse(self.jobs.as_ref(), st);
    }

    /// The start screen's "Make it": a new song from the prompt.
    fn make_song(&mut self, cx: &mut Cx, prompt: &str) {
        let ui = self.ui.clone();
        ui.label(cx, ids!(home_error)).set_text(cx, "");
        if prompt.trim().is_empty() {
            ui.label(cx, ids!(home_error)).set_text(cx, "Say first what the song should be.");
            return;
        }
        let job = Job::Song { prompt: prompt.trim().to_string(), dir: audio::songs_dir(), count: 3, sr: 48000 };
        let queued = self.jobs.as_ref().is_some_and(|j| j.submit(job));
        let line = if queued { "Claude is writing the song..." } else { "The job worker is not running" };
        ui.label(cx, ids!(home_status)).set_text(cx, line);
        cx.set_key_focus(Area::Empty);
    }

    /// A song dropped on the Remix card: open it and remix it once understood.
    fn remix_song(&mut self, cx: &mut Cx, audio: PathBuf) {
        let ui = self.ui.clone();
        let prompt = ui.text_input(cx, ids!(remix_input)).text();
        if prompt.trim().is_empty() {
            ui.label(cx, ids!(home_error)).set_text(cx, "Say first how to remix it (the field on the Remix card), then drop the song again.");
            return;
        }
        self.open(cx, Open::Audio(audio));
        let Some(st) = self.state.as_mut() else { return };
        st.pending_remix = Some(prompt.trim().to_string());
        let status = match (&self.jobs, st.project.is_analysed()) {
            (Some(jobs), true) => start_remix(st, jobs),
            _ => format!("Poing is listening to the song first; then it makes three remixes as \"{}\"", prompt.trim()),
        };
        self.set_status(cx, &status);
    }
}

/// Understand the song in the background; lanes fill in when it lands.
fn analyse(jobs: Option<&Jobs>, st: &mut EditorState) {
    {
        let (Some(jobs), Some(source)) = (jobs, st.project.source.clone()) else { return };
        let bundle = st.path.parent().map(Path::to_path_buf).unwrap_or_default();
        if jobs.submit(Job::Analyze { audio: bundle.join(source), bundle, sr: st.sr() }) {
            st.analysing = Some(("Starting analysis".into(), 0.0));
        }
    }
}

impl App {
    /// Show the inspector row for the current selection and fill its widgets.
    fn refresh_inspector(&mut self, cx: &mut Cx) {
        let Some(st) = &mut self.state else { return };
        if !st.sel_changed {
            return;
        }
        st.sel_changed = false;
        let ui = self.ui.clone();
        let playing = st.shared.playing();
        ui.button(cx, ids!(play_btn)).set_visible(cx, !playing);
        ui.button(cx, ids!(pause_btn)).set_visible(cx, playing);
        ui.view(cx, ids!(topbar)).redraw(cx);
        let (mut show_region, mut show_track, mut show_section, mut show_take) = (false, false, false, false);
        let mut show_fx = false;
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
                let what = match &t.notes {
                    Some(n) => match t.instrument.as_ref().filter(|i| i.plugin.is_some()) {
                        Some(i) => format!("MIDI, {} notes, played by {}", n.len(), i.name),
                        None => format!("MIDI, {} notes", n.len()),
                    },
                    None => t.file.clone(),
                };
                ui.label(cx, ids!(insp_title)).set_text(cx, &format!("Track: {}   ({what})", t.name));
                ui.slider(cx, ids!(track_gain)).set_value(cx, t.gain());
                ui.check_box(cx, ids!(mute_cb)).set_active(cx, t.muted(), Animate::No);
                ui.check_box(cx, ids!(solo_cb)).set_active(cx, t.soloed(), Animate::No);
                show_track = true;
            }
            Selection::Fx(lane, i) if i < st.project.chain(lane).len() => {
                let fx = st.project.chain(lane)[i].clone();
                let place = lane.map(|l| st.project.tracks[l].name.clone()).unwrap_or_else(|| "the master".into());
                let state = match (st.fx_errors.get(&fx.id), fx.plugin.is_some()) {
                    (Some(e), true) => format!("   ·   {}", e.chars().take(120).collect::<String>()),
                    (Some(e), false) => format!("   ·   does not compile: {}", e.chars().take(120).collect::<String>()),
                    (None, true) => "   ·   VST3 plugin, its main knobs".to_string(),
                    (None, false) => format!("   ·   edit {} to change it live", fx.file),
                };
                ui.label(cx, ids!(insp_title)).set_text(cx, &format!("{} on {place}{state}", fx.name));
                let knobs = st.fx_knobs(&fx.id);
                let sliders = [ids!(fx_k0), ids!(fx_k1), ids!(fx_k2), ids!(fx_k3), ids!(fx_k4), ids!(fx_k5)];
                let boxes = [ids!(fx_b0), ids!(fx_b1), ids!(fx_b2), ids!(fx_b3), ids!(fx_b4), ids!(fx_b5)];
                for (k, id) in sliders.iter().enumerate() {
                    ui.view(cx, boxes[k]).set_visible(cx, k < knobs.len());
                    let mut w = ui.widget(cx, *id);
                    match knobs.get(k) {
                        Some((name, value, min, max)) => {
                            let (name, min, max) = (name.replace('_', " "), *min, *max);
                            script_apply_eval!(cx, w, {text: #(name) min: #(min) max: #(max)});
                            ui.slider(cx, *id).set_value(cx, *value);
                        }
                        None => {}
                    }
                }
                ui.check_box(cx, ids!(fx_bypass)).set_active(cx, fx.bypassed(), Animate::No);
                show_fx = true;
            }
            Selection::Section(i) if i < st.project.sections().len() => {
                let s = st.project.sections()[i].clone();
                ui.label(cx, ids!(insp_title)).set_text(cx, &format!("Section {}   {:.2}s - {:.2}s   ·   Describe above to repaint it", i + 1, s.start, s.end));
                ui.text_input(cx, ids!(section_input)).set_text(cx, &s.label);
                let remix_on = st.song_take().is_some_and(|(_, t)| t.engine == "acestep-cover");
                ui.button(cx, ids!(use_remix_btn)).set_visible(cx, remix_on);
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
                    let title = if state::named_take(&t) {
                        format!("{}  ·  {span}", t.prompt)
                    } else {
                        format!("Take {n}  ·  {}{on}  ·  {span}  ·  seed {}", t.prompt, t.seed)
                    };
                    ui.label(cx, ids!(insp_title)).set_text(cx, &title);
                    ui.button(cx, ids!(delete_take_btn)).set_visible(cx, true);
                    let blends = t.scope == "lane" || t.scope == "layer";
                    ui.view(cx, ids!(take_blend_box)).set_visible(cx, blends);
                    ui.slider(cx, ids!(take_blend)).set_value(cx, (t.blend() * 100.0).round());
                    let keep = match t.scope.as_str() {
                        "song" if st.project.is_draft() => "Use this version",
                        "song" => "Keep as a new song",
                        "layer" => "Keep as a lane",
                        "lane" => "Keep in the lane",
                        _ => "",
                    };
                    ui.button(cx, ids!(keep_btn)).set_visible(cx, !keep.is_empty());
                    ui.button(cx, ids!(keep_btn)).set_text(cx, keep);
                    show_take = true;
                }
                None if st.project.is_draft() => {
                    ui.label(cx, ids!(insp_title)).set_text(cx, "Version 1  ·  click the versions above the lanes to compare them, then use the one you like");
                    ui.view(cx, ids!(take_blend_box)).set_visible(cx, false);
                    ui.button(cx, ids!(keep_btn)).set_visible(cx, true);
                    ui.button(cx, ids!(keep_btn)).set_text(cx, "Use this version");
                    ui.button(cx, ids!(delete_take_btn)).set_visible(cx, false);
                    show_take = true;
                }
                None => ui.label(cx, ids!(insp_title)).set_text(cx, HELP),
            },
        }
        ui.view(cx, ids!(region_row)).set_visible(cx, show_region);
        ui.view(cx, ids!(track_row)).set_visible(cx, show_track);
        ui.view(cx, ids!(section_row)).set_visible(cx, show_section);
        ui.view(cx, ids!(take_row)).set_visible(cx, show_take);
        ui.view(cx, ids!(fx_row)).set_visible(cx, show_fx);
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
                    if self.home_shown {
                        self.ui.label(cx, ids!(home_status)).set_text(cx, "");
                        self.ui.label(cx, ids!(home_error)).set_text(cx, &message);
                    }
                    message
                }
                JobUpdate::Starting { message, progress } => {
                    let line = format!("{message}  {:.0}%", progress * 100.0);
                    self.ui.label(cx, ids!(home_status)).set_text(cx, &line);
                    line
                }
                JobUpdate::SongReady { project } => {
                    self.ui.label(cx, ids!(home_status)).set_text(cx, "");
                    if let Some(st) = self.state.as_mut() {
                        let _ = st.save();
                    }
                    self.open(cx, Open::Project(project));
                    if let Some(st) = self.state.as_mut() {
                        st.analysing = Some(("Song".into(), 0.4));
                    }
                    "Version 1 is here and playing is up to you (Space). The other versions follow; each shows up above the lanes.".into()
                }
                JobUpdate::Finished { bundle, message } => {
                    if let Some(st) = self.state.as_mut().filter(|_| mine(&bundle)) {
                        st.analysing = None;
                        st.sel_changed = true;
                    }
                    message
                }
                JobUpdate::Progress { bundle, message, progress } => {
                    let Some(st) = self.state.as_mut().filter(|_| mine(&bundle)) else { continue };
                    let what = if ["Take", "Effect", "Part", "Remix", "Song"].iter().any(|p| message.starts_with(p)) { "" } else { "Analysing  ·  " };
                    let line = format!("{what}{message}  {:.0}%", progress * 100.0);
                    st.analysing = Some((message, progress));
                    line
                }
                JobUpdate::MidiReady { bundle, name, role, notes, request, note } => {
                    let Some(st) = self.state.as_mut().filter(|_| mine(&bundle)) else { continue };
                    st.analysing = None;
                    let code = synth::BUILTIN.iter().find(|(r, _, _)| *r == role).map(|(_, _, c)| *c).unwrap_or(synth::BUILTIN[2].2);
                    match st.add_midi_track(&name, code, notes, Some(request)) {
                        Ok(lane) => {
                            st.select(Selection::Track(lane));
                            let by = match st.next_instrument.take() {
                                Some((plugin, path)) => match st.set_instrument_plugin(lane, &plugin, &path, None) {
                                    Ok(()) => plugin,
                                    Err(e) => format!("Poing's {role} ({plugin} could not play it: {e})"),
                                },
                                None => format!("Poing's {role}"),
                            };
                            format!("{name} is a new MIDI lane ({note}), played by {by}. Click a note to move it, drag its end to lengthen, Delete removes it.")
                        }
                        Err(e) => format!("{name} could not be added: {e}"),
                    }
                }
                JobUpdate::FxReady { bundle, lane, name, code, request, note } => {
                    let Some(st) = self.state.as_mut().filter(|_| mine(&bundle)) else { continue };
                    st.analysing = None;
                    let place = lane.map(|l| st.project.tracks[l].name.clone()).unwrap_or_else(|| "the master".into());
                    match st.add_fx(lane, &name, &code, Some(request)) {
                        Ok(_) => {
                            let i = st.project.chain(lane).len() - 1;
                            st.select(Selection::Fx(lane, i));
                            format!("{name} is on {place} ({note}). Turn its knobs below, or edit its file to change it live.")
                        }
                        Err(e) => format!("{name} could not be added: {e}"),
                    }
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
                        Ok(()) if st.pending_remix.is_some() => {
                            show_title(&self.ui, cx, &st.project);
                            match &self.jobs {
                                Some(jobs) => format!("Understood. {}", start_remix(st, jobs)),
                                None => "The job worker is not running".into(),
                            }
                        }
                        Ok(()) => {
                            show_title(&self.ui, cx, &st.project);
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
    /// Play through the system's default output (the audio callback only
    /// runs once an output device is opened).
    fn handle_audio_devices(&mut self, cx: &mut Cx, devices: &AudioDevicesEvent) {
        for d in &devices.descs {
            log!("poing: audio device {d}");
        }
        let out = devices.default_output();
        log!("poing: playing through {:?}", out);
        // The device list can come empty first and complete a moment later.
        let none = "No audio output is available (is another app holding the sound card?): playback stays silent until one is free. Export still works.";
        if out.is_empty() {
            self.set_status(cx, none);
        } else if self.ui.label(cx, ids!(status_label)).text() == none {
            self.set_status(cx, "");
        }
        cx.use_audio_outputs(&out);
    }

    fn handle_startup(&mut self, cx: &mut Cx) {
        self.jobs = Some(Jobs::start(cx));
        self.load(cx);
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let ui = self.ui.clone();
        // A clicked button keeps the keyboard focus and would answer Space
        // (Export again): give the keys back to the song.
        if !typing(&ui, cx) && !cx.key_focus().is_empty() {
            cx.set_key_focus(Area::Empty);
        }
        let buttons = [ids!(rec0), ids!(rec1), ids!(rec2), ids!(rec3), ids!(rec4), ids!(rec5), ids!(rec6), ids!(rec7)];
        if let Some(i) = buttons.iter().position(|b| ui.button(cx, *b).clicked(actions)) {
            if let Some(p) = self.recent.get(i).cloned() {
                self.open(cx, Open::Project(p));
                self.refresh_inspector(cx);
                return;
            }
        }
        // the start screen
        let new_prompt = ui.text_input(cx, ids!(new_input)).returned(actions).map(|(t, _)| t);
        if new_prompt.is_some() || ui.button(cx, ids!(make_btn)).clicked(actions) {
            let prompt = new_prompt.unwrap_or_else(|| ui.text_input(cx, ids!(new_input)).text());
            self.make_song(cx, &prompt);
            return;
        }
        if ui.button(cx, ids!(songs_btn)).clicked(actions) {
            if let Some(st) = self.state.as_mut() {
                st.shared.set_playing(false);
                st.sel_changed = true;
                let _ = st.save();
            }
            self.show_home(cx, None);
            return;
        }
        let Some(st) = &mut self.state else { return };
        let mut status: Option<String> = None;

        if ui.button(cx, ids!(play_btn)).clicked(actions) || ui.button(cx, ids!(pause_btn)).clicked(actions) {
            st.toggle_play();
        }
        if ui.button(cx, ids!(render_btn)).clicked(actions) {
            let out_dir = st.path.parent().map(|p| p.join("renders")).unwrap_or_else(|| PathBuf::from("renders"));
            // plugins get a second instance for the render
            let (rack, missing) = st.make_render_rack();
            let job = Job::Render { stems: st.stems.clone(), mix: st.mix_snapshot(), rack: Box::new(rack), sr: st.sr(), out_dir, name: st.project.name.clone() };
            let queued = self.jobs.as_ref().map(|j| j.submit(job)).unwrap_or(false);
            status = Some(match (queued, missing.is_empty()) {
                (false, _) => "Render worker is not running".into(),
                (true, true) => "Rendering...".into(),
                (true, false) => format!("Rendering without {} (it would not load a second time)...", missing.join(", ")),
            });
        }

        if let Some((text, _)) = ui.text_input(cx, ids!(describe_input)).returned(actions) {
            if !text.trim().is_empty() {
                status = Some(describe(st, self.jobs.as_ref(), text.trim()));
                // back to the song: shortcuts work again
                cx.set_key_focus(Area::Empty);
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
        if ui.button(cx, ids!(keep_btn)).clicked(actions) && st.project.is_draft() {
            match st.use_version() {
                Ok(name) => {
                    analyse(self.jobs.as_ref(), st);
                    status = Some(format!("{name} it is. Poing is listening to it: parts, sections, chords and words fill in as they come."));
                }
                Err(e) => status = Some(format!("Could not use it: {e}")),
            }
        } else if ui.button(cx, ids!(keep_btn)).clicked(actions) && st.analysing.is_some() {
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

        if let Selection::Fx(lane, i) = st.sel {
            if let Some(id) = st.project.chain(lane).get(i).map(|f| f.id.clone()) {
                let sliders = [ids!(fx_k0), ids!(fx_k1), ids!(fx_k2), ids!(fx_k3), ids!(fx_k4), ids!(fx_k5)];
                for (k, s) in sliders.iter().enumerate() {
                    if let Some(v) = ui.slider(cx, *s).slided(actions) {
                        // knobs list the shader's params minus `bpm`
                        if let Some(index) = st.fx_param_index(&id, k) {
                            st.set_fx_param(&id, index, v);
                        }
                    }
                }
                if let Some(on) = ui.check_box(cx, ids!(fx_bypass)).changed(actions) {
                    st.set_fx_bypass(&id, on);
                }
                if ui.button(cx, ids!(fx_remove)).clicked(actions) {
                    status = st.delete_selected();
                }
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
                    status = st.delete_selected();
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
            if ui.button(cx, ids!(use_remix_btn)).clicked(actions) {
                status = Some(st.use_remix_here(i).unwrap_or_else(|e| format!("Could not use it here: {e}")));
            }
            if ui.button(cx, ids!(merge_btn)).clicked(actions) {
                status = st.delete_selected();
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
        if self.autosave.is_event(event).is_some() {
            self.autosave = Timer::empty();
            if let Some(st) = self.state.as_mut().filter(|st| st.dirty) {
                if let Err(e) = st.save() {
                    self.set_status(cx, &format!("Could not save: {e}"));
                }
            }
        }
        // A change starts (or restarts) the countdown: one save per pause, not per drag step.
        if self.state.as_ref().is_some_and(|st| st.dirty) && !matches!(event, Event::Timer(_) | Event::NextFrame(_)) {
            cx.stop_timer(self.autosave);
            self.autosave = cx.start_timeout(1.5);
        }
        // Drop a song (or a .poing folder) anywhere on the window to open it.
        match event {
            Event::Drag(e) => {
                if e.items.iter().any(|i| matches!(i, DragItem::FilePath { path, .. } if target_for(Path::new(path)).is_some())) {
                    *e.response.lock().unwrap() = DragResponse::Copy;
                }
            }
            Event::Drop(e) => {
                let target = e.items.iter().find_map(|i| match i {
                    DragItem::FilePath { path, .. } => target_for(Path::new(path)),
                    _ => None,
                });
                // on the start screen, the card it lands on says what to do with it
                let on_remix = self.home_shown && self.ui.view(cx, ids!(remix_card)).area().rect(cx).contains(e.abs);
                if let (true, Some(Open::Audio(audio))) = (on_remix, &target) {
                    if let Some(st) = self.state.as_mut() {
                        let _ = st.save();
                    }
                    self.remix_song(cx, audio.clone());
                    self.refresh_inspector(cx);
                } else if let Some(target) = target {
                    if let Some(st) = self.state.as_mut() {
                        let _ = st.save();
                    }
                    self.open(cx, target);
                    self.refresh_inspector(cx);
                }
            }
            _ => {}
        }
        // Shortcuts work anywhere but in a text field (which keeps its own keys).
        if let Event::KeyDown(ke) = event {
            if let Some(st) = self.state.as_mut() {
                if !typing(&self.ui, cx) && timeline::shortcut(st, ke) {
                    self.ui.widget(cx, ids!(timeline)).redraw(cx);
                }
            }
        }
        self.match_event(cx, event);
        match &mut self.state {
            Some(st) => {
                st.collect_retired();
                // The audio callback panicked: Makepad dropped it, so say why.
                if let Some(note) = makepad_widgets::makepad_platform::audio_output_fence::take_audio_output_panic_note(0) {
                    log!("poing: audio stopped: {note}");
                    self.ui.label(cx, ids!(status_label)).set_text(cx, &format!("Audio stopped: {note}"));
                }
                if let Some(note) = st.notice.take() {
                    self.ui.label(cx, ids!(status_label)).set_text(cx, &note);
                    self.status_at = Some(std::time::Instant::now());
                }
                if let Some(note) = st.reload_changed_fx() {
                    self.ui.label(cx, ids!(status_label)).set_text(cx, &note);
                }
                self.ui.handle_event(cx, event, &mut Scope::with_data(st))
            }
            None => self.ui.handle_event(cx, event, &mut Scope::empty()),
        }
        self.refresh_inspector(cx);
    }
}
