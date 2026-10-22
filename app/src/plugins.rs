//! VST3 plugins as lane effects: the installed library (found on disk, never
//! loaded to list it), loading an instance on the UI thread with its saved
//! knob values, and what the UI keeps to drive it. Windows for now.

use poing_model::Fx;
use std::path::{Path, PathBuf};

/// A plugin parameter the UI can show as a knob.
#[derive(Clone, Debug)]
pub struct PluginParam {
    pub id: u32,
    pub name: String,
    pub default: f64,
}

/// Installed plugins: (name, bundle path), sorted by name. Only the file
/// system is read: loading 60 plugins to list them can hang on licence checks.
pub fn library() -> Vec<(String, PathBuf)> {
    #[cfg(windows)]
    {
        let Ok(host) = vst3_host::Vst3Host::builder().scan_default_paths().build() else { return Vec::new() };
        let mut out: Vec<(String, PathBuf)> = host
            .scan_plugin_paths()
            .into_iter()
            .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p)))
            .collect();
        out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        out.dedup_by(|a, b| a.0 == b.0);
        out
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// Knobs worth showing first: named, not the plugin's own bypass or MIDI plumbing.
pub fn knob_params(params: &[PluginParam]) -> Vec<PluginParam> {
    params
        .iter()
        .filter(|p| {
            let n = p.name.trim().to_lowercase();
            !n.is_empty() && n != "-" && !n.contains("bypass") && !n.starts_with("midi") && !n.contains("program")
        })
        .take(6)
        .cloned()
        .collect()
}

/// A plugin instance ready for the rack, and the UI's handle to it.
#[cfg(windows)]
pub struct Loaded {
    pub runner: Box<crate::fx::Runner>,
    pub control: vst3_host::realtime::RtControl,
    pub params: Vec<PluginParam>,
}

/// Load a plugin on this (the UI) thread, set its saved knob values, start
/// it processing at `sample_rate` in PLUGIN_BLOCK blocks.
#[cfg(windows)]
pub fn load(path: &Path, sample_rate: u32, saved: Option<&Fx>) -> Result<Loaded, String> {
    use vst3_host::{realtime::RealtimePluginRunner, Vst3Host};
    let mut host = Vst3Host::builder()
        .sample_rate(sample_rate as f64)
        .block_size(crate::fx::PLUGIN_BLOCK)
        .build()
        .map_err(|e| e.to_string())?;
    let mut plugin = host.load_plugin(path).map_err(|e| format!("{} did not load: {e}", path.display()))?;
    let info = plugin.info().clone();
    if info.category.contains("Instrument") && !info.category.contains("Fx") {
        return Err(format!("{} is an instrument; a lane takes effects", info.name));
    }
    let params: Vec<PluginParam> = plugin
        .get_parameters()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|p| PluginParam { id: p.id, name: p.name.clone(), default: p.value })
        .collect();
    if let Some(fx) = saved {
        for p in &params {
            if let Some(v) = fx.param(&p.name) {
                let _ = plugin.set_parameter(p.id, v.clamp(0.0, 1.0));
            }
        }
    }
    let (mut runner, control) = RealtimePluginRunner::new(plugin, 512);
    runner.start().map_err(|e| format!("{} did not start: {e}", info.name))?;
    Ok(Loaded { runner: Box::new(runner), control, params })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knobs_skip_placeholders_and_bypass() {
        let p = |id, name: &str| PluginParam { id, name: name.into(), default: 0.0 };
        let all = [p(0, "Bypass"), p(1, "-"), p(2, "Decay"), p(3, "MIDI CC 1"), p(4, "Mix")];
        let names: Vec<String> = knob_params(&all).into_iter().map(|k| k.name).collect();
        assert_eq!(names, ["Decay", "Mix"]);
    }
}
