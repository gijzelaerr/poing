//! poing-plughost: one VST3 plugin in a process of its own, so a plugin that
//! crashes takes only this helper down and Poing reloads it. Poing talks to it
//! with vst3-host's isolation protocol (`HostCommand`/`HostResponse`, a JSON
//! line each way over stdin/stdout) and finds it next to `poing.exe`.
//!
//! Based on vst3-host's own `vst3-host-helper` (MIT, Helge Sverre), which
//! opens editors only on macOS. This one opens them on Windows too: the stdin
//! worker runs `Process` itself and hands every other command to the main
//! thread, which owns the editor window and pumps its messages.

#[cfg(windows)]
mod plughost;

fn main() {
    #[cfg(windows)]
    plughost::main();
    #[cfg(not(windows))]
    eprintln!("poing-plughost hosts VST3 plugins on Windows only for now");
}
