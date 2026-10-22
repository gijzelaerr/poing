//! Check VST3 hosting on this machine: list installed plugins (without loading
//! them), then load one, print its parameters and run a sine through it.
//!
//!   cargo run --release -p poing --example vst3_check -- [plugin name]

#[cfg(windows)]
fn main() {
    use std::time::Instant;
    use vst3_host::{audio::AudioBuffers, Vst3Host};

    let want = std::env::args().nth(1).unwrap_or_else(|| "Raum".into()).to_lowercase();
    let mut host = Vst3Host::builder().scan_default_paths().sample_rate(48_000.0).block_size(1024).build().expect("host");
    // Only the file system: loading every plugin to read its metadata can hang
    // or crash on licence checks.
    let t = Instant::now();
    let found = host.scan_plugin_paths();
    println!("{} plugin bundles found in {:.0} ms (not loaded)", found.len(), t.elapsed().as_secs_f64() * 1e3);
    let name = |p: &std::path::Path| p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    for p in &found {
        println!("  {}", name(p));
    }
    let Some(path) = found.iter().find(|p| name(p).to_lowercase().contains(&want)) else {
        println!("no plugin matching {want:?}");
        return;
    };
    let t = Instant::now();
    let mut plugin = host.load_plugin(path).expect("load");
    let info = plugin.info().clone();
    println!("\nloaded {} ({}, {}) in {:.0} ms", info.name, info.vendor, info.category, t.elapsed().as_secs_f64() * 1e3);
    let params = plugin.get_parameters().expect("params");
    println!("{} parameters:", params.len());
    for p in params.iter().take(12) {
        let shown = plugin.format_parameter(p.id, p.value).unwrap_or_default();
        println!("  {:>10}  {:<28} {:.3}  ({shown})", p.id, p.name, p.value);
    }
    if std::env::args().any(|a| a == "--instrument") {
        // An instrument, as Poing will run it: the Plugin itself moved to another
        // thread, a note sent at a sample offset, processed, released.
        fn assert_send<T: Send>(_: &T) {}
        assert_send(&plugin);
        println!("buses: {} in, {} out; category {}", info.audio_inputs, info.audio_outputs, info.category);
        use vst3_host::audio::{BusDirection, MediaType};
        println!("event input active: {:?}", plugin.set_bus_active(MediaType::Event, BusDirection::Input, 0, true));
        plugin.start_processing().expect("start");
        let worker = std::thread::spawn(move || {
            use vst3_host::midi::{MidiChannel, MidiEvent};
            let ins = info.audio_inputs.max(0) as usize * 2;
            let mut buffers = AudioBuffers::new(ins.min(2), 2, 1024, 48_000.0);
            let mut level = |plugin: &mut vst3_host::Plugin, blocks: usize| {
                let mut peak = 0.0f32;
                for _ in 0..blocks {
                    plugin.process_audio(&mut buffers).expect("process");
                    peak = buffers.outputs.iter().flatten().fold(peak, |p, x| p.max(x.abs()));
                }
                peak
            };
            let mut plugin = plugin;
            std::thread::sleep(std::time::Duration::from_secs(2));
            let _ = plugin.set_playing(true);
            let before = level(&mut plugin, 100);
            plugin.send_midi_event_at(MidiEvent::NoteOn { channel: MidiChannel::Ch1, note: 57, velocity: 100 }, 300).expect("note on");
            let during = level(&mut plugin, 150);
            plugin.send_midi_event_at(MidiEvent::NoteOff { channel: MidiChannel::Ch1, note: 57, velocity: 0 }, 0).expect("note off");
            let release = level(&mut plugin, 10);
            let after = level(&mut plugin, 94);
            println!("peak before {before:.4}, holding A3 {during:.4}, release {release:.4}, 2 s later {after:.4}");
            plugin
        });
        let mut plugin = worker.join().expect("worker panicked");
        plugin.stop_processing().ok();
        return;
    }
    if std::env::args().any(|a| a == "--thread") {
        // As Poing runs it: a runner made here, processed on another thread.
        let (mut runner, mut control) = vst3_host::realtime::RealtimePluginRunner::new(plugin, 512);
        runner.start().expect("start");
        let worker = std::thread::spawn(move || {
            let mut buffers = AudioBuffers::new(2, 2, 1024, 48_000.0);
            let mut results = Vec::new();
            for b in 0..20 {
                for i in 0..1024 {
                    let x = 0.3 * (std::f32::consts::TAU * 220.0 * (b * 1024 + i) as f32 / 48_000.0).sin();
                    buffers.inputs[0][i] = x;
                    buffers.inputs[1][i] = x;
                }
                buffers.block_size = if b % 2 == 0 { 1024 } else { 300 };
                let t = Instant::now();
                let r = runner.process(&mut buffers);
                results.push(format!("{:?} {:.2}ms", r.is_ok(), t.elapsed().as_secs_f64() * 1e3));
                if let Err(e) = r {
                    results.push(format!("error: {e}"));
                    break;
                }
            }
            (runner, results)
        });
        let (runner, results) = worker.join().expect("worker panicked");
        println!("processed on another thread: {}", results.join(", "));
        drop(runner);
        println!("teardown serviced: {}", control.service_teardown());
        return;
    }
    plugin.start_processing().expect("start");
    let mut buffers = AudioBuffers::new(2, 2, 1024, 48_000.0);
    let (mut peak_in, mut peak_out, mut sum_diff) = (0.0f32, 0.0f32, 0.0f64);
    let t = Instant::now();
    let blocks = 47 * 4; // ~4 s
    for b in 0..blocks {
        for i in 0..1024 {
            let n = (b * 1024 + i) as f32;
            let x = 0.3 * (std::f32::consts::TAU * 220.0 * n / 48_000.0).sin() * if (b / 12) % 2 == 0 { 1.0 } else { 0.0 };
            buffers.inputs[0][i] = x;
            buffers.inputs[1][i] = x;
            peak_in = peak_in.max(x.abs());
        }
        plugin.process_audio(&mut buffers).expect("process");
        for i in 0..1024 {
            let y = buffers.outputs[0][i];
            peak_out = peak_out.max(y.abs());
            sum_diff += ((y - buffers.inputs[0][i]) as f64).abs();
        }
    }
    let secs = t.elapsed().as_secs_f64();
    println!(
        "processed {:.1} s in {:.3} s ({:.0}x realtime): peak in {peak_in:.3}, out {peak_out:.3}, mean |out - in| {:.4}",
        blocks as f64 * 1024.0 / 48_000.0,
        secs,
        blocks as f64 * 1024.0 / 48_000.0 / secs,
        sum_diff / (blocks * 1024) as f64
    );
    plugin.stop_processing().expect("stop");
}

#[cfg(not(windows))]
fn main() {
    println!("VST3 hosting is Windows-only for now");
}
