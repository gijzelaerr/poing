use nih_plug::prelude::*;
use poing_core::SharedState;
use std::any::Any;
use std::sync::Arc;

pub struct MakepadEditor {
    pub(crate) size: (u32, u32),
    pub(crate) shared_state: SharedState,
}

impl Editor for MakepadEditor {
    fn spawn(
        &self,
        parent: ParentWindowHandle,
        _context: Arc<dyn GuiContext>,
    ) -> Box<dyn Any + Send> {
        #[cfg(target_os = "macos")]
        {
            let parent_ptr = match parent {
                ParentWindowHandle::AppKitNsView(ptr) => ptr,
                _ => panic!("Expected AppKitNsView on macOS"),
            };
            let handle = crate::macos::create_embedded_makepad(parent_ptr, self.size);
            Box::new(handle)
        }

        #[cfg(target_os = "windows")]
        {
            let _ = parent;
            panic!("Makepad embedding not yet implemented on Windows");
        }

        #[cfg(target_os = "linux")]
        {
            let _ = parent;
            panic!("Makepad embedding not yet implemented on Linux");
        }
    }

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn set_scale_factor(&self, _factor: f32) -> bool {
        true
    }

    fn param_value_changed(&self, _id: &str, _normalized_value: f32) {}

    fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {}

    fn param_values_changed(&self) {}
}
