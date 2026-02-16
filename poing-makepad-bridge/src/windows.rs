//! Windows-specific Makepad embedding.
//!
//! Creates a Makepad Cx with D3D11 rendering, lets it create a D3d11Window
//! (HWND + swap chain), then reparents the HWND into the DAW's parent window.
//! A Win32 timer drives the render loop through the DAW's existing message loop.

use makepad_widgets::{
    Cx, CxOsApi, Event, OsType,
    cx_native::EventFlow,
    makepad_platform::os::windows::{
        d3d11::{D3d11Cx, D3d11Window},
        win32_app::{init_win32_app_global, with_win32_app},
        win32_event::Win32Event,
    },
};
use poing_core::SharedState;
use std::cell::RefCell;
use std::os::raw::c_void;
use std::rc::Rc;
use std::sync::Once;

// Win32 API FFI declarations for window reparenting
#[allow(non_snake_case)]
extern "system" {
    fn SetParent(hWndChild: isize, hWndNewParent: isize) -> isize;
    fn SetWindowLongPtrW(hWnd: isize, nIndex: i32, dwNewLong: isize) -> isize;
    fn MoveWindow(hWnd: isize, X: i32, Y: i32, nWidth: i32, nHeight: i32, bRepaint: i32) -> i32;
    fn GetClientRect(hWnd: isize, lpRect: *mut WinRect) -> i32;
    fn DestroyWindow(hWnd: isize) -> i32;
}

const GWL_STYLE: i32 = -16;
const WS_CHILD: isize = 0x4000_0000;
const WS_VISIBLE: isize = 0x1000_0000;

#[repr(C)]
#[allow(non_snake_case)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

static INIT: Once = Once::new();

/// Handle returned from create_embedded_makepad. Cleans up on Drop.
pub struct EmbeddedMakepadHandle {
    hwnd: isize,
    _cx: Rc<RefCell<Cx>>,
}

unsafe impl Send for EmbeddedMakepadHandle {}

impl Drop for EmbeddedMakepadHandle {
    fn drop(&mut self) {
        with_win32_app(|app| app.stop_timer(0));
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }
}

/// Create an embedded Makepad instance inside the given parent HWND.
pub fn create_embedded_makepad(
    parent_hwnd_ptr: *mut c_void,
    size: (u32, u32),
    shared_state: SharedState,
) -> EmbeddedMakepadHandle {
    let parent_hwnd = parent_hwnd_ptr as isize;
    let _ = size;

    INIT.call_once(|| {
        init_win32_app_global(Box::new(|_| EventFlow::Poll));
    });

    // Create Cx with poing-gui's event handler
    let event_handler = poing_gui::create_event_handler(shared_state);
    let cx = Rc::new(RefCell::new(Cx::new(event_handler)));

    cx.borrow_mut().self_ref = Some(cx.clone());
    cx.borrow_mut().os_type = OsType::Windows;

    // Create D3D11 context
    let d3d11_cx = Rc::new(RefCell::new(D3d11Cx::new()));
    cx.borrow_mut().os.d3d11_device = Some(d3d11_cx.borrow().device.clone());

    cx.borrow_mut().init_cx_os();

    let d3d11_windows: Rc<RefCell<Vec<D3d11Window>>> = Rc::new(RefCell::new(Vec::new()));

    // Install event callback
    {
        let cx_clone = cx.clone();
        let d3d11_cx_clone = d3d11_cx.clone();
        let d3d11_windows_clone = d3d11_windows.clone();

        with_win32_app(|app| {
            app.event_callback = Some(Box::new(move |event| {
                let mut cx_ref = cx_clone.borrow_mut();
                let mut dcx = d3d11_cx_clone.borrow_mut();
                let mut dw = d3d11_windows_clone.borrow_mut();
                let event_flow = cx_ref.win32_event_callback(event, &mut dcx, &mut dw);
                let executor = cx_ref.executor.take().unwrap();
                drop(cx_ref);
                executor.run_until_stalled();
                let mut cx_ref = cx_clone.borrow_mut();
                cx_ref.executor = Some(executor);
                event_flow
            }));
        });
    }

    // Fire Startup event
    cx.borrow_mut().call_event_handler(&Event::Startup);
    cx.borrow_mut().redraw_all();

    // Process first Paint to create the D3d11Window
    {
        let mut cx_ref = cx.borrow_mut();
        let mut dcx = d3d11_cx.borrow_mut();
        let mut dw = d3d11_windows.borrow_mut();
        cx_ref.win32_event_callback(Win32Event::Paint, &mut dcx, &mut dw);
    }

    // Reparent the HWND into the DAW's parent
    let child_hwnd: isize = {
        let dw = d3d11_windows.borrow();
        if let Some(d3d11_window) = dw.first() {
            let hwnd = d3d11_window.win32_window.hwnd;
            // HWND is `windows::Win32::Foundation::HWND` which wraps an isize
            hwnd.0 as isize
        } else {
            panic!("Makepad did not create a D3d11Window during Startup");
        }
    };

    unsafe {
        // Set child window style
        SetWindowLongPtrW(child_hwnd, GWL_STYLE, WS_CHILD | WS_VISIBLE);
        // Reparent into DAW's window
        SetParent(child_hwnd, parent_hwnd);
        // Resize to fill parent
        let mut rect = WinRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        GetClientRect(parent_hwnd, &mut rect);
        MoveWindow(
            child_hwnd,
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            1,
        );
    }

    // Start timer for rendering
    with_win32_app(|app| app.start_timer(0, 0.008, true));

    EmbeddedMakepadHandle {
        hwnd: child_hwnd,
        _cx: cx,
    }
}
