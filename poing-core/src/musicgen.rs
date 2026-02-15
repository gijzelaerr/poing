use std::path::Path;

/// Generate audio from a text prompt using a MusicGen ONNX model.
///
/// Returns mono f32 samples at the model's native sample rate (typically 32kHz).
pub fn generate_from_text(
    _prompt: &str,
    _model_dir: &Path,
    _progress_callback: impl Fn(f32),
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    todo!("MusicGen text-to-audio inference not yet implemented")
}

/// Generate audio from a text prompt combined with input audio using a MusicGen ONNX model.
///
/// Returns mono f32 samples at the model's native sample rate (typically 32kHz).
pub fn generate_from_audio(
    _prompt: &str,
    _input_audio: &[f32],
    _model_dir: &Path,
    _progress_callback: impl Fn(f32),
) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    todo!("MusicGen audio-to-audio inference not yet implemented")
}
