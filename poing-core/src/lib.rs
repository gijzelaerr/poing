pub mod audio_buffer;
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
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            prompt: Arc::new(Mutex::new(String::new())),
            model_path: Arc::new(Mutex::new(None)),
            generation_state: Arc::new(Mutex::new(GenerationState::Idle)),
            progress: Arc::new(Mutex::new(0.0)),
            generated_audio: Arc::new(Mutex::new(None)),
            recorded_audio: Arc::new(Mutex::new(Vec::new())),
            is_recording: Arc::new(AtomicBool::new(false)),
            sample_rate: Arc::new(Mutex::new(44100.0)),
        }
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}
