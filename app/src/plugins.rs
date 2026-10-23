//! VST3 plugins as lane effects and MIDI lane instruments: the installed
//! library (found on disk, never loaded to list it), loading an instance on the
//! UI thread with its saved state, and the UI's handle to it (knobs, its own
//! editor window, state). Windows for now.

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

/// A host set up for Poing's sample rate and plugin block.
#[cfg(windows)]
fn host_for(sample_rate: u32) -> Result<vst3_host::Vst3Host, String> {
    vst3_host::Vst3Host::builder()
        .sample_rate(sample_rate as f64)
        .block_size(crate::fx::PLUGIN_BLOCK)
        .build()
        .map_err(|e| e.to_string())
}

/// A loaded plugin, shared by the UI (editor window, knobs, state, and the
/// last reference: it is destroyed on the UI thread) and the rack, whose
/// audio thread only ever `try_lock`s it (a missed block passes through).
#[cfg(windows)]
pub type Shared = std::sync::Arc<std::sync::Mutex<vst3_host::Plugin>>;

/// Blocks a plugin was busy for (the UI held it): passed through or silent.
#[cfg(windows)]
pub static MISSED_BLOCKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The UI's side of a loaded plugin: what it shows and changes.
#[cfg(windows)]
pub struct Handle {
    pub plugin: Shared,
    pub params: Vec<PluginParam>,
    pub name: String,
    window: Option<vst3_host::PluginWindow>,
    /// Knob moves the plugin was too busy to take yet: (id, value), newest per id.
    pending: Vec<(u32, f64)>,
}

#[cfg(windows)]
impl Handle {
    /// Turn a knob (normalized 0..1): now if the plugin is free, else on the next pump.
    pub fn set_param(&mut self, id: u32, value: f64) {
        self.pending.retain(|(i, _)| *i != id);
        self.pending.push((id, value.clamp(0.0, 1.0)));
        self.flush();
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        if let Ok(mut p) = self.plugin.try_lock() {
            for (id, v) in self.pending.drain(..) {
                let _ = p.set_parameter(id, v);
            }
        }
    }

    /// The plugin's whole state (presets, wavetables, every parameter), if it is free.
    pub fn save_state(&self) -> Option<Vec<u8>> {
        self.plugin.try_lock().ok().and_then(|p| p.save_state().ok())
    }

    pub fn editor_open(&self) -> bool {
        self.window.as_ref().is_some_and(|w| w.is_open())
    }

    /// Open the plugin's own window (or bring it back).
    pub fn open_editor(&mut self) -> Result<(), String> {
        if self.editor_open() {
            return Ok(());
        }
        let mut w = vst3_host::PluginWindow::new(self.plugin.clone());
        w.open().map_err(|e| format!("{} has no window here: {e}", self.name))?;
        self.window = Some(w);
        Ok(())
    }

    pub fn close_editor(&mut self) {
        if let Some(mut w) = self.window.take() {
            w.close();
        }
    }

    /// Keep the editor window and queued knobs moving (UI thread, often).
    /// True when the user closed the editor just now.
    pub fn pump(&mut self) -> bool {
        self.flush();
        let Some(w) = &self.window else { return false };
        let _ = w.service_platform_events();
        if w.closed_by_user() {
            self.close_editor();
            return true;
        }
        false
    }

    /// Is the rack done with it (only this handle holds it)?
    pub fn released(&self) -> bool {
        std::sync::Arc::strong_count(&self.plugin) == 1
    }

    /// Destroy it here, on the UI thread (once [`Self::released`]).
    pub fn shut_down(mut self) {
        self.close_editor();
        if let Ok(mut p) = self.plugin.lock() {
            let _ = p.stop_processing();
        }
    }
}

/// Plugin windows are made on the app thread, but Makepad runs its Win32
/// message loop on a thread of its own: without this nobody dispatches their
/// messages (they paint, but take no clicks). Drains only this thread's
/// queue, so Makepad's own windows are untouched. Call often while one is open.
#[cfg(windows)]
pub fn pump_window_messages() {
    #[repr(C)]
    struct Msg {
        hwnd: *mut std::ffi::c_void,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt: [i32; 2],
        private: u32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn PeekMessageW(msg: *mut Msg, hwnd: *mut std::ffi::c_void, min: u32, max: u32, remove: u32) -> i32;
        fn TranslateMessage(msg: *const Msg) -> i32;
        fn DispatchMessageW(msg: *const Msg) -> isize;
    }
    const PM_REMOVE: u32 = 1;
    let mut msg = Msg { hwnd: std::ptr::null_mut(), message: 0, wparam: 0, lparam: 0, time: 0, pt: [0, 0], private: 0 };
    // bounded: a window that floods its queue cannot hold the app thread
    for _ in 0..256 {
        // SAFETY: plain Win32 calls on a stack MSG for this thread's own queue.
        unsafe {
            if PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) == 0 {
                break;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
/// Load a plugin on this (the UI) thread: as a lane effect, or (`instrument`)
/// to play a MIDI lane. Its saved state wins over its saved knob values.
/// Returns the UI's handle and the rack's share of it.
#[cfg(windows)]
pub fn load(path: &Path, sample_rate: u32, saved: Option<&Fx>, state: Option<&[u8]>, instrument: bool) -> Result<(Handle, Shared), String> {
    use vst3_host::audio::{BusDirection, MediaType};
    let mut host = host_for(sample_rate)?;
    let mut plugin = host.load_plugin(path).map_err(|e| format!("{} did not load: {e}", path.display()))?;
    let info = plugin.info().clone();
    let is_instrument = info.category.contains("Instrument");
    if instrument && !is_instrument {
        return Err(format!("{} is not an instrument", info.name));
    }
    if !instrument && is_instrument && !info.category.contains("Fx") {
        return Err(format!("{} is an instrument; a lane takes effects", info.name));
    }
    let params: Vec<PluginParam> = plugin
        .get_parameters()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|p| PluginParam { id: p.id, name: p.name.clone(), default: p.value })
        .collect();
    let restored = state.is_some_and(|bytes| plugin.load_state(bytes).is_ok());
    if let (false, Some(fx)) = (restored, saved) {
        for p in &params {
            if let Some(v) = fx.param(&p.name) {
                let _ = plugin.set_parameter(p.id, v.clamp(0.0, 1.0));
            }
        }
    }
    if instrument {
        let _ = plugin.set_bus_active(MediaType::Event, BusDirection::Input, 0, true);
    }
    plugin.start_processing().map_err(|e| format!("{} did not start: {e}", info.name))?;
    if instrument {
        let _ = plugin.set_playing(true);
    }
    let shared: Shared = std::sync::Arc::new(std::sync::Mutex::new(plugin));
    Ok((Handle { plugin: shared.clone(), params, name: info.name, window: None, pending: Vec::new() }, shared))
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
