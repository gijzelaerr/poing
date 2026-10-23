//! Export beside the mix: stems (each lane alone, as it sounds in the mix),
//! the MIDI lanes as .mid files on the song's tempo map, and a DAWproject
//! (.dawproject, the open Bitwig/PreSonus format) to open the song in another
//! DAW. The DJ export (rekordbox) is in `djexport`.
//!
//! Everything timed goes through one [`TempoMap`]: the song's beat grid as a
//! DAW sees it, so MIDI notes, audio and section markers land where Poing
//! plays them.

use crate::audio::{self, take_lane, JobUpdate, MixParams, Stem, Stems, TakeKind};
use crate::fx::FxRack;
use crate::state::EditorState;
use poing_model::{db_to_lin, BeatGrid, Note, Project};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;

pub enum ExportJob {
    /// Each lane alone into `dir/<lane>.wav`.
    Stems { stems: Stems, rack: Box<FxRack>, sr: u32, frames: usize, parts: Vec<Part>, dir: PathBuf },
    /// A .dawproject at `path`: the parts rendered into it, the rest of the
    /// project already written as XML.
    DawProject { stems: Stems, rack: Box<FxRack>, sr: u32, frames: usize, parts: Vec<Part>, project_xml: String, metadata_xml: String, path: PathBuf },
    Dj(crate::djexport::DjJob),
}

/// One audio file of an export: what the mixer plays for it, and its name.
pub struct Part {
    pub name: String,
    pub mix: MixParams,
    /// Frames to render (0: the song's lanes).
    pub frames: usize,
}

pub fn run(job: ExportJob, tx: &Sender<JobUpdate>) {
    let done = match job {
        ExportJob::Stems { stems, mut rack, sr, frames, parts, dir } => export_stems(&stems, &mut rack, sr, frames, &parts, &dir, tx),
        ExportJob::DawProject { stems, mut rack, sr, frames, parts, project_xml, metadata_xml, path } => {
            export_dawproject(&stems, &mut rack, sr, frames, &parts, &project_xml, &metadata_xml, &path, tx)
        }
        ExportJob::Dj(job) => crate::djexport::run(job, tx),
    };
    audio::send(tx, JobUpdate::Done(done.unwrap_or_else(|e| format!("Export failed: {e}"))));
}

fn render_part(stems: &[Arc<Stem>], rack: &mut FxRack, sr: u32, frames: usize, part: &Part) -> (Vec<f32>, Vec<f32>) {
    let n = if part.frames > 0 { part.frames } else { frames };
    audio::render_aligned(stems, &part.mix, sr, n, rack)
}

fn export_stems(stems: &[Arc<Stem>], rack: &mut FxRack, sr: u32, frames: usize, parts: &[Part], dir: &Path, tx: &Sender<JobUpdate>) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // stems are parts of the mix: no master effects
    rack.master.clear();
    for (i, part) in parts.iter().enumerate() {
        audio::send(tx, JobUpdate::Status(format!("Exporting stems ({}/{}: {})...", i + 1, parts.len(), part.name)));
        let (l, r) = render_part(stems, rack, sr, frames, part);
        let path = dir.join(format!("{}.wav", part.name));
        std::fs::write(&path, wav_f32_bytes(&l, &r, sr)).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(format!("Exported {} stems to {}", parts.len(), dir.display()))
}

#[allow(clippy::too_many_arguments)]
fn export_dawproject(
    stems: &[Arc<Stem>],
    rack: &mut FxRack,
    sr: u32,
    frames: usize,
    parts: &[Part],
    project_xml: &str,
    metadata_xml: &str,
    path: &Path,
    tx: &Sender<JobUpdate>,
) -> Result<String, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    // master effects stay in Poing: the DAW gets the lanes as they sound into its own master
    rack.master.clear();
    let tmp = path.with_extension("dawproject.part");
    let mut zip = ZipWriter::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let io = |e: std::io::Error| e.to_string();
    zip.add("project.xml", project_xml.as_bytes()).map_err(io)?;
    zip.add("metadata.xml", metadata_xml.as_bytes()).map_err(io)?;
    for (i, part) in parts.iter().enumerate() {
        audio::send(tx, JobUpdate::Status(format!("Writing the DAWproject ({}/{}: {})...", i + 1, parts.len(), part.name)));
        let (l, r) = render_part(stems, rack, sr, frames, part);
        zip.add(&format!("audio/{}.wav", part.name), &wav_f32_bytes(&l, &r, sr)).map_err(io)?;
    }
    zip.finish().map_err(io)?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("Exported {}", path.display()))
}

// ------------------------------------------------------------------ the mixes

/// Lane `lane` as a stem: its level (mute heard, solo not; `pre_fader`: at
/// 0 dB and unmuted), its lane takes and layers, ducked under section takes
/// as in the mix; no other lane, section, layer of its own or song take.
/// With [`sections_stem`] they add up to the mix before the master effects.
pub fn lane_stem(p: &Project, mix: &MixParams, lane: usize, pre_fader: bool) -> MixParams {
    let mut m = mix.clone();
    for (i, g) in m.gains.iter_mut().enumerate() {
        let t = &p.tracks[i];
        *g = if i != lane {
            0.0
        } else if pre_fader {
            1.0
        } else if t.muted() {
            0.0
        } else {
            db_to_lin(t.gain())
        };
    }
    for k in m.takes.iter_mut() {
        if k.is_some_and(|k| k.kind != TakeKind::Section && take_lane(&k) != Some(lane)) {
            *k = None;
        }
    }
    m.lanes_only = true;
    m
}

/// What plays besides the lanes: section takes (remixes combined per
/// section) and layers of their own. None when there are none.
pub fn sections_stem(mix: &MixParams) -> Option<MixParams> {
    let mut m = mix.clone();
    m.gains.iter_mut().for_each(|g| *g = 0.0);
    for k in m.takes.iter_mut() {
        if k.is_some_and(|k| !matches!(k.kind, TakeKind::Section | TakeKind::Layer(None))) {
            *k = None;
        }
    }
    m.lanes_only = false;
    m.takes.iter().any(Option::is_some).then_some(m)
}

// ---------------------------------------------------------------- file names

/// A file name from a lane, song or version name: no characters Windows or
/// macOS refuse, at most `max` characters.
pub fn safe_name(s: &str, max: usize) -> String {
    let cleaned: String = s.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { ' ' } else { c }).collect();
    let mut out = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > max {
        out = out.chars().take(max).collect::<String>();
    }
    let out = out.trim_end_matches(['.', ' ']).trim().to_string();
    let reserved = ["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "LPT1", "LPT2", "LPT3"];
    if out.is_empty() || reserved.contains(&out.to_uppercase().as_str()) {
        format!("_{out}")
    } else {
        out
    }
}

/// `names`, each made unique (case-insensitively, as file systems are) by a number.
pub fn unique_names(names: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    names
        .iter()
        .map(|n| {
            let mut name = n.clone();
            let mut k = 2;
            while seen.contains(&name.to_lowercase()) {
                name = format!("{n} {k}");
                k += 1;
            }
            seen.push(name.to_lowercase());
            name
        })
        .collect()
}

// ------------------------------------------------------------------ tempo map

/// The song's beat grid as a DAW sees it: DAW beat `d` is grid beat
/// `d - shift`. `shift` is whole and puts the grid's downbeats on the DAW's
/// bar lines, with DAW beat 0 at or before the song's start (a pickup bar
/// when the first beat comes late). The tempo holds from beat to beat, so
/// the DAW maps beats to seconds exactly as Poing plays MIDI notes.
#[derive(Clone, Debug)]
pub struct TempoMap {
    grid: BeatGrid,
    pub shift: f64,
    pub per_bar: u32,
}

impl TempoMap {
    pub fn new(grid: BeatGrid) -> Self {
        let lead = (-grid.beat_at(0.0)).max(0.0);
        let per = grid.per_bar.max(1) as i64;
        let off = grid.bar_offset.round() as i64;
        let mut k = (lead - 1e-9).ceil().max(0.0) as i64;
        while (k + off).rem_euclid(per) != 0 {
            k += 1;
        }
        TempoMap { per_bar: grid.per_bar.max(1), grid, shift: k as f64 }
    }

    pub fn of(p: &Project) -> Self {
        Self::new(BeatGrid::of(p))
    }

    /// DAW beat at song time `t` (seconds).
    pub fn beat_of(&self, t: f64) -> f64 {
        self.grid.beat_at(t) + self.shift
    }

    /// Song time (seconds) of DAW beat `b`.
    pub fn seconds_of(&self, b: f64) -> f64 {
        self.grid.time_of(b - self.shift)
    }

    /// The tempo (DAW beat, bpm) from beat 0 through beat `end`, each held
    /// until the next; repeats left out.
    pub fn tempos(&self, end: f64) -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = Vec::new();
        for d in 0..=end.max(0.0).ceil() as usize {
            let secs = self.seconds_of(d as f64 + 1.0) - self.seconds_of(d as f64);
            let bpm = 60.0 / secs.max(1e-3);
            if out.last().is_none_or(|(_, last)| (last - bpm).abs() > 1e-6) {
                out.push((d as f64, bpm));
            }
        }
        out
    }

    /// The most common tempo (bpm), for a DAW's transport.
    pub fn bpm(&self) -> f64 {
        60.0 / self.grid.seconds_per_beat()
    }
}

// ----------------------------------------------------------------- MIDI files

/// Ticks per quarter note (one grid beat).
pub const PPQ: u32 = 960;

fn vlq(out: &mut Vec<u8>, mut v: u32) {
    let mut bytes = vec![(v & 0x7f) as u8];
    v >>= 7;
    while v > 0 {
        bytes.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.extend(bytes.iter().rev());
}

fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
}

/// Events (tick, bytes) as a track chunk body, in tick order (stable: equal
/// ticks keep their order), ended.
fn track_body(mut events: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    events.sort_by_key(|e| e.0);
    let mut body = Vec::new();
    let mut now = 0;
    for (tick, bytes) in events {
        vlq(&mut body, tick - now);
        body.extend_from_slice(&bytes);
        now = tick;
    }
    body.extend_from_slice(&[0x00, 0xff, 0x2f, 0x00]);
    body
}

fn meta_text(kind: u8, text: &str) -> Vec<u8> {
    let mut b = vec![0xff, kind];
    vlq(&mut b, text.len() as u32);
    b.extend_from_slice(text.as_bytes());
    b
}

/// A standard MIDI file (type 1): a tempo track with the song's tempo map
/// and meter, then the lane's notes (`channel` 9 for General MIDI drums).
/// Notes are in grid beats, as on a MIDI lane.
pub fn midi_file(song: &str, lane: &str, notes: &[Note], map: &TempoMap, channel: u8, end_beat: f64) -> Vec<u8> {
    let tick = |b: f64| ((b + map.shift).max(0.0) * PPQ as f64).round() as u32;
    // tempo: microseconds per quarter, rounded with the error carried on so
    // the file's clock never drifts from the grid
    let mut conductor = vec![(0, meta_text(0x03, song)), (0, vec![0xff, 0x58, 0x04, map.per_bar as u8, 2, 24, 8])];
    let last_note = notes.iter().map(|n| n.start + n.len).fold(end_beat, f64::max) + map.shift;
    let mut carry = 0.0;
    let mut prev: Option<u32> = None;
    for d in 0..=last_note.ceil() as usize {
        let exact = (map.seconds_of(d as f64 + 1.0) - map.seconds_of(d as f64)) * 1e6 + carry;
        let us = exact.round().clamp(1.0, 0xff_ffff as f64) as u32;
        carry = exact - us as f64;
        if prev != Some(us) {
            conductor.push((d as u32 * PPQ, vec![0xff, 0x51, 0x03, (us >> 16) as u8, (us >> 8) as u8, us as u8]));
            prev = Some(us);
        }
    }
    let ch = channel & 0x0f;
    let mut events = vec![(0, meta_text(0x03, lane))];
    // note-offs before note-ons at the same tick (a repeated note sounds again)
    let mut offs = Vec::new();
    let mut ons = Vec::new();
    for n in notes {
        let on = tick(n.start);
        let off = tick(n.start + n.len).max(on + 1);
        let vel = (n.vel * 127.0).round().clamp(1.0, 127.0) as u8;
        ons.push((on, vec![0x90 | ch, n.pitch.min(127), vel]));
        offs.push((off, vec![0x80 | ch, n.pitch.min(127), 0]));
    }
    offs.sort_by_key(|e| e.0);
    ons.sort_by_key(|e| e.0);
    events.extend(offs);
    events.extend(ons);
    // offs were added first: a stable sort keeps them first within a tick
    let mut out = Vec::new();
    chunk(&mut out, b"MThd", &[0, 1, 0, 2, (PPQ >> 8) as u8, PPQ as u8]);
    chunk(&mut out, b"MTrk", &track_body(conductor));
    chunk(&mut out, b"MTrk", &track_body(events));
    out
}

/// Does a MIDI lane play drums (General MIDI channel 10)?
pub fn is_drum_lane(t: &poing_model::Track) -> bool {
    let inst = t.instrument.as_ref();
    [Some(t.name.as_str()), inst.map(|i| i.name.as_str()), inst.and_then(|i| i.prompt.as_deref())]
        .into_iter()
        .flatten()
        .any(|s| {
            let s = s.to_lowercase();
            ["drum", "beat", "percussion", "groove"].iter().any(|w| s.contains(w))
        })
}

/// Write every MIDI lane of `p` into `dir/<lane>.mid`; the files written.
pub fn export_midi(p: &Project, dir: &Path) -> Result<Vec<PathBuf>, String> {
    let lanes: Vec<&poing_model::Track> = p.tracks.iter().filter(|t| t.is_midi()).collect();
    if lanes.is_empty() {
        return Err("this song has no MIDI lanes".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let map = TempoMap::of(p);
    let end = map.grid.beat_at(p.duration);
    let names = unique_names(&lanes.iter().map(|t| safe_name(&t.name, 60)).collect::<Vec<_>>());
    let mut written = Vec::new();
    for (t, name) in lanes.iter().zip(names) {
        let notes = t.notes.as_deref().unwrap_or_default();
        let bytes = midi_file(&p.name, &t.name, notes, &map, if is_drum_lane(t) { 9 } else { 0 }, end);
        let path = dir.join(format!("{name}.mid"));
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

// ------------------------------------------------------------------ WAV, zip

/// A 32-bit float stereo WAV: stems keep their level and never clip.
pub fn wav_f32_bytes(l: &[f32], r: &[f32], sr: u32) -> Vec<u8> {
    let frames = l.len().min(r.len());
    let data_len = (frames * 8) as u32;
    let mut b = Vec::with_capacity(58 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(50 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&18u32.to_le_bytes());
    b.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&sr.to_le_bytes());
    b.extend_from_slice(&(sr * 8).to_le_bytes());
    b.extend_from_slice(&8u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(b"fact");
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(&(frames as u32).to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        b.extend_from_slice(&l[i].to_le_bytes());
        b.extend_from_slice(&r[i].to_le_bytes());
    }
    b
}

fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    !data.iter().fold(!0u32, |c, &b| table[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8))
}

/// A zip archive of stored (uncompressed) entries, written as they come.
pub struct ZipWriter {
    out: std::io::BufWriter<std::fs::File>,
    offset: u64,
    central: Vec<u8>,
    count: u16,
}

impl ZipWriter {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        Ok(ZipWriter { out: std::io::BufWriter::new(std::fs::File::create(path)?), offset: 0, central: Vec::new(), count: 0 })
    }

    pub fn add(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        let too_big = || std::io::Error::other("zip entries over 4 GB are not supported");
        let size = u32::try_from(data.len()).map_err(|_| too_big())?;
        let offset = u32::try_from(self.offset).map_err(|_| too_big())?;
        let crc = crc32(data);
        // version 2.0, flags: UTF-8 names, stored, no date
        let common = |b: &mut Vec<u8>| {
            b.extend_from_slice(&20u16.to_le_bytes());
            b.extend_from_slice(&0x0800u16.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            b.extend_from_slice(&0x21u16.to_le_bytes()); // 1980-01-01
            b.extend_from_slice(&crc.to_le_bytes());
            b.extend_from_slice(&size.to_le_bytes());
            b.extend_from_slice(&size.to_le_bytes());
            b.extend_from_slice(&(name.len() as u16).to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
        };
        let mut local = b"PK\x03\x04".to_vec();
        common(&mut local);
        local.extend_from_slice(name.as_bytes());
        self.out.write_all(&local)?;
        self.out.write_all(data)?;
        let c = &mut self.central;
        c.extend_from_slice(b"PK\x01\x02");
        c.extend_from_slice(&20u16.to_le_bytes()); // made by
        common(c);
        // comment length, disk, internal and external attributes
        c.extend_from_slice(&[0; 10]);
        c.extend_from_slice(&offset.to_le_bytes());
        c.extend_from_slice(name.as_bytes());
        self.offset += (local.len() + data.len()) as u64;
        self.count += 1;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<()> {
        let at = u32::try_from(self.offset).map_err(|_| std::io::Error::other("zip over 4 GB"))?;
        self.out.write_all(&self.central)?;
        let mut end = b"PK\x05\x06".to_vec();
        end.extend_from_slice(&[0; 4]);
        end.extend_from_slice(&self.count.to_le_bytes());
        end.extend_from_slice(&self.count.to_le_bytes());
        end.extend_from_slice(&(self.central.len() as u32).to_le_bytes());
        end.extend_from_slice(&at.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        self.out.write_all(&end)?;
        self.out.flush()
    }
}

// ----------------------------------------------------------------- DAWproject

pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// What a DAWproject track holds.
#[derive(Clone, Debug)]
pub enum DawContent {
    /// An audio file in the archive (`audio/<file>.wav`), `seconds` long, from the song's start.
    Audio { file: String, seconds: f64 },
    /// Notes in grid beats (a MIDI lane: the DAW picks the instrument).
    Notes(Vec<Note>),
}

#[derive(Clone, Debug)]
pub struct DawTrack {
    pub name: String,
    pub content: DawContent,
    /// Fader, linear.
    pub volume: f64,
    pub mute: bool,
}

/// DAWproject colours for lanes, cycled.
const COLORS: [&str; 8] = ["#d9534f", "#f0ad4e", "#5bc0de", "#5cb85c", "#9b59b6", "#e67e22", "#1abc9c", "#3498db"];

/// project.xml of a DAWproject: the tracks into a master, the clips on a
/// beat timeline with the song's tempo map, the sections as markers.
pub fn dawproject_xml(map: &TempoMap, tracks: &[DawTrack], sections: &[poing_model::Section], sr: u32, song_end: f64) -> String {
    let f = |x: f64| format!("{:.6}", x).trim_end_matches('0').trim_end_matches('.').to_string();
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Project version=\"1.0\">\n");
    x.push_str(&format!("  <Application name=\"Poing\" version=\"{}\"/>\n", env!("CARGO_PKG_VERSION")));
    x.push_str("  <Transport>\n");
    x.push_str(&format!("    <Tempo id=\"tempo\" name=\"Tempo\" unit=\"bpm\" min=\"20\" max=\"666\" value=\"{}\"/>\n", f(map.bpm())));
    x.push_str(&format!("    <TimeSignature id=\"timesig\" numerator=\"{}\" denominator=\"4\"/>\n", map.per_bar));
    x.push_str("  </Transport>\n  <Structure>\n");
    for (i, t) in tracks.iter().enumerate() {
        let kind = match t.content {
            DawContent::Audio { .. } => "audio",
            DawContent::Notes(_) => "notes",
        };
        x.push_str(&format!(
            "    <Track id=\"track{i}\" name=\"{}\" color=\"{}\" contentType=\"{kind}\" loaded=\"true\">\n",
            xml_escape(&t.name),
            COLORS[i % COLORS.len()]
        ));
        x.push_str(&format!("      <Channel id=\"channel{i}\" role=\"regular\" audioChannels=\"2\" destination=\"master\" solo=\"false\">\n"));
        x.push_str(&format!("        <Mute id=\"mute{i}\" name=\"Mute\" value=\"{}\"/>\n", t.mute));
        x.push_str(&format!("        <Pan id=\"pan{i}\" name=\"Pan\" unit=\"normalized\" min=\"0\" max=\"1\" value=\"0.5\"/>\n"));
        x.push_str(&format!("        <Volume id=\"volume{i}\" name=\"Volume\" unit=\"linear\" min=\"0\" max=\"2\" value=\"{}\"/>\n", f(t.volume.clamp(0.0, 2.0))));
        x.push_str("      </Channel>\n    </Track>\n");
    }
    x.push_str("    <Channel id=\"master\" name=\"Master\" role=\"master\" audioChannels=\"2\" solo=\"false\">\n");
    x.push_str("      <Mute id=\"mastermute\" name=\"Mute\" value=\"false\"/>\n");
    x.push_str("      <Volume id=\"mastervolume\" name=\"Volume\" unit=\"linear\" min=\"0\" max=\"2\" value=\"1\"/>\n");
    x.push_str("    </Channel>\n  </Structure>\n");
    x.push_str("  <Arrangement id=\"arrangement\">\n    <Lanes id=\"lanes\" timeUnit=\"beats\">\n");
    let start = map.beat_of(0.0);
    let end = map.beat_of(song_end);
    for (i, t) in tracks.iter().enumerate() {
        x.push_str(&format!("      <Clips id=\"clips{i}\" track=\"track{i}\">\n"));
        match &t.content {
            // unstretched, from the song's start: the tempo map puts its beats on the grid
            DawContent::Audio { file, seconds } => {
                x.push_str(&format!(
                    "        <Clip time=\"{}\" duration=\"{}\" playStart=\"0\" playStop=\"{}\" contentTimeUnit=\"seconds\" name=\"{}\">\n",
                    f(start),
                    f(map.beat_of(*seconds) - start),
                    f(*seconds),
                    xml_escape(&t.name)
                ));
                x.push_str(&format!(
                    "          <Audio id=\"audio{i}\" channels=\"2\" sampleRate=\"{sr}\" duration=\"{}\" algorithm=\"raw\">\n            <File path=\"audio/{}.wav\"/>\n          </Audio>\n",
                    f(*seconds),
                    xml_escape(file)
                ));
            }
            DawContent::Notes(notes) => {
                let last = notes.iter().map(|n| n.start + n.len + map.shift).fold(end, f64::max);
                x.push_str(&format!("        <Clip time=\"0\" duration=\"{}\" contentTimeUnit=\"beats\" name=\"{}\">\n", f(last), xml_escape(&t.name)));
                x.push_str(&format!("          <Notes id=\"notes{i}\">\n"));
                for n in notes {
                    x.push_str(&format!(
                        "            <Note time=\"{}\" duration=\"{}\" channel=\"0\" key=\"{}\" vel=\"{}\" rel=\"0.5\"/>\n",
                        f(n.start + map.shift),
                        f(n.len),
                        n.pitch.min(127),
                        f(n.vel.clamp(0.0, 1.0))
                    ));
                }
                x.push_str("          </Notes>\n");
            }
        }
        x.push_str("        </Clip>\n      </Clips>\n");
    }
    x.push_str("    </Lanes>\n");
    if !sections.is_empty() {
        x.push_str("    <Markers id=\"markers\" timeUnit=\"beats\">\n");
        for s in sections {
            x.push_str(&format!("      <Marker time=\"{}\" name=\"{}\"/>\n", f(map.beat_of(s.start)), xml_escape(&s.label)));
        }
        x.push_str("    </Markers>\n");
    }
    x.push_str("    <TempoAutomation id=\"tempoautomation\" timeUnit=\"beats\" unit=\"bpm\">\n      <Target parameter=\"tempo\"/>\n");
    for (b, bpm) in map.tempos(end) {
        x.push_str(&format!("      <RealPoint time=\"{}\" value=\"{}\" interpolation=\"hold\"/>\n", f(b), f(bpm)));
    }
    x.push_str("    </TempoAutomation>\n  </Arrangement>\n  <Scenes/>\n</Project>\n");
    x
}

pub fn dawproject_metadata(title: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<MetaData>\n  <Title>{}</Title>\n  <Comment>Made with Poing</Comment>\n</MetaData>\n",
        xml_escape(title)
    )
}

// --------------------------------------------------------- jobs from the song

/// The lanes heard in the mix, each with its stem's file name (muted lanes
/// and empty ones left out).
pub fn stem_parts(p: &Project, mix: &MixParams) -> Vec<Part> {
    let lanes: Vec<usize> = (0..p.tracks.len()).filter(|&i| !p.tracks[i].muted()).collect();
    let mut names: Vec<String> = lanes.iter().map(|&i| safe_name(&p.tracks[i].name, 60)).collect();
    let mut mixes: Vec<MixParams> = lanes.iter().map(|&i| lane_stem(p, mix, i, false)).collect();
    if let Some(m) = sections_stem(mix) {
        names.push("Remix sections".into());
        mixes.push(m);
    }
    unique_names(&names).into_iter().zip(mixes).map(|(name, mix)| Part { name, mix, frames: 0 }).collect()
}

/// A DAWproject of the song as it plays: every lane at 0 dB with the fader
/// and mute in the DAW, MIDI lanes as notes (and as heard, rendered), the
/// combined remix sections and a version being heard as tracks of their own.
/// Returns the parts to render and project.xml.
pub fn dawproject_parts(p: &Project, mix: &MixParams, sr: u32, frames: usize, song_take: Option<(usize, String, usize)>) -> (Vec<Part>, String) {
    let map = TempoMap::of(p);
    let seconds = frames as f64 / sr as f64;
    let mut tracks: Vec<DawTrack> = Vec::new();
    let mut parts: Vec<Part> = Vec::new();
    // a version (song take) heard instead of the lanes: they come along, muted
    let version_on = song_take.is_some();
    let mut names: Vec<String> = Vec::new();
    for (i, t) in p.tracks.iter().enumerate() {
        let volume = db_to_lin(t.gain()) as f64;
        let mute = t.muted() || version_on;
        if let Some(notes) = &t.notes {
            tracks.push(DawTrack { name: t.name.clone(), content: DawContent::Notes(notes.clone()), volume, mute });
            names.push(format!("{} (audio)", t.name));
        } else {
            names.push(t.name.clone());
        }
        tracks.push(DawTrack { name: names.last().cloned().unwrap_or_default(), content: DawContent::Audio { file: String::new(), seconds }, volume, mute });
        parts.push(Part { name: String::new(), mix: lane_stem(p, mix, i, true), frames: 0 });
    }
    if let Some(m) = sections_stem(mix) {
        names.push("Remix sections".into());
        tracks.push(DawTrack { name: "Remix sections".into(), content: DawContent::Audio { file: String::new(), seconds }, volume: 1.0, mute: version_on });
        parts.push(Part { name: String::new(), mix: m, frames: 0 });
    }
    if let Some((take, label, take_frames)) = song_take {
        let mut m = MixParams::from_project(p, mix.version, &[p.takes()[take].id.clone()]);
        m.lanes_only = false;
        names.push(label.clone());
        tracks.push(DawTrack { name: label, content: DawContent::Audio { file: String::new(), seconds: take_frames as f64 / sr as f64 }, volume: 1.0, mute: false });
        parts.push(Part { name: String::new(), mix: m, frames: take_frames });
    }
    // the audio files, named after their tracks
    let files = unique_names(&names.iter().map(|n| safe_name(n, 60)).collect::<Vec<_>>());
    let mut files = files.into_iter();
    for (t, part) in tracks.iter_mut().filter(|t| matches!(t.content, DawContent::Audio { .. })).zip(parts.iter_mut()) {
        let file = files.next().unwrap_or_default();
        if let DawContent::Audio { file: f, .. } = &mut t.content {
            *f = file.clone();
        }
        part.name = file;
    }
    let xml = dawproject_xml(&map, &tracks, p.sections(), sr, seconds);
    (parts, xml)
}

// ------------------------------------------------------ jobs from the editor

/// What the Export sheet can make.
pub enum Kind {
    Stems,
    Midi,
    DawProject,
    /// The song as it plays plus these song takes (ids), as WAV or MP3.
    Dj { versions: Vec<String>, mp3: bool },
}

/// How a take is called on its chip (as the timeline shows it).
pub fn take_label(t: &poing_model::Take) -> String {
    if crate::state::named_take(t) {
        t.prompt.clone()
    } else {
        format!("Take {}", t.id.trim_start_matches('t'))
    }
}

/// The versions of the song a DJ export can add: its song takes, but not
/// the one playing (that one is the song as it plays). (take id, label)
pub fn dj_versions(st: &EditorState) -> Vec<(String, String)> {
    let playing = st.song_take().map(|(_, t)| t.id.clone());
    st.project.takes().iter().filter(|t| t.scope == "song" && Some(&t.id) != playing.as_ref()).map(|t| (t.id.clone(), take_label(t))).collect()
}

/// Frames of the song's lanes.
fn lane_frames(st: &EditorState) -> usize {
    let lanes = st.stems.iter().take(st.project.tracks.len()).map(|s| s.l.len()).max().unwrap_or(0);
    lanes.max((st.project.duration * st.sr() as f64) as usize)
}

/// Does a take keep the song's analysed timing (so its grid and cues fit)?
fn keeps_timing(t: &poing_model::Take) -> bool {
    matches!(t.engine.as_str(), "acestep-cover" | "copy")
}

/// Start an export of the open song: the job for the worker (None when it
/// is done already) and what the status line says.
pub fn start(st: &mut EditorState, kind: Kind) -> Result<(Option<ExportJob>, String), String> {
    let bundle = st.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let renders = bundle.join("renders");
    let song = safe_name(&st.project.name, 80);
    let sr = st.sr();
    let frames = lane_frames(st);
    let mix = st.mix_snapshot();
    let without = |missing: &[String]| if missing.is_empty() { String::new() } else { format!(" without {} (it would not load a second time)", missing.join(", ")) };
    match kind {
        Kind::Midi => {
            let dir = renders.join(format!("{song} midi"));
            let files = export_midi(&st.project, &dir)?;
            Ok((None, format!("Exported {} MIDI file{} to {}", files.len(), if files.len() == 1 { "" } else { "s" }, dir.display())))
        }
        Kind::Stems => {
            let parts = stem_parts(&st.project, &mix);
            if parts.is_empty() {
                return Err("every lane is muted: there is nothing to export".into());
            }
            let (rack, missing) = st.make_render_rack();
            let n = parts.len();
            let job = ExportJob::Stems { stems: st.stems.clone(), rack: Box::new(rack), sr, frames, parts, dir: renders.join(format!("{song} stems")) };
            Ok((Some(job), format!("Exporting {n} stems{}...", without(&missing))))
        }
        Kind::DawProject => {
            let take = st.song_take().map(|(stem, t)| (st.project.take_index(&t.id).unwrap_or(0), take_label(t), st.stems.get(stem).map_or(frames, |s| s.l.len())));
            let (parts, project_xml) = dawproject_parts(&st.project, &mix, sr, frames, take);
            let (rack, missing) = st.make_render_rack();
            let path = renders.join(format!("{song}.dawproject"));
            let job = ExportJob::DawProject { stems: st.stems.clone(), rack: Box::new(rack), sr, frames, parts, project_xml, metadata_xml: dawproject_metadata(&st.project.name), path };
            Ok((Some(job), format!("Writing the DAWproject{}...", without(&missing))))
        }
        Kind::Dj { versions, mp3 } => {
            let analysed = st.project.analysis.as_ref().is_some_and(|a| a.downbeats.as_ref().is_some_and(|d| !d.is_empty()) || a.beats.as_ref().is_some_and(|b| !b.is_empty()));
            let mut missing_all: Vec<String> = Vec::new();
            let mut renders_out = Vec::new();
            // the song as it plays: its lanes, or the version being heard
            let (label, timed, len) = match st.song_take() {
                Some((stem, t)) => (take_label(t), analysed && keeps_timing(t), st.stems.get(stem).map_or(frames, |s| s.l.len())),
                None => ("Mix".to_string(), analysed && !st.project.is_draft(), frames),
            };
            let (rack, missing) = st.make_render_rack();
            missing_all.extend(missing);
            renders_out.push(crate::djexport::DjRender { label, mix: mix.clone(), rack: Box::new(rack), frames: len, timed });
            for id in &versions {
                let Some(i) = st.project.take_index(id) else { continue };
                let t = st.project.takes()[i].clone();
                let stem = st.project.tracks.len() + i;
                let len = st.stems.get(stem).map_or(0, |s| s.l.len());
                if len == 0 {
                    continue;
                }
                let (rack, missing) = st.make_render_rack();
                missing_all.extend(missing);
                let m = MixParams::from_project(&st.project, mix.version, std::slice::from_ref(id));
                renders_out.push(crate::djexport::DjRender { label: take_label(&t), mix: m, rack: Box::new(rack), frames: len, timed: analysed && keeps_timing(&t) });
            }
            missing_all.sort();
            missing_all.dedup();
            let a = st.project.analysis.clone().unwrap_or_default();
            let grid = BeatGrid::of(&st.project);
            let timing = crate::djexport::DjTiming {
                beats: a.beats.unwrap_or_default(),
                downbeats: a.downbeats.unwrap_or_default(),
                per_bar: grid.per_bar,
                tempo: a.tempo,
                key: a.key,
                sections: st.project.sections().clone(),
            };
            let manifest = safe_name(&bundle.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| song.clone()), 120);
            let n = renders_out.len();
            let job = crate::djexport::DjJob {
                stems: st.stems.clone(),
                sr,
                song: st.project.name.clone(),
                renders: renders_out,
                mp3,
                dir: audio::songs_dir().join("rekordbox"),
                manifest,
                timing,
            };
            Ok((Some(ExportJob::Dj(job)), format!("Exporting {n} track{} for DJ{}...", if n == 1 { "" } else { "s" }, without(&missing_all))))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::MixTake;
    use poing_model::{Section, Track};

    fn grid(beats: &[f64], downbeats: &[f64]) -> BeatGrid {
        BeatGrid::new(beats, downbeats, Some(120.0))
    }

    #[test]
    fn tempo_map_puts_downbeats_on_bars_and_starts_at_zero() {
        // first beat late (0.3 s), first downbeat on grid beat 1
        let beats: Vec<f64> = (0..40).map(|i| 0.3 + i as f64 * 0.5).collect();
        let downbeats: Vec<f64> = (0..10).map(|i| 0.8 + i as f64 * 2.0).collect();
        let map = TempoMap::new(grid(&beats, &downbeats));
        assert_eq!(map.per_bar, 4);
        // DAW beat 0 is at or before 0 s
        assert!(map.seconds_of(0.0) <= 1e-9, "{}", map.seconds_of(0.0));
        // every downbeat on a bar line
        for d in &downbeats {
            let b = map.beat_of(*d);
            assert!((b / 4.0 - (b / 4.0).round()).abs() < 1e-9, "downbeat {d} at DAW beat {b}");
        }
        // beats and seconds round-trip
        for t in [0.0, 0.31, 5.5, 19.9, 25.0] {
            assert!((map.seconds_of(map.beat_of(t)) - t).abs() < 1e-9);
        }
        assert!((map.bpm() - 120.0).abs() < 1e-9);
        assert_eq!(map.tempos(80.0), vec![(0.0, 120.0)]);
    }

    #[test]
    fn tempo_map_follows_a_played_in_grid() {
        let mut beats = vec![0.0];
        for i in 1..32 {
            let gap = if i % 2 == 0 { 0.52 } else { 0.48 };
            beats.push(beats[i - 1] + gap);
        }
        let map = TempoMap::new(grid(&beats, &[0.0]));
        let tempos = map.tempos(30.0);
        // replaying the held tempi lands on every detected beat
        let mut t = map.seconds_of(0.0);
        for d in 0..31usize {
            let bpm = tempos.iter().rev().find(|(b, _)| *b <= d as f64).unwrap().1;
            if d as f64 >= map.shift {
                let want = beats[d - map.shift as usize];
                assert!((t - want).abs() < 1e-6, "beat {d}: {t} vs {want}");
            }
            t += 60.0 / bpm;
        }
    }

    /// Parse a type-1 SMF: (tempo events (tick, us), note events (tick, status, key, vel)).
    fn parse_smf(b: &[u8]) -> (u16, Vec<(u32, u32)>, Vec<(u32, u8, u8, u8)>) {
        assert_eq!(&b[0..4], b"MThd");
        let ppq = u16::from_be_bytes([b[12], b[13]]);
        let mut at = 14;
        let (mut tempos, mut notes) = (Vec::new(), Vec::new());
        while at < b.len() {
            assert_eq!(&b[at..at + 4], b"MTrk");
            let len = u32::from_be_bytes([b[at + 4], b[at + 5], b[at + 6], b[at + 7]]) as usize;
            let body = &b[at + 8..at + 8 + len];
            let (mut i, mut tick) = (0, 0u32);
            while i < body.len() {
                let mut d = 0u32;
                loop {
                    let c = body[i];
                    i += 1;
                    d = (d << 7) | (c & 0x7f) as u32;
                    if c & 0x80 == 0 {
                        break;
                    }
                }
                tick += d;
                let s = body[i];
                if s == 0xff {
                    let kind = body[i + 1];
                    let n = body[i + 2] as usize;
                    if kind == 0x51 {
                        tempos.push((tick, u32::from_be_bytes([0, body[i + 3], body[i + 4], body[i + 5]])));
                    }
                    i += 3 + n;
                } else {
                    notes.push((tick, s, body[i + 1], body[i + 2]));
                    i += 3;
                }
            }
            at += 8 + len;
        }
        (ppq, tempos, notes)
    }

    #[test]
    fn midi_file_plays_notes_where_poing_does() {
        // a drifting, late-starting grid
        let beats: Vec<f64> = (0..64).map(|i| 0.7 + i as f64 * 0.5 + (i as f64 * 0.37).sin() * 0.02).collect();
        let g = grid(&beats, &[beats[2], beats[6], beats[10]]);
        let map = TempoMap::new(g.clone());
        let notes = vec![
            Note { start: 0.0, len: 1.0, pitch: 36, vel: 1.0 },
            Note { start: 1.0, len: 0.5, pitch: 36, vel: 0.5 },
            Note { start: 7.25, len: 0.25, pitch: 64, vel: 0.8 },
            Note { start: 40.5, len: 2.0, pitch: 67, vel: 0.1 },
        ];
        let bytes = midi_file("Song", "Bass", &notes, &map, 9, 60.0);
        let (ppq, tempos, events) = parse_smf(&bytes);
        assert_eq!(ppq as u32, PPQ);
        // seconds of a tick through the file's own tempo events
        let secs = |tick: u32| {
            let mut t = map.seconds_of(0.0);
            let mut last = (0u32, tempos[0].1);
            for &(at, us) in tempos.iter().skip(1) {
                if at > tick {
                    break;
                }
                t += (at - last.0) as f64 / PPQ as f64 * last.1 as f64 / 1e6;
                last = (at, us);
            }
            t + (tick - last.0) as f64 / PPQ as f64 * last.1 as f64 / 1e6
        };
        let ons: Vec<_> = events.iter().filter(|e| e.1 & 0xf0 == 0x90).collect();
        assert_eq!(ons.len(), notes.len());
        for (on, n) in ons.iter().zip(&notes) {
            assert_eq!(on.1, 0x99, "drums on channel 10");
            assert_eq!(on.2, n.pitch);
            let want = g.time_of(n.start);
            assert!((secs(on.0) - want).abs() < 0.001, "note at beat {}: {} vs {want}", n.start, secs(on.0));
        }
        // the repeated kick: its note-off comes before the next note-on at the same tick
        let at = ons[1].0;
        let same: Vec<_> = events.iter().filter(|e| e.0 == at).collect();
        assert_eq!(same[0].1 & 0xf0, 0x80);
        assert_eq!(same[1].1 & 0xf0, 0x90);
    }

    #[test]
    fn names_are_safe_and_unique() {
        assert_eq!(safe_name("AC/DC: \"Back\" in <Black>?", 60), "AC DC Back in Black");
        assert_eq!(safe_name("con", 60), "_con");
        assert_eq!(safe_name("  ...  ", 60), "_");
        assert_eq!(safe_name("a very long name indeed", 6), "a very");
        assert_eq!(unique_names(&["Bass".into(), "bass".into(), "Bass".into(), "Keys".into()]), vec!["Bass", "bass 2", "Bass 3", "Keys"]);
    }

    #[test]
    fn zip_is_readable() {
        let dir = std::env::temp_dir().join(format!("poing-zip-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.zip");
        let mut z = ZipWriter::create(&path).unwrap();
        z.add("project.xml", b"<Project/>").unwrap();
        z.add("audio/b\u{e9}.wav", &[1, 2, 3]).unwrap();
        z.finish().unwrap();
        let b = std::fs::read(&path).unwrap();
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        // the end record points at a central directory of two entries
        let end = b.len() - 22;
        assert_eq!(&b[end..end + 4], b"PK\x05\x06");
        assert_eq!(u16::from_le_bytes([b[end + 10], b[end + 11]]), 2);
        let cd = u32::from_le_bytes([b[end + 16], b[end + 17], b[end + 18], b[end + 19]]) as usize;
        // each central entry: its name at 46, then the local header it points at
        let mut at = cd;
        for name in ["project.xml", "audio/b\u{e9}.wav"] {
            assert_eq!(&b[at..at + 4], b"PK\x01\x02");
            let len = u16::from_le_bytes([b[at + 28], b[at + 29]]) as usize;
            assert_eq!(&b[at + 46..at + 46 + len], name.as_bytes());
            let local = u32::from_le_bytes([b[at + 42], b[at + 43], b[at + 44], b[at + 45]]) as usize;
            assert_eq!(&b[local..local + 4], b"PK\x03\x04");
            assert_eq!(&b[local + 30..local + 30 + len], name.as_bytes());
            at += 46 + len;
        }
        assert_eq!(at, end);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn project() -> Project {
        let track = |id: &str, gain: Option<f64>, mute: Option<bool>| Track {
            id: id.into(),
            name: id.to_uppercase(),
            file: format!("{id}.wav"),
            gain_db: gain,
            mute,
            solo: None,
            fx: None,
            notes: None,
            instrument: None,
        };
        let mut p = Project::import("t", "a.wav", 2.0, 100);
        p.tracks = vec![track("a", Some(-6.0), None), track("b", None, None), track("c", None, Some(true))];
        p.regions = Some(vec![poing_model::Region { id: "r".into(), track: "b".into(), start: 0.5, end: 0.8, kind: "gain".into(), gain_db: Some(-12.0), note: None }]);
        p.sections = Some(vec![Section { start: 0.0, end: 1.0, label: "intro".into() }, Section { start: 1.0, end: 2.0, label: "chorus & co".into() }]);
        p
    }

    #[test]
    fn stems_add_up_to_the_mix() {
        let p = project();
        let sr = 100;
        let n = 200;
        let tone = |f: f32| Arc::new(Stem::new((0..n).map(|i| (i as f32 * f).sin() * 0.5).collect(), (0..n).map(|i| (i as f32 * f).cos() * 0.5).collect()));
        // lanes a, b, c; then a lane take on b, a layer of its own, a section take
        let stems: Vec<Arc<Stem>> = vec![tone(0.1), tone(0.23), tone(0.37), tone(0.05), tone(0.61), tone(0.83)];
        let mut mix = MixParams::from_project(&p, 1, &[]);
        let take = |stem: usize, start: f64, end: f64, kind: TakeKind, blend: f32| {
            Some(MixTake { stem, start, end, kind, blend, fade_in: start > 0.0, fade_out: end < 2.0 })
        };
        mix.takes[0] = take(3, 0.2, 1.5, TakeKind::Lane(1), 0.7);
        mix.takes[1] = take(4, 0.0, 1.0, TakeKind::Layer(None), 0.5);
        mix.takes[2] = take(5, 0.9, 1.9, TakeKind::Section, 1.0);
        let render = |m: &MixParams| {
            let (mut l, mut r) = (vec![0f32; n], vec![0f32; n]);
            audio::mix_into(&stems, m, sr, 0, &mut l, &mut r, None);
            (l, r)
        };
        let (ml, mr) = render(&mix);
        let parts = stem_parts(&p, &mix);
        // the muted lane is left out; the sections and the free layer are a stem
        let names: Vec<&str> = parts.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["A", "B", "Remix sections"]);
        let (mut sl, mut sr_) = (vec![0f32; n], vec![0f32; n]);
        for part in &parts {
            let (l, r) = render(&part.mix);
            for i in 0..n {
                sl[i] += l[i];
                sr_[i] += r[i];
            }
        }
        for i in 0..n {
            assert!((sl[i] - ml[i]).abs() < 1e-5 && (sr_[i] - mr[i]).abs() < 1e-5, "frame {i}: {} vs {}", sl[i], ml[i]);
        }
        // lane a's stem is lane a alone at -6 dB
        let (al, _) = render(&parts[0].mix);
        assert!((al[10] - stems[0].l[10] * db_to_lin(-6.0)).abs() < 1e-6);
    }

    #[test]
    fn solo_does_not_silence_stems() {
        let mut p = project();
        p.tracks[0].solo = Some(true);
        let mix = MixParams::from_project(&p, 1, &[]);
        let parts = stem_parts(&p, &mix);
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|s| s.mix.gains.iter().any(|g| *g > 0.0)));
    }

    #[test]
    fn dawproject_has_tracks_markers_and_tempo() {
        let mut p = project();
        p.tracks.push(Track {
            id: "midi1".into(),
            name: "Bass".into(),
            file: String::new(),
            gain_db: None,
            mute: None,
            solo: None,
            fx: None,
            notes: Some(vec![Note { start: 1.0, len: 0.5, pitch: 40, vel: 0.9 }]),
            instrument: None,
        });
        let mix = MixParams::from_project(&p, 1, &[]);
        let (parts, xml) = dawproject_parts(&p, &mix, 100, 200, None);
        // A, B, C (muted), Bass (audio) rendered; Bass notes as a note track
        let names: Vec<&str> = parts.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["A", "B", "C", "Bass (audio)"]);
        assert!(parts.iter().all(|s| s.mix.lanes_only && s.mix.gains.iter().filter(|g| **g == 1.0).count() == 1));
        assert!(xml.contains("contentType=\"notes\""));
        assert!(xml.contains("<File path=\"audio/Bass (audio).wav\"/>"));
        assert!(xml.contains("name=\"chorus &amp; co\""));
        assert!(xml.contains("<Target parameter=\"tempo\"/>"));
        assert!(xml.contains("<Mute id=\"mute2\" name=\"Mute\" value=\"true\"/>"));
        // 120 bpm, 2 beats per second; beat grid starts at 0 s: notes at grid beat + shift
        assert!(xml.contains("<Note time=\"1\" duration=\"0.5\" channel=\"0\" key=\"40\""), "{xml}");
        // -6 dB on A's fader
        assert!(xml.contains("<Volume id=\"volume0\" name=\"Volume\" unit=\"linear\" min=\"0\" max=\"2\" value=\"0.501187\"/>"), "{xml}");
    }
}
