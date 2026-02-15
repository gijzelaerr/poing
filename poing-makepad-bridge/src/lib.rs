mod editor;

#[cfg(target_os = "macos")]
mod macos;

use editor::MakepadEditor;
use nih_plug::prelude::*;
use poing_core::SharedState;

/// Create a Makepad-based editor for the plugin.
///
/// Returns `None` if the current platform is not supported.
pub fn create_editor(shared_state: SharedState, size: (u32, u32)) -> Option<Box<dyn Editor>> {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        Some(Box::new(MakepadEditor { size, shared_state }))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (shared_state, size);
        None
    }
}
