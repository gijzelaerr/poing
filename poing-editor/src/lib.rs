mod drag_source;
mod model;
mod waveform;

use model::{PoingEvent, PoingModel};
use nih_plug::prelude::Editor;
use nih_plug_vizia::vizia::prelude::*;
use nih_plug_vizia::widgets::ResizeHandle;
use nih_plug_vizia::{assets, create_vizia_editor, ViziaState, ViziaTheming};
use poing_core::SharedState;
use std::sync::Arc;
use std::time::Duration;
use waveform::WaveformView;

pub use nih_plug_vizia::ViziaState as EditorState;

const THEME_CSS: &str = include_str!("theme.css");

/// Returns the default editor window state (800x600).
pub fn default_state() -> Arc<ViziaState> {
    ViziaState::new(|| (800, 600))
}

/// Create the VIZIA-based plugin editor.
pub fn create(
    shared_state: SharedState,
    editor_state: Arc<ViziaState>,
) -> Option<Box<dyn Editor>> {
    create_vizia_editor(editor_state, ViziaTheming::Custom, move |cx, _| {
        assets::register_noto_sans_light(cx);
        assets::register_noto_sans_thin(cx);

        cx.add_stylesheet(THEME_CSS)
            .expect("Failed to add theme stylesheet");

        let poing_model = PoingModel::new(shared_state.clone());
        poing_model.build(cx);

        // Start a 30 Hz polling timer
        let timer = cx.add_timer(
            Duration::from_millis(33),
            None,
            |cx, action| {
                if let TimerAction::Tick(_) = action {
                    cx.emit(PoingEvent::TimerTick);
                }
            },
        );
        cx.start_timer(timer);

        VStack::new(cx, |cx| {
            // Header
            HStack::new(cx, |cx| {
                Label::new(cx, "Poing")
                    .class("header-title")
                    .font_family(vec![FamilyOwned::Name(String::from(
                        assets::NOTO_SANS,
                    ))])
                    .font_weight(FontWeightKeyword::Thin);

                Label::new(cx, "Neural Audio Generation").class("header-subtitle");
            })
            .height(Auto)
            .col_between(Pixels(12.0))
            .child_top(Stretch(1.0))
            .child_bottom(Stretch(1.0));

            // Model selection row
            HStack::new(cx, |cx| {
                Label::new(cx, "Model:").class("field-label");

                Dropdown::new(
                    cx,
                    |cx| {
                        Label::new(cx, PoingModel::selected_model_name)
                    },
                    |cx| {
                        Binding::new(cx, PoingModel::model_names, |cx, names_lens| {
                            let names = names_lens.get(cx);
                            for (i, name) in names.iter().enumerate() {
                                let name = name.clone();
                                Label::new(cx, &name)
                                    .class("dropdown-item")
                                    .width(Stretch(1.0))
                                    .on_press(move |cx| {
                                        cx.emit(PoingEvent::SelectModel(i));
                                        cx.emit(PopupEvent::Close);
                                    });
                            }
                        });
                    },
                )
                .width(Stretch(1.0));

                Button::new(
                    cx,
                    |cx| cx.emit(PoingEvent::BrowseModel),
                    |cx| Label::new(cx, "Browse"),
                );

                Button::new(
                    cx,
                    |cx| cx.emit(PoingEvent::RemoveModel),
                    |cx| Label::new(cx, "Remove"),
                );
            })
            .height(Auto)
            .col_between(Pixels(8.0))
            .child_top(Stretch(1.0))
            .child_bottom(Stretch(1.0));

            // Prompt input
            Textbox::new(cx, PoingModel::prompt)
                .on_edit(|cx, text| {
                    cx.emit(PoingEvent::SetPrompt(text));
                })
                .width(Stretch(1.0))
                .height(Auto);

            // Controls row
            HStack::new(cx, |cx| {
                Button::new(
                    cx,
                    |cx| cx.emit(PoingEvent::Generate),
                    |cx| Label::new(cx, "Generate"),
                )
                .class("generate");

                Button::new(
                    cx,
                    |cx| cx.emit(PoingEvent::ToggleRecording),
                    |cx| Label::new(cx, PoingModel::record_button_text),
                );

                Button::new(
                    cx,
                    |cx| cx.emit(PoingEvent::Export),
                    |cx| Label::new(cx, "Export"),
                );
            })
            .height(Auto)
            .col_between(Pixels(12.0));

            // Status
            Label::new(cx, PoingModel::status_text).class("status-label");

            // Progress bar
            ProgressBar::horizontal(cx, PoingModel::progress)
                .height(Pixels(6.0))
                .width(Stretch(1.0));

            // Waveform display
            WaveformView::new(cx)
                .width(Stretch(1.0))
                .height(Stretch(1.0))
                .class("waveform-container");
        })
        .width(Stretch(1.0))
        .height(Stretch(1.0));

        ResizeHandle::new(cx);
    })
}
