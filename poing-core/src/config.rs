use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PoingConfig {
    pub model_paths: Vec<PathBuf>,
}

const REQUIRED_MODEL_FILES: &[&str] = &[
    "text_encoder.onnx",
    "decoder_model_merged.onnx",
    "encodec_decode.onnx",
    "tokenizer.json",
];

pub fn config_path() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("poing");
    path.push("config.json");
    path
}

pub fn load_config() -> PoingConfig {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => PoingConfig::default(),
    }
}

pub fn save_config(config: &PoingConfig) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(&path, json);
    }
}

pub fn validate_model_dir(path: &Path) -> bool {
    REQUIRED_MODEL_FILES
        .iter()
        .all(|file| path.join(file).exists())
}
