//! Recording the set: what live mode plays (both decks through the mixer and
//! the beat FX) into a 32-bit float WAV file: a mix that goes over full scale
//! (two decks up, or one with its trim up) is kept as it is, not cut off. The audio callback puts its frames in
//! a ring ([`crate::decks::Decks::tape`]); a thread of its own takes them out
//! and writes the file, so the disk never holds the audio thread up. The
//! header is brought up to date as it goes: a recording that is cut off
//! (a crash, the power) is still a WAV file of what was written.

use crate::audio::Shared;
use crate::decks::CueRing;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// The longest set: the frames 4 GB of data hold (a WAV file counts its
/// bytes in 32 bits), 2 hours 53 minutes at 48 kHz.
const MAX_FRAMES: u64 = 4_000_000_000 / 8;

/// The header's length.
pub const HEADER: usize = 58;

/// A 32-bit float stereo WAV header for `frames` frames (with the `fact`
/// chunk a format other than PCM has).
pub fn wav_header(sr: u32, frames: u64) -> [u8; HEADER] {
    let frames = frames.min(MAX_FRAMES);
    let data_len = (frames * 8) as u32;
    let mut b = [0u8; HEADER];
    b[0..4].copy_from_slice(b"RIFF");
    b[4..8].copy_from_slice(&(HEADER as u32 - 8 + data_len).to_le_bytes());
    b[8..16].copy_from_slice(b"WAVEfmt ");
    b[16..20].copy_from_slice(&18u32.to_le_bytes());
    b[20..22].copy_from_slice(&3u16.to_le_bytes()); // IEEE float
    b[22..24].copy_from_slice(&2u16.to_le_bytes());
    b[24..28].copy_from_slice(&sr.to_le_bytes());
    b[28..32].copy_from_slice(&(sr * 8).to_le_bytes());
    b[32..34].copy_from_slice(&8u16.to_le_bytes());
    b[34..36].copy_from_slice(&32u16.to_le_bytes());
    // (no extension: bytes 36..38 stay 0)
    b[38..42].copy_from_slice(b"fact");
    b[42..46].copy_from_slice(&4u32.to_le_bytes());
    b[46..50].copy_from_slice(&(frames as u32).to_le_bytes());
    b[50..54].copy_from_slice(b"data");
    b[54..58].copy_from_slice(&data_len.to_le_bytes());
    b
}

/// What a finished recording is.
#[derive(Clone, Debug, PartialEq)]
pub struct Recorded {
    pub path: PathBuf,
    pub seconds: f64,
    /// Frames the writer was too late for (the disk stood still for over two thirds of a second): a gap in the file.
    pub lost: u64,
    /// The largest sample in it (1.0 is full scale).
    pub peak: f32,
}

/// A recording in progress.
pub struct SetRec {
    pub path: PathBuf,
    sr: u32,
    stop: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    lost: Arc<AtomicU64>,
    /// The largest sample so far (f32 bits; positive floats order like their bits).
    peak: Arc<std::sync::atomic::AtomicU32>,
    writer: Option<std::thread::JoinHandle<Result<(), String>>>,
}

/// A name for a set started now: `set-2026-11-11-153000.wav` (the date and time in UTC).
pub fn file_name() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) % 86400;
    format!("set-{}-{:02}{:02}{:02}.wav", crate::djexport::today(), secs / 3600, secs / 60 % 60, secs % 60)
}

impl SetRec {
    /// Start writing what `ring` gets from now on to `path` (its folder is made).
    pub fn start(ring: Arc<dyn Tape>, path: &Path, sr: u32) -> Result<SetRec, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        file.write_all(&wav_header(sr, 0)).map_err(|e| e.to_string())?;
        let (stop, frames, lost) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let (stop2, frames2, lost2) = (stop.clone(), frames.clone(), lost.clone());
        let peak = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let peak2 = peak.clone();
        let mut at = ring.ring().written();
        let writer = std::thread::Builder::new()
            .name("poing-set-recorder".into())
            .spawn(move || -> Result<(), String> {
                const CHUNK: usize = 4096;
                let (mut l, mut r) = (vec![0.0f32; CHUNK], vec![0.0f32; CHUNK]);
                let mut bytes = Vec::with_capacity(CHUNK * 8);
                let mut headed = 0u64;
                loop {
                    // read the flag first: what was written before it was set is still taken
                    let last = stop2.load(Ordering::Acquire);
                    let ring = ring.ring();
                    loop {
                        let (n, missed) = ring.take(&mut at, &mut l, &mut r);
                        if missed > 0 {
                            lost2.fetch_add(missed, Ordering::Relaxed);
                        }
                        if n == 0 {
                            break;
                        }
                        bytes.clear();
                        let mut most = 0.0f32;
                        for i in 0..n {
                            for x in [l[i], r[i]] {
                                let x = if x.is_finite() { x } else { 0.0 };
                                most = most.max(x.abs());
                                bytes.extend_from_slice(&x.to_le_bytes());
                            }
                        }
                        peak2.fetch_max(most.to_bits(), Ordering::Relaxed);
                        file.write_all(&bytes).map_err(|e| e.to_string())?;
                        frames2.fetch_add(n as u64, Ordering::Relaxed);
                    }
                    let total = frames2.load(Ordering::Relaxed);
                    // the header, about once a second and at the end
                    if last || total >= headed + sr as u64 {
                        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
                        file.write_all(&wav_header(sr, total)).map_err(|e| e.to_string())?;
                        file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
                        headed = total;
                    }
                    if last || total >= MAX_FRAMES {
                        return file.flush().map_err(|e| e.to_string());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(40));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(SetRec { path: path.to_path_buf(), sr, stop, frames, lost, peak, writer: Some(writer) })
    }

    /// Seconds written so far.
    pub fn seconds(&self) -> f64 {
        self.frames.load(Ordering::Relaxed) as f64 / self.sr as f64
    }

    /// The writer stopped by itself (the disk is full, or the file is as long as a WAV file gets).
    pub fn ended(&self) -> bool {
        self.writer.as_ref().is_none_or(|w| w.is_finished())
    }

    /// Stop: the rest is written and the file closed.
    pub fn finish(mut self) -> Result<Recorded, String> {
        self.stop.store(true, Ordering::Release);
        let done = self.writer.take().map(|w| w.join().unwrap_or_else(|_| Err("the recorder stopped unexpectedly".into()))).unwrap_or(Ok(()));
        done.map_err(|e| format!("{}: {e}", self.path.display()))?;
        Ok(Recorded { path: self.path.clone(), seconds: self.seconds(), lost: self.lost.load(Ordering::Relaxed), peak: f32::from_bits(self.peak.load(Ordering::Relaxed)) })
    }
}

impl Drop for SetRec {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }
}

/// Where a recorder finds its ring (the app's shared audio state, or a ring alone in the tests).
pub trait Tape: Send + Sync {
    fn ring(&self) -> &CueRing;
}

impl Tape for Shared {
    fn ring(&self) -> &CueRing {
        &self.decks.tape
    }
}

impl Tape for CueRing {
    fn ring(&self) -> &CueRing {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_is_written_as_it_is_played() {
        let dir = std::env::temp_dir().join(format!("poing-setrec-{}", std::process::id()));
        let path = dir.join("sets").join("test.wav");
        let ring = Arc::new(CueRing::default());
        // what was played before the recording started is not in it
        ring.push(&[0.9; 1000], &[0.9; 1000]);
        let rec = SetRec::start(ring.clone(), &path, 48000).unwrap();
        // 2.5 s in blocks of 480; the writer is waited for, as real time would
        let total = 120_000usize;
        let value = |i: usize| if i == 70_000 { 1.5 } else { ((i % 2000) as f32 / 2000.0) - 0.5 };
        let mut i = 0;
        while i < total {
            let l: Vec<f32> = (i..i + 480).map(value).collect();
            let r: Vec<f32> = (i..i + 480).map(|k| -value(k)).collect();
            ring.push(&l, &r);
            i += 480;
            let t0 = std::time::Instant::now();
            while i % 9600 == 0 && rec.seconds() * 48000.0 < i as f64 - 0.5 && t0.elapsed() < std::time::Duration::from_secs(20) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let done = rec.finish().unwrap();
        assert_eq!((done.lost, done.path.clone()), (0, path.clone()));
        assert!((done.seconds - 2.5).abs() < 1e-9, "{}", done.seconds);
        assert!((done.peak - 1.5).abs() < 1e-6, "a sample over full scale is kept, and told: {}", done.peak);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), HEADER + total * 8);
        assert_eq!(bytes[..HEADER], wav_header(48000, total as u64));
        let sample = |k: usize| f32::from_le_bytes(bytes[HEADER + k * 4..HEADER + k * 4 + 4].try_into().unwrap());
        for f in [0usize, 1, 479, 480, 65535, 65536, 65537, 70_000, total - 1] {
            assert!(sample(2 * f) == value(f) && sample(2 * f + 1) == -value(f), "frame {f}: {} {}", sample(2 * f), sample(2 * f + 1));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_writer_that_falls_behind_says_what_it_lost() {
        let ring = CueRing::default();
        let mut at = ring.written();
        // far more than the ring holds before the writer comes by
        for _ in 0..200 {
            ring.push(&[0.25; 1000], &[0.25; 1000]);
        }
        let (mut l, mut r) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
        let (mut got, mut lost) = (0u64, 0u64);
        loop {
            let (n, missed) = ring.take(&mut at, &mut l, &mut r);
            lost += missed;
            if n == 0 {
                break;
            }
            assert!(l[..n].iter().all(|x| *x == 0.25));
            got += n as u64;
        }
        assert_eq!(got + lost, 200_000, "every frame is written or counted as lost");
        assert!(lost > 100_000 && got > 30_000, "lost {lost}, got {got}");
    }
}
