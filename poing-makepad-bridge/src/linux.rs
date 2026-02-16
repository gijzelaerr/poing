//! Linux X11-specific Makepad embedding.
//!
//! Creates a Makepad Cx with OpenGL/EGL rendering, lets it create an OpenglWindow
//! (X11 window + EGL surface), then reparents the X11 window into the DAW's parent.
//! An X11 timer drives the render loop.

use makepad_widgets::{
    Cx, CxOsApi, Event, OsType,
    cx_native::EventFlow,
    makepad_platform::{
        LinuxWindowParams,
        GpuPerformance,
        os::linux::{
            egl_sys,
            opengl_cx::OpenglCx,
            x11::{
                linux_x11::X11Cx,
                opengl_x11::OpenglWindow,
                x11_sys,
                xlib_app::{get_xlib_app_global, init_xlib_app_global},
                xlib_event::XlibEvent,
            },
        },
    },
};
use poing_core::SharedState;
use std::cell::RefCell;
use std::os::raw::c_ulong;
use std::rc::Rc;
use std::sync::Once;

// X11 functions not in makepad's x11_sys bindings
extern "C" {
    fn XReparentWindow(
        display: *mut x11_sys::Display,
        w: x11_sys::Window,
        parent: x11_sys::Window,
        x: i32,
        y: i32,
    ) -> i32;
    fn XResizeWindow(
        display: *mut x11_sys::Display,
        w: x11_sys::Window,
        width: u32,
        height: u32,
    ) -> i32;
}

static INIT: Once = Once::new();

/// Handle returned from create_embedded_makepad. Cleans up on Drop.
pub struct EmbeddedMakepadHandle {
    x11_window: c_ulong,
    display: *mut x11_sys::Display,
    _cx: Rc<RefCell<Cx>>,
}

unsafe impl Send for EmbeddedMakepadHandle {}

impl Drop for EmbeddedMakepadHandle {
    fn drop(&mut self) {
        get_xlib_app_global().stop_timer(0);
        unsafe {
            x11_sys::XDestroyWindow(self.display, self.x11_window);
        }
    }
}

/// Create an embedded Makepad instance inside the given parent X11 window.
pub fn create_embedded_makepad(
    parent_xid: u32,
    size: (u32, u32),
    shared_state: SharedState,
) -> EmbeddedMakepadHandle {
    let parent_window: c_ulong = parent_xid as c_ulong;

    // Create Cx with poing-gui's event handler
    let event_handler = poing_gui::create_event_handler(shared_state);
    let cx = Rc::new(RefCell::new(Cx::new(event_handler)));

    cx.borrow_mut().self_ref = Some(cx.clone());
    cx.borrow_mut().os_type = OsType::LinuxWindow(LinuxWindowParams {
        custom_window_chrome: false,
    });
    cx.borrow_mut().gpu_info.performance = GpuPerformance::Tier1;

    let opengl_windows: Rc<RefCell<Vec<OpenglWindow>>> = Rc::new(RefCell::new(Vec::new()));

    // Create the X11Cx wrapper and install event callback
    let mut x11_cx = X11Cx {
        cx: cx.clone(),
        internal_drag_items: None,
    };

    let opengl_windows_for_cb = opengl_windows.clone();
    INIT.call_once(|| {
        init_xlib_app_global(Box::new(move |xlib_app, event| {
            let mut ow = opengl_windows_for_cb.borrow_mut();
            x11_cx.xlib_event_callback(xlib_app, event, &mut *ow)
        }));
    });

    let display = get_xlib_app_global().display;

    // Create OpenGL context via EGL
    cx.borrow_mut().os.opengl_cx = Some(unsafe {
        OpenglCx::from_egl_platform_display(egl_sys::EGL_PLATFORM_X11_EXT, display)
    });

    cx.borrow_mut().init_cx_os();

    // Fire Startup event
    cx.borrow_mut().call_event_handler(&Event::Startup);
    cx.borrow_mut().redraw_all();

    // Process first Paint to create the OpenglWindow
    get_xlib_app_global().do_callback(XlibEvent::Paint);

    // Get the child X11 window and reparent it
    let child_window: c_ulong = {
        let ow = opengl_windows.borrow();
        if let Some(opengl_window) = ow.first() {
            opengl_window
                .xlib_window
                .window
                .expect("Makepad XlibWindow has no X11 window")
        } else {
            panic!("Makepad did not create an OpenglWindow during Startup");
        }
    };

    unsafe {
        XReparentWindow(display, child_window, parent_window, 0, 0);
        XResizeWindow(display, child_window, size.0, size.1);
        x11_sys::XMapWindow(display, child_window);
    }

    // Start timer for rendering
    get_xlib_app_global().start_timer(0, 0.008, true);

    EmbeddedMakepadHandle {
        x11_window: child_window,
        display,
        _cx: cx,
    }
}
