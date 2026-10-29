//! Hearing the song for the agent (`listen`, `compare`): a short range is
//! rendered off the audio thread with the same mixer as playback and export,
//! then measured: loudness (ITU BS.1770 / EBU R128), peaks and clipping,
//! crest factor, stereo image and energy per band, so "muddy", "buried" or
//! "harsh" become numbers Claude can check before and after a change.

use crate::audio::{mix_into, MixParams, Stem};
use crate::fx::FxRack;
use crate::state::EditorState;
use makepad_strict_json::{self as json, Value};
use std::sync::Arc;

/// The longest range one call renders.
pub const MAX_RANGE_S: f64 = 30.0;
/// Rendered before the range and thrown away, so reverbs and delays are warm.
const PREROLL_S: f64 = 2.0;

/// Bands of the coarse spectrum: name, from, to (Hz).
pub const BANDS: [(&str, f64, f64); 9] = [
    ("sub", 20.0, 60.0),
    ("bass", 60.0, 120.0),
    ("upper_bass", 120.0, 250.0),
    ("low_mid", 250.0, 500.0),
    ("mid", 500.0, 1000.0),
    ("upper_mid", 1000.0, 2000.0),
    ("high_mid", 2000.0, 4000.0),
    ("presence", 4000.0, 8000.0),
    ("air", 8000.0, 20000.0),
];

/// Render [start, end) seconds of the mix as `mix` and `rack` play it.
pub fn render(stems: &[Arc<Stem>], mix: &MixParams, rack: &mut FxRack, sr: u32, start: f64, end: f64) -> (Vec<f32>, Vec<f32>) {
    let s = (start.max(0.0) * sr as f64) as usize;
    let e = (end * sr as f64) as usize;
    let pre = s.saturating_sub((PREROLL_S * sr as f64) as usize);
    let n = e.saturating_sub(pre);
    let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
    mix_into(stems, mix, sr, pre, &mut l, &mut r, Some(rack));
    l.drain(..s - pre);
    r.drain(..s - pre);
    (l, r)
}

/// What a range sounds like. Levels are dB (None: silence).
#[derive(Clone, Debug, Default)]
pub struct Measure {
    /// Integrated loudness (LUFS), gated.
    pub lufs: Option<f64>,
    /// Loudest 3 s window (LUFS); None when the range is shorter.
    pub short_term_max: Option<f64>,
    pub sample_peak: Option<f64>,
    /// 4x oversampled peak (dBTP).
    pub true_peak: Option<f64>,
    /// Samples at or over full scale.
    pub clipped: usize,
    pub rms: Option<f64>,
    /// Sample peak over RMS (dB): low is dense or squashed, high is punchy or spiky.
    pub crest: Option<f64>,
    /// L/R correlation: 1 mono, 0 unrelated, -1 out of phase.
    pub correlation: Option<f64>,
    /// Side over mid level: 0 mono, about 1 as wide as unrelated channels.
    pub width: Option<f64>,
    /// Mean square per band (linear; see `BANDS`).
    pub bands: [f64; BANDS.len()],
}

fn db_power(x: f64) -> Option<f64> {
    (x > 1e-12 && x.is_finite()).then(|| 10.0 * x.log10())
}

fn db_amp(x: f64) -> Option<f64> {
    db_power(x * x)
}

pub fn measure(l: &[f32], r: &[f32], sr: u32) -> Measure {
    let n = l.len().min(r.len());
    let (l, r) = (&l[..n], &r[..n]);
    let (lufs, short_term_max) = loudness(l, r, sr);
    let (mut peak, mut clipped) = (0f32, 0usize);
    let (mut ll, mut rr, mut lr) = (0f64, 0f64, 0f64);
    for (&a, &b) in l.iter().zip(r) {
        peak = peak.max(a.abs()).max(b.abs());
        clipped += (a.abs() >= 1.0) as usize + (b.abs() >= 1.0) as usize;
        ll += a as f64 * a as f64;
        rr += b as f64 * b as f64;
        lr += a as f64 * b as f64;
    }
    let ms = if n > 0 { (ll + rr) / (2 * n) as f64 } else { 0.0 };
    let rms = db_power(ms);
    let sample_peak = db_amp(peak as f64);
    let correlation = (ll > 0.0 && rr > 0.0).then(|| (lr / (ll * rr).sqrt()).clamp(-1.0, 1.0));
    // mid = (l + r) / 2, side = (l - r) / 2
    let (mid, side) = ((ll + rr + 2.0 * lr) / 4.0, ((ll + rr - 2.0 * lr) / 4.0).max(0.0));
    let width = (ll + rr > 0.0).then(|| if mid > 1e-12 { (side / mid).sqrt().min(10.0) } else { 10.0 });
    Measure {
        lufs,
        short_term_max,
        sample_peak,
        true_peak: db_amp(true_peak(l).max(true_peak(r)).max(peak) as f64),
        clipped,
        rms,
        crest: sample_peak.zip(rms).map(|(p, r)| p - r),
        correlation,
        width,
        bands: bands(l, r, sr),
    }
}

// ---------------------------------------------------------------- loudness

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    fn run(&self, x: &[f32], out: &mut [f64]) {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for (i, &v) in x.iter().enumerate() {
            let v = v as f64;
            let y = self.b[0] * v + self.b[1] * x1 + self.b[2] * x2 - self.a[0] * y1 - self.a[1] * y2;
            (x2, x1, y2, y1) = (x1, v, y1, y);
            out[i] = y;
        }
    }

    fn run_in_place(&self, x: &mut [f64]) {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for v in x.iter_mut() {
            let y = self.b[0] * *v + self.b[1] * x1 + self.b[2] * x2 - self.a[0] * y1 - self.a[1] * y2;
            (x2, x1, y2, y1) = (x1, *v, y1, y);
            *v = y;
        }
    }
}

/// BS.1770's K-weighting at any rate (the filter design libebur128 uses).
fn k_weighting(sr: u32) -> [Biquad; 2] {
    let fs = sr as f64;
    // stage 1: high shelf, +4 dB above ~1.5 kHz (the head)
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    };
    // stage 2: high pass at ~38 Hz
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad { b: [1.0, -2.0, 1.0], a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0] };
    [shelf, hp]
}

/// Integrated loudness (400 ms blocks, 75 % overlap, gated at -70 LUFS and
/// -10 LU) and the loudest short-term (3 s) window, in LUFS.
fn loudness(l: &[f32], r: &[f32], sr: u32) -> (Option<f64>, Option<f64>) {
    let n = l.len();
    let step = (sr as usize / 10).max(1); // 100 ms
    let chunks = n / step;
    if chunks < 4 {
        return (None, None);
    }
    // K-weighted energy per 100 ms, both channels summed (weights 1)
    let mut energy = vec![0f64; chunks];
    let mut buf = vec![0f64; n];
    let [shelf, hp] = k_weighting(sr);
    for ch in [l, r] {
        shelf.run(ch, &mut buf);
        hp.run_in_place(&mut buf);
        for (c, e) in energy.iter_mut().enumerate() {
            *e += buf[c * step..(c + 1) * step].iter().map(|x| x * x).sum::<f64>() / step as f64;
        }
    }
    let lufs = |z: f64| -0.691 + 10.0 * z.log10();
    let window = |w: usize| -> Vec<f64> { (0..=chunks - w).map(|i| energy[i..i + w].iter().sum::<f64>() / w as f64).collect() };
    let blocks: Vec<f64> = window(4).into_iter().filter(|&z| z > 0.0 && lufs(z) > -70.0).collect();
    let integrated = if blocks.is_empty() {
        None
    } else {
        let gate = lufs(blocks.iter().sum::<f64>() / blocks.len() as f64) - 10.0;
        let kept: Vec<f64> = blocks.into_iter().filter(|&z| lufs(z) > gate).collect();
        (!kept.is_empty()).then(|| lufs(kept.iter().sum::<f64>() / kept.len() as f64))
    };
    let short = (chunks >= 30).then(|| window(30).into_iter().fold(0f64, f64::max)).filter(|&z| z > 0.0).map(lufs);
    (integrated, short)
}

// -------------------------------------------------------------- true peak

/// Peak of `x` upsampled 4x (windowed-sinc interpolation between samples).
fn true_peak(x: &[f32]) -> f32 {
    const TAPS: usize = 12; // per phase
    let mut kernel = [[0f32; TAPS]; 3];
    for (p, phase) in kernel.iter_mut().enumerate() {
        let frac = (p + 1) as f64 / 4.0;
        for (k, h) in phase.iter_mut().enumerate() {
            // tap k sits at sample n - TAPS/2 + 1 + k
            let t = k as f64 - (TAPS / 2 - 1) as f64 - frac;
            let sinc = if t.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t) };
            let w = 0.5 + 0.5 * (std::f64::consts::PI * t / (TAPS as f64 / 2.0 + 1.0)).cos();
            *h = (sinc * w) as f32;
        }
    }
    let mut peak = 0f32;
    for n in TAPS / 2 - 1..x.len().saturating_sub(TAPS / 2) {
        let win = &x[n + 1 - TAPS / 2..n + 1 + TAPS / 2];
        for phase in &kernel {
            let y: f32 = win.iter().zip(phase).map(|(a, b)| a * b).sum();
            peak = peak.max(y.abs());
        }
    }
    peak
}

// --------------------------------------------------------------- spectrum

const FFT_N: usize = 4096;

/// In-place radix-2 FFT of (re, im), length a power of two.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                (cr, ci) = (cr * wr - ci * wi, cr * wi + ci * wr);
            }
        }
        len <<= 1;
    }
}

/// Mean square per band (Hann-windowed spectra, half overlapping, both
/// channels averaged; the bands of a full-range signal add up to its mean square).
fn bands(l: &[f32], r: &[f32], sr: u32) -> [f64; BANDS.len()] {
    let mut out = [0f64; BANDS.len()];
    let n = l.len();
    if n < FFT_N {
        return out;
    }
    let window: Vec<f64> = (0..FFT_N).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / FFT_N as f64).cos()).collect();
    let wpow: f64 = window.iter().map(|w| w * w).sum();
    let band_of: Vec<Option<usize>> = (0..=FFT_N / 2)
        .map(|k| {
            let f = k as f64 * sr as f64 / FFT_N as f64;
            BANDS.iter().position(|&(_, lo, hi)| f >= lo && f < hi)
        })
        .collect();
    let (mut re, mut im) = (vec![0f64; FFT_N], vec![0f64; FFT_N]);
    let mut frames = 0usize;
    for start in (0..=n - FFT_N).step_by(FFT_N / 2) {
        for ch in [l, r] {
            for i in 0..FFT_N {
                re[i] = ch[start + i] as f64 * window[i];
                im[i] = 0.0;
            }
            fft(&mut re, &mut im);
            for (k, b) in band_of.iter().enumerate() {
                if let Some(b) = b {
                    let p = re[k] * re[k] + im[k] * im[k];
                    out[*b] += if k == 0 || k == FFT_N / 2 { p } else { 2.0 * p };
                }
            }
        }
        frames += 2;
    }
    for v in &mut out {
        *v /= frames as f64 * FFT_N as f64 * wpow;
    }
    out
}

// ------------------------------------------------------------- as JSON

fn round(x: f64, places: i32) -> Value {
    let m = 10f64.powi(places);
    if x.is_finite() {
        Value::F64((x * m).round() / m)
    } else {
        Value::Null
    }
}

fn opt(x: Option<f64>, places: i32) -> Value {
    x.map_or(Value::Null, |x| round(x, places))
}

impl Measure {
    /// Band levels in dB (mean square, like `rms`), by band name.
    pub fn band_db(&self) -> Vec<Option<f64>> {
        self.bands.iter().map(|&b| db_power(b)).collect()
    }

    pub fn silent(&self) -> bool {
        self.rms.is_none_or(|r| r < -90.0)
    }

    /// The measurement as (name, value) fields.
    pub fn fields(&self) -> Vec<(&'static str, Value)> {
        let bands = BANDS.iter().zip(self.band_db()).map(|(&(name, _, _), d)| (name, opt(d, 1))).collect();
        vec![
            ("lufs", opt(self.lufs, 1)),
            ("short_term_max_lufs", opt(self.short_term_max, 1)),
            ("true_peak_dbtp", opt(self.true_peak, 1)),
            ("sample_peak_db", opt(self.sample_peak, 1)),
            ("clipped_samples", Value::Int(self.clipped as i64)),
            ("rms_db", opt(self.rms, 1)),
            ("crest_db", opt(self.crest, 1)),
            ("correlation", opt(self.correlation, 2)),
            ("width", opt(self.width, 2)),
            ("band_db", json::obj(bands)),
        ]
    }

    /// One line: "-11.8 LUFS, true peak -0.4 dBTP, 0 clipped".
    pub fn line(&self) -> String {
        if self.silent() {
            return "silent".into();
        }
        let d = |x: Option<f64>| x.map_or("-inf".into(), |x| format!("{x:.1}"));
        format!("{} LUFS, true peak {} dBTP, {} clipped", d(self.lufs), d(self.true_peak), self.clipped)
    }
}

/// For each band, the lanes loudest in it (name, dB), loudest first, at most
/// three, leaving out lanes 30 dB or more under the loudest: who masks whom.
pub fn band_leaders(lanes: &[(String, Measure)]) -> Value {
    let pairs = BANDS
        .iter()
        .enumerate()
        .map(|(b, &(name, _, _))| {
            let mut v: Vec<(&str, f64)> = lanes.iter().filter_map(|(n, m)| Some((n.as_str(), db_power(m.bands[b])?))).collect();
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
            let top = v.first().map_or(0.0, |x| x.1);
            let arr = v.into_iter().take_while(|x| x.1 > top - 30.0).take(3).map(|(n, d)| Value::Arr(vec![json::s(n), round(d, 1)])).collect();
            (name, Value::Arr(arr))
        })
        .collect();
    json::obj(pairs)
}

// ------------------------------------------------------------ the tools

/// The range to render: [start, end] clamped to the song, at most MAX_RANGE_S.
pub fn range(st: &EditorState, start: Option<f64>, end: Option<f64>) -> Result<(f64, f64), String> {
    let dur = st.duration();
    let start = start.unwrap_or(0.0).clamp(0.0, dur);
    let end = end.unwrap_or(start + MAX_RANGE_S).min(dur).min(start + MAX_RANGE_S);
    if end - start < 0.5 {
        return Err(format!("give a range of at least 0.5 s inside the song (0-{dur:.1} s)"));
    }
    Ok((start, end))
}

/// Why lane `i`'s measurement is not quite what plays.
fn lane_notes(st: &EditorState, mix: &MixParams, i: usize) -> Vec<String> {
    let t = &st.project.tracks[i];
    let mut notes = Vec::new();
    if mix.gains.get(i).is_none_or(|&g| g == 0.0) {
        notes.push(if t.muted() { "muted in the mix".to_string() } else { "not heard in the mix (another lane is soloed)".to_string() });
    }
    notes
}

/// The plugins left out because they would not load a second time.
fn missing_note(missing: &[String]) -> Option<String> {
    (!missing.is_empty()).then(|| format!("these plugins would not load a second time and are left out: {}", missing.join(", ")))
}

/// A section take replaces every lane over its range: lanes measured there
/// are not what plays.
fn section_note(mix: &MixParams, start: f64, end: f64) -> Option<String> {
    let ranges: Vec<String> = mix.section_takes_between(start, end).iter().map(|(a, b)| format!("{a:.1}-{b:.1} s")).collect();
    (!ranges.is_empty()).then(|| {
        format!(
            "a section take replaces the lanes over {}: lanes are measured as if they played there, the mix is what plays; to hear the lanes, measure a range outside it",
            ranges.join(" and ")
        )
    })
}

fn notes_value(notes: Vec<String>) -> Value {
    Value::Arr(notes.into_iter().map(json::s).collect())
}

/// `listen`: the mix as it plays now over [start, end), or lane `lane` alone
/// (with its effects, before the master). Returns its line and data.
pub fn listen(st: &mut EditorState, start: f64, end: f64, lane: Option<usize>) -> Result<(String, Value), String> {
    let t0 = std::time::Instant::now();
    let mix = st.mix_snapshot();
    let (mut rack, missing) = st.measure_rack();
    let mut notes: Vec<String> = missing_note(&missing).into_iter().collect();
    let what = match lane {
        Some(i) => {
            if mix.song_take().is_some() {
                return Err("a whole-song take is on: lanes are not heard, listen to the mix instead".into());
            }
            notes.extend(lane_notes(st, &mix, i));
            notes.extend(section_note(&mix, start, end));
            rack.master.clear();
            format!("Lane {i} ({})", st.project.tracks[i].name)
        }
        None => "Mix".to_string(),
    };
    let mix = lane.map_or(mix.clone(), |i| mix.lane_alone(i));
    let (l, r) = render(&st.stems, &mix, &mut rack, st.sr(), start, end);
    let m = measure(&l, &r, st.sr());
    let line = format!("{what} {start:.1}-{end:.1} s: {}", m.line());
    let mut fields = vec![("what", json::s(what)), ("start", round(start, 2)), ("end", round(end, 2))];
    fields.extend(m.fields());
    fields.push(("notes", notes_value(notes)));
    fields.push(("compute_ms", Value::Int(t0.elapsed().as_millis() as i64)));
    Ok((line, json::obj(fields)))
}

/// `compare`: each of `lanes` alone (all lanes when empty) and the mix, over
/// [start, end), rendered side by side, with which lanes lead each band.
pub fn compare(st: &mut EditorState, start: f64, end: f64, lanes: &[usize]) -> Result<(String, Value), String> {
    let t0 = std::time::Instant::now();
    let mix = st.mix_snapshot();
    if mix.song_take().is_some() {
        return Err("a whole-song take is on: lanes are not heard, use listen on the mix instead".into());
    }
    let lanes: Vec<usize> = if lanes.is_empty() { (0..st.project.tracks.len()).collect() } else { lanes.to_vec() };
    let sr = st.sr();
    // one render per lane and one of the mix, each with a rack of its own: side by
    // side, or one after another when plugins play (each has one measuring instance)
    let plugins = st.has_plugin_sound();
    let mut missing = Vec::new();
    let mut jobs: Vec<(Option<usize>, MixParams, FxRack)> = Vec::new();
    for &i in &lanes {
        let mut rack = st.make_rack();
        rack.master.clear();
        jobs.push((Some(i), mix.lane_alone(i), rack));
    }
    jobs.push((None, mix.clone(), st.make_rack()));
    let measured: Vec<(Option<usize>, Measure)> = if plugins {
        jobs.into_iter()
            .map(|(lane, mix, _)| {
                let (mut rack, gone) = st.measure_rack();
                if lane.is_some() {
                    rack.master.clear();
                }
                missing = gone;
                let (l, r) = render(&st.stems, &mix, &mut rack, sr, start, end);
                (lane, measure(&l, &r, sr))
            })
            .collect()
    } else {
        let stems = &st.stems;
        std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .into_iter()
            .map(|(lane, mix, mut rack)| {
                s.spawn(move || {
                    let (l, r) = render(stems, &mix, &mut rack, sr, start, end);
                    (lane, measure(&l, &r, sr))
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
        })
    };
    let (mut per_lane, mut heard, mut mix_m) = (Vec::new(), Vec::new(), Measure::default());
    for (lane, m) in measured {
        let Some(i) = lane else {
            mix_m = m;
            continue;
        };
        let name = st.project.tracks[i].name.clone();
        let mut fields = vec![("index", Value::Int(i as i64)), ("name", json::s(name.clone()))];
        // the short form: loudness, peak, punch, image and bands
        fields.extend(m.fields().into_iter().filter(|(k, _)| matches!(*k, "lufs" | "true_peak_dbtp" | "crest_db" | "width" | "band_db")));
        let notes = lane_notes(st, &mix, i);
        if !notes.is_empty() {
            fields.push(("notes", notes_value(notes)));
        }
        per_lane.push(json::obj(fields));
        if !m.silent() {
            heard.push((name, m));
        }
    }
    let mut notes: Vec<String> = missing_note(&missing).into_iter().collect();
    notes.extend(section_note(&mix, start, end));
    let loudest = heard.iter().filter_map(|(n, m)| Some((n, m.lufs?))).max_by(|a, b| a.1.total_cmp(&b.1));
    let line = format!(
        "{} lanes {start:.1}-{end:.1} s: mix {}{}",
        lanes.len(),
        mix_m.line(),
        loudest.map_or(String::new(), |(n, l)| format!(", loudest lane {n} at {l:.1} LUFS"))
    );
    let data = json::obj(vec![
        ("start", round(start, 2)),
        ("end", round(end, 2)),
        ("mix", json::obj(mix_m.fields())),
        ("lanes", Value::Arr(per_lane)),
        ("band_leaders", band_leaders(&heard)),
        ("notes", notes_value(notes)),
        ("compute_ms", Value::Int(t0.elapsed().as_millis() as i64)),
    ]);
    Ok((line, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, amp: f64, sr: u32, secs: f64) -> Vec<f32> {
        (0..(sr as f64 * secs) as usize).map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / sr as f64).sin()) as f32).collect()
    }

    fn noise(n: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
            })
            .map(|x| x * 0.3)
            .collect()
    }

    #[test]
    fn a_stereo_sine_at_minus_23_dbfs_reads_minus_23_lufs() {
        // EBU Tech 3341 case 1, at both common rates
        for sr in [48000, 44100] {
            let x = sine(1000.0, 10f64.powf(-23.0 / 20.0), sr, 20.0);
            let m = measure(&x, &x, sr);
            let lufs = m.lufs.unwrap();
            assert!((lufs + 23.0).abs() < 0.1, "{sr}: {lufs}");
            assert!((m.short_term_max.unwrap() + 23.0).abs() < 0.1);
            assert!((m.sample_peak.unwrap() + 23.0).abs() < 0.05);
            // a sine's crest factor is 3 dB
            assert!((m.crest.unwrap() - 3.01).abs() < 0.05, "{:?}", m.crest);
            assert_eq!(m.clipped, 0);
        }
    }

    #[test]
    fn quiet_parts_do_not_drag_loudness_down() {
        // 10 s at -20 dBFS then 10 s at -60: the relative gate drops the quiet half
        let sr = 48000;
        let mut x = sine(1000.0, 10f64.powf(-20.0 / 20.0), sr, 10.0);
        x.extend(sine(1000.0, 10f64.powf(-60.0 / 20.0), sr, 10.0));
        let lufs = measure(&x, &x, sr).lufs.unwrap();
        assert!((lufs + 20.0).abs() < 0.2, "{lufs}");
        let silence = vec![0f32; sr as usize * 5];
        let m = measure(&silence, &silence, sr);
        assert!(m.lufs.is_none() && m.rms.is_none() && m.silent());
        assert!(json::obj(m.fields()).to_json().contains(r#""lufs":null"#));
    }

    #[test]
    fn a_sine_lands_in_its_band() {
        let sr = 48000;
        for (freq, band) in [(35.0, "sub"), (90.0, "bass"), (350.0, "low_mid"), (3000.0, "high_mid"), (12000.0, "air")] {
            let x = sine(freq, 0.5, sr, 3.0);
            let m = measure(&x, &x, sr);
            let db = m.band_db();
            let i = BANDS.iter().position(|b| b.0 == band).unwrap();
            // the band holds the sine's level (0.5 peak: -9 dB), the others far less
            assert!((db[i].unwrap() - m.rms.unwrap()).abs() < 0.5, "{freq} Hz: {db:?}");
            for (j, d) in db.iter().enumerate() {
                if j != i {
                    assert!(d.is_none_or(|d| d < db[i].unwrap() - 20.0), "{freq} Hz leaks into {}: {db:?}", BANDS[j].0);
                }
            }
        }
    }

    #[test]
    fn clipping_is_counted_and_true_peak_sees_between_samples() {
        let sr = 48000;
        let clipped: Vec<f32> = sine(100.0, 2.0, sr, 1.0).into_iter().map(|x| x.clamp(-1.0, 1.0)).collect();
        let m = measure(&clipped, &clipped, sr);
        assert!(m.clipped > 1000, "{}", m.clipped);
        assert!(m.crest.unwrap() < 2.0, "squashed: {:?}", m.crest);
        // a quarter-rate sine sampled 45 degrees off its peaks: samples at 0.707, true peak 1.0
        let x: Vec<f32> = (0..4800).map(|i| (std::f64::consts::PI / 2.0 * i as f64 + std::f64::consts::PI / 4.0).sin() as f32).collect();
        let m = measure(&x, &x, sr);
        assert!((m.sample_peak.unwrap() + 3.01).abs() < 0.05);
        assert!(m.true_peak.unwrap() > -0.5, "{:?}", m.true_peak);
        assert_eq!(m.clipped, 0);
    }

    #[test]
    fn mono_and_wide_are_told_apart() {
        let sr = 48000;
        let a = noise(sr as usize * 2, 1);
        let b = noise(sr as usize * 2, 2);
        let mono = measure(&a, &a, sr);
        assert!((mono.correlation.unwrap() - 1.0).abs() < 1e-6);
        assert!(mono.width.unwrap() < 1e-3);
        let wide = measure(&a, &b, sr);
        assert!(wide.correlation.unwrap().abs() < 0.05, "{:?}", wide.correlation);
        assert!((wide.width.unwrap() - 1.0).abs() < 0.1, "{:?}", wide.width);
        let inverted: Vec<f32> = a.iter().map(|x| -x).collect();
        let flipped = measure(&a, &inverted, sr);
        assert!(flipped.correlation.unwrap() < -0.99);
    }

    #[test]
    fn band_leaders_rank_lanes() {
        let sr = 48000;
        let bass = sine(90.0, 0.5, sr, 2.0);
        let keys = sine(90.0, 0.1, sr, 2.0);
        let lanes = vec![("Keys".to_string(), measure(&keys, &keys, sr)), ("Bass".to_string(), measure(&bass, &bass, sr))];
        let v = band_leaders(&lanes).to_json();
        assert!(v.contains(r#""bass":[["Bass",-9.0],["Keys",-23.0]]"#), "{v}");
    }
}
