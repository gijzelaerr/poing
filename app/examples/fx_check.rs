//! Check an effect shader against a real stem: compile, audition (Makepad's
//! `check`), then time it over the whole file at 48 kHz.
//!
//!   cargo run --release -p poing --example fx_check -- shader.splash stem.wav

use makepad_script_audio_aot as aot;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [shader, stem] = [&args[0], &args[1]];
    let code = std::fs::read_to_string(shader).expect("shader file");
    let t = Instant::now();
    let compiled = match aot::compile(&code) {
        Ok(s) => s,
        Err(errors) => {
            for e in errors {
                eprintln!("error: {e:?}");
            }
            std::process::exit(1);
        }
    };
    println!("compiled in {:.1} ms: {:?}, backend {:?}", t.elapsed().as_secs_f64() * 1e3, *compiled, compiled.backend());
    match aot::check(&code) {
        Ok(r) => println!(
            "check: peak {:.2} rms {:.3} dc {:.4} ops/frame {:.0} warnings {:?}",
            r.peak, r.rms, r.dc, r.ops_per_frame, r.warnings
        ),
        Err(e) => println!("check failed: {e:?}"),
    }
    // Harmonics: a 55 Hz sine in, the 2nd..5th harmonic levels out (dB
    // under the fundamental), at a quiet and a loud input level.
    for amp in [0.1f32, 0.5] {
        let sr = 48000.0;
        let n = 96000;
        let sine: Vec<f32> = (0..n).map(|i| amp * (std::f32::consts::TAU * 55.0 * i as f32 / sr).sin()).collect();
        let mut inst = aot::Instance::new(compiled.clone(), sr);
        let (mut ol, mut or) = (vec![0.0f32; n], vec![0.0f32; n]);
        for s in (0..n).step_by(512) {
            let e = (s + 512).min(n);
            inst.process(&sine[s..e], &sine[s..e], &mut ol[s..e], &mut or[s..e]);
        }
        let tail = &ol[n / 2..];
        let goertzel = |f: f32| {
            let w = std::f32::consts::TAU * f / sr;
            let (mut s1, mut s2) = (0.0f64, 0.0f64);
            for &x in tail {
                let s0 = x as f64 + 2.0 * (w as f64).cos() * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            (s1 * s1 + s2 * s2 - 2.0 * (w as f64).cos() * s1 * s2).sqrt()
        };
        let f1 = goertzel(55.0);
        let h: Vec<String> = (2..=5).map(|k| format!("{:.0}", 20.0 * (goertzel(55.0 * k as f32) / f1).log10())).collect();
        println!("sine at {:.0} dBFS: harmonics 2..5 at [{}] dB under the fundamental", 20.0 * amp.log10(), h.join(", "));
    }
    let audio = makepad_audio_decode::decode_any(&std::fs::read(stem).expect("stem file")).expect("decode");
    let ch = audio.channels.max(1) as usize;
    let frames = audio.frames();
    let l: Vec<f32> = (0..frames).map(|f| audio.pcm_interleaved_f32[f * ch]).collect();
    let r: Vec<f32> = (0..frames).map(|f| audio.pcm_interleaved_f32[f * ch + (ch > 1) as usize]).collect();
    let mut inst = aot::Instance::new(compiled, 48000.0);
    let (mut ol, mut or) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let t = Instant::now();
    for start in (0..frames).step_by(512) {
        let end = (start + 512).min(frames);
        inst.process(&l[start..end], &r[start..end], &mut ol[start..end], &mut or[start..end]);
    }
    let secs = t.elapsed().as_secs_f64();
    let audio_secs = frames as f64 / audio.rate as f64;
    println!("processed {audio_secs:.1} s of audio in {:.3} s: {:.0}x realtime, {:.2}% of a core", secs, audio_secs / secs, 100.0 * secs / audio_secs);
}
