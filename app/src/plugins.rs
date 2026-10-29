//! VST3 plugins as lane effects and MIDI lane instruments: the installed
//! library (found on disk, never loaded to list it), loading an instance on the
//! UI thread with its saved state, and the UI's handle to it (knobs, its own
//! editor window, state, its latency). Windows for now.
//!
//! A plugin runs in Poing, or ("Run in its own process") in poing-plughost,
//! where a crash takes only that process down: Poing reloads the plugin there
//! with its last state. The audio thread never talks to another process: an
//! isolated plugin's worker ([`Bridge`]) does, through lock-free rings, and its
//! audio comes back [`BRIDGE_DELAY`] frames late (compensated like any latency).

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

/// A host set up for Poing's sample rate and plugin block; `isolated`: the
/// plugin runs in poing-plughost (next to poing.exe).
#[cfg(windows)]
fn host_for(sample_rate: u32, isolated: bool) -> Result<vst3_host::Vst3Host, String> {
    let mut b = vst3_host::Vst3Host::builder().sample_rate(sample_rate as f64).block_size(crate::fx::PLUGIN_BLOCK);
    if isolated {
        let helper = helper_path().ok_or("poing-plughost.exe is missing next to poing.exe")?;
        // a crash is ours to handle (reload with the plugin's state), never a silent reset
        b = b.with_process_isolation(true).helper_path(helper).auto_recover_plugins(false);
    }
    b.build().map_err(|e| e.to_string())
}

/// poing-plughost next to this executable (tests run one folder deeper, in deps/).
#[cfg(windows)]
pub fn helper_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let found = [Some(dir), dir.parent()].into_iter().flatten().map(|d| d.join("poing-plughost.exe")).find(|p| p.exists());
    found
}

/// A loaded plugin, shared by the UI (editor window, knobs, state, and the
/// last reference: it is destroyed on the UI thread) and the rack. In Poing
/// the audio thread only ever `try_lock`s it (a missed block passes through);
/// in poing-plughost its bridge worker talks to it instead.
#[cfg(windows)]
pub struct Hosted {
    pub plugin: std::sync::Arc<std::sync::Mutex<vst3_host::Plugin>>,
    pub bridge: Option<Bridge>,
}

#[cfg(windows)]
pub type Shared = std::sync::Arc<Hosted>;

/// Blocks a plugin was busy for (the UI held it, or its process fell behind): passed through or silent.
#[cfg(windows)]
pub static MISSED_BLOCKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How late an isolated plugin's audio comes back, on top of its own latency:
/// room for its worker to answer one audio block (it takes whatever has
/// arrived, so device blocks up to this many frames at the song's rate).
/// Compensated like any latency.
pub const BRIDGE_DELAY: usize = 2 * crate::fx::PLUGIN_BLOCK;

/// Frames the bridge rings hold.
#[cfg(windows)]
const RING: usize = 1 << 15;
/// Note events that can wait for the worker.
#[cfg(windows)]
const EVENTS: usize = 1024;

/// What the audio thread and an isolated plugin's worker share: lock-free
/// rings of audio (f32 bits) and note events, by absolute frame. Output frame
/// f is the plugin's answer to input frame f.
#[cfg(windows)]
struct Ring {
    in_l: Box<[std::sync::atomic::AtomicU32]>,
    in_r: Box<[std::sync::atomic::AtomicU32]>,
    out_l: Box<[std::sync::atomic::AtomicU32]>,
    out_r: Box<[std::sync::atomic::AtomicU32]>,
    /// Input frames the audio side wrote.
    written: std::sync::atomic::AtomicU64,
    /// Output frames [from, done) are the plugin's (a skip moves `from` up).
    from: std::sync::atomic::AtomicU64,
    done: std::sync::atomic::AtomicU64,
    /// Note events: the frame, and pitch | velocity << 8 | on << 16.
    ev_frame: Box<[std::sync::atomic::AtomicU64]>,
    ev_data: Box<[std::sync::atomic::AtomicU32]>,
    ev_written: std::sync::atomic::AtomicU64,
    stop: std::sync::atomic::AtomicBool,
    /// The plugin's process died or hung: the UI reloads it.
    crashed: std::sync::atomic::AtomicBool,
    /// Blocks the worker sent, and the slowest round trip (µs).
    blocks: std::sync::atomic::AtomicU64,
    worst_us: std::sync::atomic::AtomicU64,
}

#[cfg(windows)]
fn atomics<T: Default>(n: usize) -> Box<[T]> {
    (0..n).map(|_| T::default()).collect()
}

/// The audio thread's side of an isolated plugin, and the worker that talks
/// to its process.
#[cfg(windows)]
pub struct Bridge {
    ring: std::sync::Arc<Ring>,
    worker: Option<std::thread::JoinHandle<()>>,
    instrument: bool,
}

#[cfg(windows)]
impl Bridge {
    fn start(plugin: std::sync::Arc<std::sync::Mutex<vst3_host::Plugin>>, instrument: bool, sample_rate: u32) -> Self {
        use std::sync::atomic::{AtomicBool, AtomicU64};
        let ring = std::sync::Arc::new(Ring {
            in_l: atomics(RING),
            in_r: atomics(RING),
            out_l: atomics(RING),
            out_r: atomics(RING),
            written: AtomicU64::new(0),
            from: AtomicU64::new(0),
            done: AtomicU64::new(0),
            ev_frame: atomics(EVENTS),
            ev_data: atomics(EVENTS),
            ev_written: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            crashed: AtomicBool::new(false),
            blocks: AtomicU64::new(0),
            worst_us: AtomicU64::new(0),
        });
        let r = ring.clone();
        let worker = std::thread::Builder::new()
            .name("poing-plugin-bridge".into())
            .spawn(move || bridge_worker(&r, &plugin, instrument, sample_rate))
            .ok();
        Bridge { ring, worker, instrument }
    }

    /// A note for the plugin, at the frame the next block starts (audio thread).
    /// A MIDI event for the worker: `kind` 0 note-off, 1 note-on (pitch, velocity),
    /// 2 controller (number, value).
    pub fn event(&self, a: u8, b: u8, kind: u8) {
        use std::sync::atomic::Ordering;
        let ring = &*self.ring;
        let k = ring.ev_written.load(Ordering::Relaxed);
        let slot = (k as usize) % EVENTS;
        ring.ev_frame[slot].store(ring.written.load(Ordering::Relaxed), Ordering::Relaxed);
        ring.ev_data[slot].store(a as u32 | (b as u32) << 8 | (kind as u32) << 16, Ordering::Relaxed);
        ring.ev_written.store(k + 1, Ordering::Release);
    }

    /// Hand the worker l/r (an effect's input) and take back the plugin's
    /// output from BRIDGE_DELAY frames ago: into l/r (an effect) or added to
    /// them (an instrument). `feed_only`: l/r stay as they are (bypassed, but
    /// the plugin keeps time). Never waits: frames the worker has not made
    /// yet are silent and counted as missed. Audio thread.
    pub fn run(&self, l: &mut [f32], r: &mut [f32], feed_only: bool) {
        use std::sync::atomic::Ordering;
        let ring = &*self.ring;
        let n = l.len().min(r.len()).min(RING - BRIDGE_DELAY - crate::fx::PLUGIN_BLOCK);
        let base = ring.written.load(Ordering::Relaxed);
        if !self.instrument {
            for i in 0..n {
                let s = (base as usize + i) % RING;
                ring.in_l[s].store(l[i].to_bits(), Ordering::Relaxed);
                ring.in_r[s].store(r[i].to_bits(), Ordering::Relaxed);
            }
        }
        ring.written.store(base + n as u64, Ordering::Release);
        if feed_only {
            return;
        }
        let done = ring.done.load(Ordering::Acquire);
        let from = ring.from.load(Ordering::Relaxed);
        let mut missed = false;
        for i in 0..n {
            let (a, b) = match (base + i as u64).checked_sub(BRIDGE_DELAY as u64) {
                Some(f) if f >= from && f < done => {
                    let s = f as usize % RING;
                    (f32::from_bits(ring.out_l[s].load(Ordering::Relaxed)), f32::from_bits(ring.out_r[s].load(Ordering::Relaxed)))
                }
                Some(_) => {
                    missed = true;
                    (0.0, 0.0)
                }
                None => (0.0, 0.0), // the first BRIDGE_DELAY frames: nothing yet
            };
            let a = if a.is_finite() { a.clamp(-4.0, 4.0) } else { 0.0 };
            let b = if b.is_finite() { b.clamp(-4.0, 4.0) } else { 0.0 };
            if self.instrument {
                l[i] += a;
                r[i] += b;
            } else {
                l[i] = a;
                r[i] = b;
            }
        }
        if missed {
            MISSED_BLOCKS.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn crashed(&self) -> bool {
        self.ring.crashed.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// (blocks sent, slowest round trip in µs) so far.
    pub fn stats(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering;
        (self.ring.blocks.load(Ordering::Relaxed), self.ring.worst_us.load(Ordering::Relaxed))
    }

    fn stop(&mut self) {
        self.ring.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(windows)]
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The worker: what the audio thread wrote goes to the plugin's process in
/// blocks of up to PLUGIN_BLOCK, its answers into the out ring. It polls, so
/// the audio thread never has to wake it (no syscalls there).
#[cfg(windows)]
fn bridge_worker(ring: &Ring, plugin: &std::sync::Mutex<vst3_host::Plugin>, instrument: bool, sample_rate: u32) {
    use std::sync::atomic::Ordering;
    use vst3_host::midi::{MidiChannel, MidiEvent};
    let block = crate::fx::PLUGIN_BLOCK;
    let mut buffers = vst3_host::audio::AudioBuffers::new(if instrument { 0 } else { 2 }, 2, block, sample_rate as f64);
    let (mut read, mut ev_read) = (0u64, 0u64);
    while !ring.stop.load(Ordering::Relaxed) {
        let written = ring.written.load(Ordering::Acquire);
        if written - read > (RING - BRIDGE_DELAY - 2 * block) as u64 {
            // far behind (the process hung): drop what waited, carry on from now
            read = written;
            ring.from.store(read, Ordering::Relaxed);
            ring.done.store(read, Ordering::Release);
        }
        if written == read {
            std::thread::sleep(std::time::Duration::from_micros(300));
            continue;
        }
        let k = ((written - read) as usize).min(block);
        crate::fx::fit(&mut buffers, k);
        if !instrument {
            for i in 0..k {
                let s = (read as usize + i) % RING;
                buffers.inputs[0][i] = f32::from_bits(ring.in_l[s].load(Ordering::Relaxed));
                buffers.inputs[1][i] = f32::from_bits(ring.in_r[s].load(Ordering::Relaxed));
            }
        }
        let t0 = std::time::Instant::now();
        let ok = {
            let mut p = plugin.lock().unwrap_or_else(|e| e.into_inner());
            let ev_written = ring.ev_written.load(Ordering::Acquire);
            ev_read = ev_read.max(ev_written.saturating_sub(EVENTS as u64));
            while ev_read < ev_written {
                let slot = (ev_read as usize) % EVENTS;
                let frame = ring.ev_frame[slot].load(Ordering::Relaxed);
                if frame >= read + k as u64 {
                    break;
                }
                let d = ring.ev_data[slot].load(Ordering::Relaxed);
                let (note, velocity) = ((d & 0x7f) as u8, ((d >> 8) & 0x7f) as u8);
                let event = match d >> 16 {
                    1 => MidiEvent::NoteOn { channel: MidiChannel::Ch1, note, velocity },
                    2 => MidiEvent::ControlChange { channel: MidiChannel::Ch1, controller: note, value: velocity },
                    _ => MidiEvent::NoteOff { channel: MidiChannel::Ch1, note, velocity: 0 },
                };
                let _ = p.send_midi_event_at(event, frame.saturating_sub(read) as i32);
                ev_read += 1;
            }
            match p.process_audio(&mut buffers) {
                Ok(()) => true,
                Err(vst3_host::Error::PluginCrashed | vst3_host::Error::PluginTimeout) => {
                    ring.crashed.store(true, Ordering::Relaxed);
                    false
                }
                Err(_) => false,
            }
        };
        ring.blocks.fetch_add(1, Ordering::Relaxed);
        ring.worst_us.fetch_max(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
        let outs = &buffers.outputs;
        for i in 0..k {
            let s = (read as usize + i) % RING;
            let (a, b) = if ok { (outs[0][i], outs[outs.len().min(2) - 1][i]) } else { (0.0, 0.0) };
            ring.out_l[s].store(a.to_bits(), Ordering::Relaxed);
            ring.out_r[s].store(b.to_bits(), Ordering::Relaxed);
        }
        read += k as u64;
        ring.done.store(read, Ordering::Release);
        if !ok {
            // a dead process answers at once: do not spin on it
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

/// What a pump of a plugin found.
#[cfg(windows)]
#[derive(Default)]
pub struct Pumped {
    /// The user closed its editor just now.
    pub closed: bool,
    /// Its latency changed: the rack is rebuilt to compensate.
    pub latency: bool,
    /// Its process is gone: the caller reloads it with its state.
    pub crashed: bool,
}

/// The UI's side of a loaded plugin: what it shows and changes.
#[cfg(windows)]
pub struct Handle {
    pub hosted: Shared,
    pub params: Vec<PluginParam>,
    pub name: String,
    /// In poing-plughost ("Run in its own process").
    pub isolated: bool,
    instrument: bool,
    /// The plugin's own latency as last read (frames).
    plugin_latency: u32,
    /// Its last known state, to reload it with after a crash.
    pub last_state: Option<Vec<u8>>,
    /// Times it came back after a crash.
    pub recoveries: u32,
    window: Option<vst3_host::PluginWindow>,
    /// An isolated plugin's editor (a window of its own process) is open.
    remote_editor: bool,
    /// Knob moves the plugin was too busy to take yet: (id, value), newest per id.
    pending: Vec<(u32, f64)>,
    last_check: std::time::Instant,
    /// When a knob last moved (its latency may follow).
    last_touch: std::time::Instant,
}

#[cfg(windows)]
impl Handle {
    /// Turn a knob (normalized 0..1): now if the plugin is free, else on the next pump.
    pub fn set_param(&mut self, id: u32, value: f64) {
        self.last_touch = std::time::Instant::now();
        self.pending.retain(|(i, _)| *i != id);
        self.pending.push((id, value.clamp(0.0, 1.0)));
        self.flush();
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        if let Ok(mut p) = self.hosted.plugin.try_lock() {
            for (id, v) in self.pending.drain(..) {
                let _ = p.set_parameter(id, v);
            }
        }
    }

    /// The articulations the plugin switches by key, as it reports them (VST3
    /// keyswitches): (name, key). Many sample libraries report none.
    pub fn keyswitches(&self) -> Vec<(String, u8)> {
        let Ok(p) = self.hosted.plugin.try_lock() else { return Vec::new() };
        p.keyswitches(0, vst3_host::midi::MidiChannel::Ch1)
            .map(|ks| ks.into_iter().map(|k| (k.title, k.remapped_key.unwrap_or(k.key_min).clamp(0, 127) as u8)).collect())
            .unwrap_or_default()
    }

    /// The plugin's whole state (presets, wavetables, every parameter), if it is free.
    pub fn save_state(&self) -> Option<Vec<u8>> {
        self.hosted.plugin.try_lock().ok().and_then(|p| p.save_state().ok())
    }

    /// [`Self::save_state`], kept to reload it with after a crash.
    pub fn snapshot(&mut self) -> Option<Vec<u8>> {
        let bytes = self.save_state()?;
        self.last_state = Some(bytes.clone());
        Some(bytes)
    }

    /// Frames its audio comes back late: the plugin's own latency, plus the
    /// bridge's when it runs in its own process.
    pub fn latency(&self) -> u32 {
        self.plugin_latency + if self.hosted.bridge.is_some() { BRIDGE_DELAY as u32 } else { 0 }
    }

    /// poing-plughost's process id (isolated plugins).
    pub fn helper_pid(&self) -> Option<u32> {
        self.hosted.plugin.try_lock().ok().and_then(|p| p.isolation_pid())
    }

    /// (blocks sent, slowest round trip in µs) of an isolated plugin.
    pub fn bridge_stats(&self) -> Option<(u64, u64)> {
        self.hosted.bridge.as_ref().map(|b| b.stats())
    }

    pub fn editor_open(&self) -> bool {
        self.remote_editor || self.window.as_ref().is_some_and(|w| w.is_open())
    }

    /// Open the plugin's own window (or bring it back).
    pub fn open_editor(&mut self) -> Result<(), String> {
        if self.isolated {
            // poing-plughost makes the window (or raises it) in its own process
            let mut p = self.hosted.plugin.lock().map_err(|e| e.to_string())?;
            if let Some(pid) = p.isolation_pid() {
                allow_foreground(pid);
            }
            // SAFETY: an isolated plugin never touches the parent: its helper owns the window.
            let none = unsafe { vst3_host::WindowHandle::from_raw(std::ptr::null_mut()) };
            p.open_editor(none).map_err(|e| format!("{} has no window here: {e}", self.name))?;
            self.remote_editor = true;
            return Ok(());
        }
        if self.editor_open() {
            return Ok(());
        }
        let mut w = vst3_host::PluginWindow::new(self.hosted.plugin.clone());
        w.open().map_err(|e| format!("{} has no window here: {e}", self.name))?;
        self.window = Some(w);
        Ok(())
    }

    pub fn close_editor(&mut self) {
        if std::mem::take(&mut self.remote_editor) {
            if let Ok(mut p) = self.hosted.plugin.lock() {
                let _ = p.close_editor();
            }
        }
        if let Some(mut w) = self.window.take() {
            w.close();
        }
    }

    /// Keep the editor window and queued knobs moving (UI thread, often), and
    /// every quarter second see whether an isolated plugin's editor was
    /// closed, whether its process still runs, and (while it may have changed:
    /// its editor open, a knob just moved) the plugin's latency. A plugin in
    /// Poing is left alone otherwise: each look takes it from the audio thread.
    pub fn pump(&mut self) -> Pumped {
        let mut out = Pumped::default();
        self.flush();
        if let Some(w) = &self.window {
            let _ = w.service_platform_events();
            if w.closed_by_user() {
                self.close_editor();
                out.closed = true;
            }
        }
        if self.hosted.bridge.as_ref().is_some_and(|b| b.crashed()) {
            out.crashed = true;
            return out;
        }
        if self.last_check.elapsed() < std::time::Duration::from_millis(250) {
            return out;
        }
        self.last_check = std::time::Instant::now();
        if self.remote_editor && self.helper_pid().is_some_and(|pid| helper_windows(pid).is_empty()) {
            // the user closed it in poing-plughost
            self.remote_editor = false;
            out.closed = true;
        }
        let touched = self.last_touch.elapsed() < std::time::Duration::from_secs(2);
        if !(self.isolated || self.editor_open() || touched) {
            return out;
        }
        let Ok(mut p) = self.hosted.plugin.try_lock() else { return out };
        match p.service_host_requests() {
            Err(vst3_host::Error::PluginCrashed | vst3_host::Error::PluginTimeout) => out.crashed = true,
            _ => {
                let latency = p.latency_samples();
                if latency != self.plugin_latency {
                    self.plugin_latency = latency;
                    out.latency = true;
                }
            }
        }
        out
    }

    /// Bring an isolated plugin back after its process died: a new process,
    /// the plugin loaded again and given `state` (its last known).
    pub fn recover(&mut self, state: Option<&[u8]>) -> Result<(), String> {
        use vst3_host::audio::{BusDirection, MediaType};
        let mut p = self.hosted.plugin.lock().map_err(|e| e.to_string())?;
        p.recover().map_err(|e| format!("{} did not come back: {e}", self.name))?;
        if let Some(bytes) = state {
            let _ = p.load_state(bytes);
        }
        if self.instrument {
            let _ = p.stop_processing();
            let _ = p.set_bus_active(MediaType::Event, BusDirection::Input, 0, true);
            let _ = p.start_processing();
            let _ = p.set_playing(true);
        }
        let _ = p.service_host_requests();
        self.plugin_latency = p.latency_samples();
        drop(p);
        self.remote_editor = false;
        self.recoveries += 1;
        if let Some(b) = &self.hosted.bridge {
            b.ring.crashed.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    /// Is the rack done with it (only this handle holds it)?
    pub fn released(&self) -> bool {
        std::sync::Arc::strong_count(&self.hosted) == 1
    }

    /// Destroy it here, on the UI thread (once [`Self::released`]): its
    /// bridge worker stops first.
    pub fn shut_down(mut self) {
        self.close_editor();
        if let Some(h) = std::sync::Arc::get_mut(&mut self.hosted) {
            h.bridge = None;
        }
        if let Ok(mut p) = self.hosted.plugin.lock() {
            let _ = p.stop_processing();
        }
    }
}

/// The visible top-level windows of process `pid` (an isolated plugin's editor).
#[cfg(windows)]
pub fn helper_windows(pid: u32) -> Vec<isize> {
    type EnumProc = unsafe extern "system" fn(*mut std::ffi::c_void, isize) -> i32;
    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(f: EnumProc, l: isize) -> i32;
        fn GetWindowThreadProcessId(hwnd: *mut std::ffi::c_void, pid: *mut u32) -> u32;
        fn IsWindowVisible(hwnd: *mut std::ffi::c_void) -> i32;
    }
    unsafe extern "system" fn each(hwnd: *mut std::ffi::c_void, l: isize) -> i32 {
        let (pid, out) = &mut *(l as *mut (u32, Vec<isize>));
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, &mut owner);
        if owner == *pid && IsWindowVisible(hwnd) != 0 {
            out.push(hwnd as isize);
        }
        1
    }
    let mut found: (u32, Vec<isize>) = (pid, Vec::new());
    // SAFETY: enumerates top-level windows into a local, for the call's duration.
    unsafe {
        EnumWindows(each, &mut found as *mut _ as isize);
    }
    found.1
}

/// Let poing-plughost `pid` bring its editor window to the front.
#[cfg(windows)]
fn allow_foreground(pid: u32) {
    #[link(name = "user32")]
    extern "system" {
        fn AllowSetForegroundWindow(pid: u32) -> i32;
    }
    // SAFETY: a plain Win32 call.
    unsafe {
        AllowSetForegroundWindow(pid);
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

/// Release every note an instrument may hold (each key off, then All Notes Off):
/// heard from its next processed block. UI thread, the plugin not playing elsewhere.
#[cfg(windows)]
pub fn all_notes_off(shared: &Shared) {
    use vst3_host::midi::{MidiChannel, MidiEvent};
    let Ok(mut p) = shared.plugin.lock() else { return };
    for note in 0..128u8 {
        let _ = p.send_midi_event_at(MidiEvent::NoteOff { channel: MidiChannel::Ch1, note, velocity: 0 }, 0);
    }
    let _ = p.send_midi_event_at(MidiEvent::ControlChange { channel: MidiChannel::Ch1, controller: 123, value: 0 }, 0);
}

/// How to load a plugin.
#[cfg(windows)]
#[derive(Clone, Copy, Default)]
pub struct LoadAs {
    /// To play a MIDI lane (else a lane effect).
    pub instrument: bool,
    /// In poing-plughost, not in Poing ("Run in its own process").
    pub isolated: bool,
    /// For playback: an isolated plugin gets its bridge worker. An offline
    /// render calls it directly (it may wait).
    pub realtime: bool,
}

/// Load a plugin on this (the UI) thread: as a lane effect, or to play a MIDI
/// lane. Its saved state wins over its saved knob values. Returns the UI's
/// handle and the rack's share of it.
#[cfg(windows)]
pub fn load(path: &Path, sample_rate: u32, saved: Option<&Fx>, state: Option<&[u8]>, how: LoadAs) -> Result<(Handle, Shared), String> {
    use vst3_host::audio::{BusDirection, MediaType};
    let instrument = how.instrument;
    let mut host = host_for(sample_rate, how.isolated)?;
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
    // an offline render (export, a measurement) runs faster than real time: in
    // offline mode a sample player loads what a note needs instead of dropping it
    if !how.realtime {
        let _ = plugin.set_process_mode(vst3_host::ProcessMode::Offline);
    }
    plugin.start_processing().map_err(|e| format!("{} did not start: {e}", info.name))?;
    if instrument {
        let _ = plugin.set_playing(true);
    }
    // a state that changed its latency (linear phase) has said so by now
    let _ = plugin.service_host_requests();
    let plugin_latency = plugin.latency_samples();
    let plugin = std::sync::Arc::new(std::sync::Mutex::new(plugin));
    let bridge = (how.isolated && how.realtime).then(|| Bridge::start(plugin.clone(), instrument, sample_rate));
    let shared: Shared = std::sync::Arc::new(Hosted { plugin, bridge });
    let handle = Handle {
        hosted: shared.clone(),
        params,
        name: info.name,
        isolated: how.isolated,
        instrument,
        plugin_latency,
        last_state: state.map(<[u8]>::to_vec),
        recoveries: 0,
        window: None,
        remote_editor: false,
        pending: Vec::new(),
        last_check: std::time::Instant::now(),
        last_touch: std::time::Instant::now(),
    };
    Ok((handle, shared))
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

/// By hand on the Windows PC (FabFilter Pro-Q 3 installed, poing-plughost built):
/// `cargo build --release -p poing --bin poing-plughost`, then
/// `cargo test --release -p poing --bin poing plugins::bench -- --ignored --nocapture --test-threads 1`
#[cfg(all(test, windows))]
mod bench {
    use super::*;
    use crate::audio::{mix_into, MixParams, Stem};
    use crate::fx::{FxControls, FxRack, FxUnit, PLUGIN_BLOCK};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const PRO_Q: &str = "C:/Program Files/Common Files/VST3/FabFilter Pro-Q 3.vst3";
    /// Pro-Q 3's "Processing Mode": 0 zero latency, 0.5 natural phase, 1 linear phase.
    const PROCESSING_MODE: u32 = 312;
    const SR: u32 = 48000;

    fn pro_q(isolated: bool, realtime: bool) -> (Handle, Shared) {
        let (mut h, s) = load(Path::new(PRO_Q), SR, None, None, LoadAs { instrument: false, isolated, realtime }).unwrap();
        h.set_param(PROCESSING_MODE, 1.0);
        // its latency changes with the mode: the pump reads it (as the app's does)
        let t0 = Instant::now();
        while h.plugin_latency == 0 && t0.elapsed() < Duration::from_secs(5) {
            h.last_check = Instant::now() - Duration::from_secs(1);
            let _ = h.pump();
        }
        (h, s)
    }

    fn click(at: usize, len: usize) -> Arc<Stem> {
        let mut v = vec![0.0f32; len];
        v[at] = 1.0;
        Arc::new(Stem::new(v.clone(), v))
    }

    /// Lane 0 through Pro-Q (linear phase), lane 1 dry, a click on each.
    fn rack_with(shared: Shared, latency: u32) -> FxRack {
        let fx = poing_model::Fx { id: "fx1".into(), name: "Pro-Q 3".into(), file: String::new(), params: None, bypass: None, prompt: None, plugin: Some(PRO_Q.into()), isolate: None };
        let mut rack = FxRack::new(vec![vec![FxUnit::plugin("fx1", FxControls::for_plugin(&fx), SR)], Vec::new()], Vec::new());
        assert!(rack.install("fx1", shared).is_ok());
        rack.compensate(&|id| if id == "fx1" { latency } else { 0 });
        rack
    }

    fn mix(gains: [f32; 2]) -> MixParams {
        MixParams { gains: gains.to_vec(), ..Default::default() }
    }

    fn loudest(l: &[f32]) -> (usize, f32) {
        l.iter().enumerate().fold((0, 0.0f32), |(i, m), (j, x)| if x.abs() > m { (j, x.abs()) } else { (i, m) })
    }

    #[test]
    #[ignore]
    fn a_click_through_linear_phase_pro_q_lines_up_with_a_dry_one() {
        let at = 9600;
        let stems = vec![click(at, 96000), click(at, 96000)];
        for isolated in [false, true] {
            let (h, shared) = pro_q(isolated, false);
            let lat = h.latency();
            assert!(lat > 0, "linear phase reports its latency");
            // without compensation the plugin lane is `lat` late
            let mut raw = rack_with(shared.clone(), 0);
            let (mut l, mut r) = (vec![0.0; 48000], vec![0.0; 48000]);
            mix_into(&stems, &mix([1.0, 0.0]), SR, 0, &mut l, &mut r, Some(&mut raw));
            let uncompensated = loudest(&l).0;
            drop(raw);
            let mut rack = rack_with(shared, lat);
            let (wet, _) = crate::audio::render_aligned(&stems, &mix([1.0, 0.0]), SR, 48000, &mut rack);
            let (dry, _) = crate::audio::render_aligned(&stems, &mix([0.0, 1.0]), SR, 48000, &mut rack);
            let (both, _) = crate::audio::render_aligned(&stems, &mix([1.0, 1.0]), SR, 48000, &mut rack);
            println!(
                "{}: latency {lat}, uncompensated click at {uncompensated}; compensated: plugin lane {:?}, dry lane {:?}, both {:?}",
                if isolated { "isolated" } else { "in-process" },
                loudest(&wet),
                loudest(&dry),
                loudest(&both)
            );
            assert_eq!(uncompensated, at + lat as usize);
            assert_eq!(loudest(&wet).0, at);
            assert_eq!(loudest(&dry).0, at);
            assert!(loudest(&both).1 > 1.8, "one click, twice as loud: {:?}", loudest(&both));
            drop(rack);
            h.shut_down();
        }
    }

    /// Live, as the audio callback runs it: 480-frame blocks in real time, in
    /// Poing and through the bridge (plugin latency + BRIDGE_DELAY), both
    /// clicks still one, and nothing missed.
    #[test]
    #[ignore]
    fn live_the_clicks_still_line_up() {
        for isolated in [false, true] {
            live(isolated);
        }
    }

    fn live(isolated: bool) {
        let at = 9600;
        let stems = vec![click(at, 96000), click(at, 96000)];
        let (h, shared) = pro_q(isolated, true);
        let lat = h.latency() as usize;
        let mut rack = rack_with(shared, lat as u32);
        let missed0 = MISSED_BLOCKS.load(std::sync::atomic::Ordering::Relaxed);
        let n = 48000;
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        let block = 480;
        let t0 = Instant::now();
        for (k, a) in (0..n).step_by(block).enumerate() {
            let b = (a + block).min(n);
            mix_into(&stems, &mix([1.0, 1.0]), SR, a, &mut l[a..b], &mut r[a..b], Some(&mut rack));
            // the device clock: the next block is due 10 ms after this one
            let due = Duration::from_micros((k as u64 + 1) * 10_000);
            if let Some(wait) = due.checked_sub(t0.elapsed()) {
                std::thread::sleep(wait);
            }
        }
        let missed = MISSED_BLOCKS.load(std::sync::atomic::Ordering::Relaxed) - missed0;
        let (blocks, worst) = h.bridge_stats().unwrap_or_default();
        println!("live, isolated {isolated}: latency {lat}, loudest {:?}, missed {missed}, {blocks} round trips, slowest {worst} µs", loudest(&l));
        assert_eq!(loudest(&l).0, at + lat);
        assert!(loudest(&l).1 > 1.8);
        assert_eq!(missed, 0);
        drop(rack);
        h.shut_down();
    }

    fn percentiles(mut us: Vec<u64>) -> (u64, u64, u64) {
        us.sort_unstable();
        (us[us.len() / 2], us[us.len() * 99 / 100], us[us.len() - 1])
    }

    /// The cost of one PLUGIN_BLOCK through Pro-Q 3, in Poing and through
    /// poing-plughost (a JSON round trip each block).
    #[test]
    #[ignore]
    fn cost_per_block_in_process_and_isolated() {
        let blocks = 2000;
        let mut seed = 1u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        let input: Vec<f32> = (0..PLUGIN_BLOCK).map(|_| noise()).collect();
        for mode in [0.0, 1.0] {
            for isolated in [false, true] {
                let (mut h, _) = load(Path::new(PRO_Q), SR, None, None, LoadAs { instrument: false, isolated, realtime: false }).unwrap();
                h.set_param(PROCESSING_MODE, mode);
                let mut b = vst3_host::audio::AudioBuffers::new(2, 2, PLUGIN_BLOCK, SR as f64);
                let mut us = Vec::with_capacity(blocks);
                {
                    let mut p = h.hosted.plugin.lock().unwrap();
                    let _ = p.service_host_requests();
                    for i in 0..blocks + 100 {
                        b.inputs[0].copy_from_slice(&input);
                        b.inputs[1].copy_from_slice(&input);
                        let t = Instant::now();
                        p.process_audio(&mut b).unwrap();
                        if i >= 100 {
                            us.push(t.elapsed().as_micros() as u64);
                        }
                    }
                }
                let (p50, p99, max) = percentiles(us);
                println!(
                    "{} {}: p50 {p50} µs, p99 {p99} µs, max {max} µs per {PLUGIN_BLOCK}-frame block (budget {} µs)",
                    if mode == 0.0 { "zero latency" } else { "linear phase" },
                    if isolated { "isolated" } else { "in-process" },
                    PLUGIN_BLOCK as u64 * 1_000_000 / SR as u64
                );
                h.shut_down();
            }
        }
    }

    /// poing-plughost `pid`'s visible windows: (title, answers messages).
    fn windows_of(pid: u32) -> Vec<(String, bool)> {
        #[link(name = "user32")]
        extern "system" {
            fn GetWindowTextW(hwnd: isize, s: *mut u16, n: i32) -> i32;
            fn SendMessageTimeoutW(hwnd: isize, m: u32, w: usize, l: isize, flags: u32, ms: u32, out: *mut usize) -> isize;
        }
        helper_windows(pid)
            .into_iter()
            .map(|hwnd| {
                let mut buf = [0u16; 256];
                let mut r = 0usize;
                // SAFETY: plain Win32 calls on a live window handle; WM_NULL answered within 500 ms: its messages are pumped
                unsafe {
                    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), 256).max(0) as usize;
                    (String::from_utf16_lossy(&buf[..n]), SendMessageTimeoutW(hwnd, 0, 0, 0, 2, 500, &mut r) != 0)
                }
            })
            .collect()
    }

    /// An isolated plugin's editor is a window of poing-plughost, and it is pumped.
    #[test]
    #[ignore]
    fn an_isolated_plugin_s_editor_opens_in_its_process() {
        let (mut h, _) = pro_q(true, true);
        let pid = h.helper_pid().unwrap();
        h.open_editor().unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        let open = windows_of(pid);
        h.open_editor().unwrap(); // again: raised, not a second window
        std::thread::sleep(Duration::from_millis(300));
        let again = windows_of(pid);
        // the user closes it (its close box): Poing notices
        #[link(name = "user32")]
        extern "system" {
            fn PostMessageW(hwnd: isize, m: u32, w: usize, l: isize) -> i32;
        }
        // SAFETY: WM_CLOSE to the helper's own window.
        unsafe { PostMessageW(helper_windows(pid)[0], 0x0010, 0, 0) };
        std::thread::sleep(Duration::from_millis(500));
        h.last_check = Instant::now() - Duration::from_secs(1);
        let noticed = h.pump().closed && !h.editor_open();
        h.open_editor().unwrap(); // and it opens again
        std::thread::sleep(Duration::from_millis(800));
        let reopened = windows_of(pid);
        h.close_editor();
        std::thread::sleep(Duration::from_millis(300));
        let closed = windows_of(pid);
        println!("helper {pid}: open {open:?}, again {again:?}, user close noticed {noticed}, reopened {reopened:?}, closed {closed:?}");
        assert!(open.len() == 1 && open[0].0.contains("Pro-Q") && open[0].1, "{open:?}");
        assert_eq!(again.len(), 1);
        assert!(noticed);
        assert_eq!(reopened.len(), 1);
        assert!(closed.is_empty());
        h.shut_down();
    }

    /// A plugin instrument in its own process plays its notes, as late as it says.
    #[test]
    #[ignore]
    fn an_isolated_instrument_plays_its_notes() {
        use crate::synth::{MidiLane, NoteEvent, PluginSound, Sound};
        const SERUM: &str = "C:/Program Files/Common Files/VST3/Serum2.vst3";
        for isolated in [false, true] {
            let (h, shared) = load(Path::new(SERUM), SR, None, None, LoadAs { instrument: true, isolated, realtime: true }).unwrap();
            let mut sound = PluginSound::new("midi1", SR);
            sound.hosted = Some(shared);
            let note = |frame, on| NoteEvent::note(frame, 60, 0.9, on);
            let mut lane = MidiLane::new(0, vec![note(4800, true), note(28800, false)], Sound::Plugin(sound));
            let n = 48000;
            let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
            let t0 = Instant::now();
            for (k, a) in (0..n).step_by(480).enumerate() {
                lane.render(a as u64, &mut l[a..a + 480], &mut r[a..a + 480]);
                if let Some(wait) = Duration::from_micros((k as u64 + 1) * 10_000).checked_sub(t0.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
            let first = l.iter().position(|x| x.abs() > 1e-4);
            println!("Serum 2, isolated {isolated}: latency {}, first sound at {first:?} (note at 4800)", h.latency());
            let first = first.expect("it sounds");
            assert!(first >= 4800 + h.latency() as usize && first < 4800 + h.latency() as usize + 480, "{first}");
            // a key played live while stopped (render_live): heard as late as the plugin is
            let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
            lane.live_key(64, 100, true);
            let t0 = Instant::now();
            for (k, a) in (0..n).step_by(480).enumerate() {
                lane.render_live(&mut l[a..a + 480], &mut r[a..a + 480]);
                if let Some(wait) = Duration::from_micros((k as u64 + 1) * 10_000).checked_sub(t0.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
            lane.live_release();
            let live = l.iter().position(|x| x.abs() > 1e-4);
            println!("  live key, isolated {isolated}: first sound at {live:?}");
            let live = live.expect("the live key sounds");
            assert!(live >= h.latency() as usize && live < h.latency() as usize + 480, "{live}");
            drop(lane);
            h.shut_down();
        }
    }

    /// Smoke test for sample players and synths at their default patch: each loads as an
    /// instrument, takes a held note and says whether it sounds. Kontakt and BBC SO open
    /// with nothing loaded, so silence there is expected until a sound is chosen.
    #[test]
    #[ignore]
    fn installed_instruments_load_and_play() {
        use crate::synth::{MidiLane, NoteEvent, PluginSound, Sound};
        const VST3: &str = "C:/Program Files/Common Files/VST3";
        let names = std::env::var("POING_SMOKE")
            .unwrap_or_else(|_| "Kontakt 8,BBC Symphony Orchestra (64 Bit),Serum2,Vital,Massive X,FM8".into());
        let mut report = Vec::new();
        for name in names.split(',') {
            let path = Path::new(VST3).join(format!("{name}.vst3"));
            let t0 = Instant::now();
            let loaded = load(&path, SR, None, None, LoadAs { instrument: true, isolated: true, realtime: false });
            let (h, shared) = match loaded {
                Ok(x) => x,
                Err(e) => {
                    report.push(format!("{name}: did not load: {e}"));
                    continue;
                }
            };
            let took = t0.elapsed();
            {
                let p = shared.plugin.lock().unwrap();
                let i = p.info();
                println!("{name}: in {} out {} midi-in {} category {:?}; buses {:?}", i.audio_inputs, i.audio_outputs, i.has_midi_input, i.category, p.bus_arrangements().map(|b| format!("{b:?}")));
            }
            let mut sound = PluginSound::new("midi1", SR);
            sound.hosted = Some(shared);
            let note = |frame, on| NoteEvent::note(frame, 60, 0.8, on);
            let mut lane = MidiLane::new(0, vec![note(4800, true), note(4 * 48000, false)], Sound::Plugin(sound));
            let n = 5 * 48000;
            let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
            for a in (0..n).step_by(480) {
                lane.render(a as u64, &mut l[a..a + 480], &mut r[a..a + 480]);
            }
            let peak = l.iter().chain(&r).fold(0.0f32, |m, x| m.max(x.abs()));
            let saved = h.save_state();
            if let (Some(dir), Some(bytes)) = (std::env::var_os("POING_SMOKE_STATE_OUT"), saved.as_ref()) {
                let _ = std::fs::write(Path::new(&dir).join(format!("{name}.state")), bytes);
            }
            let state = saved.map(|s| s.len()).unwrap_or(0);
            report.push(format!("{name}: loaded in {took:.1?}, {} params, state {state} B, peak {:.1} dBFS",
                h.params.len(), 20.0 * peak.max(1e-9).log10()));
            drop(lane);
            h.shut_down();
        }
        println!("{}", report.join("\n"));
    }

    /// The sound library on this PC, then sounds from it played with a dynamics
    /// curve: `POING_SMOKE_SOUNDS="violins 1,discover piano"` picks them.
    #[test]
    #[ignore]
    fn sounds_from_the_library_play() {
        use crate::synth::{events, MidiLane, PluginSound, Sound};
        use poing_model::{BeatGrid, Cc, Note};
        let plugins = library();
        let t0 = Instant::now();
        let lib = crate::sounds::library(&plugins);
        let mut by_plugin: std::collections::BTreeMap<&str, usize> = Default::default();
        for s in &lib {
            *by_plugin.entry(s.plugin.as_str()).or_default() += 1;
        }
        println!("{} sounds in {:.1?}: {by_plugin:?}", lib.len(), t0.elapsed());
        let picks = std::env::var("POING_SMOKE_SOUNDS").unwrap_or_else(|_| "discover violins 1,discover celli,massive x pad".into());
        let grid = BeatGrid::new(&[], &[], Some(120.0)); // a beat is 0.5 s
        for q in picks.split(',') {
            let Some(s) = crate::sounds::search(&lib, q).into_iter().next() else {
                println!("{q}: no sound");
                continue;
            };
            let path = plugins.iter().find(|(p, _)| *p == s.plugin).map(|(_, p)| p.clone()).unwrap();
            let template = crate::sounds::needs_template(s).then(|| {
                let (h, _) = load(&path, SR, None, None, LoadAs { instrument: true, isolated: false, realtime: false }).unwrap();
                let t = h.save_state().unwrap();
                h.shut_down();
                t
            });
            let state = crate::sounds::state_of(s, template.as_deref()).unwrap();
            let t0 = Instant::now();
            let (h, shared) = load(&path, SR, None, Some(&state), LoadAs { instrument: true, isolated: std::env::var_os("POING_SMOKE_INPROC").is_none(), realtime: false }).unwrap();
            let took = t0.elapsed();
            {
                let p = shared.plugin.lock().unwrap();
                let ks = p.keyswitches(0, vst3_host::midi::MidiChannel::Ch1).map(|k| k.iter().map(|k| format!("{} {}", k.title, k.key_min)).collect::<Vec<_>>());
                let names: Vec<String> = p.get_parameters().map(|ps| ps.iter().filter(|x| { let n = x.name.to_lowercase(); n.contains("artic") || n.contains("dynamic") || n.contains("express") }).map(|x| x.name.clone()).take(8).collect()).unwrap_or_default();
                println!("{q}: keyswitches {ks:?}; params {names:?}");
            }
            let mut sound = PluginSound::new("midi1", SR);
            sound.hosted = Some(shared);
            // an A held 6 beats, dynamics from pp up to ff
            let ks: Option<u8> = std::env::var("POING_SMOKE_KS").ok().and_then(|k| k.parse().ok());
            let mut notes = vec![Note { start: 1.0, len: 6.0, pitch: 69, vel: 0.7 }];
            if let Some(k) = ks {
                notes.insert(0, Note { start: 0.5, len: 0.25, pitch: k, vel: 0.7 });
            }
            let cc = [Cc { beat: 0.0, num: 1, value: 0.1 }, Cc { beat: 7.0, num: 1, value: 1.0 }];
            let mut lane = MidiLane::new(0, events(&notes, &cc, &grid, SR), Sound::Plugin(sound));
            let n = 5 * SR as usize;
            let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
            for a in (0..n).step_by(480) {
                lane.render(a as u64, &mut l[a..a + 480], &mut r[a..a + 480]);
                // a sample player streams from disk while it plays: give it real time
                std::thread::sleep(Duration::from_millis(2));
            }
            let rms = |a: usize, b: usize| (l[a..b].iter().map(|x| x * x).sum::<f32>() / (b - a) as f32).sqrt();
            let db = |x: f32| 20.0 * x.max(1e-9).log10();
            let s1 = SR as usize;
            println!("{q} -> {} ({}): loaded in {took:.1?}; soft 0.7-1.2 s {:.1} dB, loud 2.9-3.4 s {:.1} dB", s.name, s.plugin, db(rms(s1 * 7 / 10, s1 * 12 / 10)), db(rms(s1 * 29 / 10, s1 * 34 / 10)));
            drop(lane);
            h.shut_down();
        }
    }

    /// A plugin's own state, saved after a knob change, loads into a fresh instance:
    /// `POING_SMOKE_PLUGIN=<bundle>`.
    #[test]
    #[ignore]
    fn a_saved_state_round_trips() {
        let path = std::env::var("POING_SMOKE_PLUGIN").unwrap_or_else(|_| "C:/Program Files/Common Files/VST3/Serum2.vst3".into());
        let how = LoadAs { instrument: true, isolated: std::env::var_os("POING_SMOKE_INPROC").is_none(), realtime: false };
        let (mut h, _) = load(Path::new(&path), SR, None, None, how).unwrap();
        let knob = knob_params(&h.params).into_iter().next().unwrap();
        let before = h.hosted.plugin.lock().unwrap().get_parameter(knob.id).unwrap();
        let want = if before > 0.5 { 0.1 } else { 0.9 };
        h.set_param(knob.id, want);
        let run = |h: &Handle| {
            let mut b = vst3_host::audio::AudioBuffers::new(0, 2, PLUGIN_BLOCK, SR as f64);
            for _ in 0..40 {
                let _ = h.hosted.plugin.lock().unwrap().process_audio(&mut b);
            }
        };
        run(&h);
        std::thread::sleep(Duration::from_millis(300));
        let state = h.save_state().unwrap();
        h.shut_down();
        let (h2, _) = load(Path::new(&path), SR, None, Some(&state), how).unwrap();
        run(&h2);
        let after = h2.hosted.plugin.lock().unwrap().get_parameter(knob.id).unwrap();
        println!("{path}: knob {} {before:.3} -> set {want} -> fresh instance with the state reads {after:.3} (state {} B)", knob.name, state.len());
        h2.shut_down();
    }

    /// A plugin given a file as its state (a Kontakt .nki/.nkm, a Serum 2 or Vital preset):
    /// does it load and play? `POING_SMOKE_PLUGIN=<bundle> POING_SMOKE_NKI="<file>|<pitch>;..."`;
    /// Kontakt 8 by default. (Kontakt takes minutes over such a state and comes up empty.)
    #[test]
    #[ignore]
    fn a_state_file_plays() {
        use crate::synth::{MidiLane, NoteEvent, PluginSound, Sound};
        let plugin_path = std::env::var("POING_SMOKE_PLUGIN").unwrap_or_else(|_| "C:/Program Files/Common Files/VST3/Kontakt 8.vst3".into());
        let kontakt = plugin_path.as_str();
        let picks = std::env::var("POING_SMOKE_NKI").unwrap_or_default();
        for pick in picks.split(';').filter(|p| !p.is_empty()) {
            let (file, pitch) = pick.split_once('|').unwrap_or((pick, "60"));
            let bytes = std::fs::read(file).unwrap();
            let t0 = Instant::now();
            let (h, shared) = match load(Path::new(kontakt), SR, None, Some(&bytes), LoadAs { instrument: true, isolated: std::env::var_os("POING_SMOKE_INPROC").is_none(), realtime: false }) {
                Ok(x) => x,
                Err(e) => {
                    println!("{file}: did not load: {e}");
                    continue;
                }
            };
            let took = t0.elapsed();
            let mut sound = PluginSound::new("midi1", SR);
            sound.hosted = Some(shared);
            let p: u8 = pitch.parse().unwrap_or(60);
            let mut lane = MidiLane::new(0, vec![NoteEvent::note(4800, p, 0.8, true), NoteEvent::note(3 * 48000, p, 0.0, false)], Sound::Plugin(sound));
            let n = 4 * SR as usize;
            let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
            // a sample player loads its samples in the background: give it time, then play
            std::thread::sleep(Duration::from_secs(8));
            for a in (0..n).step_by(480) {
                lane.render(a as u64, &mut l[a..a + 480], &mut r[a..a + 480]);
            }
            let peak = l.iter().chain(&r).fold(0.0f32, |m, x| m.max(x.abs()));
            let back = h.save_state().map(|s| s.len()).unwrap_or(0);
            println!("{file}: loaded in {took:.1?}, peak {:.1} dBFS, state after {back} B (nki {} B)", 20.0 * peak.max(1e-9).log10(), bytes.len());
            drop(lane);
            h.shut_down();
        }
    }

    /// A linear-phase Pro-Q 3 state for an in-app test song: `POING_STATE_OUT=<file>`.
    #[test]
    #[ignore]
    fn write_linear_phase_state() {
        let Some(out) = std::env::var_os("POING_STATE_OUT") else { return };
        let (mut h, _) = pro_q(false, false);
        let mut b = vst3_host::audio::AudioBuffers::new(2, 2, PLUGIN_BLOCK, SR as f64);
        for _ in 0..8 {
            h.hosted.plugin.lock().unwrap().process_audio(&mut b).unwrap();
        }
        std::fs::write(&out, h.snapshot().unwrap()).unwrap();
        println!("latency {} written to {out:?}", h.latency());
        h.shut_down();
    }

    /// Kill poing-plughost under a playing plugin: the audio side goes quiet,
    /// the UI sees the crash, and the plugin comes back with its state.
    #[test]
    #[ignore]
    fn a_killed_helper_comes_back_with_its_state() {
        let (mut h, shared) = pro_q(true, true);
        let pid = h.helper_pid().unwrap();
        let mut rack = rack_with(shared, h.latency());
        let stems = vec![Arc::new(Stem::new(vec![0.25; 480000], vec![0.25; 480000])), click(0, 480000)];
        let play = |rack: &mut FxRack, from: usize, blocks: usize| -> f32 {
            let mut loud = 0.0f32;
            for k in 0..blocks {
                let (mut l, mut r) = (vec![0.0; 480], vec![0.0; 480]);
                mix_into(&stems, &mix([1.0, 0.0]), SR, from + k * 480, &mut l, &mut r, Some(rack));
                loud = loud.max(loudest(&l).1);
                std::thread::sleep(Duration::from_millis(10));
            }
            loud
        };
        assert!(play(&mut rack, 0, 50) > 0.2, "it plays");
        // (a knob reaches the plugin's saved state once it has processed)
        let state = h.snapshot().unwrap();
        let before = h.hosted.plugin.lock().unwrap().get_parameter(PROCESSING_MODE).unwrap();
        let killed = std::process::Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).output().unwrap();
        assert!(killed.status.success(), "{}", String::from_utf8_lossy(&killed.stderr));
        let t0 = Instant::now();
        let _ = play(&mut rack, 50 * 480, 30);
        let mut seen = false;
        while t0.elapsed() < Duration::from_secs(5) {
            h.last_check = Instant::now() - Duration::from_secs(1);
            if h.pump().crashed {
                seen = true;
                break;
            }
        }
        assert!(seen, "the crash is seen");
        let quiet = play(&mut rack, 80 * 480, 20);
        let t1 = Instant::now();
        h.recover(Some(&state)).unwrap();
        let back_in = t1.elapsed();
        let new_pid = h.helper_pid().unwrap();
        let after = h.hosted.plugin.lock().unwrap().get_parameter(PROCESSING_MODE).unwrap();
        let loud = play(&mut rack, 100 * 480, 60);
        println!("killed {pid}: seen after {:?}, quiet {quiet}, back in {back_in:?} as {new_pid}, mode {before} -> {after}, playing {loud}", t0.elapsed());
        assert_ne!(pid, new_pid);
        assert!(quiet < 1e-6, "a dead plugin is silent, not garbage");
        assert_eq!(before, after, "its state came back");
        assert!(loud > 0.2, "it plays again");
        drop(rack);
        h.shut_down();
    }
}
