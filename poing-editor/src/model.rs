use nih_plug_vizia::vizia::prelude::*;
use poing_core::config;
use poing_core::{GenerationState, SharedState};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq)]
pub enum PoingEvent {
    Generate,
    ToggleRecording,
    Export,
    ExportStatus(String),
    BrowseModel,
    BrowseModelResult(PathBuf),
    RemoveModel,
    SelectModel(usize),
    SetPrompt(String),
    StartDrag,
    TimerTick,
}

#[derive(Lens)]
pub struct PoingModel {
    #[lens(ignore)]
    pub shared_state: SharedState,
    #[lens(ignore)]
    proxy: ContextProxy,
    #[lens(ignore)]
    was_generating: bool,

    pub status_text: String,
    pub progress: f32,
    pub prompt: String,
    pub model_names: Vec<String>,
    pub selected_model_index: usize,
    pub is_generating: bool,
    pub record_button_text: String,
    pub selected_model_name: String,
    pub waveform_data: Arc<Vec<(f32, f32)>>,
}

impl PoingModel {
    pub fn new(shared_state: SharedState, proxy: ContextProxy) -> Self {
        let model_paths = shared_state.model_paths.lock().unwrap().clone();
        let model_names = Self::paths_to_names(&model_paths);
        let selected_model_name = model_names.first().cloned().unwrap_or_else(|| "No models loaded".into());

        Self {
            shared_state,
            proxy,
            was_generating: false,
            status_text: "Ready".into(),
            progress: 0.0,
            prompt: String::new(),
            model_names,
            selected_model_index: 0,
            is_generating: false,
            record_button_text: "Record".into(),
            selected_model_name,
            waveform_data: Arc::new(Vec::new()),
        }
    }

    fn paths_to_names(paths: &[PathBuf]) -> Vec<String> {
        if paths.is_empty() {
            vec!["No models loaded".into()]
        } else {
            paths
                .iter()
                .map(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.to_string_lossy().into_owned())
                })
                .collect()
        }
    }

    fn poll_shared_state(&mut self, cx: &mut EventContext) {
        let gen_state = self.shared_state.generation_state.lock().unwrap().clone();
        let progress = *self.shared_state.progress.lock().unwrap();
        let is_recording = self.shared_state.is_recording.load(Ordering::Relaxed);

        // Update waveform when generation transitions from Generating to Complete
        if self.was_generating && matches!(gen_state, GenerationState::Complete) {
            let audio = self.shared_state.generated_audio.lock().unwrap().clone();
            if let Some(audio) = audio {
                self.waveform_data = Arc::new(compute_waveform_columns(&audio, 1024));
            }
        }
        self.was_generating = matches!(gen_state, GenerationState::Generating);
        self.is_generating = matches!(gen_state, GenerationState::Generating);

        // Update progress
        self.progress = progress;

        // Update status label -- recording takes priority
        self.status_text = if is_recording {
            "Recording...".into()
        } else {
            match &gen_state {
                GenerationState::Idle => "Ready".into(),
                GenerationState::Generating => {
                    format!("Generating... {:.0}%", progress * 100.0)
                }
                GenerationState::Complete => {
                    let samples = self
                        .shared_state
                        .generated_audio
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map_or(0, |a| a.len());
                    format!("Complete \u{2014} {} samples generated", samples)
                }
                GenerationState::Error(e) => format!("Error: {}", e),
            }
        };

        // Update record button text
        self.record_button_text = if is_recording {
            "Stop Recording".into()
        } else {
            "Record".into()
        };

        cx.needs_redraw();
    }

    fn start_generation(&mut self, cx: &mut EventContext) {
        // Prevent multiple simultaneous generations (set immediately, don't wait for timer)
        if self.is_generating {
            return;
        }
        self.is_generating = true;
        self.status_text = "Loading models...".into();
        self.progress = 0.0;
        cx.needs_redraw();

        let prompt = self.prompt.clone();
        if prompt.trim().is_empty() {
            self.is_generating = false;
            *self.shared_state.generation_state.lock().unwrap() =
                GenerationState::Error("Please enter a prompt".into());
            return;
        }

        if self.shared_state.model_path.lock().unwrap().is_none() {
            self.is_generating = false;
            *self.shared_state.generation_state.lock().unwrap() =
                GenerationState::Error("No model path configured".into());
            return;
        }

        *self.shared_state.prompt.lock().unwrap() = prompt.clone();
        *self.shared_state.generation_state.lock().unwrap() = GenerationState::Generating;
        *self.shared_state.progress.lock().unwrap() = 0.0;
        *self.shared_state.generated_audio.lock().unwrap() = None;

        let state = self.shared_state.clone();
        let mut proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let model_dir = state.model_path.lock().unwrap().clone();
                if let Some(model_dir) = model_dir {
                    let progress_state = state.progress.clone();
                    let progress_proxy = Mutex::new(proxy.clone());
                    match poing_core::musicgen::generate_from_text(
                        &prompt,
                        &model_dir,
                        move |p| {
                            *progress_state.lock().unwrap() = p;
                            let _ = progress_proxy.lock().unwrap().emit(PoingEvent::TimerTick);
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
            }));
            if let Err(panic) = result {
                let msg = if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = panic.downcast_ref::<&str>() {
                    s.to_string()
                } else {
                    "Unknown panic".to_string()
                };
                eprintln!("[poing] Generation thread panicked: {}", msg);
                if let Ok(mut gen_state) = state.generation_state.try_lock() {
                    *gen_state = GenerationState::Error(format!("Internal error: {}", msg));
                }
            }
            // Final UI update after generation completes (or errors)
            let _ = proxy.emit(PoingEvent::TimerTick);
        });
    }

    fn toggle_recording(&mut self, _cx: &mut EventContext) {
        let was_recording = self.shared_state.is_recording.load(Ordering::Relaxed);
        self.shared_state
            .is_recording
            .store(!was_recording, Ordering::Relaxed);

        if was_recording {
            // Just stopped recording
            let recorded = self.shared_state.recorded_audio.lock().unwrap().clone();
            if !recorded.is_empty() {
                self.waveform_data = Arc::new(compute_waveform_columns(&recorded, 1024));
                self.status_text = format!("Recorded {} samples", recorded.len());
            }
        } else {
            // Just started recording
            self.shared_state.recorded_audio.lock().unwrap().clear();
            self.status_text = "Recording...".into();
        }
    }

    fn export_audio(&mut self, _cx: &mut EventContext) {
        let audio = self.shared_state.generated_audio.lock().unwrap().clone();
        let Some(samples) = audio else {
            self.status_text = "No audio to export".into();
            return;
        };

        // Spawn dialog on background thread to avoid RefCell re-entrancy from
        // macOS modal event loop. rfd dispatches to the main thread internally.
        let mut proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .set_file_name("poing_generated.wav")
                .add_filter("WAV", &["wav"])
                .save_file();

            if let Some(path) = result {
                let status = match poing_core::wav::write_wav(&samples, 32000, &path) {
                    Ok(()) => format!("Exported to {}", path.display()),
                    Err(e) => format!("Export failed: {}", e),
                };
                let _ = proxy.emit(PoingEvent::ExportStatus(status));
            }
        });
    }

    fn browse_model(&mut self, _cx: &mut EventContext) {
        // Spawn dialog on background thread to avoid RefCell re-entrancy from
        // macOS modal event loop. rfd dispatches to the main thread internally.
        let mut proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = rfd::FileDialog::new()
                .set_title("Select Model Directory")
                .pick_folder();

            if let Some(path) = result {
                let _ = proxy.emit(PoingEvent::BrowseModelResult(path));
            }
        });
    }

    fn handle_browse_result(&mut self, path: &PathBuf) {
        if !config::validate_model_dir(path) {
            *self.shared_state.generation_state.lock().unwrap() = GenerationState::Error(
                "Invalid model directory: missing required files (text_encoder.onnx, decoder_model_merged.onnx, encodec_decode.onnx, tokenizer.json)".into(),
            );
            return;
        }

        let index = {
            let mut model_paths = self.shared_state.model_paths.lock().unwrap();
            if !model_paths.contains(path) {
                model_paths.push(path.clone());
                let cfg = config::PoingConfig {
                    model_paths: model_paths.clone(),
                };
                config::save_config(&cfg);
            }
            model_paths
                .iter()
                .position(|p| p == path)
                .unwrap_or(0)
        };

        *self.shared_state.model_path.lock().unwrap() = Some(path.clone());
        let model_paths = self.shared_state.model_paths.lock().unwrap().clone();
        self.model_names = Self::paths_to_names(&model_paths);
        self.selected_model_index = index;
        self.selected_model_name = self.model_names.get(index).cloned().unwrap_or_default();

        *self.shared_state.generation_state.lock().unwrap() = GenerationState::Idle;
    }

    fn remove_selected_model(&mut self, _cx: &mut EventContext) {
        let mut model_paths = self.shared_state.model_paths.lock().unwrap();
        let selected = self.selected_model_index;

        if selected < model_paths.len() {
            model_paths.remove(selected);
            let cfg = config::PoingConfig {
                model_paths: model_paths.clone(),
            };
            config::save_config(&cfg);

            if model_paths.is_empty() {
                drop(model_paths);
                *self.shared_state.model_path.lock().unwrap() = None;
                self.model_names = Self::paths_to_names(&[]);
                self.selected_model_index = 0;
                self.selected_model_name = "No models loaded".into();
            } else {
                let idx = selected.min(model_paths.len() - 1);
                let path = model_paths[idx].clone();
                let names = Self::paths_to_names(&model_paths);
                drop(model_paths);
                *self.shared_state.model_path.lock().unwrap() = Some(path);
                self.model_names = names;
                self.selected_model_index = idx;
                self.selected_model_name = self.model_names.get(idx).cloned().unwrap_or_default();
            }
        }
    }

    fn select_model(&mut self, index: usize) {
        let model_paths = self.shared_state.model_paths.lock().unwrap().clone();
        if let Some(path) = model_paths.get(index) {
            *self.shared_state.model_path.lock().unwrap() = Some(path.clone());
            self.selected_model_index = index;
            self.selected_model_name = self.model_names.get(index).cloned().unwrap_or_default();
        }
    }

    fn start_drag(&self) {
        let audio = self.shared_state.generated_audio.lock().unwrap().clone();
        if let Some(samples) = audio {
            if let Ok(path) = poing_core::wav::write_wav_temp(&samples, 32000) {
                crate::drag_source::start_file_drag(&path);
            }
        }
    }
}

impl Model for PoingModel {
    fn event(&mut self, cx: &mut EventContext, event: &mut Event) {
        event.map(|e, _| match e {
            PoingEvent::Generate => self.start_generation(cx),
            PoingEvent::ToggleRecording => self.toggle_recording(cx),
            PoingEvent::Export => self.export_audio(cx),
            PoingEvent::ExportStatus(status) => {
                self.status_text = status.clone();
                cx.needs_redraw();
            }
            PoingEvent::BrowseModel => self.browse_model(cx),
            PoingEvent::BrowseModelResult(path) => {
                self.handle_browse_result(path);
                cx.needs_redraw();
            }
            PoingEvent::RemoveModel => self.remove_selected_model(cx),
            PoingEvent::SelectModel(index) => self.select_model(*index),
            PoingEvent::SetPrompt(text) => self.prompt = text.clone(),
            PoingEvent::StartDrag => self.start_drag(),
            PoingEvent::TimerTick => self.poll_shared_state(cx),
        });
    }
}

/// Compute min/max pairs per column from audio samples for waveform rendering.
pub fn compute_waveform_columns(samples: &[f32], num_cols: usize) -> Vec<(f32, f32)> {
    if samples.is_empty() {
        return vec![(0.0, 0.0); num_cols];
    }

    let samples_per_col = samples.len() as f32 / num_cols as f32;
    (0..num_cols)
        .map(|col| {
            let start = (col as f32 * samples_per_col) as usize;
            let end = (((col + 1) as f32 * samples_per_col) as usize).min(samples.len());
            let mut min_val: f32 = 0.0;
            let mut max_val: f32 = 0.0;
            for i in start..end {
                min_val = min_val.min(samples[i]);
                max_val = max_val.max(samples[i]);
            }
            (min_val, max_val)
        })
        .collect()
}
