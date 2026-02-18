use std::path::Path;

/// Initiate an OS-level file drag from the plugin window.
///
/// This writes the generated audio to a temp WAV file and asks the OS
/// to start a drag operation so the user can drop it onto a DAW timeline.
pub fn start_file_drag(file_path: &Path) {
    #[cfg(target_os = "macos")]
    macos::start_file_drag_macos(file_path);

    #[cfg(target_os = "windows")]
    windows::start_file_drag_windows(file_path);

    #[cfg(target_os = "linux")]
    linux::start_file_drag_linux(file_path);
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::Path;

    #[allow(deprecated)]
    pub fn start_file_drag_macos(file_path: &Path) {
        use cocoa::base::{id, nil};
        use cocoa::foundation::{NSArray, NSPoint, NSString};
        use objc::runtime::Class;
        use objc::{msg_send, sel, sel_impl};

        unsafe {
            let path_str = file_path.to_string_lossy();

            // Create NSURL from file path
            let ns_path = NSString::alloc(nil).init_str(&path_str);
            let nsurl_class = Class::get("NSURL").unwrap();
            let url: id = msg_send![nsurl_class, fileURLWithPath: ns_path];

            // Create pasteboard item
            let pasteboard_item_class = Class::get("NSPasteboardItem").unwrap();
            let item: id = msg_send![pasteboard_item_class, alloc];
            let item: id = msg_send![item, init];

            let file_url_type = NSString::alloc(nil).init_str("public.file-url");
            let url_string: id = msg_send![url, absoluteString];
            let _: () = msg_send![item, setString: url_string forType: file_url_type];

            // Create dragging item
            let dragging_item_class = Class::get("NSDraggingItem").unwrap();
            let drag_item: id = msg_send![dragging_item_class, alloc];
            let drag_item: id = msg_send![drag_item, initWithPasteboardWriter: item];

            // Set dragging frame (small rect at origin)
            let frame = cocoa::foundation::NSRect::new(
                NSPoint::new(0.0, 0.0),
                cocoa::foundation::NSSize::new(64.0, 64.0),
            );
            let _: () = msg_send![drag_item, setDraggingFrame: frame contents: nil];

            // Get the key window's content view to begin the dragging session
            let app: id = msg_send![Class::get("NSApplication").unwrap(), sharedApplication];
            let window: id = msg_send![app, keyWindow];
            if window == nil {
                return;
            }
            let view: id = msg_send![window, contentView];
            if view == nil {
                return;
            }

            let items = NSArray::arrayWithObject(nil, drag_item);
            let event: id = msg_send![app, currentEvent];
            if event == nil {
                return;
            }

            let _: id = msg_send![view,
                beginDraggingSessionWithItems: items
                event: event
                source: view
            ];
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::path::Path;

    pub fn start_file_drag_windows(file_path: &Path) {
        use std::mem;
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::System::Com::{
            CoInitializeEx, COINIT_APARTMENTTHREADED,
        };
        use windows::Win32::System::Ole::DoDragDrop;
        use windows::Win32::System::Ole::DROPEFFECT_COPY;
        use windows::core::HRESULT;

        // DoDragDrop requires COM and a proper IDataObject + IDropSource implementation.
        // For now, log the path; a full implementation requires ~100 lines of COM boilerplate.
        // TODO: Implement COM IDataObject with CF_HDROP for full drag-drop support.
        let _ = file_path;
        eprintln!(
            "poing: drag-drop not yet implemented on Windows (file: {})",
            file_path.display()
        );
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::Path;

    pub fn start_file_drag_linux(file_path: &Path) {
        // XDND protocol requires setting atoms on the X11 window and handling
        // a sequence of client messages. For now, log the path.
        // TODO: Implement XDND protocol for full drag-drop support.
        let _ = file_path;
        eprintln!(
            "poing: drag-drop not yet implemented on Linux (file: {})",
            file_path.display()
        );
    }
}
