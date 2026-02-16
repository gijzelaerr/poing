//! macOS-specific Makepad embedding.
//!
//! Creates a Makepad Cx, lets it create a MetalWindow (NSWindow + NSView + CAMetalLayer),
//! then reparents the NSView into the DAW's parent view. An NSTimer drives the render loop
//! through the DAW's existing run loop.

use makepad_widgets::{
    Cx, CxOsApi, Event, OsType,
    cx_native::EventFlow,
    makepad_platform::{
        self,
        makepad_objc_sys::{msg_send, sel, sel_impl},
        os::apple::{
            apple_classes::init_apple_classes_global,
            apple_sys::ObjcId,
            macos::{
                macos::MetalWindow,
                macos_app::{init_macos_app_global, with_macos_app},
                macos_event::MacosEvent,
            },
            metal::MetalCx,
        },
    },
};
use poing_core::SharedState;
use std::cell::RefCell;
use std::os::raw::c_void;
use std::rc::Rc;
use std::sync::Once;

static INIT: Once = Once::new();

/// Handle returned from create_embedded_makepad. Cleans up on Drop.
pub struct EmbeddedMakepadHandle {
    // Store the view pointer for cleanup
    view: ObjcId,
    // Keep Rc to prevent early drop (the event callback holds a clone)
    _cx: Rc<RefCell<Cx>>,
}

// Safety: The handle is just a token for lifecycle management.
// All actual GUI work happens on the main thread via the DAW's run loop.
// This matches the pattern used by all nih-plug editor backends.
unsafe impl Send for EmbeddedMakepadHandle {}

impl Drop for EmbeddedMakepadHandle {
    fn drop(&mut self) {
        // Stop Makepad's timer
        with_macos_app(|app| app.stop_timer(0));
        // Remove our view from the DAW's parent
        unsafe {
            let () = msg_send![self.view, removeFromSuperview];
        }
    }
}

/// Create an embedded Makepad instance inside the given parent NSView.
///
/// This replicates the setup from `Cx::event_loop()` but without calling
/// `MacosApp::event_loop()` (which blocks forever). Instead, Makepad's
/// timer system drives rendering through the DAW's existing run loop.
pub fn create_embedded_makepad(
    parent_nsview: *mut c_void,
    size: (u32, u32),
    shared_state: SharedState,
) -> EmbeddedMakepadHandle {
    let parent: ObjcId = parent_nsview as ObjcId;
    let _ = size; // Size is determined by the parent view

    // One-time initialization: register Objective-C classes.
    // init_macos_app_global registers MacosClasses (NSView subclass, timer delegate, etc.)
    // and creates the MacosApp singleton. This also calls NSApplication::sharedApplication
    // which is safe in a plugin context (returns the existing app).
    INIT.call_once(|| {
        init_apple_classes_global();
        // Provide a dummy callback; we'll replace it below.
        init_macos_app_global(Box::new(|_| EventFlow::Poll));
    });

    // Create Cx with poing-gui's event handler
    let event_handler = poing_gui::create_event_handler(shared_state);
    let cx = Rc::new(RefCell::new(Cx::new(event_handler)));

    // Set up Cx (replicating Cx::event_loop setup)
    cx.borrow_mut().self_ref = Some(cx.clone());
    cx.borrow_mut().os_type = OsType::Macos;

    // Create Metal context
    let metal_cx: Rc<RefCell<MetalCx>> = Rc::new(RefCell::new(MetalCx::new()));
    cx.borrow_mut().os.metal_device = Some(metal_cx.borrow().device);

    // Initialize OS-specific Cx state
    cx.borrow_mut().init_cx_os();

    // Create metal_windows container
    let metal_windows: Rc<RefCell<Vec<MetalWindow>>> = Rc::new(RefCell::new(Vec::new()));

    // Install our event callback into MacosApp.
    // This replaces the dummy callback from INIT and routes events through our Cx.
    {
        let cx_clone = cx.clone();
        let metal_cx_clone = metal_cx.clone();
        let metal_windows_clone = metal_windows.clone();

        with_macos_app(|app| {
            app.event_callback = Some(Box::new(move |event| {
                let mut cx_ref = cx_clone.borrow_mut();
                let mut mcx = metal_cx_clone.borrow_mut();
                let mut mw = metal_windows_clone.borrow_mut();
                let event_flow = cx_ref.cocoa_event_callback(event, &mut mcx, &mut mw);
                let executor = cx_ref.executor.take().unwrap();
                drop(cx_ref);
                executor.run_until_stalled();
                let mut cx_ref = cx_clone.borrow_mut();
                cx_ref.executor = Some(executor);
                event_flow
            }));
        });
    }

    // Fire Startup event - this triggers the app to create its Window widget,
    // which queues CxOsOp::CreateWindow in platform_ops.
    cx.borrow_mut().call_event_handler(&Event::Startup);
    cx.borrow_mut().redraw_all();

    // Process the first Paint event to handle platform_ops (creates MetalWindow)
    // and perform the initial render.
    {
        let mut cx_ref = cx.borrow_mut();
        let mut mcx = metal_cx.borrow_mut();
        let mut mw = metal_windows.borrow_mut();
        cx_ref.cocoa_event_callback(MacosEvent::Paint, &mut mcx, &mut mw);
    }

    // Reparent: take the NSView from MetalWindow's NSWindow and add it to the DAW's parent.
    let view: ObjcId = {
        let mw = metal_windows.borrow();
        if let Some(metal_window) = mw.first() {
            let view = metal_window.cocoa_window.view;
            unsafe {
                // Remove from MetalWindow's NSWindow
                let () = msg_send![view, removeFromSuperview];
                // Add as subview of DAW parent
                let () = msg_send![parent, addSubview: view];
                // Set frame to fill the parent
                let parent_bounds: makepad_platform::makepad_math::DVec4 =
                    msg_send![parent, bounds];
                let () = msg_send![view, setFrame: parent_bounds];
                // Enable autoresizing to follow parent size changes
                let mask: u64 = (1 << 1) | (1 << 4); // NSViewWidthSizable | NSViewHeightSizable
                let () = msg_send![view, setAutoresizingMask: mask];
            }
            view
        } else {
            panic!("Makepad did not create a MetalWindow during Startup");
        }
    };

    // Start Makepad's timer to drive rendering.
    // The timer fires through the DAW's existing NSRunLoop.
    cx.borrow_mut().ensure_timer0_started();

    EmbeddedMakepadHandle { view, _cx: cx }
}
