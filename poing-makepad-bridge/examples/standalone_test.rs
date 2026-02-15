//! Standalone test: runs the Makepad GUI as a normal desktop application.
//!
//! This bypasses the DAW embedding and uses Makepad's standard event loop,
//! useful for testing the GUI without a DAW.
//!
//! Run with: cargo run --example standalone_test -p poing-makepad-bridge

use makepad_widgets::*;

fn main() {
    if Cx::pre_start() {
        return;
    }

    let event_handler = poing_gui::create_event_handler();
    let cx = std::rc::Rc::new(std::cell::RefCell::new(Cx::new(event_handler)));
    cx.borrow_mut().init_cx_os();
    Cx::event_loop(cx);
}
