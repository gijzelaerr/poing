// Arrangement edits on the open song: sections moved, repeated or cut, the
// song made longer, and lyrics and chords corrected by hand. The time map is
// `poing_model::arrange`'s; here the audio follows it. Every lane and take is
// spliced at the same frames (crossfaded over SPLICE_XF_S at each cut) into
// new files, so the old files stay for undo.
//
// Included into state.rs as its child module `state::arrange` (it works on
// EditorState's own fields), so poing and poing-node find it alike.

use super::{EditorState, Selection};
use crate::audio::{self, Stem, Stems};
use crate::model::arrange::{self, Edit, Op, Piece, Realign};
use crate::model::{Project, Take};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Each cut crossfades over this long, centred on it.
pub const SPLICE_XF_S: f64 = 0.015;
/// Arrangement edits whose audio stays in memory for undo; older ones are
/// read back from their files.
const KEEP_IN_MEMORY: u64 = 2;
/// The engine of a take that makes the song longer.
pub const EXTEND: &str = "acestep-extend";

/// Audio placed on the old song's timeline: `l`/`r` start at frame `offset`;
/// outside them it is `fill` (the lane under a lane take), else silence.
pub struct Source<'a> {
    pub l: &'a [f32],
    pub r: &'a [f32],
    pub offset: usize,
    pub fill: Option<&'a Stem>,
}

impl Source<'_> {
    #[inline]
    fn at(&self, f: i64) -> (f32, f32) {
        if f < 0 {
            return (0.0, 0.0);
        }
        let f = f as usize;
        match f.checked_sub(self.offset) {
            Some(j) if j < self.l.len().min(self.r.len()) => (self.l[j], self.r[j]),
            _ => match self.fill {
                Some(s) if f < s.l.len().min(s.r.len()) => (s.l[f], s.r[f]),
                _ => (0.0, 0.0),
            },
        }
    }
}

/// Pieces in frames of the old song: `[start, end)`.
pub fn frames(pieces: &[Piece], sr: u32) -> Vec<(usize, usize)> {
    let f = |t: f64| (t.max(0.0) * sr as f64).round() as usize;
    pieces.iter().map(|p| (f(p.start), f(p.end))).collect()
}

/// The audio of `pieces` (frames of the old song) one after another. At each
/// cut the piece before carries on past its end and the next one starts a
/// little early, crossfaded with a raised cosine over `xf` frames, so the
/// seam has no step.
pub fn splice(src: &Source, pieces: &[(usize, usize)], xf: usize) -> (Vec<f32>, Vec<f32>) {
    let len: usize = pieces.iter().map(|(a, b)| b - a).sum();
    let (mut l, mut r) = (Vec::with_capacity(len), Vec::with_capacity(len));
    for &(a, b) in pieces {
        for f in a..b {
            let (x, y) = src.at(f as i64);
            l.push(x);
            r.push(y);
        }
    }
    let mut at = 0usize;
    for w in pieces.windows(2) {
        let ((a0, b0), (a1, b1)) = (w[0], w[1]);
        at += b0 - a0;
        if b0 == a1 {
            continue; // the old song went on here: no cut
        }
        let h = (xf / 2).min((b0 - a0) / 2).min((b1 - a1) / 2) as i64;
        for i in -h..h {
            let x = (i + h) as f64 / (2 * h) as f64;
            let g = (0.5 - 0.5 * (std::f64::consts::PI * x).cos()) as f32;
            let (ol, or) = src.at(b0 as i64 + i);
            let (nl, nr) = src.at(a1 as i64 + i);
            let k = (at as i64 + i) as usize;
            l[k] = ol * (1.0 - g) + nl * g;
            r[k] = or * (1.0 - g) + nr * g;
        }
    }
    (l, r)
}

/// The largest sample-to-sample step in `x[a..b]` (a click shows as one).
#[cfg(test)]
pub fn max_step(x: &[f32], a: usize, b: usize) -> f32 {
    x[a.max(1)..b.min(x.len())].iter().zip(&x[a.max(1) - 1..]).map(|(v, u)| (v - u).abs()).fold(0.0, f32::max)
}

/// A file name for an edit's audio next to `file`: `stems/vocals.wav` ->
/// `stems/vocals.a3.wav` (an earlier edit's `.a2` is replaced).
fn arranged_file(file: &str, n: u64) -> String {
    let path = Path::new(file);
    let dir = path.parent().map(|d| d.to_string_lossy().replace('\\', "/")).unwrap_or_default();
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "audio".into());
    let base = match stem.rsplit_once(".a") {
        Some((b, k)) if !k.is_empty() && k.chars().all(|c| c.is_ascii_digit()) => b.to_string(),
        _ => stem,
    };
    if dir.is_empty() {
        format!("{base}.a{n}.wav")
    } else {
        format!("{dir}/{base}.a{n}.wav")
    }
}

/// What happens to one take in an edit.
enum TakeAfter {
    /// Its audio as it was, moved.
    Moved(Take, Arc<Stem>),
    /// New audio, to be written to the take's (new) file.
    Spliced(Take, Stem),
}

impl EditorState {
    /// Move, repeat or cut a section: every lane and take is spliced, and
    /// everything timed on the song follows. Ctrl+Z undoes it.
    pub fn arrange(&mut self, op: &Op) -> Result<String, String> {
        if self.project.is_draft() {
            return Err("pick a version first".into());
        }
        if self.analysing.is_some() {
            return Err("a job is still running; arrange the song when it is done".into());
        }
        let edit = arrange::plan(&self.project, op)?;
        let says = arrange::says(op, self.project.sections());
        // the section to select after: the one moved or the copy
        let select = match *op {
            Op::Move { section, to } => Some(if to > section { to - 1 } else { to }),
            Op::Duplicate { to, .. } => Some(to),
            Op::Delete { section } => Some(section.min(edit.sections.len() - 1)),
        };
        self.checkpoint();
        if let Err(e) = self.apply_edit(&edit) {
            // nothing of it was published: the checkpoint goes again
            self.undo.pop();
            return Err(e);
        }
        // a whole-song remix keeps its own order: the lanes are what changed
        let songs: Vec<String> = self.project.takes().iter().filter(|t| t.scope == "song").map(|t| t.id.clone()).collect();
        if self.active.iter().any(|a| songs.contains(a)) {
            self.active.retain(|a| !songs.contains(a));
            self.focus = None;
            self.push_mix();
        }
        self.set_loop(None);
        self.pps = 0.0;
        self.select(select.map_or(Selection::None, Selection::Section));
        if let Some(s) = select.and_then(|i| self.project.sections().get(i)) {
            self.seek(s.start);
        }
        self.save()?;
        Ok(format!("{says}  ·  Ctrl+Z to undo"))
    }

    fn apply_edit(&mut self, edit: &Edit) -> Result<(), String> {
        let sr = self.sr();
        let dir = self.dir();
        // past any edit undone since (its files stay on disk)
        let mut n = self.project.arranged.unwrap_or(0) + 1;
        while self.project.tracks.iter().any(|t| !t.file.is_empty() && dir.join(arranged_file(&t.file, n)).exists()) {
            n += 1;
        }
        let xf = (SPLICE_XF_S * sr as f64) as usize;
        let all = frames(&edit.pieces, sr);
        let tracks = self.project.tracks.len();
        let old = self.project.clone();
        let old_stems = self.stems.clone();
        let mut writes: Vec<(PathBuf, Arc<Stem>)> = Vec::new();

        // the lanes (a MIDI lane's notes move with the model)
        let mut lanes: Vec<Arc<Stem>> = Vec::with_capacity(tracks);
        let mut files: Vec<String> = Vec::with_capacity(tracks);
        let spliced: Vec<Option<Stem>> = std::thread::scope(|s| {
            let jobs: Vec<_> = (0..tracks)
                .map(|i| {
                    let (stem, midi) = (&old_stems[i], old.tracks[i].is_midi());
                    let all = &all;
                    s.spawn(move || {
                        (!midi).then(|| {
                            let (l, r) = splice(&Source { l: &stem.l, r: &stem.r, offset: 0, fill: None }, all, xf);
                            Stem::new(l, r)
                        })
                    })
                })
                .collect();
            jobs.into_iter().map(|j| j.join().unwrap_or(None)).collect()
        });
        for (i, s) in spliced.into_iter().enumerate() {
            match s {
                Some(stem) => {
                    let file = arranged_file(&old.tracks[i].file, n);
                    let stem = Arc::new(stem);
                    writes.push((dir.join(&file), stem.clone()));
                    lanes.push(stem);
                    files.push(file);
                }
                None => {
                    lanes.push(old_stems[i].clone());
                    files.push(old.tracks[i].file.clone());
                }
            }
        }

        // the takes
        let reach = audio::TAKE_RAMP_S * 0.5;
        let mut after: Vec<TakeAfter> = Vec::new();
        let mut realign: Vec<Realign> = old.realign.clone().unwrap_or_default();
        let mut gone: Vec<String> = Vec::new();
        let mut became: Vec<(String, String)> = Vec::new();
        let mut next_take = old.clone();
        for (j, t) in old.takes().iter().enumerate() {
            let stem = &old_stems[tracks + j];
            let offset = (t.start.max(0.0) * sr as f64) as usize;
            match t.scope.as_str() {
                "song" => {
                    // a remix that lined up with the song lines up through a map now
                    if t.engine == "acestep-cover" {
                        // at first it is the song's own time, as far as it goes
                        let len = stem.l.len() as f64 / sr as f64;
                        let mut identity = vec![Piece { start: 0.0, end: len.min(old.duration) }];
                        if old.duration > len {
                            identity.push(arrange::gap(old.duration - len));
                        }
                        let map = realign.iter().find(|r| r.take == t.id).map(|r| r.pieces.clone()).unwrap_or(identity);
                        let pieces = arrange::compose(&map, &edit.pieces);
                        realign.retain(|r| r.take != t.id);
                        realign.push(Realign { take: t.id.clone(), pieces });
                    }
                    after.push(TakeAfter::Moved(t.clone(), stem.clone()));
                }
                "section" => {
                    let runs: Vec<(f64, Vec<Piece>)> = arrange::runs(&edit.pieces, t.start, t.end, reach).into_iter().filter(|(_, r)| arrange::duration(r) >= 1.0).collect();
                    if runs.is_empty() {
                        gone.push(t.id.clone());
                    }
                    for (k, (at, run)) in runs.into_iter().enumerate() {
                        let mut nt = t.clone();
                        if k > 0 {
                            let (id, seed) = next_take.next_take();
                            nt.id = id;
                            nt.seed = seed;
                            became.push((t.id.clone(), nt.id.clone()));
                        }
                        nt.start = at;
                        nt.end = at + arrange::duration(&run);
                        if run.len() == 1 && (run[0].start - t.start).abs() < 1e-6 && (run[0].end - t.end).abs() < 1e-6 {
                            after.push(TakeAfter::Moved(nt, stem.clone()));
                        } else {
                            nt.file = arranged_file(&format!("takes/{}.wav", nt.id), n);
                            let (l, r) = splice(&Source { l: &stem.l, r: &stem.r, offset, fill: None }, &frames(&run, sr), xf);
                            after.push(TakeAfter::Spliced(nt, Stem::new(l, r)));
                        }
                    }
                }
                _ => {
                    // a lane take or a layer: one take over where it is heard now,
                    // the lane itself (or silence) in any gap
                    let runs = arrange::runs(&edit.pieces, t.start, t.end, 0.0);
                    let (Some(first), Some(last)) = (runs.first(), runs.last()) else {
                        gone.push(t.id.clone());
                        continue;
                    };
                    let (lo, hi) = (first.0, last.0 + arrange::duration(&last.1));
                    if hi - lo < 1.0 {
                        gone.push(t.id.clone());
                        continue;
                    }
                    let mut nt = t.clone();
                    nt.start = lo;
                    nt.end = hi;
                    if runs.len() == 1 && runs[0].1.len() == 1 && (runs[0].1[0].start - t.start).abs() < 1e-6 && (runs[0].1[0].end - t.end).abs() < 1e-6 {
                        after.push(TakeAfter::Moved(nt, stem.clone()));
                        continue;
                    }
                    let lane = t.track.as_deref().and_then(|id| old.track_index(id)).filter(|_| t.scope == "lane");
                    let fill = lane.map(|i| &*old_stems[i]);
                    let (l, r) = splice(&Source { l: &stem.l, r: &stem.r, offset, fill }, &all, xf);
                    let (a, b) = ((lo * sr as f64) as usize, ((hi * sr as f64) as usize).min(l.len()));
                    nt.file = arranged_file(&t.file, n);
                    after.push(TakeAfter::Spliced(nt, Stem::new(l[a..b].to_vec(), r[a..b].to_vec())));
                }
            }
        }

        // the project: lanes, takes, then everything timed on the song
        let mut p = old.clone();
        for (i, f) in files.into_iter().enumerate() {
            if p.source.as_deref() == Some(p.tracks[i].file.as_str()) && f != p.tracks[i].file {
                p.source = Some(f.clone());
            }
            p.tracks[i].file = f;
        }
        let mut stems: Vec<Arc<Stem>> = lanes;
        let mut takes = Vec::with_capacity(after.len());
        for a in after {
            match a {
                TakeAfter::Moved(t, s) => {
                    takes.push(t);
                    stems.push(s);
                }
                TakeAfter::Spliced(t, s) => {
                    let s = Arc::new(s);
                    writes.push((dir.join(&t.file), s.clone()));
                    takes.push(t);
                    stems.push(s);
                }
            }
        }
        // the source, if it is not a lane, is spliced too: repaints and remixes read it
        if let Some(src) = p.source.clone().filter(|s| !p.tracks.iter().any(|t| &t.file == s)) {
            if let Ok(stem) = audio::load_stem(&dir.join(&src), sr) {
                let (l, r) = splice(&Source { l: &stem.l, r: &stem.r, offset: 0, fill: None }, &all, xf);
                let file = arranged_file(&src, n);
                writes.push((dir.join(&file), Arc::new(Stem::new(l, r))));
                p.source = Some(file);
            }
        }
        p.takes = Some(takes);
        p.takes_made = next_take.takes_made;
        p.arranged = Some(n);
        p.realign = (!realign.is_empty()).then_some(realign);
        let mut next_region = self.next_region;
        arrange::apply(&mut p, edit, &mut || {
            next_region += 1;
            format!("r{}", next_region - 1)
        });

        // the files first: the project only ever points at audio that is there
        std::fs::create_dir_all(dir.join("takes")).map_err(|e| e.to_string())?;
        let written: Vec<Result<(), String>> = std::thread::scope(|s| {
            let jobs: Vec<_> = writes
                .iter()
                .map(|(path, stem)| s.spawn(move || audio::write_wav(path, &stem.l, &stem.r, sr, 1.0).map_err(|e| format!("{}: {e}", path.display()))))
                .collect();
            jobs.into_iter().map(|j| j.join().unwrap_or_else(|_| Err("a writer stopped".into()))).collect()
        });
        written.into_iter().collect::<Result<Vec<()>, String>>()?;

        self.next_region = next_region;
        self.project = p;
        for (from, to) in became {
            if self.take_on(&from) {
                self.active.push(to);
            }
        }
        self.active.retain(|a| !gone.contains(a));
        if self.focus.as_ref().is_some_and(|f| gone.contains(f)) {
            self.focus = None;
        }
        self.publish_stems(Arc::new(stems));
        self.publish_fx();
        self.push_mix();
        self.write_lyrics();
        self.forget_old_audio();
        Ok(())
    }

    /// Undo steps more than KEEP_IN_MEMORY arrangement edits away let go of
    /// their audio; undoing to them reads it back from the files.
    pub(crate) fn forget_old_audio(&mut self) {
        let now = self.project.arranged.unwrap_or(0);
        for (p, stems) in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            if now.abs_diff(p.arranged.unwrap_or(0)) > KEEP_IN_MEMORY {
                *stems = None;
            }
        }
    }

    /// The audio of a project from its files, sharing what is loaded now.
    pub(crate) fn load_stems(&self, p: &Project) -> Result<Stems, String> {
        let dir = self.dir();
        let mut loaded: HashMap<&str, Arc<Stem>> = HashMap::new();
        let files = self.project.tracks.iter().map(|t| t.file.as_str()).chain(self.project.takes().iter().map(|t| t.file.as_str()));
        for (f, s) in files.zip(self.stems.iter()) {
            if !f.is_empty() {
                loaded.insert(f, s.clone());
            }
        }
        let mut out = Vec::new();
        let tracks = p.tracks.iter().map(|t| (t.file.as_str(), t.is_midi()));
        for (file, midi) in tracks.chain(p.takes().iter().map(|t| (t.file.as_str(), false))) {
            out.push(match loaded.get(file) {
                _ if midi => Arc::new(Stem::new(Vec::new(), Vec::new())),
                Some(s) => s.clone(),
                None => Arc::new(audio::load_stem(&dir.join(file), self.sr())?),
            });
        }
        Ok(Arc::new(out))
    }

    /// The remix over [a, b] of the song as it is now, as far as it reaches:
    /// (where it starts, where it ends, its audio). A remix made before an
    /// arrangement edit is spliced through how it lines up now.
    pub fn remix_range(&self, stem: usize, take: &Take, a: f64, b: f64) -> Option<(f64, f64, Stem)> {
        let src = &self.stems[stem];
        let sr = self.sr();
        let len = src.l.len() as f64 / sr as f64;
        let map = self.project.realign.as_ref().and_then(|r| r.iter().find(|r| r.take == take.id)).map(|r| r.pieces.clone()).unwrap_or_else(|| vec![Piece { start: 0.0, end: len }]);
        // where the map plays the remix, in song time: (from, to, remix time at from)
        let spans: Vec<(f64, f64, f64)> = map
            .iter()
            .zip(arrange::placed(&map))
            .filter_map(|(m, d)| {
                let (s, e) = (a.max(d), b.min(d + m.len()));
                (e - s > 1e-6 && !m.is_gap()).then_some((s, e, m.start + s - d))
            })
            .collect();
        let (lo, hi) = (spans.first()?.0, spans.last()?.1);
        if hi - lo < 1.0 {
            return None;
        }
        // silence (past the remix's end) where it has nothing
        let silence = len + 1.0;
        let mut pieces = Vec::new();
        let mut at = lo;
        for (s, e, from) in spans {
            if s > at + 1e-6 {
                pieces.push(Piece { start: silence, end: silence + s - at });
            }
            pieces.push(Piece { start: from, end: from + e - s });
            at = e;
        }
        let (l, r) = splice(&Source { l: &src.l, r: &src.r, offset: 0, fill: None }, &frames(&pieces, sr), (SPLICE_XF_S * sr as f64) as usize);
        Some((lo, hi, Stem::new(l, r)))
    }

    /// A take that makes the song longer has landed: the song now runs to its
    /// end, with an outro section there and the beat going on.
    pub(crate) fn grow_for(&mut self, take: &Take, audio: &Stem) {
        if take.engine != EXTEND || take.end <= self.project.duration + 0.5 {
            return;
        }
        let (from, to) = (self.project.duration, take.end);
        self.extend_source(take, audio);
        let grid = poing_model::BeatGrid::of(&self.project);
        let per_bar = grid.per_bar.max(1) as f64;
        if let Some(a) = self.project.analysis.as_mut() {
            // the grid goes on into the new part, so its bars can be cut too
            let first = grid.beat_at(from).ceil();
            let beats: Vec<f64> = (0..).map(|k| first + k as f64).map(|b| (b, grid.time_of(b))).take_while(|(_, t)| *t < to).map(|(_, t)| t).collect();
            let downs: Vec<f64> = (0..).map(|k| first + k as f64).take_while(|b| grid.time_of(*b) < to).filter(|b| ((b - grid.bar_offset) / per_bar).fract().abs() < 1e-6).map(|b| grid.time_of(b)).collect();
            a.beats.get_or_insert_with(Vec::new).extend(beats);
            a.downbeats.get_or_insert_with(Vec::new).extend(downs);
        }
        // an end made earlier hands over to this one (whose first second is it)
        for t in self.project.takes_mut().iter_mut().filter(|t| t.engine == EXTEND && t.end > take.start) {
            t.end = take.start + audio::TAKE_RAMP_S;
        }
        match self.project.sections_mut().last_mut() {
            Some(last) if last.label == "outro" => last.end = to,
            last => {
                let start = last.map(|s| s.end).unwrap_or(0.0).min(from);
                self.project.sections_mut().push(poing_model::Section { start, end: to, label: "outro".into() });
            }
        }
        self.project.duration = to;
        self.pps = 0.0;
    }

    /// The source (what repaints, remixes and the next extension read) goes
    /// on with the new end, as a new file; so does the Original lane playing it.
    fn extend_source(&mut self, take: &Take, audio: &Stem) {
        let Some(src) = self.project.source.clone() else { return };
        let (dir, sr) = (self.dir(), self.sr());
        let lane = self.project.tracks.iter().position(|t| t.file == src);
        let old = match lane {
            Some(i) => self.stems[i].clone(),
            None => match audio::load_stem(&dir.join(&src), sr) {
                Ok(s) => Arc::new(s),
                Err(_) => return,
            },
        };
        // the take's first second is the song itself: the new end follows on from it
        let at = ((take.start.max(0.0) * sr as f64) as usize).min(old.l.len());
        let (mut l, mut r) = (old.l[..at].to_vec(), old.r[..at].to_vec());
        l.extend_from_slice(&audio.l);
        r.extend_from_slice(&audio.r);
        let mut n = self.project.arranged.unwrap_or(0) + 1;
        while dir.join(arranged_file(&src, n)).exists() {
            n += 1;
        }
        let file = arranged_file(&src, n);
        if audio::write_wav(&dir.join(&file), &l, &r, sr, 1.0).is_err() {
            return;
        }
        self.project.arranged = Some(n);
        self.project.source = Some(file.clone());
        if let Some(i) = lane {
            self.project.tracks[i].file = file;
            let mut lanes: Vec<Arc<Stem>> = self.stems.iter().cloned().collect();
            lanes[i] = Arc::new(Stem::new(l, r));
            self.publish_stems(Arc::new(lanes));
        }
    }

    /// A song made longer plays its new end again when it is reopened.
    pub fn restore_extensions(&mut self) {
        let ids: Vec<String> = self.project.takes().iter().filter(|t| t.engine == EXTEND).map(|t| t.id.clone()).collect();
        for id in ids {
            if !self.take_on(&id) {
                self.toggle_take(&id);
            }
        }
    }

    /// Correct a lyric line's words (the transcription mishears).
    pub fn set_lyric(&mut self, i: usize, text: &str) {
        if let Some(l) = self.project.analysis.as_mut().and_then(|a| a.lyrics.as_mut()).and_then(|v| v.get_mut(i)) {
            l.text = text.to_string();
            self.dirty = true;
        }
    }

    /// Correct a chord's name.
    pub fn set_chord(&mut self, i: usize, label: &str) {
        if let Some(c) = self.project.analysis.as_mut().and_then(|a| a.chords.as_mut()).and_then(|v| v.get_mut(i)) {
            c.label = label.trim().to_string();
            self.dirty = true;
        }
    }

    /// The words as edited go to `lyrics.txt`, which covers and repaints sing.
    pub fn write_lyrics(&self) {
        if self.project.analysis.as_ref().and_then(|a| a.lyrics.as_ref()).is_some_and(|l| !l.is_empty()) {
            let _ = std::fs::write(self.dir().join("lyrics.txt"), self.project.sung_lyrics());
        }
    }

    /// Merge a section into the one before it (or the next one, for the first).
    pub fn merge_section(&mut self, i: usize) -> Option<String> {
        if self.project.sections().len() < 2 || i >= self.project.sections().len() {
            return None;
        }
        self.checkpoint();
        let secs = self.project.sections_mut();
        let removed = secs.remove(i);
        let into = if i > 0 {
            secs[i - 1].end = removed.end;
            secs[i - 1].label.clone()
        } else {
            secs[0].start = removed.start;
            secs[0].label.clone()
        };
        self.dirty = true;
        self.select(Selection::None);
        Some(format!("Merged the {} into the {into}  ·  Ctrl+Z to undo", removed.label))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, phase: f64, frames: usize, sr: u32) -> Vec<f32> {
        (0..frames).map(|i| (0.8 * (2.0 * std::f64::consts::PI * freq * i as f64 / sr as f64 + phase).sin()) as f32).collect()
    }

    #[test]
    fn a_splice_has_no_click_at_its_cuts() {
        let sr = 48000;
        // a low tone cut where its phase jumps: a hard cut steps by ~1.1
        let x = sine(55.0, 0.0, sr as usize * 4, sr);
        let pieces = [(0, 48000 + 2000), (24000 + 300, 96000)];
        let src = Source { l: &x, r: &x, offset: 0, fill: None };
        let hard = splice(&src, &pieces, 0);
        let soft = splice(&src, &pieces, (SPLICE_XF_S * sr as f64) as usize);
        let cut = pieces[0].1;
        let typical = max_step(&soft.0, 1000, 40000);
        let hard_step = max_step(&hard.0, cut - 2, cut + 2);
        let soft_step = max_step(&soft.0, cut - 2000, cut + 2000);
        assert!(hard_step > 20.0 * typical, "the test cut is a real jump ({hard_step} vs {typical})");
        assert!(soft_step < 1.5 * typical, "crossfaded it steps no more than the tone itself ({soft_step} vs {typical})");
        assert_eq!(soft.0.len(), 50000 + 71700);
        // away from the cut the audio is the pieces, sample for sample
        assert_eq!(&soft.0[..cut - 400], &x[..cut - 400]);
        assert_eq!(&soft.0[cut + 400..cut + 1000], &x[24300 + 400..24300 + 1000]);
    }

    #[test]
    fn pieces_that_follow_each_other_are_not_crossfaded() {
        let x: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let src = Source { l: &x, r: &x, offset: 0, fill: None };
        let (l, _) = splice(&src, &[(0, 500), (500, 1000)], 100);
        assert_eq!(l, x);
    }

    #[test]
    fn a_take_is_spliced_in_song_time_with_the_lane_in_its_gaps() {
        // a take over frames 100..200 of a 400-frame lane, as a lane take
        let lane = Stem::new(vec![1.0; 400], vec![1.0; 400]);
        let take = vec![2.0f32; 100];
        let src = Source { l: &take, r: &take, offset: 100, fill: Some(&lane) };
        // frames 150..250 then 0..50
        let (l, _) = splice(&src, &[(150, 250), (0, 50)], 0);
        assert_eq!(&l[..50], &[2.0; 50]);
        assert_eq!(&l[50..100], &[1.0; 50], "past the take: the lane");
        assert_eq!(&l[100..], &[1.0; 50]);
        // a layer has silence there
        let src = Source { l: &take, r: &take, offset: 100, fill: None };
        let (l, _) = splice(&src, &[(150, 250)], 0);
        assert_eq!(&l[50..], &[0.0; 50]);
    }

    /// A 4-section song (8 s each at 1 kHz): two lanes whose samples say the
    /// time they were at, a section take over the chorus, a lane take on the
    /// whole of lane b, a remix.
    fn song(dir: &Path) -> EditorState {
        use crate::model::{Section, Track};
        let sr = 1000u32;
        let mut p = Project::import("t", "a.wav", 32.0, sr);
        let track = |id: &str| Track { id: id.into(), name: id.into(), file: format!("stems/{id}.wav"), gain_db: None, mute: None, solo: None, fx: None, notes: None, instrument: None };
        p.tracks = vec![track("a"), track("b")];
        p.source = Some("stems/a.wav".into());
        *p.sections_mut() = ["intro", "verse", "chorus", "outro"].iter().enumerate().map(|(i, l)| Section { start: i as f64 * 8.0, end: i as f64 * 8.0 + 8.0, label: l.to_string() }).collect();
        let take = |id: &str, scope: &str, start: f64, end: f64, track: Option<&str>, engine: &str| Take {
            id: id.into(),
            scope: scope.into(),
            start,
            end,
            engine: engine.into(),
            prompt: id.into(),
            seed: 1,
            file: format!("takes/{id}.wav"),
            track: track.map(String::from),
            blend: None,
        };
        *p.takes_mut() = vec![take("t1", "section", 15.75, 24.25, None, "copy"), take("t2", "lane", 0.0, 32.0, Some("b"), "dereverb"), take("t3", "song", 0.0, 32.0, None, "acestep-cover")];
        p.takes_made = Some(3);
        let ramp = |len: f64, k: f32| Stem::new((0..(len * sr as f64) as usize).map(|f| k + f as f32 / sr as f32 / 1000.0).collect(), vec![0.0; (len * sr as f64) as usize]);
        let stems = vec![ramp(32.0, 0.0), ramp(32.0, 0.1), ramp(8.5, 0.2), ramp(32.0, 0.3), ramp(32.0, 0.4)];
        std::fs::create_dir_all(dir.join("stems")).unwrap();
        std::fs::create_dir_all(dir.join("takes")).unwrap();
        let path = dir.join("project.json");
        p.save(&path).unwrap();
        EditorState::new(p, path, stems, None)
    }

    #[test]
    fn a_repeated_chorus_takes_its_take_along_and_undo_brings_it_all_back() {
        let dir = std::env::temp_dir().join(format!("poing-arrange-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut st = song(&dir);
        st.toggle_take("t1");
        let said = st.arrange(&Op::Duplicate { section: 2, to: 3 }).unwrap();
        assert!(said.starts_with("Repeated the chorus"), "{said}");
        let p = &st.project;
        assert_eq!(p.duration, 40.0);
        assert_eq!(p.tracks[0].file, "stems/a.a1.wav");
        assert_eq!(p.source.as_deref(), Some("stems/a.a1.wav"), "repaints read the arranged song");
        assert!(dir.join("stems/a.a1.wav").exists() && dir.join("stems/b.a1.wav").exists());
        // the lanes: the chorus twice, then the outro
        let a = &st.stems[0];
        assert_eq!(a.l.len(), 40_000);
        let at = |s: &Stem, t: f64| s.l[(t * 1000.0) as usize];
        assert!((at(a, 20.0) - 0.020).abs() < 1e-4 && (at(a, 28.0) - 0.020).abs() < 1e-4 && (at(a, 36.0) - 0.028).abs() < 1e-4);
        // the chorus's take covers both choruses, cut like the lanes, and still plays
        let t1 = &p.takes()[0];
        assert_eq!((t1.id.as_str(), t1.start, t1.end), ("t1", 15.75, 32.25));
        assert!(st.take_on("t1"));
        assert_eq!(st.stems[2].l.len(), 16_500);
        assert!((at(&st.stems[2], 12.0) - 0.204).abs() < 1e-4, "8 s into the take is the chorus again");
        // the lane take follows lane b over the whole song
        let t2 = &p.takes()[1];
        assert_eq!((t2.start, t2.end, t2.file.as_str()), (0.0, 40.0, "takes/t2.a1.wav"));
        // the remix stays as it was and lines up through a map
        assert_eq!(p.takes()[2].file, "takes/t3.wav");
        let map = &p.realign.as_ref().unwrap()[0];
        assert_eq!((map.take.as_str(), map.pieces.len()), ("t3", 2));
        let (a, b, chorus) = st.remix_range(st.project.tracks.len() + 2, &st.project.takes()[2].clone(), 24.0, 32.0).unwrap();
        assert_eq!((a, b), (24.0, 32.0));
        assert!((chorus.l[100] - (0.4 + 0.0161)).abs() < 1e-4, "the copied chorus plays the remix's chorus");
        // the copied chorus at the end, through the map
        let (a, b, _) = st.remix_range(st.project.tracks.len() + 2, &st.project.takes()[2].clone(), 36.0, 40.0).unwrap();
        assert_eq!((a, b), (36.0, 40.0));
        // cut the verse: the take moves without new audio
        st.arrange(&Op::Delete { section: 1 }).unwrap();
        let t1 = &st.project.takes()[0];
        assert_eq!((t1.start, t1.end, t1.file.as_str()), (7.75, 24.25, "takes/t1.a1.wav"));
        assert_eq!(st.project.sections().iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["intro", "chorus", "chorus", "outro"]);
        // undo twice: the song as it was, from memory
        st.undo(false);
        st.undo(false);
        assert_eq!(st.project.duration, 32.0);
        assert_eq!(st.project.tracks[0].file, "stems/a.wav");
        assert_eq!(st.stems[0].l.len(), 32_000);
        assert_eq!(st.project.takes()[0].start, 15.75);
        // redo reads nothing it does not have
        st.undo(true);
        assert_eq!(st.project.duration, 40.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn old_arrangements_are_read_back_from_their_files() {
        let dir = std::env::temp_dir().join(format!("poing-arrange-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut st = song(&dir);
        // the original files, for reading back
        for (i, t) in st.project.tracks.clone().iter().enumerate() {
            audio::write_wav(&dir.join(&t.file), &st.stems[i].l, &st.stems[i].r, 1000, 1.0).unwrap();
        }
        for (j, t) in st.project.takes().clone().iter().enumerate() {
            let s = st.stems[2 + j].clone();
            audio::write_wav(&dir.join(&t.file), &s.l, &s.r, 1000, 1.0).unwrap();
        }
        for _ in 0..4 {
            st.arrange(&Op::Duplicate { section: 0, to: 1 }).unwrap();
        }
        assert!(st.undo.iter().filter(|(_, s)| s.is_none()).count() >= 1, "the oldest let go of their audio");
        for _ in 0..4 {
            assert!(st.undo(false).is_some_and(|m| m.starts_with("Undone")));
        }
        assert_eq!(st.project.duration, 32.0);
        assert_eq!(st.stems[0].l.len(), 32_000);
        assert!((st.stems[0].l[20_000] - 0.020).abs() < 1e-4, "24-bit audio read back");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_song_made_longer_twice_hands_over_and_its_source_goes_on() {
        let dir = std::env::temp_dir().join(format!("poing-arrange-grow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut st = song(&dir);
        let extend = |id: &str, start: f64, end: f64| Take {
            id: id.into(),
            scope: "section".into(),
            start,
            end,
            engine: EXTEND.into(),
            prompt: "make it longer".into(),
            seed: 1,
            file: format!("takes/{id}.wav"),
            track: None,
            blend: None,
        };
        let audio = |len: f64, k: f32| Stem::new(vec![k; (len * 1000.0) as usize], vec![k; (len * 1000.0) as usize]);
        st.project.analysis = Some(crate::model::Analysis {
            tempo: Some(120.0),
            beats: Some((0..64).map(|b| b as f64 * 0.5).collect()),
            downbeats: Some((0..16).map(|b| b as f64 * 2.0).collect()),
            ..Default::default()
        });
        st.add_take(extend("t4", 27.0, 40.0), audio(13.0, 0.5)).unwrap();
        assert_eq!(st.project.duration, 40.0);
        assert_eq!(st.project.sections().last().map(|s| (s.start, s.end, s.label.as_str())), Some((24.0, 40.0, "outro")), "its own outro goes on");
        // the source (lane a here) is the song to its new end, read by the next one
        assert_eq!(st.project.source.as_deref(), Some("stems/a.a1.wav"));
        assert_eq!(st.stems[0].l.len(), 40_000);
        assert_eq!((st.stems[0].l[26_000], st.stems[0].l[30_000]), (0.026, 0.5));
        let a = st.project.analysis.as_ref().unwrap();
        assert_eq!(a.beats.as_ref().map(|b| (b.len(), b[79])), Some((80, 39.5)), "the beat goes on");
        assert_eq!(a.downbeats.as_ref().and_then(|d| d.last().copied()), Some(38.0), "so do the bars");
        st.add_take(extend("t5", 37.0, 50.0), audio(13.0, 0.7)).unwrap();
        assert_eq!(st.project.duration, 50.0);
        assert_eq!(st.project.sections().iter().filter(|s| s.label == "outro").count(), 1, "the outro grows");
        let t4 = &st.project.takes()[st.project.take_index("t4").unwrap()];
        assert_eq!(t4.end, 37.0 + audio::TAKE_RAMP_S, "the first end hands over where the second starts");
        assert!(st.take_on("t4") && st.take_on("t5"));
        assert_eq!(st.project.source.as_deref(), Some("stems/a.a2.wav"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn arranged_files_never_repeat() {
        assert_eq!(arranged_file("stems/vocals.wav", 1), "stems/vocals.a1.wav");
        assert_eq!(arranged_file("stems/vocals.a1.wav", 2), "stems/vocals.a2.wav");
        assert_eq!(arranged_file("original.mp3", 3), "original.a3.wav");
        assert_eq!(arranged_file("takes/t4.wav", 2), "takes/t4.a2.wav");
        assert_eq!(arranged_file("stems/my.alto.wav", 2), "stems/my.alto.a2.wav");
    }
}
