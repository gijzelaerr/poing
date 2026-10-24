//! Poing: a song's stems as lanes, its sections, and marked regions (bad /
//! regenerate / level fixes with notes for the AI). Edits play live; Save writes
//! project.json for the AI pipeline, Render writes a WAV and a mastered MP3.
//!
//!   poing path/to/project.json      (or POING_PROJECT)
//!   POING_MUTE=1 keeps agent/test instances silent.

pub use makepad_widgets;

mod agent;
mod audio;
mod deckmap;
mod decks;
mod djexport;
mod export;
mod fx;
mod fx_writer;
mod listen;
mod live;
mod live_view;
mod llm;
mod plugins;
mod remote;
mod synth;
mod midi_writer;
mod model_view;
mod models;
mod paths;
mod song_writer;
pub(crate) use poing_model as model;
mod state;
mod timeline;
mod wire;

use audio::{Job, JobUpdate, Jobs, LayerJob, LegoJob, RepaintJob, SongJob, Stem, Yue2Fixture};
use makepad_widgets::*;
use makepad_micro_serde::{DeJson, DeJsonErr, DeJsonState, SerJson, SerJsonState};
use model::{default_gain_for, demucs_stem_for, is_master_effect_text, lane_intent, snap_to_downbeats, Analysis, LaneIntent, Project, Take, LEGO_TRACKS, REGION_KINDS};
use models::Downloads;
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
        grab_key_focus: false
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
        grab_key_focus: false
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

    // One downloadable pack on the Models page: name, what and size, one button.
    let ModelRow = View{
        visible: false
        width: Fill height: Fit flow: Right spacing: 14
        padding: Inset{left: 0 right: 0 top: 6 bottom: 6}
        align: Align{y: 0.5}
    }
    let ModelName = Label{
        draw_text +: {color: TEXT text_style: theme.font_bold{font_size: 10.5}}
    }

    // A song tab: its name, a close cross, an accent line when it is in front.
    let SongTab = View{
        visible: false
        width: Fit height: Fill flow: Down
    }
    let TabRow = View{
        width: Fit height: 30 flow: Right
        align: Align{y: 0.5}
    }
    let TabButton = ButtonFlatter{
        height: 30
        grab_key_focus: false
        padding: Inset{left: 12 right: 28 top: 0 bottom: 0}
        align: Align{x: 0.5 y: 0.5}
        draw_text +: {
            color: TEXT_2
            color_hover: #xffffff
            text_style: theme.font_regular{font_size: 9.5}
        }
        draw_bg +: {
            border_radius: 6.0
            border_size: 0.0
            color: #00000000
            color_hover: #x1f2229
            color_down: #x272b33
            color_focus: #00000000
        }
    }
    let TabClose = ButtonFlatter{
        width: 20 height: 20
        margin: Inset{left: -26 right: 6 top: 5 bottom: 0}
        grab_key_focus: false
        padding: Inset{left: 0 right: 0 top: 0 bottom: 0}
        align: Align{x: 0.5 y: 0.5}
        text: "×"
        draw_text +: {
            color: #x6b717c
            color_hover: #xffffff
            text_style: theme.font_regular{font_size: 11}
        }
        draw_bg +: {
            border_radius: 10.0
            border_size: 0.0
            color: #00000000
            color_hover: #x30353f
            color_down: #x3a3f49
            color_focus: #00000000
        }
    }
    let TabLine = SolidView{
        visible: false
        width: 28 height: 2
        margin: Inset{left: 12 right: 0 top: 0 bottom: 0}
        show_bg: true
        draw_bg +: {color: ACCENT}
    }

    // One setting: its name on the left, what it is and how to change it on the right.
    let SettingsRow = RoundedView{
        width: Fill height: Fit flow: Right spacing: 20
        padding: Inset{left: 18 right: 18 top: 14 bottom: 14}
        show_bg: true
        draw_bg +: {color: #x181a20 border_radius: 10.0 border_size: 1.0 border_color: HAIRLINE}
    }

    // Live mode: a deck's panel, and its big readouts.
    let DeckPanel = RoundedView{
        width: Fill height: Fill flow: Down spacing: 10
        padding: Inset{left: 20 right: 20 top: 18 bottom: 18}
        show_bg: true
        draw_bg +: {color: #x181a20 border_radius: 12.0 border_size: 1.0 border_color: HAIRLINE}
    }
    let DeckBig = Label{
        draw_text +: {color: TEXT text_style: theme.font_bold{font_size: 22}}
    }
    let DeckLine = Label{
        width: Fill
        draw_text +: {color: TEXT text_style: theme.font_regular{font_size: 10.5}}
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

                    topbar := SolidView{
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
                        live_btn := PillButton{
                            margin: Inset{top: 3 bottom: 0 left: 0 right: 0}
                            text: "Live"
                        }
                        render_btn := PillButton{
                            margin: Inset{top: 3 bottom: 0 left: 0 right: 0}
                            text: "Export"
                        }
                        settings_btn := PillButton{
                            margin: Inset{top: 3 bottom: 0 left: 0 right: 0}
                            text: "Settings"
                        }
                    }
                    // An AI feature that needs a download says so here, once it is
                    // used: Download or Not now. Then the download's progress.
                    offer_bar := SolidView{
                        visible: false
                        width: Fill height: Fit flow: Right spacing: 10
                        padding: Inset{left: 18 right: 14 top: 7 bottom: 7}
                        align: Align{y: 0.5}
                        show_bg: true
                        draw_bg +: {color: #x14202f}
                        offer_label := Label{
                            width: Fill
                            draw_text +: {color: TEXT text_style: theme.font_regular{font_size: 9.5}}
                        }
                        offer_yes := PillButton{text: "Download"}
                        offer_no := PillButton{text: "Not now"}
                    }
                    // Open songs, one tab each: click to switch (playing on, at the same time), x to close.
                    tab_strip := SolidView{
                        visible: false
                        width: Fill height: 32 flow: Right spacing: 2
                        padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
                        show_bg: true
                        draw_bg +: {color: #x121317}
                        tab0 := SongTab{TabRow{tab0_btn := TabButton{} tab0_x := TabClose{}} tab0_line := TabLine{}}
                        tab1 := SongTab{TabRow{tab1_btn := TabButton{} tab1_x := TabClose{}} tab1_line := TabLine{}}
                        tab2 := SongTab{TabRow{tab2_btn := TabButton{} tab2_x := TabClose{}} tab2_line := TabLine{}}
                        tab3 := SongTab{TabRow{tab3_btn := TabButton{} tab3_x := TabClose{}} tab3_line := TabLine{}}
                        tab4 := SongTab{TabRow{tab4_btn := TabButton{} tab4_x := TabClose{}} tab4_line := TabLine{}}
                        tab5 := SongTab{TabRow{tab5_btn := TabButton{} tab5_x := TabClose{}} tab5_line := TabLine{}}
                        new_tab_btn := TabButton{text: "+"}
                    }
                    SolidView{width: Fill height: 1 draw_bg +: {color: HAIRLINE}}

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
                                pick_split_btn := PillButton{text: "Choose a song…"}
                                Caption{text: "or drop one on this card"}
                            }
                            remix_card := HomeCard{
                                CardTitle{text: "Remix a song"}
                                CardText{text: "Say how, then drop the song on this card. Poing makes three remixes to compare, and you can mix them per section."}
                                View{width: Fill height: Fill}
                                PromptFrame{
                                    remix_input := PromptInput{empty_text: "e.g. 80s disco, Dutch female vocals"}
                                }
                                pick_remix_btn := PillButton{text: "Choose the song…"}
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
                        // a song to remix that already has a project: add to it, or start over
                        remix_choice := View{
                            visible: false
                            width: Fit height: Fit flow: Right spacing: 10
                            align: Align{y: 0.5}
                            remix_choice_label := Label{
                                text: ""
                                draw_text +: {color: TEXT text_style: theme.font_regular{font_size: 10}}
                            }
                            remix_add_btn := PillButton{text: "Add the remixes to it"}
                            remix_new_btn := PillButton{text: "Start a new project"}
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

                    // Settings: what Poing plays through, where its AI runs, what it found.
                    settings := ScrollYView{
                        visible: false
                        width: Fill height: Fill flow: Down spacing: 18
                        padding: Inset{left: 60 right: 60 top: 36 bottom: 24}
                        Label{
                            text: "Settings"
                            draw_text +: {color: TEXT text_style: theme.font_bold{font_size: 20}}
                        }
                        SettingsRow{
                            CardTitle{width: 160 text: "Sound"}
                            View{width: Fill height: Fit flow: Down spacing: 6
                                out_dd := DropDown{width: 420 labels: ["Default output"]}
                                out_status := Caption{width: Fill text: ""}
                            }
                        }
                        SettingsRow{
                            CardTitle{width: 160 text: "AI"}
                            View{width: Fill height: Fit flow: Down spacing: 6
                                ai_dd := DropDown{width: 420 labels: ["Poing's own model (no subscription)", "Claude Code"]}
                                ai_status := CardText{text: ""}
                            }
                        }
                        SettingsRow{
                            CardTitle{width: 160 text: "Compute"}
                            View{width: Fill height: Fit flow: Down spacing: 6
                                compute_dd := DropDown{width: 420 labels: ["This PC"]}
                                compute_status := CardText{text: ""}
                            }
                        }
                        SettingsRow{
                            CardTitle{width: 160 text: "Plugins"}
                            View{width: Fill height: Fit flow: Down spacing: 6
                                plugins_status := CardText{text: ""}
                                rescan_btn := PillButton{text: "Look for plugins again"}
                            }
                        }
                        SettingsRow{
                            CardTitle{width: 160 text: "Models"}
                            View{width: Fill height: Fit flow: Down spacing: 6
                                models_status := CardText{text: ""}
                                mp0 := ModelRow{View{width: Fill height: Fit flow: Down spacing: 3 mp0_name := ModelName{} mp0_info := CardText{}} mp0_btn := PillButton{}}
                                mp1 := ModelRow{View{width: Fill height: Fit flow: Down spacing: 3 mp1_name := ModelName{} mp1_info := CardText{}} mp1_btn := PillButton{}}
                                mp2 := ModelRow{View{width: Fill height: Fit flow: Down spacing: 3 mp2_name := ModelName{} mp2_info := CardText{}} mp2_btn := PillButton{}}
                                mp3 := ModelRow{View{width: Fill height: Fit flow: Down spacing: 3 mp3_name := ModelName{} mp3_info := CardText{}} mp3_btn := PillButton{}}
                                mp4 := ModelRow{View{width: Fill height: Fit flow: Down spacing: 3 mp4_name := ModelName{} mp4_info := CardText{}} mp4_btn := PillButton{}}
                                mp5 := ModelRow{View{width: Fill height: Fit flow: Down spacing: 3 mp5_name := ModelName{} mp5_info := CardText{}} mp5_btn := PillButton{}}
                                mp6 := ModelRow{View{width: Fill height: Fit flow: Down spacing: 3 mp6_name := ModelName{} mp6_info := CardText{}} mp6_btn := PillButton{}}
                                models_note := CardText{text: ""}
                            }
                        }
                        settings_done := PillButton{text: "Done"}
                    }

                    // Export: the mix, stems, MIDI files, a DAWproject, or for DJ (rekordbox).
                    export_sheet := View{
                        visible: false
                        width: Fill height: Fill flow: Down spacing: 14
                        padding: Inset{left: 60 right: 60 top: 36 bottom: 24}
                        Label{
                            text: "Export"
                            draw_text +: {color: TEXT text_style: theme.font_bold{font_size: 20}}
                        }
                        SettingsRow{
                            CardTitle{width: 160 text: "Mix"}
                            View{width: Fill height: Fit flow: Down spacing: 6
                                CardText{text: "The song as it plays: a WAV and an MP3 mastered to -14 LUFS."}
                                export_mix_btn := PillButton{text: "Export the mix"}
                            }
                        }
                        SettingsRow{
                            CardTitle{width: 160 text: "Stems"}
                            View{width: Fill height: Fit flow: Down spacing: 6
                                CardText{text: "Each lane alone as it sounds in the mix (its level, takes and effects), as WAVs that add up to the mix before the master effects."}
                                export_stems_btn := PillButton{text: "Export stems"}
                            }
                        }
                        SettingsRow{
                            CardTitle{width: 160 text: "MIDI"}
                            View{width: Fill height: Fit flow: Down spacing: 6
                                export_midi_text := CardText{text: ""}
                                export_midi_btn := PillButton{text: "Export MIDI"}
                            }
                        }
                        SettingsRow{
                            CardTitle{width: 160 text: "DAWproject"}
                            View{width: Fill height: Fit flow: Down spacing: 6
                                CardText{text: "Open the song in Bitwig, Studio One or Cubase: every lane with its takes and effects, MIDI lanes as notes, the sections as markers, the tempo map."}
                                export_daw_btn := PillButton{text: "Export DAWproject"}
                            }
                        }
                        SettingsRow{
                            CardTitle{width: 160 text: "For DJ"}
                            View{width: Fill height: Fit flow: Down spacing: 6
                                CardText{text: "For rekordbox and the OMNIS-DUO: the song as it plays plus the versions ticked here, with its beatgrid, key, a memory cue at every section and hot cues on intro, chorus, bridge and outro. In rekordbox, show the rekordbox xml pane (Preferences, View) and point it at Poing.xml in the folder named below."}
                                View{width: Fill height: Fit flow: Right spacing: 12
                                    dj_v0 := CheckBox{text: ""}
                                    dj_v1 := CheckBox{text: ""}
                                    dj_v2 := CheckBox{text: ""}
                                    dj_v3 := CheckBox{text: ""}
                                    dj_v4 := CheckBox{text: ""}
                                    dj_v5 := CheckBox{text: ""}
                                }
                                View{width: Fill height: Fit flow: Right spacing: 12 align: Align{y: 0.5}
                                    dj_format := DropDown{width: 200 labels: ["WAV, 44.1 kHz" "MP3, 320 kbps"]}
                                    export_dj_btn := PillButton{text: "Export for rekordbox"}
                                }
                            }
                        }
                        export_status := Caption{width: Fill text: ""}
                        export_done := PillButton{text: "Done"}
                    }

                    // Live: two decks played from the OMNIS-DUO (docs/LIVE.md).
                    live_view := View{
                        visible: false
                        width: Fill height: Fill flow: Down spacing: 14
                        padding: Inset{left: 36 right: 36 top: 26 bottom: 22}
                        View{
                            width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            Label{
                                text: "Live"
                                draw_text +: {color: TEXT text_style: theme.font_bold{font_size: 20}}
                            }
                            live_status := Caption{width: Fill text: ""}
                            live_done := PillButton{text: "Back to the song"}
                        }
                        View{
                            width: Fill height: Fill flow: Right spacing: 14
                            DeckPanel{
                                d0_title := CardTitle{text: ""}
                                d0_time := DeckBig{text: ""}
                                d0_info := DeckLine{text: ""}
                                d0_pads := DeckLine{text: ""}
                                d0_stems := DeckLine{text: ""}
                                d0_loop := DeckLine{text: ""}
                                d0_mix := CardText{text: ""}
                            }
                            RoundedView{
                                width: 230 height: Fill flow: Down spacing: 10
                                padding: Inset{left: 18 right: 18 top: 18 bottom: 18}
                                show_bg: true
                                draw_bg +: {color: #x14161b border_radius: 12.0 border_size: 1.0 border_color: HAIRLINE}
                                Caption{text: "Tempo"}
                                live_bpm := DeckBig{text: ""}
                                Caption{text: "Crossfader"}
                                live_xf := DeckLine{text: ""}
                                Caption{text: "Beat FX"}
                                live_fx := DeckLine{text: ""}
                            }
                            DeckPanel{
                                d1_title := CardTitle{text: ""}
                                d1_time := DeckBig{text: ""}
                                d1_info := DeckLine{text: ""}
                                d1_pads := DeckLine{text: ""}
                                d1_stems := DeckLine{text: ""}
                                d1_loop := DeckLine{text: ""}
                                d1_mix := CardText{text: ""}
                            }
                        }
                        live_help := CardText{text: "Pads 1-4: hot cues. Shift + pads: mute vocals, drums, bass, other. Loop encoder: beat loop (turn for its size). Arrows: previous or next section. Load: the song in front onto that deck. The map is app/live/omnis-duo.map; a copy in Poing's config folder overrides it."}
                    }

                    timeline_box := View{
                        width: Fill height: Fill flow: Down
                        timeline := mod.widgets.DawTimeline{}
                    }

                    inspector_box := View{
                    width: Fill height: Fit flow: Down
                    SolidView{width: Fill height: 1 draw_bg +: {color: HAIRLINE}}
                    inspector := SolidView{
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
                            rec_box := View{
                                visible: false width: Fit height: Fit flow: Right spacing: 14
                                align: Align{y: 0.5}
                                rec_btn := PillButton{text: "Record"}
                                quant_dd := DropDown{width: 170 labels: ["As played" "Quantise 1/8" "Quantise 1/16" "Quantise 1/16 swing"]}
                                overdub_cb := CheckBox{text: "On top of the notes"}
                            }
                        }
                        section_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            section_input := TextInput{width: 200 empty_text: "section name"}
                            merge_btn := PillButton{text: "Merge into previous"}
                            use_remix_btn := PillButton{text: "Use the playing remix here"}
                        }
                        words_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            words_input := TextInput{width: 420 empty_text: "the right words"}
                        }
                        take_row := View{
                            visible: false width: Fill height: Fit flow: Right spacing: 14
                            align: Align{y: 0.5}
                            take_blend_box := View{width: Fit height: Fit take_blend := Slider{width: 300 text: "Blend %" min: 0.0 max: 100.0 step: 1.0 precision: 0 default: 100.0}}
                            keep_btn := PillButton{text: "Keep as a new song"}
                            delete_take_btn := PillButton{text: "Delete take"}
                        }
                        fx_row := View{
                            visible: false width: Fill height: Fit flow: Flow.Right{wrap: true, row_align: RowAlign.Center} spacing: 12
                            align: Align{y: 0.5}
                            fx_b0 := View{width: Fit height: Fit fx_k0 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_b1 := View{width: Fit height: Fit fx_k1 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_b2 := View{width: Fit height: Fit fx_k2 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_b3 := View{width: Fit height: Fit fx_k3 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_b4 := View{width: Fit height: Fit fx_k4 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_b5 := View{width: Fit height: Fit fx_k5 := Slider{width: 170 text: "" min: 0.0 max: 1.0 step: 0.0 precision: 2}}
                            fx_editor := PillButton{text: "Open its window"}
                            fx_isolate := CheckBox{text: "Run in its own process"}
                            fx_bypass := CheckBox{text: "Bypass"}
                            fx_remove := PillButton{text: "Remove"}
                        }
                        View{
                            width: Fill height: Fit flow: Right spacing: 10
                            align: Align{y: 0.5}
                            status_label := Caption{text: ""}
                            redo_btn := PillButton{visible: false text: "Make it again"}
                        }
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
    /// The Settings panel is showing (over the start screen or a song).
    #[rust]
    settings_shown: bool,
    /// The Export sheet is showing (over the song).
    #[rust]
    export_shown: bool,
    /// Audio devices as the system last reported them.
    #[rust]
    audio_devices: Vec<AudioDeviceDesc>,
    /// The nodes Settings' Compute list offers after "This PC", in order.
    #[rust]
    compute_nodes: Vec<String>,
    /// Jobs finished so far (the worker's JobDone numbers).
    #[rust]
    jobs_done: u64,
    /// A close was refused because a job runs: closing again soon quits.
    #[rust]
    close_armed: Option<std::time::Instant>,
    /// Requests whose job has not finished: (song folder, request).
    #[rust]
    requests: Vec<(PathBuf, Request)>,
    /// Requests of an earlier run, cut off when Poing closed, for "Make it again".
    #[rust]
    interrupted: Vec<(PathBuf, Request)>,
    /// Poing's tools for an agent (Claude Code over `makepad-remote mcp poing`).
    #[rust]
    ai_port: Option<makepad_ai_services::port::AiServicePort>,
    /// Tool calls waiting for the job they started: (call id, job number).
    #[rust]
    parked: Vec<(String, u64)>,
    /// Claude working on a request from the Describe bar: its answer arrives here.
    #[rust]
    agent_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// Songs open in background tabs (the one in front is `state`).
    #[rust]
    others: Vec<EditorState>,
    /// Tab order: project files.
    #[rust]
    tabs: Vec<PathBuf>,
    /// The audio output's state, shared by every song (made with the first).
    #[rust]
    shared: Option<std::sync::Arc<audio::Shared>>,
    /// Services open plugin windows (and knob moves waiting for a busy plugin).
    #[rust]
    plugin_pump: Timer,
    /// While the keys play a MIDI lane: shows its meter moving (also stopped)
    /// and takes in a recording's keys.
    #[rust]
    keys_pump: Timer,
    /// MIDI keyboards plugged in (none: the computer keyboard plays).
    #[rust]
    midi_inputs: usize,
    /// The computer keyboard as a piano: its octave (C of it) and the keys down.
    #[rust]
    piano_octave: Option<u8>,
    #[rust]
    piano_down: Vec<(KeyCode, u8)>,
    /// Slower, always: plugins whose process died or whose latency changed.
    #[rust]
    plugin_watch: Timer,
    /// A remix waiting for the answer to "this song already has a project":
    /// (the song, the prompt, its existing project).
    #[rust]
    remix_choice: Option<(PathBuf, String, PathBuf)>,
    /// The model manager: the download that runs, if any.
    #[rust]
    downloads: Downloads,
    /// A download offered in the offer bar, and what to do once it is in.
    #[rust]
    offer: Option<model_view::Offer>,
    /// Live mode (two decks from the OMNIS-DUO) is showing, its readouts'
    /// timer, the controller map and its ports, and deck songs the audio
    /// thread may still hold.
    #[rust]
    live_shown: bool,
    #[rust]
    live_pump: Timer,
    #[rust]
    live_map: Option<std::sync::Arc<deckmap::Map>>,
    #[rust]
    live_ports: (Vec<u64>, Vec<u64>),
    #[rust]
    live_retired: Vec<std::sync::Arc<decks::DeckSong>>,
}

/// The song a job belongs to, in front or in a background tab.
fn song_in<'a>(front: &'a mut Option<EditorState>, others: &'a mut [EditorState], bundle: &Path) -> Option<&'a mut EditorState> {
    if front.as_ref().is_some_and(|st| st.path.parent() == Some(bundle)) {
        return front.as_mut();
    }
    others.iter_mut().find(|st| st.path.parent() == Some(bundle))
}

/// The song (bundle folder) a job update is about.
fn update_bundle(u: &JobUpdate) -> Option<PathBuf> {
    match u {
        JobUpdate::Progress { bundle, .. }
        | JobUpdate::Failed { bundle, .. }
        | JobUpdate::Analyzed { bundle, .. }
        | JobUpdate::TakeReady { bundle, .. }
        | JobUpdate::MidiReady { bundle, .. }
        | JobUpdate::FxReady { bundle, .. }
        | JobUpdate::Finished { bundle, .. } => Some(bundle.clone()),
        _ => None,
    }
}

/// The inspector's six knob sliders, set to these (name, value, min, max).
fn fill_knobs(ui: &WidgetRef, cx: &mut Cx, knobs: &[(String, f64, f64, f64)]) {
    let sliders = [ids!(fx_k0), ids!(fx_k1), ids!(fx_k2), ids!(fx_k3), ids!(fx_k4), ids!(fx_k5)];
    let boxes = [ids!(fx_b0), ids!(fx_b1), ids!(fx_b2), ids!(fx_b3), ids!(fx_b4), ids!(fx_b5)];
    for (k, id) in sliders.iter().enumerate() {
        ui.view(cx, boxes[k]).set_visible(cx, k < knobs.len());
        let mut w = ui.widget(cx, *id);
        if let Some((name, value, min, max)) = knobs.get(k) {
            let (name, min, max) = (name.replace('_', " "), *min, *max);
            script_apply_eval!(cx, w, {text: #(name) min: #(min) max: #(max)});
            ui.slider(cx, *id).set_value(cx, *value);
        }
    }
}

/// The start screen's file dialogs: a song to split, or to remix.
const SPLIT_DIALOG: LiveId = live_id!(poing_split_song);
const REMIX_DIALOG: LiveId = live_id!(poing_remix_song);

fn choose_song(cx: &mut Cx, id: LiveId, title: &str) {
    let dialog = FileDialog::new()
        .set_id(id)
        .set_title(title.to_string())
        .add_filter("Songs".to_string(), AUDIO_EXT.iter().map(|e| e.to_string()).collect());
    cx.open_select_file_dialog(dialog);
}

/// Songs open at once (each keeps its stems and takes decoded in memory).
const MAX_TABS: usize = 6;

/// One spelling of a project path, to tell whether a song is already open.
fn same_song(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| {
        let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        c.to_string_lossy().trim_start_matches(r"\\?\").to_lowercase()
    };
    canon(a) == canon(b)
}

/// Audio Poing can import (what makepad-audio-decode reads).
const AUDIO_EXT: [&str; 4] = ["mp3", "wav", "flac", "ogg"];

/// A request whose job has not finished, kept in the song's folder
/// (`jobs/pending.json`) so one cut off by closing Poing can be made again.
#[derive(SerJson, DeJson, Clone, Debug)]
pub struct Request {
    /// The Poing run that asked (requests of another run were cut off).
    pub run: u64,
    /// The job number; u64::MAX while it waits (a remix waits for the analysis).
    pub seq: u64,
    /// "describe" (the Describe bar, on `lane` / `section` / the song) or "remix".
    pub kind: String,
    pub text: String,
    pub lane: Option<String>,
    pub section: Option<usize>,
}

/// Keep a request until its job is done (`seq_before`: the last job number
/// before it was made; nothing new submitted, nothing to keep). A remix
/// waits for its song's analysis (seq u64::MAX until it is submitted).
#[allow(clippy::too_many_arguments)]
fn note_request_in(requests: &mut Vec<(PathBuf, Request)>, jobs: Option<&Jobs>, bundle: &Path, seq_before: u64, kind: &str, text: &str, lane: Option<String>, section: Option<usize>) {
    let seq = match (jobs, kind) {
        (_, "remix") => u64::MAX,
        (Some(jobs), _) if jobs.last_seq() > seq_before => jobs.last_seq(),
        _ => return,
    };
    requests.push((bundle.to_path_buf(), Request { run: run_id(), seq, kind: kind.into(), text: text.into(), lane, section }));
    let mine: Vec<Request> = requests.iter().filter(|(b, _)| b == bundle).map(|(_, r)| r.clone()).collect();
    write_requests(bundle, &mine);
}

/// This run of Poing (its start time and process id).
fn run_id() -> u64 {
    static RUN: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *RUN.get_or_init(|| {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        t.wrapping_mul(1000).wrapping_add(std::process::id() as u64)
    })
}

fn requests_file(bundle: &Path) -> PathBuf {
    bundle.join("jobs").join("pending.json")
}

fn read_requests(bundle: &Path) -> Vec<Request> {
    std::fs::read_to_string(requests_file(bundle)).ok().and_then(|t| Vec::<Request>::deserialize_json_lenient(&t).ok()).unwrap_or_default()
}

fn write_requests(bundle: &Path, list: &[Request]) {
    let file = requests_file(bundle);
    if list.is_empty() {
        let _ = std::fs::remove_file(&file);
        return;
    }
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, list.to_vec().serialize_json());
}

/// Poing's own settings (`settings.json` in its config folder).
#[derive(SerJson, DeJson, Clone, Debug, Default)]
pub struct Settings {
    /// The sound output by name (None: the system default).
    pub audio_output: Option<String>,
    /// Who writes and answers: "local" (Poing's own model) or "claude"
    /// (None: local, unless `POING_LLM` says otherwise).
    pub ai: Option<String>,
}

fn load_settings() -> Settings {
    config_dir().and_then(|d| std::fs::read_to_string(d.join("settings.json")).ok()).and_then(|t| Settings::deserialize_json_lenient(&t).ok()).unwrap_or_default()
}

fn save_settings(s: &Settings) {
    if let Some(d) = config_dir() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("settings.json"), s.serialize_json());
    }
}

/// What the command line asks to open: a project, or a song to import.
enum Open {
    Project(PathBuf),
    Audio(PathBuf),
}

/// Is a text field (or a slider's typed value) taking the keyboard? Then
/// keys are text; otherwise they are shortcuts (a clicked button keeps the
/// focus but does not want keys).
/// MIDI keyboards: a thread takes their notes as they come and queues them
/// straight to the audio thread (the selected MIDI lane plays them). The live
/// controller's port goes to the decks instead (while live mode is on), and
/// its lights are written back from here.
fn start_midi_input(cx: &mut Cx, shared: std::sync::Arc<audio::Shared>, map: std::sync::Arc<deckmap::Map>) {
    let mut input = cx.midi_input();
    let output = cx.midi_output();
    let _ = std::thread::Builder::new().name("poing-midi".into()).spawn(move || {
        let mut dec = deckmap::Decoder::default();
        let mut leds = decks::Leds::default();
        let mut lit_at = std::time::Instant::now();
        let mut was_on = false;
        // injected test messages arrive on the port named after the map's
        let injected = makepad_widgets::makepad_platform::midi_inject::port_id_for("OMNIS-DUO").0 .0;
        loop {
            while let Some((port, data)) = input.receive() {
                if shared.decks.is_port(port.0 .0) || port.0 .0 == injected {
                    if shared.decks.on() {
                        shared.decks.midi(&map, &mut dec, data.data);
                    }
                    continue;
                }
                if let MidiEvent::Note(n) = data.decode() {
                    shared.live.key(n.note_number, n.velocity, n.is_on);
                }
            }
            let on = shared.decks.on();
            if on && lit_at.elapsed().as_millis() >= 30 {
                lit_at = std::time::Instant::now();
                if !was_on {
                    leds.reset();
                }
                let mut ports: Vec<u64> = shared.decks.outputs().collect();
                if ports.is_empty() {
                    ports.push(injected);
                }
                for m in leds.update(&map, &shared.decks) {
                    for p in &ports {
                        output.send(Some(MidiPortId(LiveId(*p))), MidiData { data: m });
                    }
                }
            }
            was_on = on;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    });
}

impl App {
    /// The computer keyboard as a piano for the selected MIDI lane when no
    /// MIDI keyboard is plugged in: A W S E D F T G Y H U J K are the keys
    /// from C up, Z and X go an octave down and up. True when the key was
    /// the piano's (then it is no shortcut: S solos and F fits elsewhere).
    fn piano(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let (ke, down) = match event {
            Event::KeyDown(ke) => (ke, true),
            Event::KeyUp(ke) => (ke, false),
            _ => return false,
        };
        let Some(st) = self.state.as_mut() else { return false };
        // a key let go ends its note, whatever happened since it went down
        if !down {
            let Some(i) = self.piano_down.iter().position(|(k, _)| *k == ke.key_code) else { return false };
            let (_, pitch) = self.piano_down.remove(i);
            st.shared.live.key(pitch, 0, false);
            return true;
        }
        let m = &ke.modifiers;
        if self.midi_inputs > 0 || m.control || m.logo || m.alt || st.keys_lane().is_none() || typing(&self.ui, cx) {
            return false;
        }
        let octave = self.piano_octave.get_or_insert(60);
        let shift = match ke.key_code {
            KeyCode::KeyZ => Some(-12),
            KeyCode::KeyX => Some(12),
            _ => None,
        };
        if let Some(by) = shift {
            *octave = (*octave as i32 + by).clamp(12, 108) as u8;
            st.notice = Some(format!("The keys A to K play from C{}", *octave as i32 / 12 - 1));
            return true;
        }
        let Some(semitone) = live::piano_key(ke.key_code) else { return false };
        if ke.is_repeat || self.piano_down.iter().any(|(k, _)| *k == ke.key_code) {
            return true;
        }
        let pitch = (*octave + semitone).min(127);
        st.shared.live.key(pitch, 100, true);
        self.piano_down.push((ke.key_code, pitch));
        true
    }
}

fn typing(ui: &WidgetRef, cx: &Cx) -> bool {
    let focus = cx.key_focus();
    if focus.is_empty() {
        return false;
    }
    let fields = [ids!(describe_input), ids!(note_input), ids!(section_input), ids!(words_input), ids!(remix_input), ids!(new_input)];
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
        // copies made by test runs live in the temp folder
        .filter(|p| !p.starts_with(std::env::temp_dir()) && !p.to_string_lossy().to_lowercase().contains("\\appdata\\local\\temp\\"))
        .take(8)
        .collect()
}

fn remember_song(project: &Path) {
    let Some(dir) = config_dir() else { return };
    // test runs (scratch copies in the temp folder) are not songs to come back to
    let temp = std::env::temp_dir();
    if project.starts_with(&temp) || std::fs::canonicalize(project).ok().zip(std::fs::canonicalize(&temp).ok()).is_some_and(|(p, t)| p.starts_with(t)) {
        return;
    }
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

/// The project a song was imported into before, if any.
fn existing_project(audio: &Path) -> Option<PathBuf> {
    let name = audio.file_stem()?.to_str()?;
    Some(audio.with_file_name(format!("{name}.poing")).join("project.json")).filter(|p| p.exists())
}

/// Import a song into a new project next to it even when one exists
/// (`<song> 2.poing`, `<song> 3.poing`, ...). Returns the project file.
fn import_new(audio: &Path, sr: u32) -> Result<PathBuf, String> {
    let name = audio.file_stem().and_then(|s| s.to_str()).ok_or("bad file name")?.to_string();
    let bundle = (2..1000)
        .map(|n| audio.with_file_name(format!("{name} {n}.poing")))
        .find(|b| !b.exists())
        .ok_or("no free project name")?;
    std::fs::create_dir_all(&bundle).map_err(|e| format!("create {}: {e}", bundle.display()))?;
    let ext = audio.extension().and_then(|e| e.to_str()).unwrap_or("wav").to_ascii_lowercase();
    let source = format!("original.{ext}");
    std::fs::copy(audio, bundle.join(&source)).map_err(|e| format!("copy {}: {e}", audio.display()))?;
    let original = audio::load_stem(&bundle.join(&source), sr)?;
    let duration = original.l.len() as f64 / sr as f64;
    let project_file = bundle.join("project.json");
    let title = bundle.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name);
    Project::import(&title, &source, duration, sr).save(&project_file)?;
    Ok(project_file)
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

/// A remix prompt cut to a chip's length ("80s disco techno, femal…").
fn short_prompt(prompt: &str) -> String {
    let p = prompt.trim();
    if p.chars().count() <= 24 { p.to_string() } else { format!("{}…", p.chars().take(23).collect::<String>().trim_end()) }
}

/// The remix asked for when the song was dropped (once it is understood):
/// three ACE-Step covers, close to free, each a song take.
fn start_remix(st: &mut EditorState, jobs: &Jobs) -> String {
    if st.pending_remix.is_none() {
        return String::new();
    }
    if let Some(note) = model_view::gate(st, "remix", model_view::Resume::Remix(st.path.clone())) {
        return note;
    }
    let Some(prompt) = st.pending_remix.take() else { return String::new() };
    let bundle = st.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let Some(source) = st.project.source.clone() else { return "This song has no audio to remix".into() };
    let a = st.project.analysis.clone().unwrap_or_default();
    let lyrics = std::fs::read_to_string(bundle.join("lyrics.txt")).unwrap_or_else(|_| st.project.sung_lyrics()).replace("\r\n", "\n");
    let how = [(0.6, "close"), (0.45, "looser"), (0.3, "free")];
    let mut takes = Vec::new();
    for (_, name) in how.iter() {
        let (id, seed) = st.project.next_take();
        takes.push(Take {
            file: format!("takes/{id}.wav"),
            id,
            scope: "song".into(),
            start: 0.0,
            end: 0.0,
            engine: "acestep-cover".into(),
            // the prompt names the batch: several remix batches stay apart
            prompt: format!("{} · {name}", short_prompt(&prompt)),
            seed,
            track: None,
            blend: None,
            region: None,
            kept: None,
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
    // a chord or lyric line is selected to correct its words: Describe works on the song
    if let Selection::Chord(_) | Selection::Lyric(_) = st.sel {
        st.select(Selection::None);
    }
    // A marked region made again: "regenerate", "fix this", "make this part better"
    if let Selection::Region(r) = st.sel {
        if wants_regenerate(st, r, text) {
            if st.analysing.is_some() {
                return "A job is still running; make this part again when it is done.".into();
            }
            return regenerate_region(st, jobs, r, text);
        }
    }
    // "Fix all the bad spots": every region marked bad
    if st.sel == Selection::None && model::is_fix_marks_text(text) && st.project.regions().iter().any(|r| r.kind == "bad") {
        if st.analysing.is_some() {
            return "A job is still running; fix the bad spots when it is done.".into();
        }
        return fix_marks(st, jobs, text);
    }
    if !st.project.is_analysed() {
        if st.analysing.is_none() {
            return "This song isn't split into parts yet. Mark a range on it (drag on the lane) to repaint just that, or download the analysis in Settings → Models.".into();
        }
        return "Poing is still listening to the song. Describe it again when the lanes are in.".into();
    }
    if st.analysing.is_some() {
        return "A job is still running; describe the next take when it is done.".into();
    }
    let bundle = st.path.parent().map(Path::to_path_buf).unwrap_or_default();
    // The song's order: "repeat the chorus", "cut the second verse", "make it 30 s longer"
    if let Selection::None | Selection::Section(_) = st.sel {
        let selected = if let Selection::Section(i) = st.sel { Some(i) } else { None };
        match model::arrange::arrange_intent(text, st.project.sections(), selected) {
            Some(Ok(model::arrange::Arrange::Op(op))) => return st.arrange(&op).unwrap_or_else(|e| format!("Could not change the arrangement: {e}")),
            Some(Ok(model::arrange::Arrange::Extend(secs))) => return extend_song(st, jobs, bundle, secs, text),
            Some(Err(e)) => return format!("Could not change the arrangement: {e}"),
            None => {}
        }
    }
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
    // With nothing selected, anything else goes to the assistant (from the bar only):
    // it can change the mix with Poing's tools, or still ask for a remix.
    if st.sel == Selection::None && st.agent_ok {
        match agent::unavailable() {
            None => {
                if let Some(note) = llm::feature().and_then(|f| model_view::gate(st, f, model_view::Resume::Ask(st.path.clone(), text.to_string()))) {
                    return note;
                }
                st.ask_agent = Some(text.to_string());
                return format!("{} is working on it (it uses Poing's own tools; Ctrl+Z undoes the whole pass)...", llm::name());
            }
            Some(why) if agent::is_mix_text(text) => return format!("Changing the mix in words needs {} here, but {why}. Use the lane levels and chips meanwhile.", llm::name()),
            Some(_) => {}
        }
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
        region: None,
        kept: None,
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
    repaint_range(st, jobs, bundle, (section.start, section.end), &section.label, text, lyrics)
}

/// ACE-Step repaints [start, end] of the song (a section, or a range marked on it).
fn repaint_range(st: &mut EditorState, jobs: &Jobs, bundle: PathBuf, (start, end): (f64, f64), label: &str, text: &str, lyrics: String) -> String {
    let Some(source) = st.project.source.clone() else { return "This project has no source audio to repaint".into() };
    if let Some(note) = model_view::gate(st, "repaint", model_view::Resume::Describe(st.path.clone(), text.to_string())) {
        return note;
    }
    let (id, seed) = st.project.next_take();
    let n = id.trim_start_matches('t').to_string();
    let take = Take {
        file: format!("takes/{id}.wav"),
        id,
        scope: "section".into(),
        start,
        end,
        engine: "acestep".into(),
        prompt: text.to_string(),
        seed,
        track: None,
        blend: None,
        region: None,
        kept: None,
    };
    let a = st.project.analysis.clone().unwrap_or_default();
    let job = RepaintJob {
        start,
        end,
        margin: audio::TAKE_MARGIN_S,
        caption: text.to_string(),
        lyrics,
        seed,
        bpm: a.tempo.map(f64::round),
        key: a.key,
        ..Default::default()
    };
    if !jobs.submit(Job::Repaint { src: bundle.join(source), bundle, takes: vec![take], job, sr: st.sr() }) {
        return "The job worker is not running".into();
    }
    st.analysing = Some((format!("Take {n}"), 0.0));
    let _ = st.save();
    format!("Repainting the {} ({:.0}s - {:.0}s) as take {n} (under a minute)...", label, start, end)
}

/// What the region inspector says a mark does (or what it made).
fn region_hint(st: &EditorState, r: &model::Region) -> String {
    let versions = st.region_versions(&r.id);
    if !versions.is_empty() {
        let ids = versions.iter().map(|t| t.trim_start_matches('t')).collect::<Vec<_>>().join(", ");
        return format!("   ·   versions: takes {ids} (click them above the lanes, then Keep this version)");
    }
    // only drums and bass are played again on their own; elsewhere the mix is repainted
    let alone = st.project.is_analysed() && matches!(r.track.as_str(), "drums" | "bass");
    let what = if alone { "this lane" } else { "the whole mix here" };
    match r.kind.as_str() {
        "bad" | "regenerate" => format!("   ·   choose regenerate, or Describe \"fix this\", to make {what} again in 3 versions"),
        _ => String::new(),
    }
}

/// How many versions a marked region is made in: compare them, keep one.
const REGION_VERSIONS: usize = 3;

/// Does Describe on region `r` ask for it to be made again? Words that say so
/// ("fix this", "again"), or anything but an effect, an echo change or a new
/// part on a region marked bad or regenerate. A song not split into lanes
/// yet can only repaint it.
fn wants_regenerate(st: &EditorState, r: usize, text: &str) -> bool {
    let Some(reg) = st.project.regions().get(r) else { return false };
    if !st.project.is_analysed() {
        return true;
    }
    let track = st.project.track_index(&reg.track).map(|i| st.project.tracks[i].id.clone()).unwrap_or_default();
    model::region_regenerates(text, &track, reg.kind == "bad" || reg.kind == "regenerate")
}

/// What ACE-Step is told the new part should be: the Describe words (unless
/// they only ask for it again) and the region's note. Neither: the song as it is.
fn region_caption(text: &str, note: &str) -> String {
    let said = Some(text.trim()).filter(|t| !t.is_empty() && !model::is_fix_text(t));
    let parts: Vec<&str> = said.into_iter().chain(Some(note.trim()).filter(|n| !n.is_empty())).collect();
    if parts.is_empty() {
        "the same song in the same style, played again".into()
    } else {
        parts.join(". ")
    }
}

/// Make marked region `r` again in `REGION_VERSIONS` versions, over the
/// range widened to whole bars, with its note as the hint. On a drums or bass
/// lane lego plays just that lane again (lane takes); anywhere else ACE-Step
/// repaints the mix there (section takes). The versions share one slot, so
/// clicking one above the lanes switches to it; the newest plays.
fn regenerate_region(st: &mut EditorState, jobs: &Jobs, r: usize, text: &str) -> String {
    let Some(reg) = st.project.regions().get(r).cloned() else { return "No such region".into() };
    let Some(lane) = st.project.track_index(&reg.track) else { return "That region's lane is gone".into() };
    let track = st.project.tracks[lane].clone();
    if track.is_midi() {
        return format!("{} is a MIDI part: select the lane and Describe it to have it written again.", track.name);
    }
    let Some(source) = st.project.source.clone() else { return "This project has no source audio to repaint".into() };
    let resume = if text.trim().is_empty() { "regenerate" } else { text };
    if let Some(note) = model_view::gate(st, "repaint", model_view::Resume::Describe(st.path.clone(), resume.to_string())) {
        return note;
    }
    let bundle = st.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let a = st.project.analysis.clone().unwrap_or_default();
    let (start, end) = model::pad_to_downbeats(reg.start, reg.end, a.downbeats.as_deref().unwrap_or(&[]));
    let caption = region_caption(text, reg.note.as_deref().unwrap_or(""));
    let prompt = if text.trim().is_empty() { "regenerate".to_string() } else { text.trim().to_string() };
    let mut takes = Vec::new();
    for _ in 0..REGION_VERSIONS {
        let (id, seed) = st.project.next_take();
        takes.push(Take {
            file: format!("takes/{id}.wav"),
            id,
            scope: "section".into(),
            start,
            end,
            engine: "acestep".into(),
            prompt: prompt.clone(),
            seed,
            track: None,
            blend: None,
            region: Some(reg.id.clone()),
            kept: None,
        });
    }
    let ids = takes.iter().map(|t| t.id.trim_start_matches('t')).collect::<Vec<_>>().join(", ");
    let span = format!("{start:.1}s - {end:.1}s");
    // Only drums and bass can be played again on their own (lego, then cleaned to the lane's stem).
    let lego = st.project.is_analysed() && matches!(track.id.as_str(), "drums" | "bass");
    if lego {
        let lane_file = st.project.track_path(&st.path, lane).to_string_lossy().into_owned();
        for mut take in takes {
            take.scope = "lane".into();
            take.engine = "lego".into();
            take.track = Some(track.id.clone());
            take.blend = Some(1.0);
            let lego = LegoJob {
                src: bundle.join(&source).to_string_lossy().into_owned(),
                track: track.id.clone(),
                caption: caption.clone(),
                seed: take.seed,
                bpm: a.tempo.map(f64::round),
                key: a.key.clone(),
                ..Default::default()
            };
            let clean = LayerJob { stem: demucs_stem_for(&track.id).to_string(), match_lane: Some(lane_file.clone()), start, end, ..Default::default() };
            if !jobs.submit(Job::Layer { bundle: bundle.clone(), take, lego, clean, sr: st.sr() }) {
                return "The job worker is not running".into();
            }
        }
        st.analysing = Some((format!("Takes {ids}"), 0.0));
        let _ = st.save();
        return format!("Playing {} again over {span} (whole bars) in {REGION_VERSIONS} versions, takes {ids} (about a minute and a half each)...", track.name);
    }
    let lyrics = std::fs::read_to_string(bundle.join("lyrics.txt")).unwrap_or_else(|_| st.project.sung_lyrics()).replace("\r\n", "\n");
    let job = RepaintJob {
        start,
        end,
        margin: audio::TAKE_MARGIN_S,
        caption,
        lyrics,
        bpm: a.tempo.map(f64::round),
        key: a.key,
        ..Default::default()
    };
    if !jobs.submit(Job::Repaint { src: bundle.join(source), bundle, takes, job, sr: st.sr() }) {
        return "The job worker is not running".into();
    }
    st.analysing = Some((format!("Takes {ids}"), 0.0));
    let _ = st.save();
    // say so when the lane alone is not what is made again
    let whole = if st.project.tracks.len() > 1 {
        format!(" This repaints the whole mix there, not just {}: only drums and bass can be played again on their own.", track.name)
    } else {
        String::new()
    };
    format!("Repainting {span} (whole bars) in {REGION_VERSIONS} versions, takes {ids} (about a minute).{whole}")
}

/// Every region marked bad made again, each in its versions (one job each).
fn fix_marks(st: &mut EditorState, jobs: &Jobs, text: &str) -> String {
    let bad: Vec<usize> = st.project.regions().iter().enumerate().filter(|(_, r)| r.kind == "bad").map(|(i, _)| i).collect();
    let mut lines = Vec::new();
    for r in bad.iter().copied() {
        let (start, end) = (st.project.regions()[r].start, st.project.regions()[r].end);
        lines.push(format!("{start:.0}-{end:.0}s: {}", regenerate_region(st, jobs, r, text)));
    }
    format!("Making the {} bad spot{} again. {}", bad.len(), if bad.len() == 1 { "" } else { "s" }, lines.join(" "))
}

/// "Make it 30 s longer", "add an outro": ACE-Step makes the last bars again
/// and goes on past the end. The take that comes back makes the song longer.
fn extend_song(st: &mut EditorState, jobs: &Jobs, bundle: PathBuf, secs: f64, text: &str) -> String {
    let Some(source) = st.project.source.clone() else { return "This project has no source audio to extend".into() };
    if let Some(note) = model_view::gate(st, "repaint", model_view::Resume::Describe(st.path.clone(), text.to_string())) {
        return note;
    }
    let end = st.project.duration;
    // the source goes on to the song's end (an extension made before Poing kept it so does not)
    if let Some(i) = st.project.tracks.iter().position(|t| t.file == source) {
        if (st.stems[i].l.len() as f64) < (end - 1.0) * st.sr() as f64 {
            return "The song's source audio stops before its end, so ACE-Step has nothing to go on from. Undo back to before it was made longer, then make it longer in one go.".into();
        }
    }
    let a = st.project.analysis.clone().unwrap_or_default();
    // from a downbeat a few seconds before the end: the song carries on rather than stopping and starting again
    let from = a.downbeats.iter().flatten().copied().filter(|d| *d <= end - 4.0 && *d >= end - 12.0).last().unwrap_or((end - 6.0).max(0.0));
    let (id, seed) = st.project.next_take();
    let n = id.trim_start_matches('t').to_string();
    let take = Take {
        file: format!("takes/{id}.wav"),
        id,
        scope: "section".into(),
        start: from,
        end: end + secs,
        engine: state::arrange::EXTEND.into(),
        prompt: text.to_string(),
        seed,
        track: None,
        blend: None,
        region: None,
        kept: None,
    };
    let job = RepaintJob {
        start: from,
        end: end + secs,
        margin: audio::TAKE_MARGIN_S,
        caption: "the song goes on in the same style, then comes to its end".into(),
        lyrics: "[Outro]\n[Instrumental]".into(),
        seed,
        bpm: a.tempo.map(f64::round),
        key: a.key,
        ..Default::default()
    };
    if !jobs.submit(Job::Repaint { src: bundle.join(source), bundle, takes: vec![take], job, sr: st.sr() }) {
        return "The job worker is not running".into();
    }
    st.analysing = Some((format!("Take {n}"), 0.0));
    let _ = st.save();
    format!("Making the song {secs:.0} s longer as take {n} (about a minute)...")
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
            format!("{name} is on {place}. Its main knobs are below; \"Open its window\" shows all of it.")
        }
        Err(e) => format!("{name} could not be added: {e}"),
    })
}

/// Claude writes a MIDI part for the selected section (or the whole song).
fn write_midi(st: &mut EditorState, jobs: &Jobs, bundle: PathBuf, role: &str, text: &str) -> String {
    if let Some(note) = llm::feature().and_then(|f| model_view::gate(st, f, model_view::Resume::Describe(st.path.clone(), text.to_string()))) {
        return note;
    }
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
    format!("{} is writing a {role} part for {place} from the song's key, chords and sections (under a minute)...", llm::name())
}

/// Claude writes an effect for a lane (or the master, `None`) from `text`.
fn write_effect(st: &mut EditorState, jobs: &Jobs, bundle: PathBuf, lane: Option<usize>, text: &str) -> String {
    if let Some(note) = llm::feature().and_then(|f| model_view::gate(st, f, model_view::Resume::Describe(st.path.clone(), text.to_string()))) {
        return note;
    }
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
    format!("{} is writing \"{text}\" for {place}; Poing checks it before you hear it (under a minute)...", llm::name())
}

/// Start the assistant on `request` (one undo step for the whole pass); its
/// answer arrives on the receiver.
fn start_agent(st: &mut EditorState, request: String) -> std::sync::mpsc::Receiver<Result<String, String>> {
    st.checkpoint();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let work = st.path.parent().map(|p| p.join("agent").join(stamp.to_string())).unwrap_or_default();
    let (tx, rx) = std::sync::mpsc::channel();
    agent::run(request, work, tx);
    rx
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
        region: None,
        kept: None,
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

const HELP: &str = "Click a lane, section or take  ·  drag across a lane to mark a region  ·  Space play  ·  L loop selection  ·  F fit  ·  B split section  ·  R record a MIDI lane  ·  Ctrl+wheel zoom  ·  Del delete  ·  Ctrl+Z undo";

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
        // two songs with one name: say which folder each is in
        let names: Vec<String> = songs.iter().map(|p| song_name(p)).collect();
        let label = |i: usize| {
            let twin = names.iter().filter(|n| **n == names[i]).count() > 1;
            let folder = songs[i].parent().and_then(|b| b.parent()).and_then(|d| d.file_name()).map(|d| d.to_string_lossy().into_owned());
            match (twin, folder) {
                (true, Some(d)) => format!("{}  ·  {d}", names[i]),
                _ => names[i].clone(),
            }
        };
        for (i, b) in buttons.iter().enumerate() {
            let btn = ui.button(cx, *b);
            match songs.get(i) {
                Some(_) => {
                    btn.set_text(cx, &label(i));
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
        self.export_shown = false;
        ui.view(cx, ids!(export_sheet)).set_visible(cx, false);
        ui.view(cx, ids!(describe_frame)).set_visible(cx, song);
        ui.button(cx, ids!(render_btn)).set_visible(cx, song);
        ui.button(cx, ids!(live_btn)).set_visible(cx, song);
        if !song {
            ui.button(cx, ids!(play_btn)).set_visible(cx, false);
            ui.button(cx, ids!(pause_btn)).set_visible(cx, false);
            ui.label(cx, ids!(project_label)).set_text(cx, "Poing");
            ui.label(cx, ids!(project_meta)).set_text(cx, "");
        }
        for id in [ids!(timeline_box), ids!(inspector_box)] {
            ui.view(cx, id).set_visible(cx, song && !self.live_shown);
        }
        ui.view(cx, ids!(live_view)).set_visible(cx, song && self.live_shown);
        self.refresh_tabs(cx);
        ui.redraw(cx);
    }

    /// Open a project or import a song. The audio output is installed once;
    /// later songs reuse its Shared and swap their stems in by generation.
    fn open(&mut self, cx: &mut Cx, target: Open) {
        let path = match target {
            Open::Project(path) => Ok(path),
            Open::Audio(audio) => import(&audio, 48000).map(|(p, _)| p),
        };
        let path = match path {
            Ok(p) => p,
            Err(e) => return self.open_failed(cx, &e),
        };
        // Already open: its tab comes to the front.
        if self.state.as_ref().is_some_and(|st| same_song(&st.path, &path)) {
            self.show_song_view(cx, true);
            self.refresh_tabs(cx);
            return;
        }
        if let Some(open) = self.others.iter().find(|st| same_song(&st.path, &path)).map(|st| st.path.clone()) {
            self.switch_to(cx, &open);
            return;
        }
        if self.agent_rx.is_some() {
            return self.set_status(cx, "The assistant is still working on this song; open the other one when it is done.");
        }
        let loaded = (|| -> Result<(Project, Vec<Stem>, bool), String> {
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
            Ok((project, stems, analyse))
        })();
        let (project, stems, mut analyse) = match loaded {
            Ok(x) => x,
            Err(e) => return self.open_failed(cx, &e),
        };
        // The song in front goes to a background tab first.
        let handover = self.state.take().map(|mut old| {
            let _ = old.save();
            let given = old.deactivate();
            self.others.push(old);
            given
        });
        let first = self.shared.is_none();
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let sr = project.sr();
        let mut st = EditorState::new(project, path.clone(), stems, self.shared.clone());
        self.shared = Some(st.shared.clone());
        if let Some((_controls, sets)) = handover {
            // its plugins and stem sets may still be in the audio thread's hands
            #[cfg(windows)]
            st.adopt_plugin_controls(_controls);
            for set in sets {
                st.retire(set);
            }
        }
        st.restore_combined();
        st.restore_kept();
        st.restore_extensions();
        // Analysed while another song was open: apply it from disk, no second run.
        if analyse {
            if let Some((analysis, stems)) = finished_analysis(&dir, sr) {
                if st.apply_analysis(analysis, stems).is_ok() {
                    analyse = false;
                }
            }
        }
        show_title(&self.ui, cx, &st.project);
        remember_song(&st.path);
        if first {
            audio::start_output(cx, st.shared.clone(), st.sr());
            start_midi_input(cx, st.shared.clone(), self.live_map());
        }
        if analyse {
            self.start_analysis(&mut st);
        }
        self.state = Some(st);
        self.tabs.retain(|p| !same_song(p, &path));
        self.tabs.push(path);
        // too many open: the one longest in the background closes (it is saved)
        while self.tabs.len() > MAX_TABS {
            let Some(i) = self.tabs.iter().position(|p| self.others.iter().any(|o| same_song(&o.path, p))) else { break };
            let p = self.tabs.remove(i);
            if let Some(k) = self.others.iter().position(|o| same_song(&o.path, &p)) {
                let mut gone = self.others.remove(k);
                let _ = gone.save();
            }
        }
        self.show_song_view(cx, true);
        self.refresh_tabs(cx);
        self.offer_interrupted(cx);
    }

    fn open_failed(&mut self, cx: &mut Cx, e: &str) {
        log!("poing: load failed: {e}");
        if self.state.is_some() && !self.home_shown {
            self.set_status(cx, &format!("Could not open that: {e}"));
        } else {
            self.show_home(cx, Some(&format!("Could not open that: {e}")));
        }
    }

    /// Bring a background song to the front. Playing on, it plays on at the
    /// same time in the other song, so versions and songs compare by ear.
    fn switch_to(&mut self, cx: &mut Cx, path: &Path) {
        if self.agent_rx.is_some() {
            return self.set_status(cx, "The assistant is still working on this song; switch when it is done.");
        }
        let Some(i) = self.others.iter().position(|st| same_song(&st.path, path)) else { return };
        let mut next = self.others.remove(i);
        if let Some(mut cur) = self.state.take() {
            let (playing, at) = (cur.shared.playing(), cur.playhead());
            let _ = cur.save();
            let (_controls, sets) = cur.deactivate();
            #[cfg(windows)]
            next.adopt_plugin_controls(_controls);
            for set in sets {
                next.retire(set);
            }
            next.activate(playing.then_some(at));
            self.others.push(cur);
        } else {
            next.activate(None);
        }
        show_title(&self.ui, cx, &next.project);
        self.state = Some(next);
        self.show_song_view(cx, true);
        self.refresh_tabs(cx);
        self.refresh_inspector(cx);
    }

    /// Close a song's tab (it is saved). The one in front hands over to the
    /// next tab; the last open song cannot close (Songs goes to the start screen).
    fn close_tab(&mut self, cx: &mut Cx, path: &Path) {
        let in_front = self.state.as_ref().is_some_and(|st| same_song(&st.path, path));
        if in_front {
            let Some(next) = self.tabs.iter().rev().find(|p| !same_song(p, path)).cloned() else {
                return self.set_status(cx, "This is the only open song: Songs (+) goes to the start screen.");
            };
            self.switch_to(cx, &next);
            if self.state.as_ref().is_some_and(|st| same_song(&st.path, path)) {
                return; // the switch was refused
            }
        }
        if let Some(k) = self.others.iter().position(|o| same_song(&o.path, path)) {
            let mut gone = self.others.remove(k);
            let _ = gone.save();
        }
        self.tabs.retain(|p| !same_song(p, path));
        self.refresh_tabs(cx);
    }

    /// The tab strip: a tab per open song, the one in front underlined.
    fn refresh_tabs(&mut self, cx: &mut Cx) {
        let ui = self.ui.clone();
        let front = self.state.as_ref().filter(|_| !self.home_shown).map(|st| st.path.clone());
        let slots = [
            (ids!(tab0), ids!(tab0_btn), ids!(tab0_line)),
            (ids!(tab1), ids!(tab1_btn), ids!(tab1_line)),
            (ids!(tab2), ids!(tab2_btn), ids!(tab2_line)),
            (ids!(tab3), ids!(tab3_btn), ids!(tab3_line)),
            (ids!(tab4), ids!(tab4_btn), ids!(tab4_line)),
            (ids!(tab5), ids!(tab5_btn), ids!(tab5_line)),
        ];
        for (k, (tab, btn, line)) in slots.iter().enumerate() {
            match self.tabs.get(k) {
                Some(p) => {
                    let name = self.state.iter().chain(self.others.iter()).find(|s| same_song(&s.path, p)).map(|s| s.project.name.clone()).unwrap_or_else(|| song_name(p));
                    let busy = self.state.iter().chain(self.others.iter()).find(|s| same_song(&s.path, p)).is_some_and(|s| s.analysing.is_some());
                    ui.button(cx, *btn).set_text(cx, &if busy { format!("{name}  …") } else { name });
                    ui.view(cx, *line).set_visible(cx, front.as_deref().is_some_and(|f| same_song(f, p)));
                    ui.view(cx, *tab).set_visible(cx, true);
                }
                None => ui.view(cx, *tab).set_visible(cx, false),
            }
        }
        ui.view(cx, ids!(tab_strip)).set_visible(cx, !self.tabs.is_empty());
        ui.view(cx, ids!(tab_strip)).redraw(cx);
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
        // the language model writes the words: without it the ACE-Step download would be for nothing
        if let Some(why) = llm::unavailable() {
            ui.label(cx, ids!(home_error)).set_text(cx, &format!("New songs need {} to write the words, but {why}.", llm::name()));
            return;
        }
        if let Some(f) = llm::feature() {
            if self.needs_download(cx, f, model_view::Resume::MakeSong(prompt.trim().to_string())) {
                return;
            }
        }
        if self.needs_download(cx, "new-song", model_view::Resume::MakeSong(prompt.trim().to_string())) {
            return;
        }
        let job = Job::Song { prompt: prompt.trim().to_string(), dir: audio::songs_dir(), count: 3, sr: 48000 };
        let queued = self.jobs.as_ref().is_some_and(|j| j.submit(job));
        let line = if queued { format!("{} is writing the song...", llm::name()) } else { "The job worker is not running".into() };
        ui.label(cx, ids!(home_status)).set_text(cx, &line);
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
        // made before: ask whether these remixes join that project or start a new one
        if let Some(project) = existing_project(&audio) {
            let takes = Project::load(&project).map(|p| p.takes().len()).unwrap_or(0);
            let what = match takes {
                0 => "a project".to_string(),
                1 => "a project with 1 take".to_string(),
                n => format!("a project with {n} takes"),
            };
            ui.label(cx, ids!(remix_choice_label)).set_text(cx, &format!("{} already has {what}.", song_name(&project)));
            ui.view(cx, ids!(remix_choice)).set_visible(cx, true);
            ui.label(cx, ids!(home_error)).set_text(cx, "");
            self.remix_choice = Some((audio, prompt.trim().to_string(), project));
            ui.redraw(cx);
            return;
        }
        self.start_remix_of(cx, Open::Audio(audio), prompt.trim().to_string());
    }

    /// Open the song (`target`) and make its remixes once it is understood.
    fn start_remix_of(&mut self, cx: &mut Cx, target: Open, prompt: String) {
        self.ui.view(cx, ids!(remix_choice)).set_visible(cx, false);
        self.open(cx, target);
        let Some(bundle) = self.state.as_ref().and_then(|st| st.path.parent().map(Path::to_path_buf)) else { return };
        self.note_request(&bundle, 0, "remix", &prompt, None, None);
        let Some(st) = self.state.as_mut() else { return };
        st.pending_remix = Some(prompt.clone());
        // not analysed and not being analysed (its download was declined): remix as it is
        let now = st.project.is_analysed() || st.analysing.is_none();
        let status = match (&self.jobs, now) {
            (Some(jobs), true) => start_remix(st, jobs),
            _ => format!("Poing is listening to the song first; then it makes three remixes as \"{prompt}\""),
        };
        self.set_status(cx, &status);
        self.sync_waiting_requests();
    }
}

/// Understand the song in the background; lanes fill in when it lands.
fn analyse(jobs: Option<&Jobs>, st: &mut EditorState) {
    if let Some(note) = model_view::gate(st, "analysis", model_view::Resume::Analyse(st.path.clone())) {
        // a download on offer shows in the offer bar; a reason it can't run, here
        if st.needs_download.is_none() {
            st.notice = Some(note);
        }
        return;
    }
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
        // an arrangement edit changes the song's length
        show_title(&ui, cx, &st.project);
        let playing = st.shared.playing();
        ui.button(cx, ids!(play_btn)).set_visible(cx, !playing);
        ui.button(cx, ids!(pause_btn)).set_visible(cx, playing);
        ui.view(cx, ids!(topbar)).redraw(cx);
        let (mut show_region, mut show_track, mut show_section, mut show_take) = (false, false, false, false);
        let mut show_words = false;
        let mut show_fx = false;
        match st.sel {
            Selection::Region(i) if i < st.project.regions().len() => {
                let r = st.project.regions()[i].clone();
                let track = st.project.track_index(&r.track).map(|t| st.project.tracks[t].name.clone()).unwrap_or_default();
                ui.label(cx, ids!(insp_title)).set_text(
                    cx,
                    &format!("Region {} on {}   {:.2}s - {:.2}s ({:.2}s){}", r.id, track, r.start, r.end, r.end - r.start, region_hint(st, &r)),
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
                        Some(i) => format!("MIDI, {} notes, played by {}{}", n.len(), i.name, st.plugin_note(&t.id)),
                        None => format!("MIDI, {} notes", n.len()),
                    },
                    None => t.file.clone(),
                };
                ui.label(cx, ids!(insp_title)).set_text(cx, &format!("Track: {}   ({what})", t.name));
                ui.slider(cx, ids!(track_gain)).set_value(cx, t.gain());
                ui.check_box(cx, ids!(mute_cb)).set_active(cx, t.muted(), Animate::No);
                ui.check_box(cx, ids!(solo_cb)).set_active(cx, t.soloed(), Animate::No);
                show_track = true;
                // a MIDI lane records from a MIDI keyboard (or A to K)
                ui.view(cx, ids!(rec_box)).set_visible(cx, t.is_midi());
                if t.is_midi() {
                    let q = poing_model::record::Quantise::ALL.iter().position(|q| *q == st.quantise).unwrap_or(0);
                    ui.drop_down(cx, ids!(quant_dd)).set_selected_item(cx, q);
                    ui.check_box(cx, ids!(overdub_cb)).set_active(cx, st.overdub, Animate::No);
                    ui.button(cx, ids!(rec_btn)).set_text(cx, if st.recording() { "Stop recording (R)" } else { "Record (R)" });
                    let keys = if self.midi_inputs > 0 { "your MIDI keyboard" } else { "a MIDI keyboard, or A to K (Z/X octave)" };
                    ui.label(cx, ids!(insp_title)).set_text(cx, &format!("Track: {}   ({what})   ·   play it from {keys}", t.name));
                }
                // a built-in instrument's knobs, under the lane's level; a plugin's own window
                let knobs = st.inst_knobs(i);
                let plugin = t.instrument.as_ref().is_some_and(|i| i.plugin.is_some());
                if !knobs.is_empty() || plugin {
                    fill_knobs(&ui, cx, &knobs);
                    ui.check_box(cx, ids!(fx_bypass)).set_visible(cx, false);
                    ui.button(cx, ids!(fx_remove)).set_visible(cx, false);
                    ui.button(cx, ids!(fx_editor)).set_visible(cx, plugin);
                    ui.check_box(cx, ids!(fx_isolate)).set_visible(cx, plugin);
                    ui.check_box(cx, ids!(fx_isolate)).set_active(cx, t.instrument.as_ref().is_some_and(|i| i.isolated()), Animate::No);
                    show_fx = true;
                }
            }
            Selection::Fx(lane, i) if i < st.project.chain(lane).len() => {
                let fx = st.project.chain(lane)[i].clone();
                let place = lane.map(|l| st.project.tracks[l].name.clone()).unwrap_or_else(|| "the master".into());
                let state = match (st.fx_errors.get(&fx.id), fx.plugin.is_some()) {
                    (Some(e), true) => format!("   ·   {}", e.chars().take(120).collect::<String>()),
                    (Some(e), false) => format!("   ·   does not compile: {}", e.chars().take(120).collect::<String>()),
                    (None, true) => format!("   ·   VST3 plugin, its main knobs{}", st.plugin_note(&fx.id)),
                    (None, false) => format!("   ·   edit {} to change it live", fx.file),
                };
                ui.label(cx, ids!(insp_title)).set_text(cx, &format!("{} on {place}{state}", fx.name));
                fill_knobs(&ui, cx, &st.fx_knobs(&fx.id));
                ui.button(cx, ids!(fx_editor)).set_visible(cx, fx.plugin.is_some());
                ui.check_box(cx, ids!(fx_isolate)).set_visible(cx, fx.plugin.is_some());
                ui.check_box(cx, ids!(fx_isolate)).set_active(cx, fx.isolated(), Animate::No);
                ui.check_box(cx, ids!(fx_bypass)).set_visible(cx, true);
                ui.button(cx, ids!(fx_remove)).set_visible(cx, true);
                ui.check_box(cx, ids!(fx_bypass)).set_active(cx, fx.bypassed(), Animate::No);
                show_fx = true;
            }
            Selection::Section(i) if i < st.project.sections().len() => {
                let s = st.project.sections()[i].clone();
                ui.label(cx, ids!(insp_title)).set_text(cx, &format!("Section {}   {:.2}s - {:.2}s   ·   drag it to move it, Alt-drag to repeat it, Delete cuts it   ·   Describe above to repaint it", i + 1, s.start, s.end));
                ui.text_input(cx, ids!(section_input)).set_text(cx, &s.label);
                let remix_on = st.song_take().is_some_and(|(_, t)| t.engine == "acestep-cover");
                ui.button(cx, ids!(use_remix_btn)).set_visible(cx, remix_on);
                show_section = true;
            }
            Selection::Chord(i) | Selection::Lyric(i) => {
                let a = st.project.analysis.clone().unwrap_or_default();
                let found = match st.sel {
                    Selection::Chord(_) => a.chords.and_then(|c| c.get(i).map(|c| ("Chord", c.start, c.end, c.label.clone()))),
                    _ => a.lyrics.and_then(|l| l.get(i).map(|l| ("Line", l.start, l.end, l.text.trim().to_string()))),
                };
                if let Some((what, start, end, text)) = found {
                    ui.label(cx, ids!(insp_title)).set_text(cx, &format!("{what}   {start:.2}s - {end:.2}s   ·   type to correct it, drag its edges to retime it, Delete removes it"));
                    ui.text_input(cx, ids!(words_input)).set_text(cx, &text);
                    show_words = true;
                }
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
                        _ if t.region.is_some() => "Keep this version",
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
        ui.view(cx, ids!(words_row)).set_visible(cx, show_words);
        ui.view(cx, ids!(take_row)).set_visible(cx, show_take);
        ui.view(cx, ids!(fx_row)).set_visible(cx, show_fx);
        ui.widget(cx, ids!(timeline)).redraw(cx);
        ui.view(cx, ids!(inspector)).redraw(cx);
    }

    fn set_status(&mut self, cx: &mut Cx, s: &str) {
        self.ui.label(cx, ids!(status_label)).set_text(cx, s);
        self.status_at = Some(std::time::Instant::now());
        // the Export sheet covers the song: it shows how an export is going too
        if self.export_shown {
            self.ui.label(cx, ids!(export_status)).set_text(cx, s);
        }
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
            if let JobUpdate::JobDone { seq } = u {
                self.jobs_done = self.jobs_done.max(seq);
                self.request_done(seq);
                let status = self.ui.label(cx, ids!(status_label)).text();
                let (done, waiting): (Vec<_>, Vec<_>) = self.parked.drain(..).partition(|(_, s)| *s <= seq);
                self.parked = waiting;
                if let Some(port) = self.ai_port.as_ref() {
                    for (call_id, _) in done {
                        port.reply(makepad_ai_services::wire::ToolResult::ok(&call_id, status.clone(), ""));
                    }
                }
                continue;
            }
            if let (JobUpdate::Progress { message, progress, .. }, Some(port)) = (&u, self.ai_port.as_ref()) {
                for (call_id, _) in &self.parked {
                    port.progress(call_id, message, (progress * 1000.0) as u16);
                }
            }
            // a background song's news: its tab shows it is busy; results name the song
            let background = update_bundle(&u).filter(|b| !b.as_os_str().is_empty() && !mine(b)).and_then(|b| self.others.iter().find(|st| st.path.parent() == Some(b.as_path())).map(|st| st.project.name.clone()));
            let quiet = background.is_some() && matches!(u, JobUpdate::Progress { .. });
            let status = match u {
                JobUpdate::Status(s) | JobUpdate::Done(s) => s,
                JobUpdate::Failed { bundle, message } => {
                    if let Some(st) = song_in(&mut self.state, &mut self.others, &bundle) {
                        st.analysing = None;
                    }
                    if self.home_shown {
                        self.ui.label(cx, ids!(home_status)).set_text(cx, "");
                        self.ui.label(cx, ids!(home_error)).set_text(cx, &message);
                    }
                    message
                }
                JobUpdate::JobDone { .. } => continue,
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
                    if let Some(st) = song_in(&mut self.state, &mut self.others, &bundle) {
                        st.analysing = None;
                        st.sel_changed = true;
                    }
                    message
                }
                JobUpdate::Progress { bundle, message, progress } => {
                    let Some(st) = song_in(&mut self.state, &mut self.others, &bundle) else { continue };
                    let what = if ["Take", "Effect", "Part", "Remix", "Song"].iter().any(|p| message.starts_with(p)) { "" } else { "Analysing  ·  " };
                    let line = format!("{what}{message}  {:.0}%", progress * 100.0);
                    st.analysing = Some((message, progress));
                    line
                }
                JobUpdate::MidiReady { bundle, name, role, notes, request, note } => {
                    let Some(st) = song_in(&mut self.state, &mut self.others, &bundle) else { continue };
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
                    let Some(st) = song_in(&mut self.state, &mut self.others, &bundle) else { continue };
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
                    match song_in(&mut self.state, &mut self.others, &bundle) {
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
                    let Some(st) = song_in(&mut self.state, &mut self.others, &bundle) else { continue };
                    let file = bundle.join("analysis.json");
                    let result = std::fs::read_to_string(&file)
                        .map_err(|e| format!("read {}: {e}", file.display()))
                        .and_then(|t| Analysis::deserialize_json_lenient(&t).map_err(|e| format!("parse {}: {e:?}", file.display())))
                        .and_then(|a| st.apply_analysis(a, stems));
                    match result {
                        Ok(()) if st.pending_remix.is_some() => {
                            if st.is_current() {
                                show_title(&self.ui, cx, &st.project);
                            }
                            match &self.jobs {
                                Some(jobs) => format!("Understood. {}", start_remix(st, jobs)),
                                None => "The job worker is not running".into(),
                            }
                        }
                        Ok(()) => {
                            if st.is_current() {
                                show_title(&self.ui, cx, &st.project);
                            }
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
            if !quiet {
                match &background {
                    Some(name) => self.set_status(cx, &format!("{name}: {status}")),
                    None => self.set_status(cx, &status),
                }
            }
        }
        self.sync_waiting_requests();
        self.refresh_tabs(cx);
        self.ui.widget(cx, ids!(timeline)).redraw(cx);
    }
}

impl App {
    /// Calls from an agent: answered here, on the UI thread, like a click.
    fn drain_ai_port(&mut self, cx: &mut Cx, event: &Event) {
        use makepad_ai_services::port::PortEvent;
        let events = match self.ai_port.as_mut() {
            Some(port) => port.handle_event(cx, event),
            None => return,
        };
        for e in events {
            match e {
                PortEvent::Call(call) => {
                    // None: parked until the job it started is done
                    if let Some(result) = self.agent_call(cx, &call) {
                        if let Some(port) = self.ai_port.as_ref() {
                            port.reply(result);
                        }
                    }
                }
                PortEvent::Cancel { call_id } => self.parked.retain(|(id, _)| *id != call_id),
                _ => {}
            }
        }
    }

    fn agent_call(&mut self, cx: &mut Cx, call: &makepad_ai_services::wire::ServiceCall) -> Option<makepad_ai_services::wire::ToolResult> {
        let seq_before = self.jobs.as_ref().map_or(0, |j| j.last_seq());
        let result = self.agent_tool(cx, call);
        let seq_after = self.jobs.as_ref().map_or(0, |j| j.last_seq());
        // a job was started: answer when it is done, with what came of it
        if seq_after > seq_before && result.outcome == makepad_ai_services::wire::ToolOutcome::Ok {
            self.parked.push((call.call_id.clone(), seq_after));
            return None;
        }
        Some(result)
    }

    fn agent_tool(&mut self, cx: &mut Cx, call: &makepad_ai_services::wire::ServiceCall) -> makepad_ai_services::wire::ToolResult {
        use agent::{get, index, num};
        use makepad_ai_services::wire::ToolResult;
        let id = call.call_id.clone();
        let a = match agent::args(&call.args) {
            Ok(a) => a,
            Err(e) => return ToolResult::refused(&id, e),
        };
        let status_now = self.ui.label(cx, ids!(status_label)).text();
        if self.state.is_none() {
            return ToolResult::unavailable(&id, "no song is open (Poing shows its start screen)");
        }
        if self.home_shown {
            self.show_song_view(cx, true);
        }
        if call.tool == "live" {
            return match self.live_tool(cx, &a) {
                Ok((line, data)) => ToolResult::ok(&id, line, "").with_data(data),
                Err(e) => ToolResult::refused(&id, e),
            };
        }
        let Some(st) = self.state.as_mut() else { return ToolResult::unavailable(&id, "no song is open") };
        let lanes = st.project.tracks.len();
        let result: Result<String, String> = match call.tool.as_str() {
            "song" => {
                let data = agent::summary(st, &status_now);
                return ToolResult::ok(&id, format!("{} ({} lanes, {} sections, {} takes)", st.project.name, lanes, st.project.sections().len(), st.project.takes().len()), "").with_data(data);
            }
            "describe" => match get(&a, "text").and_then(|v| v.as_str()) {
                None => Err("say what to change in `text`".into()),
                Some(text) => {
                    let place = match (index(&a, "lane"), index(&a, "section")) {
                        (Some(l), _) if l < lanes => Ok(Selection::Track(l)),
                        (None, Some(s)) if s < st.project.sections().len() => Ok(Selection::Section(s)),
                        (None, None) => Ok(Selection::None),
                        _ => Err("no such lane or section".to_string()),
                    };
                    place.map(|sel| {
                        st.select(sel);
                        describe(st, self.jobs.as_ref(), text.trim())
                    })
                }
            },
            "add_part" => match (get(&a, "role").and_then(|v| v.as_str()), get(&a, "text").and_then(|v| v.as_str())) {
                (Some(role @ ("bass" | "keys" | "lead" | "drums")), Some(text)) => {
                    let role: &'static str = match role {
                        "bass" => "bass",
                        "keys" => "keys",
                        "drums" => "drums",
                        _ => "lead",
                    };
                    match index(&a, "section") {
                        Some(s) if s >= st.project.sections().len() => Err("no such section".into()),
                        sel => {
                            st.select(sel.map_or(Selection::None, Selection::Section));
                            let named = get(&a, "instrument").and_then(|v| v.as_str()).map(|n| n.to_string());
                            st.next_instrument = named.as_deref().and_then(|n| named_plugin(st, n));
                            if named.is_some() && st.next_instrument.is_none() {
                                Err(format!("no installed instrument called {}", named.unwrap_or_default()))
                            } else if st.analysing.is_some() {
                                Err("a job is still running; try again when song.busy is false".into())
                            } else {
                                let bundle = st.path.parent().map(Path::to_path_buf).unwrap_or_default();
                                match self.jobs.as_ref() {
                                    Some(jobs) => Ok(write_midi(st, jobs, bundle, role, text)),
                                    None => Err("the job worker is not running".into()),
                                }
                            }
                        }
                    }
                }
                _ => Err("give a role (bass, keys, lead or drums) and the style in `text`".into()),
            },
            "regenerate" => {
                let text = get(&a, "text").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                match (index(&a, "region"), self.jobs.as_ref()) {
                    (_, None) => Err("the job worker is not running".into()),
                    _ if st.analysing.is_some() => Err("a job is still running; try again when song.busy is false".into()),
                    (Some(r), _) if r >= st.project.regions().len() => Err("no such region (see song.regions)".into()),
                    (Some(r), Some(jobs)) => {
                        st.select(Selection::Region(r));
                        Ok(regenerate_region(st, jobs, r, &text))
                    }
                    (None, Some(jobs)) if st.project.regions().iter().any(|r| r.kind == "bad") => {
                        st.select(Selection::None);
                        Ok(fix_marks(st, jobs, &text))
                    }
                    (None, Some(_)) => Err("no region is marked bad; give `region` to make another one again".into()),
                }
            }
            "set_lane" => match index(&a, "lane").filter(|&l| l < lanes) {
                None => Err("no such lane".into()),
                Some(l) => {
                    // during Claude's pass from the bar, one undo step covers it all
                    if self.agent_rx.is_none() {
                        st.checkpoint();
                    }
                    let t = &mut st.project.tracks[l];
                    let was = t.gain();
                    if let Some(g) = get(&a, "gain_db").and_then(num) {
                        t.gain_db = Some(g.clamp(-36.0, 12.0));
                    }
                    if let Some(d) = get(&a, "change_db").and_then(num) {
                        t.gain_db = Some((t.gain() + d).clamp(-36.0, 12.0));
                    }
                    if let Some(m) = get(&a, "mute").and_then(|v| v.as_bool()) {
                        t.mute = Some(m);
                    }
                    if let Some(s) = get(&a, "solo").and_then(|v| v.as_bool()) {
                        t.solo = Some(s);
                    }
                    let line = format!("{}: {:+.1} dB (was {was:+.1}){}{}", t.name, t.gain(), if t.muted() { ", muted" } else { "" }, if t.soloed() { ", soloed" } else { "" });
                    st.push_mix();
                    st.sel_changed = true;
                    Ok(line)
                }
            },
            "effect" => {
                let lane = index(&a, "lane");
                match index(&a, "effect").and_then(|i| (lane.map_or(true, |l| l < lanes)).then(|| st.project.chain(lane).get(i).cloned()).flatten()) {
                    None => Err("no such effect (see `song`)".into()),
                    Some(fx) => {
                        let mut done = Vec::new();
                        if let Some(b) = get(&a, "bypass").and_then(|v| v.as_bool()) {
                            st.set_fx_bypass(&fx.id, b);
                            done.push(if b { "bypassed".to_string() } else { "on".to_string() });
                        }
                        if let Some(b) = get(&a, "own_process").and_then(|v| v.as_bool()) {
                            done.push(st.set_isolated(&fx.id, b).unwrap_or_else(|e| e));
                        }
                        let mut err = None;
                        if let (Some(knob), Some(v)) = (get(&a, "knob").and_then(|v| v.as_str()), get(&a, "value").and_then(num)) {
                            let norm = |n: &str| n.to_lowercase().replace('_', " ");
                            let knobs = st.fx_knobs(&fx.id);
                            match knobs.iter().position(|(n, ..)| norm(n) == norm(knob)) {
                                Some(k) => {
                                    let (name, _, min, max) = knobs[k].clone();
                                    if let Some(i) = st.fx_param_index(&fx.id, k) {
                                        st.set_fx_param(&fx.id, i, v.clamp(min, max));
                                        done.push(format!("{} = {:.3}", name.replace('_', " "), v.clamp(min, max)));
                                    }
                                }
                                None => err = Some(format!("{} has no knob `{knob}`; it has: {}", fx.name, knobs.iter().map(|(n, ..)| n.clone()).collect::<Vec<_>>().join(", "))),
                            }
                        }
                        st.sel_changed = true;
                        match err {
                            Some(e) => Err(e),
                            None => Ok(format!("{}: {}", fx.name, if done.is_empty() { "unchanged".into() } else { done.join(", ") })),
                        }
                    }
                }
            }
            "take" => match get(&a, "id").and_then(|v| v.as_str()) {
                Some("original") => {
                    st.all_takes_off();
                    Ok("Every take is off: the lanes play".into())
                }
                Some(tid) if st.project.take_index(tid).is_some() => {
                    let on = get(&a, "on").and_then(|v| v.as_bool()).unwrap_or(true);
                    if on != st.take_on(tid) {
                        st.toggle_take(tid);
                    }
                    if get(&a, "use").and_then(|v| v.as_bool()) == Some(true) {
                        if !st.take_on(tid) {
                            st.toggle_take(tid);
                        }
                        st.use_version().map(|name| {
                            analyse(self.jobs.as_ref(), st);
                            format!("{name} is the song now; Poing is analysing it (song.busy)")
                        })
                    } else {
                        Ok(format!("Take {tid} is {}", if st.take_on(tid) { "on" } else { "off" }))
                    }
                }
                _ => Err("no such take (see `song`)".into()),
            },
            "combine" => match (index(&a, "section"), get(&a, "take").and_then(|v| v.as_str())) {
                (Some(s), Some(tid)) if st.project.take_index(tid).is_some() => {
                    if !st.take_on(tid) {
                        st.toggle_take(tid);
                    }
                    st.use_remix_here(s)
                }
                _ => Err("give a section index and a take id (see `song`)".into()),
            },
            "arrange" => {
                use model::arrange::Op;
                let n = st.project.sections().len();
                let section = index(&a, "section");
                let to = index(&a, "to");
                let op = match (get(&a, "op").and_then(|v| v.as_str()), section) {
                    (Some("extend"), _) => None,
                    (_, None) => return ToolResult::refused(&id, "give the `section` index (see `song`)"),
                    (_, Some(s)) if s >= n => return ToolResult::refused(&id, "no such section"),
                    (Some("move"), Some(s)) => match to {
                        Some(to) => Some(Op::Move { section: s, to }),
                        None => return ToolResult::refused(&id, "say where to move it in `to`"),
                    },
                    (Some("duplicate"), Some(s)) => Some(Op::Duplicate { section: s, to: to.unwrap_or(s + 1) }),
                    (Some("delete"), Some(s)) => Some(Op::Delete { section: s }),
                    _ => return ToolResult::refused(&id, "op is move, duplicate, delete or extend"),
                };
                match op {
                    Some(op) => st.arrange(&op),
                    None => match (self.jobs.as_ref(), st.analysing.is_some()) {
                        (_, true) => Err("a job is still running; try again when song.busy is false".into()),
                        (None, _) => Err("the job worker is not running".into()),
                        (Some(jobs), _) => {
                            let secs = get(&a, "seconds").and_then(num).unwrap_or(30.0).clamp(5.0, 120.0);
                            let bundle = st.path.parent().map(Path::to_path_buf).unwrap_or_default();
                            Ok(extend_song(st, jobs, bundle, secs, &format!("make it {secs:.0} s longer")))
                        }
                    },
                }
            }
            "transport" => {
                if let Some(t) = get(&a, "seek").and_then(num) {
                    st.seek(t);
                }
                match a.iter().find(|(k, _)| k == "loop").map(|(_, v)| v) {
                    Some(v) if v.is_null() => st.set_loop(None),
                    Some(v) => {
                        if let Some([x, y]) = v.as_arr().map(|r| r.iter().filter_map(num).collect::<Vec<_>>()).as_deref() {
                            st.set_loop(Some((x.min(*y), x.max(*y))));
                        }
                    }
                    None => {}
                }
                if let Some(p) = get(&a, "play").and_then(|v| v.as_bool()) {
                    if p != st.shared.playing() {
                        st.toggle_play();
                    }
                }
                st.sel_changed = true;
                Ok(format!("{} at {:.1}s", if st.shared.playing() { "Playing" } else { "Paused" }, st.playhead()))
            }
            // read tools: they answer with measurements and leave the status line alone
            "listen" | "compare" => {
                let heard = listen::range(st, get(&a, "start").and_then(num), get(&a, "end").and_then(num)).and_then(|(start, end)| {
                    if call.tool == "listen" {
                        match index(&a, "lane") {
                            Some(l) if l >= lanes => Err("no such lane".into()),
                            lane => listen::listen(st, start, end, lane),
                        }
                    } else {
                        let picked: Vec<usize> = get(&a, "lanes").and_then(|v| v.as_arr()).unwrap_or_default().iter().filter_map(|v| v.as_u64()).map(|i| i as usize).collect();
                        match picked.iter().find(|&&l| l >= lanes) {
                            Some(l) => Err(format!("no lane {l}")),
                            None => listen::compare(st, start, end, &picked),
                        }
                    }
                });
                return match heard {
                    Ok((line, data)) => ToolResult::ok(&id, line, "").with_data(data.to_json()),
                    Err(e) => ToolResult::failed(&id, e),
                };
            }
            "export" => {
                let out_dir = st.path.parent().map(|p| p.join("renders")).unwrap_or_else(|| PathBuf::from("renders"));
                let (rack, missing) = st.make_render_rack();
                let job = Job::Render { stems: st.stems.clone(), mix: st.mix_snapshot(), rack: Box::new(rack), sr: st.sr(), out_dir, name: st.project.name.clone() };
                match self.jobs.as_ref().map(|j| j.submit(job)) {
                    Some(true) if missing.is_empty() => Ok("Rendering; song.status names the file when it is done".into()),
                    Some(true) => Ok(format!("Rendering without {}; song.status names the file when it is done", missing.join(", "))),
                    _ => Err("the render worker is not running".into()),
                }
            }
            other => Err(format!("Poing has no tool `{other}`")),
        };
        self.refresh_inspector(cx);
        self.ui.widget(cx, ids!(timeline)).redraw(cx);
        match result {
            Ok(line) => {
                self.set_status(cx, &line);
                ToolResult::ok(&id, line, "")
            }
            Err(e) => ToolResult::failed(&id, e),
        }
    }
}

impl App {
    /// Claude's answer to a request from the Describe bar.
    fn drain_agent(&mut self, cx: &mut Cx) {
        let Some(rx) = &self.agent_rx else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.agent_rx = None;
        let line = result.unwrap_or_else(|e| format!("{} could not do it: {e}", llm::name()));
        if let Some(st) = self.state.as_mut() {
            st.sel_changed = true;
        }
        self.set_status(cx, &line);
        self.refresh_inspector(cx);
        self.ui.widget(cx, ids!(timeline)).redraw(cx);
    }
}

impl App {
    /// Keep a request until its job is done (`seq_before`: the last job
    /// number before it was made; nothing new submitted, nothing to keep).
    fn note_request(&mut self, bundle: &Path, seq_before: u64, kind: &str, text: &str, lane: Option<String>, section: Option<usize>) {
        note_request_in(&mut self.requests, self.jobs.as_ref(), bundle, seq_before, kind, text, lane, section);
    }

    fn save_requests(&self, bundle: &Path) {
        let mine: Vec<Request> = self.requests.iter().filter(|(b, _)| b == bundle).map(|(_, r)| r.clone()).collect();
        write_requests(bundle, &mine);
    }

    /// Job `seq` is done: its request is kept no longer.
    fn request_done(&mut self, seq: u64) {
        let bundles: Vec<PathBuf> = self.requests.iter().filter(|(_, r)| r.seq == seq).map(|(b, _)| b.clone()).collect();
        self.requests.retain(|(_, r)| r.seq != seq);
        for b in bundles {
            self.save_requests(&b);
        }
    }

    /// A remix waits for its song's analysis; once it is submitted it gets its job number.
    fn sync_waiting_requests(&mut self) {
        let Some(last) = self.jobs.as_ref().map(|j| j.last_seq()) else { return };
        let mut changed = Vec::new();
        for (b, r) in self.requests.iter_mut().filter(|(_, r)| r.seq == u64::MAX) {
            let waiting = self.state.iter().chain(self.others.iter()).find(|st| st.path.parent() == Some(b.as_path())).is_some_and(|st| st.pending_remix.is_some());
            if !waiting {
                r.seq = last;
                changed.push(b.clone());
            }
        }
        for b in changed {
            self.save_requests(&b);
        }
    }

    /// What Poing is still busy with, if anything (closing would stop it).
    fn busy(&self) -> Option<String> {
        if self.agent_rx.is_some() {
            return Some("Claude is working on a request".into());
        }
        if self.downloads.busy() {
            return Some("a download (it picks up where it was next time)".into());
        }
        let running = self.jobs.as_ref().is_some_and(|j| j.last_seq() > self.jobs_done);
        if !running {
            return None;
        }
        let what = self.state.iter().chain(self.others.iter()).find_map(|st| st.analysing.as_ref().map(|(w, p)| format!("{} ({w}, {:.0}%)", st.project.name, p * 100.0)));
        Some(what.unwrap_or_else(|| "a job".into()))
    }

    /// Requests of an earlier run left in this song's folder: offer them again (once).
    fn offer_interrupted(&mut self, cx: &mut Cx) {
        let Some(bundle) = self.state.as_ref().and_then(|st| st.path.parent().map(Path::to_path_buf)) else { return };
        let all = read_requests(&bundle);
        let (stale, current): (Vec<_>, Vec<_>) = all.into_iter().partition(|r| r.run != run_id());
        if stale.is_empty() {
            return;
        }
        write_requests(&bundle, &current);
        let what = match stale.first() {
            Some(r) if r.kind == "remix" => format!("The remix \"{}\"", r.text),
            Some(r) => format!("\"{}\"", r.text),
            None => String::new(),
        };
        let more = if stale.len() > 1 { format!(" (and {} more)", stale.len() - 1) } else { String::new() };
        self.interrupted = stale.into_iter().map(|r| (bundle.clone(), r)).collect();
        self.set_status(cx, &format!("{what}{more} stopped when Poing closed before it was done."));
        self.ui.button(cx, ids!(redo_btn)).set_visible(cx, true);
    }

    /// "Make it again": the interrupted requests of the song in front, asked again.
    fn redo_interrupted(&mut self, cx: &mut Cx) {
        self.ui.button(cx, ids!(redo_btn)).set_visible(cx, false);
        let Some(bundle) = self.state.as_ref().and_then(|st| st.path.parent().map(Path::to_path_buf)) else { return };
        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.interrupted).into_iter().partition(|(b, _)| *b == bundle);
        self.interrupted = rest;
        let mut said = Vec::new();
        for (_, r) in mine {
            let Some(st) = self.state.as_mut() else { break };
            let before = self.jobs.as_ref().map_or(0, |j| j.last_seq());
            if r.kind == "remix" {
                st.pending_remix = Some(r.text.clone());
                let line = match (&self.jobs, st.project.is_analysed()) {
                    (Some(jobs), true) => start_remix(st, jobs),
                    _ => format!("Poing is listening to the song first; then it makes the remixes as \"{}\"", r.text),
                };
                said.push(line);
            } else {
                let sel = match (&r.lane, r.section) {
                    (Some(id), _) => st.project.track_index(id).map(Selection::Track).unwrap_or(Selection::None),
                    (None, Some(i)) if i < st.project.sections().len() => Selection::Section(i),
                    _ => Selection::None,
                };
                st.select(sel);
                said.push(describe(st, self.jobs.as_ref(), &r.text));
            }
            self.note_request(&bundle, before, &r.kind, &r.text, r.lane.clone(), r.section);
        }
        self.sync_waiting_requests();
        self.set_status(cx, &said.join("  ·  "));
        self.refresh_inspector(cx);
    }

    /// Show or hide the Settings panel (over the start screen or the song).
    fn show_settings(&mut self, cx: &mut Cx, on: bool) {
        let ui = self.ui.clone();
        self.settings_shown = on;
        ui.view(cx, ids!(settings)).set_visible(cx, on);
        if on {
            self.export_shown = false;
            ui.view(cx, ids!(export_sheet)).set_visible(cx, false);
            ui.view(cx, ids!(home)).set_visible(cx, false);
            for id in [ids!(timeline_box), ids!(inspector_box), ids!(live_view)] {
                ui.view(cx, id).set_visible(cx, false);
            }
            self.refresh_settings(cx);
        } else {
            let song = self.state.is_some() && !self.home_shown;
            self.show_song_view(cx, song);
        }
        ui.redraw(cx);
    }

    /// Show or hide the Export sheet (over the song).
    fn show_export(&mut self, cx: &mut Cx, on: bool) {
        let ui = self.ui.clone();
        if !on || self.state.is_none() {
            self.show_song_view(cx, self.state.is_some() && !self.home_shown);
            return;
        }
        self.settings_shown = false;
        ui.view(cx, ids!(settings)).set_visible(cx, false);
        self.export_shown = true;
        ui.view(cx, ids!(export_sheet)).set_visible(cx, true);
        for id in [ids!(timeline_box), ids!(inspector_box), ids!(live_view)] {
            ui.view(cx, id).set_visible(cx, false);
        }
        let Some(st) = self.state.as_ref() else { return };
        let midi = st.project.tracks.iter().filter(|t| t.is_midi()).count();
        ui.button(cx, ids!(export_midi_btn)).set_visible(cx, midi > 0);
        let text = if midi > 0 {
            "Each MIDI lane as a .mid file on the song's tempo, so it lines up with the audio."
        } else {
            "This song has no MIDI lanes yet."
        };
        ui.label(cx, ids!(export_midi_text)).set_text(cx, text);
        let versions = export::dj_versions(st);
        for (i, id) in [ids!(dj_v0), ids!(dj_v1), ids!(dj_v2), ids!(dj_v3), ids!(dj_v4), ids!(dj_v5)].into_iter().enumerate() {
            let cb = ui.check_box(cx, id);
            cb.set_visible(cx, i < versions.len());
            if let Some((_, label)) = versions.get(i) {
                cb.set_text(label);
                cb.set_active(cx, false, Animate::No);
            }
        }
        ui.label(cx, ids!(export_status)).set_text(cx, "");
        ui.redraw(cx);
    }

    /// Start an export from the sheet: the worker renders, the status line follows.
    fn export(&mut self, cx: &mut Cx, kind: export::Kind) {
        let Some(st) = self.state.as_mut() else { return };
        let line = match export::start(st, kind) {
            Ok((None, line)) => line,
            Ok((Some(job), line)) => match self.jobs.as_ref().map(|j| j.submit(Job::Export(Box::new(job)))) {
                Some(true) => line,
                _ => "The export worker is not running".into(),
            },
            Err(e) => format!("Export: {e}"),
        };
        self.set_status(cx, &line);
    }

    fn refresh_settings(&mut self, cx: &mut Cx) {
        let ui = self.ui.clone();
        let outputs: Vec<&AudioDeviceDesc> = self.audio_devices.iter().filter(|d| d.device_type.is_output()).collect();
        let saved = load_settings().audio_output;
        let mut labels = vec!["The system's default output".to_string()];
        labels.extend(outputs.iter().map(|d| d.name.clone()));
        let chosen = saved.as_ref().and_then(|n| outputs.iter().position(|d| &d.name == n)).map_or(0, |i| i + 1);
        ui.drop_down(cx, ids!(out_dd)).set_labels(cx, labels);
        ui.drop_down(cx, ids!(out_dd)).set_selected_item(cx, chosen);
        let now = match (&saved, outputs.iter().find(|d| d.is_default)) {
            (Some(n), _) if chosen > 0 => format!("Playing through {n}."),
            (Some(n), _) => format!("{n} is not connected: playing through the default output."),
            (None, Some(d)) => format!("Playing through the default output ({}).", d.name),
            (None, None) => "No sound output found.".into(),
        };
        ui.label(cx, ids!(out_status)).set_text(cx, &now);
        let ai = match (llm::provider(), agent::claude_installed(), agent::unavailable()) {
            (llm::Provider::Local, claude, why) => format!(
                "Poing's own model (Qwen3.5 4B, on this PC's GPU) writes effects, MIDI parts and new songs and answers the Describe bar: no subscription needed.{}{}",
                why.map(|w| format!(" Not now: {w}.")).unwrap_or_default(),
                if claude { " Claude Code is installed too: pick it above to use it instead." } else { "" }
            ),
            (llm::Provider::ClaudeCode, true, None) => "Claude Code writes effects, MIDI parts and new songs and answers free-form requests in the Describe bar.".to_string(),
            (llm::Provider::ClaudeCode, true, Some(why)) => format!("Claude Code writes effects, MIDI parts and new songs. Free-form requests in the Describe bar also need Makepad's makepad-remote: {why}."),
            (llm::Provider::ClaudeCode, false, _) => "Claude Code was chosen but is not installed: written effects, MIDI parts and new songs need it (or Poing's own model).".to_string(),
        };
        ui.drop_down(cx, ids!(ai_dd)).set_selected_item(cx, usize::from(llm::provider() == llm::Provider::ClaudeCode));
        ui.label(cx, ids!(ai_status)).set_text(cx, &ai);
        // where the GPU jobs run: this PC, or a poing-node (docs/REMOTE.md)
        let nodes = remote::targets();
        let target = remote::target();
        let mut labels = vec!["This PC".to_string()];
        labels.extend(nodes.iter().map(|n| if n.discovered { format!("{} (found on the network)", n.address) } else { n.address.clone() }));
        self.compute_nodes = nodes.into_iter().map(|n| n.address).collect();
        let chosen = match &target {
            remote::Target::Local => 0,
            remote::Target::Node(a) => self.compute_nodes.iter().position(|n| n == a).map_or(0, |i| i + 1),
        };
        ui.drop_down(cx, ids!(compute_dd)).set_labels(cx, labels);
        ui.drop_down(cx, ids!(compute_dd)).set_selected_item(cx, chosen);
        let compute = match remote::status_line() {
            Some(line) => format!("{line}. Analysis, remixes and new songs go there; playback, plugins and the mix stay here."),
            None if self.compute_nodes.is_empty() => "This PC: analysis, remixes and new songs run here. To have another PC do them, run poing-node there (docs/REMOTE.md).".to_string(),
            None => "This PC: analysis, remixes and new songs run here.".to_string(),
        };
        ui.label(cx, ids!(compute_status)).set_text(cx, &compute);
        let plugins = self.state.as_ref().map(|st| st.plugin_library.len()).unwrap_or_else(|| plugins::library().len());
        let ptext = if cfg!(windows) {
            format!("{plugins} VST3 plugins found in the standard folders. Name one in the Describe bar to use it.")
        } else {
            "Plugin hosting is Windows-only for now.".to_string()
        };
        ui.label(cx, ids!(plugins_status)).set_text(cx, &ptext);
        self.refresh_models(cx);
    }
}

impl MatchEvent for App {
    /// Play through the system's default output (the audio callback only
    /// runs once an output device is opened).
    fn handle_audio_devices(&mut self, cx: &mut Cx, devices: &AudioDevicesEvent) {
        for d in &devices.descs {
            log!("poing: audio device {d}");
        }
        self.audio_devices = devices.descs.clone();
        // the output chosen in Settings, while it is connected; else the default
        let chosen = load_settings().audio_output.map(|n| devices.descs.iter().filter(|d| d.device_type.is_output() && !d.has_failed && d.name == n).map(|d| d.device_id).collect::<Vec<_>>()).filter(|v| !v.is_empty());
        let out = chosen.unwrap_or_else(|| devices.default_output());
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
        // the AI chosen in Settings; POING_LLM still wins for a test run
        if std::env::var_os("POING_LLM").is_none() {
            if let Some(ai) = load_settings().ai {
                llm::set_provider(if ai == "claude" { llm::Provider::ClaudeCode } else { llm::Provider::Local });
            }
        }
        self.jobs = Some(Jobs::start(cx));
        self.ai_port = makepad_ai_services::port::AiServicePort::open(cx, agent::manifest());
        self.plugin_watch = cx.start_interval(0.25);
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
        if self.model_actions(cx, actions) {
            return;
        }
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
        // Settings
        if ui.button(cx, ids!(settings_btn)).clicked(actions) {
            self.show_settings(cx, !self.settings_shown);
            return;
        }
        if ui.button(cx, ids!(settings_done)).clicked(actions) {
            self.show_settings(cx, false);
            return;
        }
        if let Some(i) = ui.drop_down(cx, ids!(out_dd)).changed(actions) {
            let outputs: Vec<AudioDeviceDesc> = self.audio_devices.iter().filter(|d| d.device_type.is_output()).cloned().collect();
            let mut settings = load_settings();
            settings.audio_output = if i == 0 { None } else { outputs.get(i - 1).map(|d| d.name.clone()) };
            save_settings(&settings);
            let id = match i {
                0 => AudioDevicesEvent { descs: self.audio_devices.clone() }.default_output(),
                _ => outputs.get(i - 1).map(|d| vec![d.device_id]).unwrap_or_default(),
            };
            cx.use_audio_outputs(&id);
            self.refresh_settings(cx);
        }
        if let Some(i) = ui.drop_down(cx, ids!(ai_dd)).changed(actions) {
            let claude = i == 1;
            llm::set_provider(if claude { llm::Provider::ClaudeCode } else { llm::Provider::Local });
            if claude {
                // free the GPU the local model was holding
                llm::release();
            }
            let mut settings = load_settings();
            settings.ai = Some(if claude { "claude" } else { "local" }.into());
            save_settings(&settings);
            self.refresh_settings(cx);
        }
        if let Some(i) = ui.drop_down(cx, ids!(compute_dd)).changed(actions) {
            let target = match i.checked_sub(1).and_then(|k| self.compute_nodes.get(k)) {
                Some(a) => remote::Target::Node(a.clone()),
                None => remote::Target::Local,
            };
            if let Err(e) = remote::set_target(target) {
                self.set_status(cx, &format!("Could not save where jobs run: {e}"));
            }
            self.refresh_settings(cx);
        }
        if ui.button(cx, ids!(rescan_btn)).clicked(actions) {
            let found = plugins::library();
            for st in self.state.iter_mut().chain(self.others.iter_mut()) {
                st.plugin_library = found.clone();
            }
            self.refresh_settings(cx);
        }
        if ui.button(cx, ids!(redo_btn)).clicked(actions) {
            self.redo_interrupted(cx);
            return;
        }
        // a remix of a song that already has a project: the answer
        if ui.button(cx, ids!(remix_add_btn)).clicked(actions) {
            if let Some((_, prompt, project)) = self.remix_choice.take() {
                self.start_remix_of(cx, Open::Project(project), prompt);
                self.refresh_inspector(cx);
                return;
            }
        }
        if ui.button(cx, ids!(remix_new_btn)).clicked(actions) {
            if let Some((audio, prompt, _)) = self.remix_choice.take() {
                match import_new(&audio, 48000) {
                    Ok(project) => self.start_remix_of(cx, Open::Project(project), prompt),
                    Err(e) => ui.label(cx, ids!(home_error)).set_text(cx, &format!("Could not start a new project: {e}")),
                }
                self.refresh_inspector(cx);
                return;
            }
        }
        // the start screen's file pickers, and what they chose
        if ui.button(cx, ids!(pick_split_btn)).clicked(actions) {
            choose_song(cx, SPLIT_DIALOG, "Choose a song to split");
        }
        if ui.button(cx, ids!(pick_remix_btn)).clicked(actions) {
            if ui.text_input(cx, ids!(remix_input)).text().trim().is_empty() {
                ui.label(cx, ids!(home_error)).set_text(cx, "Say first how to remix it (the field on the Remix card).");
            } else {
                choose_song(cx, REMIX_DIALOG, "Choose the song to remix");
            }
        }
        for action in actions {
            if let Some(FileDialogAction::FileSelected { id, paths }) = action.downcast_ref::<FileDialogAction>() {
                let Some(path) = paths.first().cloned() else { continue };
                if *id == SPLIT_DIALOG {
                    self.open(cx, Open::Audio(path));
                    self.refresh_inspector(cx);
                    return;
                }
                if *id == REMIX_DIALOG {
                    self.remix_song(cx, path);
                    self.refresh_inspector(cx);
                    return;
                }
            }
        }
        // tabs: a song to the front, or closed; + is the start screen
        let tab_ids = [
            (ids!(tab0_btn), ids!(tab0_x)),
            (ids!(tab1_btn), ids!(tab1_x)),
            (ids!(tab2_btn), ids!(tab2_x)),
            (ids!(tab3_btn), ids!(tab3_x)),
            (ids!(tab4_btn), ids!(tab4_x)),
            (ids!(tab5_btn), ids!(tab5_x)),
        ];
        for (k, (btn, x)) in tab_ids.iter().enumerate() {
            let Some(path) = self.tabs.get(k).cloned() else { continue };
            if ui.button(cx, *x).clicked(actions) {
                self.close_tab(cx, &path);
                self.refresh_inspector(cx);
                return;
            }
            if ui.button(cx, *btn).clicked(actions) {
                if self.state.as_ref().is_some_and(|st| same_song(&st.path, &path)) {
                    self.show_song_view(cx, true);
                    self.refresh_tabs(cx);
                } else {
                    self.switch_to(cx, &path);
                }
                self.refresh_inspector(cx);
                return;
            }
        }
        if ui.button(cx, ids!(new_tab_btn)).clicked(actions) {
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
        if ui.button(cx, ids!(live_btn)).clicked(actions) {
            let on = !self.live_shown;
            self.show_live(cx, on);
            return;
        }
        if ui.button(cx, ids!(live_done)).clicked(actions) {
            self.show_live(cx, false);
            return;
        }
        if ui.button(cx, ids!(render_btn)).clicked(actions) {
            let on = !self.export_shown;
            self.show_export(cx, on);
            return;
        }
        if ui.button(cx, ids!(export_done)).clicked(actions) {
            self.show_export(cx, false);
            return;
        }
        let kind = if ui.button(cx, ids!(export_stems_btn)).clicked(actions) {
            Some(export::Kind::Stems)
        } else if ui.button(cx, ids!(export_midi_btn)).clicked(actions) {
            Some(export::Kind::Midi)
        } else if ui.button(cx, ids!(export_daw_btn)).clicked(actions) {
            Some(export::Kind::DawProject)
        } else if ui.button(cx, ids!(export_dj_btn)).clicked(actions) {
            let ticked = [ids!(dj_v0), ids!(dj_v1), ids!(dj_v2), ids!(dj_v3), ids!(dj_v4), ids!(dj_v5)].map(|id| ui.check_box(cx, id).active(cx));
            let versions = export::dj_versions(st).into_iter().zip(ticked).filter(|(_, on)| *on).map(|((id, _), _)| id).collect();
            Some(export::Kind::Dj { versions, mp3: ui.drop_down(cx, ids!(dj_format)).selected_item() == 1 })
        } else {
            None
        };
        if let Some(kind) = kind {
            self.export(cx, kind);
            return;
        }
        if ui.button(cx, ids!(export_mix_btn)).clicked(actions) {
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
            if self.agent_rx.is_some() && !text.trim().is_empty() {
                status = Some("The assistant is still working on the last request.".into());
            } else if !text.trim().is_empty() {
                let before = self.jobs.as_ref().map_or(0, |j| j.last_seq());
                let lane = match st.sel {
                    Selection::Track(i) => st.project.tracks.get(i).map(|t| t.id.clone()),
                    Selection::Region(r) => st.project.regions().get(r).map(|g| g.track.clone()),
                    _ => None,
                };
                let section = if let Selection::Section(i) = st.sel { Some(i) } else { None };
                let bundle = st.path.parent().map(Path::to_path_buf).unwrap_or_default();
                st.agent_ok = true;
                status = Some(describe(st, self.jobs.as_ref(), text.trim()));
                st.agent_ok = false;
                note_request_in(&mut self.requests, self.jobs.as_ref(), &bundle, before, "describe", text.trim(), lane, section);
                ui.text_input(cx, ids!(describe_input)).set_text(cx, "");
                if let Some(request) = st.ask_agent.take() {
                    self.agent_rx = Some(start_agent(st, request));
                }
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
        } else if ui.button(cx, ids!(keep_btn)).clicked(actions) && st.focused_take().is_some_and(|t| t.region.is_some()) {
            let id = st.focus.clone().unwrap_or_default();
            status = Some(st.keep_version(&id).unwrap_or_else(|e| format!("Keep failed: {e}")));
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

        if let Some(on) = ui.check_box(cx, ids!(fx_isolate)).changed(actions) {
            let id = match st.sel {
                Selection::Track(l) => st.project.tracks.get(l).map(|t| t.id.clone()),
                Selection::Fx(lane, i) => st.project.chain(lane).get(i).map(|f| f.id.clone()),
                _ => None,
            };
            if let Some(id) = id {
                status = Some(st.set_isolated(&id, on).unwrap_or_else(|e| e));
                st.sel_changed = true;
            }
        }
        if ui.button(cx, ids!(fx_editor)).clicked(actions) {
            let id = match st.sel {
                Selection::Track(l) => st.project.tracks.get(l).map(|t| t.id.clone()),
                Selection::Fx(lane, i) => st.project.chain(lane).get(i).map(|f| f.id.clone()),
                _ => None,
            };
            if let Some(id) = id {
                status = Some(st.open_editor(&id).unwrap_or_else(|e| e));
                if st.editors_open() && self.plugin_pump.is_empty() {
                    self.plugin_pump = cx.start_interval(0.016);
                }
            }
        }
        if let Selection::Track(lane) = st.sel {
            let sliders = [ids!(fx_k0), ids!(fx_k1), ids!(fx_k2), ids!(fx_k3), ids!(fx_k4), ids!(fx_k5)];
            for (k, s) in sliders.iter().enumerate() {
                if let Some(v) = ui.slider(cx, *s).slided(actions) {
                    st.set_inst_knob(lane, k, v);
                }
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
                    let regenerate = kind == "regenerate";
                    let r = &mut st.project.regions_mut()[i];
                    r.gain_db = Some(default_gain_for(&kind));
                    r.kind = kind;
                    st.push_mix();
                    st.sel_changed = true;
                    // "regenerate" makes it again straight away, with its note as the hint
                    if regenerate {
                        status = Some(describe(st, self.jobs.as_ref(), "regenerate"));
                    }
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
            if ui.button(cx, ids!(rec_btn)).clicked(actions) {
                status = Some(st.toggle_record());
            }
            if let Some(q) = ui.drop_down(cx, ids!(quant_dd)).selected(actions) {
                st.quantise = poing_model::record::Quantise::ALL.get(q).copied().unwrap_or_default();
            }
            if let Some(v) = ui.check_box(cx, ids!(overdub_cb)).changed(actions) {
                st.overdub = v;
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
                status = st.merge_section(i);
            }
        }
        if let Selection::Chord(i) | Selection::Lyric(i) = st.sel {
            if let Some(text) = ui.text_input(cx, ids!(words_input)).changed(actions) {
                // one undo step for the whole correction
                if st.editing_words != Some(st.sel) {
                    st.checkpoint();
                    st.editing_words = Some(st.sel);
                }
                match st.sel {
                    Selection::Chord(_) => st.set_chord(i, &text),
                    _ => st.set_lyric(i, &text),
                }
            }
            let input = ui.text_input(cx, ids!(words_input));
            if st.editing_words.is_some() && (input.returned(actions).is_some() || input.key_focus_lost(actions)) {
                st.editing_words = None;
                // the words as corrected are what covers and repaints sing
                st.write_lyrics();
                status = Some(match st.save() {
                    Ok(()) => "Corrected  ·  Ctrl+Z to undo".into(),
                    Err(e) => format!("Could not save: {e}"),
                });
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
        if let Event::WindowCloseRequested(e) = event {
            // a job still running stops with Poing: say so once; closing again quits
            let armed = self.close_armed.is_some_and(|t| t.elapsed().as_secs_f64() < 10.0);
            match self.busy() {
                Some(what) if !armed => {
                    e.accept_close.set(false);
                    self.close_armed = Some(std::time::Instant::now());
                    let line = format!("Poing is still working on {what}. Close again to quit anyway: it stops, and you can make it again next time.");
                    self.set_status(cx, &line);
                    self.ui.label(cx, ids!(home_status)).set_text(cx, &line);
                    self.ui.redraw(cx);
                    return;
                }
                _ => {
                    for st in self.state.iter_mut().chain(self.others.iter_mut()) {
                        let _ = st.save();
                    }
                }
            }
        }
        if let Event::Signal = event {
            self.drain_downloads(cx);
            self.drain_jobs(cx);
            self.drain_agent(cx);
        }
        self.drain_ai_port(cx, event);
        if let Some(st) = self.state.as_mut() {
            st.sync_keys();
            if st.keys_lane().is_some() && self.keys_pump.is_empty() {
                self.keys_pump = cx.start_interval(0.03);
            }
        }
        if self.keys_pump.is_event(event).is_some() {
            match self.state.as_mut().filter(|st| st.keys_lane().is_some()) {
                Some(st) => {
                    if let Some(note) = st.pump_recording() {
                        st.notice = Some(note);
                    }
                    // the meters are read when the timeline draws: it draws while they move
                    let heard = st.shared.meters.iter().any(|m| m.load(std::sync::atomic::Ordering::Relaxed) != 0);
                    if heard || st.recording() {
                        self.ui.widget(cx, ids!(timeline)).redraw(cx);
                    }
                }
                None => {
                    cx.stop_timer(self.keys_pump);
                    self.keys_pump = Timer::empty();
                }
            }
        }
        if self.live_pump.is_event(event).is_some() {
            self.pump_live(cx);
        }
        if self.plugin_watch.is_event(event).is_some() && self.plugin_pump.is_empty() {
            if let Some(said) = self.state.as_mut().filter(|st| st.has_plugins()).and_then(|st| st.pump_plugins()) {
                self.set_status(cx, &said);
                if let Some(st) = self.state.as_mut() {
                    st.sel_changed = true;
                }
            }
        }
        if self.plugin_pump.is_event(event).is_some() {
            match self.state.as_mut() {
                Some(st) => {
                    if let Some(said) = st.pump_plugins() {
                        st.sel_changed = true;
                        self.ui.label(cx, ids!(status_label)).set_text(cx, &said);
                    }
                    let Some(st) = self.state.as_mut() else { return };
                    if !st.editors_open() {
                        cx.stop_timer(self.plugin_pump);
                        self.plugin_pump = Timer::empty();
                    }
                }
                None => {
                    cx.stop_timer(self.plugin_pump);
                    self.plugin_pump = Timer::empty();
                }
            }
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
        if let Event::MidiPorts(ports) = event {
            let inputs = ports.all_inputs();
            cx.use_midi_inputs(&inputs);
            let names: Vec<&str> = ports.descs.iter().filter(|d| d.port_type.is_input()).map(|d| d.name.as_str()).collect();
            log!("poing: MIDI inputs: {}", if names.is_empty() { "none (A to K play)".into() } else { names.join(", ") });
            // the live controller: its inputs play the decks, only its outputs get lights
            let map = self.live_map();
            let dj = |input: bool| ports.descs.iter().filter(|d| d.port_type.is_input() == input && map.is_port(&d.name)).map(|d| d.port_id).collect::<Vec<_>>();
            let (dj_in, dj_out) = (dj(true), dj(false));
            if !dj_out.is_empty() {
                cx.use_midi_outputs(&dj_out);
            }
            self.live_ports = (dj_in.iter().map(|p| p.0 .0).collect(), dj_out.iter().map(|p| p.0 .0).collect());
            if let Some(sh) = &self.shared {
                sh.decks.set_ports(&self.live_ports.0, &self.live_ports.1);
            }
            if !dj_in.is_empty() {
                log!("poing: live: {} is the live controller (port `{}`)", ports.descs.iter().find(|d| d.port_id == dj_in[0]).map_or("?", |d| d.name.as_str()), map.port);
            }
            self.midi_inputs = inputs.len() - dj_in.len();
        }
        let piano = self.piano(cx, event);
        // Shortcuts work anywhere but in a text field (which keeps its own keys).
        if let (Event::KeyDown(ke), false) = (event, piano) {
            if let Some(st) = self.state.as_mut() {
                if !typing(&self.ui, cx) && timeline::shortcut(st, ke) {
                    self.ui.widget(cx, ids!(timeline)).redraw(cx);
                }
            }
        }
        self.match_event(cx, event);
        if let Some((feature, resume)) = self.state.as_mut().and_then(|st| st.needs_download.take()) {
            self.offer_download(cx, feature, resume);
        }
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
