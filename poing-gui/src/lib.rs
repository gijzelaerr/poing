use makepad_widgets::*;
use poing_core::{GenerationState, SharedState};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;

script_mod! {
    use mod.prelude.widgets.*

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                pass.clear_color: vec4(0.08, 0.08, 0.12, 1.0)
                window.inner_size: vec2(800, 600)
                body +: {
                    width: Fill height: Fill
                    flow: Down
                    padding: {left: 24, top: 24, right: 24, bottom: 24}
                    spacing: 16

                    // Header
                    View {
                        width: Fill height: Fit
                        flow: Right
                        align: {y: 0.5}
                        spacing: 12

                        Label {
                            text: "Poing"
                            draw_text.color: #xfff
                            draw_text.text_style.font_size: 28
                        }

                        Label {
                            text: "Neural Audio Generation"
                            draw_text.color: #x666
                            draw_text.text_style.font_size: 12
                            margin: {top: 10}
                        }
                    }

                    // Prompt input
                    prompt_input := TextInput {
                        width: Fill height: Fit
                        text: ""
                        draw_bg: {color: #x1a1a2e}
                        draw_text: {color: #xddd, text_style: {font_size: 12}}
                    }

                    // Controls row
                    View {
                        width: Fill height: Fit
                        flow: Right
                        spacing: 12

                        generate_btn := Button {
                            text: "Generate"
                        }

                        record_btn := Button {
                            text: "Record"
                        }
                    }

                    // Status
                    status_label := Label {
                        text: "Ready"
                        draw_text.color: #x888
                        draw_text.text_style.font_size: 11
                    }

                    // Progress bar (shader-driven fill)
                    progress_bar := View {
                        width: Fill height: 6
                        draw_bg: {
                            instance progress: 0.0
                            fn pixel(self) -> vec4 {
                                let track = vec4(0.1, 0.1, 0.15, 1.0);
                                let fill = vec4(0.24, 0.48, 0.82, 1.0);
                                if self.pos.x < self.progress {
                                    return fill;
                                }
                                return track;
                            }
                        }
                    }

                    // Waveform display
                    waveform_view := View {
                        width: Fill height: Fill
                        draw_bg: {
                            fn pixel(self) -> vec4 {
                                let bg = vec4(0.06, 0.06, 0.09, 1.0);
                                let center_dist = abs(self.pos.y - 0.5);
                                let center_line = 1.0 - smoothstep(0.001, 0.004, center_dist);
                                let line_color = vec4(0.18, 0.18, 0.25, 1.0);
                                return mix(bg, line_color, center_line);
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
    shared_state: Option<SharedState>,
}

impl App {
    pub fn run(vm: &mut ScriptVm) -> Self {
        makepad_widgets::script_mod(vm);
        App::from_script_mod(vm, self::script_mod)
    }

    fn start_generation(&mut self, cx: &mut Cx) {
        let shared_state = match &self.shared_state {
            Some(s) => s.clone(),
            None => return,
        };

        let prompt = self.ui.text_input(cx, ids!(prompt_input)).text();
        if prompt.is_empty() {
            return;
        }

        *shared_state.prompt.lock().unwrap() = prompt.clone();
        *shared_state.generation_state.lock().unwrap() = GenerationState::Generating;
        *shared_state.progress.lock().unwrap() = 0.0;
        *shared_state.generated_audio.lock().unwrap() = None;

        let state = shared_state;
        std::thread::spawn(move || {
            let model_dir = state.model_path.lock().unwrap().clone();
            if let Some(model_dir) = model_dir {
                let progress_state = state.progress.clone();
                match poing_core::musicgen::generate_from_text(
                    &prompt,
                    &model_dir,
                    move |p| {
                        *progress_state.lock().unwrap() = p;
                    },
                ) {
                    Ok(audio) => {
                        *state.generated_audio.lock().unwrap() = Some(audio);
                        *state.generation_state.lock().unwrap() = GenerationState::Complete;
                    }
                    Err(e) => {
                        *state.generation_state.lock().unwrap() =
                            GenerationState::Error(e.to_string());
                    }
                }
            } else {
                *state.generation_state.lock().unwrap() =
                    GenerationState::Error("No model path configured".into());
            }
        });

        cx.new_next_frame();
    }

    fn toggle_recording(&mut self, cx: &mut Cx) {
        let shared_state = match &self.shared_state {
            Some(s) => s,
            None => return,
        };

        let was_recording = shared_state.is_recording.load(Ordering::Relaxed);
        shared_state
            .is_recording
            .store(!was_recording, Ordering::Relaxed);

        let label = if !was_recording {
            "Stop Recording"
        } else {
            "Record"
        };
        self.ui.button(cx, ids!(record_btn)).set_text(cx, label);
    }

    fn update_from_shared_state(&mut self, cx: &mut Cx) {
        let shared_state = match &self.shared_state {
            Some(s) => s,
            None => return,
        };

        let gen_state = shared_state.generation_state.lock().unwrap().clone();
        let progress = *shared_state.progress.lock().unwrap();

        // Update status label
        let status = match &gen_state {
            GenerationState::Idle => "Ready".into(),
            GenerationState::Generating => format!("Generating... {:.0}%", progress * 100.0),
            GenerationState::Complete => {
                let samples = shared_state
                    .generated_audio
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map_or(0, |a| a.len());
                format!("Complete \u{2014} {} samples generated", samples)
            }
            GenerationState::Error(e) => format!("Error: {}", e),
        };
        self.ui
            .label(cx, ids!(status_label))
            .set_text(cx, &status);

        // Keep requesting frames while generating
        if matches!(gen_state, GenerationState::Generating) {
            cx.new_next_frame();
        }

        cx.redraw_all();
    }
}

impl MatchEvent for App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.ui.button(cx, ids!(generate_btn)).clicked(actions) {
            self.start_generation(cx);
        }
        if self.ui.button(cx, ids!(record_btn)).clicked(actions) {
            self.toggle_recording(cx);
        }
    }
}

impl AppMain for App {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        cx.with_widget_tree(|cx| {
            self.match_event(cx, event);
            self.ui.handle_event(cx, event, &mut Scope::empty());
        });

        if let Event::NextFrame(_) = event {
            self.update_from_shared_state(cx);
        }
    }
}

/// Create the Makepad event handler closure for embedding.
/// This is called by poing-makepad-bridge to set up the Cx.
pub fn create_event_handler(shared_state: SharedState) -> Box<dyn FnMut(&mut Cx, &Event)> {
    let app: Rc<RefCell<Option<App>>> = Rc::new(RefCell::new(None));
    Box::new(move |cx, event| {
        if let Event::Startup = event {
            let mut new_app = cx.with_vm(|vm| App::run(vm));
            new_app.shared_state = Some(shared_state.clone());
            *app.borrow_mut() = Some(new_app);
        }
        if let Some(app) = &mut *app.borrow_mut() {
            <dyn AppMain>::handle_event(app, cx, event);
        }
    })
}
