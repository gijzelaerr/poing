pub mod audio_buffer;
pub mod config;
pub mod model;
pub mod musicgen;
pub mod wav;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub enum GenerationState {
    Idle,
    Generating,
    Complete,
    Error(String),
}

/// Shared state for cross-thread communication between the audio thread,
/// GUI, and inference thread.
#[derive(Clone)]
pub struct SharedState {
    pub prompt: Arc<Mutex<String>>,
    pub model_path: Arc<Mutex<Option<PathBuf>>>,
    pub generation_state: Arc<Mutex<GenerationState>>,
    pub progress: Arc<Mutex<f32>>,
    pub generated_audio: Arc<Mutex<Option<Vec<f32>>>>,
    pub recorded_audio: Arc<Mutex<Vec<f32>>>,
    pub is_recording: Arc<AtomicBool>,
    pub sample_rate: Arc<Mutex<f32>>,
    pub model_paths: Arc<Mutex<Vec<PathBuf>>>,
    pub pending_browse: Arc<AtomicBool>,
    pub browse_result: Arc<Mutex<Option<PathBuf>>>,
}

impl SharedState {
    pub fn new() -> Self {
        let cfg = config::load_config();
        let first_path = cfg.model_paths.first().cloned();
        Self {
            prompt: Arc::new(Mutex::new(String::new())),
            model_path: Arc::new(Mutex::new(first_path)),
            generation_state: Arc::new(Mutex::new(GenerationState::Idle)),
            progress: Arc::new(Mutex::new(0.0)),
            generated_audio: Arc::new(Mutex::new(None)),
            recorded_audio: Arc::new(Mutex::new(Vec::new())),
            is_recording: Arc::new(AtomicBool::new(false)),
            sample_rate: Arc::new(Mutex::new(44100.0)),
            model_paths: Arc::new(Mutex::new(cfg.model_paths)),
            pending_browse: Arc::new(AtomicBool::new(false)),
            browse_result: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}
