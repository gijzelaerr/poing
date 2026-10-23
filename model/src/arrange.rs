//! Arranging a song: sections moved, repeated or cut. An edit is a list of
//! pieces of the song as it was, played one after another; everything timed
//! on the song (sections, regions, chords, lyrics, beats, MIDI notes, takes)
//! goes where its piece goes. The audio itself is spliced by the app.

use crate::{BeatGrid, Project, Section};
use makepad_micro_serde::*;

/// A range of the song before the edit (seconds).
#[derive(SerJson, DeJson, Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub start: f64,
    pub end: f64,
}

impl Piece {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }
}

/// A remix made before the song was rearranged: the pieces of it (in its
/// own time) that line up with the song as it is now.
#[derive(SerJson, DeJson, Clone, Debug)]
pub struct Realign {
    pub take: String,
    pub pieces: Vec<Piece>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// Section `section` goes before section `to` (the count: to the end).
    Move { section: usize, to: usize },
    /// A copy of section `section` goes before section `to`.
    Duplicate { section: usize, to: usize },
    Delete { section: usize },
}

/// What a Describe asks of the song's arrangement.
#[derive(Clone, Debug, PartialEq)]
pub enum Arrange {
    Op(Op),
    /// Make the song this many seconds longer at the end.
    Extend(f64),
}

/// An edit worked out: the pieces in their new order and the sections after it.
#[derive(Clone, Debug)]
pub struct Edit {
    pub pieces: Vec<Piece>,
    pub sections: Vec<Section>,
    /// Where each old section starts (and the last one ends), on the downbeats.
    pub cuts: Vec<f64>,
}

/// Sections are cut on their edges, moved to the nearest downbeat within half
/// a second; the song's own ends are 0 and its length.
pub fn cut_points(sections: &[Section], downbeats: &[f64], duration: f64) -> Vec<f64> {
    let n = sections.len();
    let mut cuts = Vec::with_capacity(n + 1);
    cuts.push(0.0);
    for i in 1..n {
        let t = sections[i].start;
        let snapped = downbeats
            .iter()
            .copied()
            .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
            .filter(|d| (d - t).abs() <= 0.5)
            .unwrap_or(t);
        // never closer than half a second to the cut before it
        let before = *cuts.last().unwrap();
        cuts.push(if snapped > before + 0.5 { snapped } else { t.max(before) });
    }
    cuts.push(duration.max(*cuts.last().unwrap()));
    cuts
}

/// The pieces that play the old sections in `order` (runs that follow each
/// other in the song stay one piece: no cut where nothing changed).
fn pieces_of(order: &[usize], cuts: &[f64]) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let mut prev: Option<usize> = None;
    for &k in order {
        let p = Piece { start: cuts[k], end: cuts[k + 1] };
        match (prev, out.last_mut()) {
            (Some(j), Some(last)) if j + 1 == k => last.end = p.end,
            _ => out.push(p),
        }
        prev = Some(k);
    }
    out
}

/// Work out an edit on a project's sections.
pub fn plan(p: &Project, op: &Op) -> Result<Edit, String> {
    let secs = p.sections();
    let n = secs.len();
    if n == 0 {
        return Err("the song has no sections yet".into());
    }
    let downbeats = p.analysis.as_ref().and_then(|a| a.downbeats.clone()).unwrap_or_default();
    let cuts = cut_points(secs, &downbeats, p.duration);
    let mut order: Vec<usize> = (0..n).collect();
    match *op {
        Op::Move { section, to } => {
            if section >= n || to > n {
                return Err("no such section".into());
            }
            if to == section || to == section + 1 {
                return Err(format!("the {} is already there", secs[section].label));
            }
            order.remove(section);
            order.insert(if to > section { to - 1 } else { to }, section);
        }
        Op::Duplicate { section, to } => {
            if section >= n || to > n {
                return Err("no such section".into());
            }
            order.insert(to, section);
        }
        Op::Delete { section } => {
            if section >= n {
                return Err("no such section".into());
            }
            if n == 1 {
                return Err("the last section stays: a song needs one".into());
            }
            order.remove(section);
        }
    }
    let mut sections = Vec::with_capacity(order.len());
    let mut t = 0.0;
    for &k in &order {
        let len = cuts[k + 1] - cuts[k];
        sections.push(Section { start: t, end: t + len, label: secs[k].label.clone() });
        t += len;
    }
    // the first section starts where the song's did (an analysis starts at 0.01)
    if let (Some(first), Some(&k)) = (sections.first_mut(), order.first()) {
        if k == 0 {
            first.start = secs[0].start.max(0.0);
        }
    }
    Ok(Edit { pieces: pieces_of(&order, &cuts), sections, cuts })
}

/// Where each piece starts after the edit.
pub fn placed(pieces: &[Piece]) -> Vec<f64> {
    let mut at = 0.0;
    pieces
        .iter()
        .map(|p| {
            let a = at;
            at += p.len();
            a
        })
        .collect()
}

pub fn duration(pieces: &[Piece]) -> f64 {
    pieces.iter().map(Piece::len).sum()
}

/// Every place the old song's time t is heard after the edit.
pub fn map_point(pieces: &[Piece], t: f64) -> Vec<f64> {
    pieces.iter().zip(placed(pieces)).filter(|(p, _)| t >= p.start && t < p.end).map(|(p, d)| d + t - p.start).collect()
}

/// Every place the old range [a, b] is heard after the edit, in order.
pub fn map_span(pieces: &[Piece], a: f64, b: f64) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    // the old end the last span reached
    let mut reached: Option<f64> = None;
    for (p, d) in pieces.iter().zip(placed(pieces)) {
        let (s, e) = (a.max(p.start), b.min(p.end));
        if e - s > 1e-9 {
            let span = (d + s - p.start, d + e - p.start);
            // pieces that followed each other in the old song carry a span on as one
            match out.last_mut() {
                Some(last) if (last.1 - span.0).abs() < 1e-9 && reached.is_some_and(|r| (r - s).abs() < 1e-9) => last.1 = span.1,
                _ => out.push(span),
            }
            reached = Some(e);
        } else {
            reached = None;
        }
    }
    out
}

/// Where audio over [a, b] of the old song goes: runs of pieces heard one
/// after another, each (where it starts after the edit, its pieces). Where a
/// cut trims a run while the audio goes on, the run keeps up to `reach`
/// seconds of it past the cut (a take's own fade, over its neighbour).
pub fn runs(pieces: &[Piece], a: f64, b: f64, reach: f64) -> Vec<(f64, Vec<Piece>)> {
    let mut out: Vec<(f64, Vec<Piece>)> = Vec::new();
    let mut last: Option<usize> = None;
    for (i, (p, d)) in pieces.iter().zip(placed(pieces)).enumerate() {
        let (s, e) = (a.max(p.start), b.min(p.end));
        if e - s <= 1e-9 {
            continue;
        }
        let seg = Piece { start: s, end: e };
        let joins = last.is_some_and(|j| j + 1 == i)
            && (s - p.start).abs() < 1e-9
            && out.last().and_then(|(_, r)| r.last()).is_some_and(|q| (q.end - pieces[i - 1].end).abs() < 1e-9);
        if joins {
            out.last_mut().unwrap().1.push(seg);
        } else {
            out.push((d + s - p.start, vec![seg]));
        }
        last = Some(i);
    }
    for (at, run) in out.iter_mut() {
        let first = run.first_mut().unwrap();
        let ext = reach.min(first.start - a).max(0.0);
        first.start -= ext;
        *at -= ext;
        let last = run.last_mut().unwrap();
        last.end += reach.min(b - last.end).max(0.0);
    }
    out
}

/// Where a remix has nothing (the song runs on past it): a piece in a time
/// it never has, `len` long. A gap never runs on into a real piece.
pub fn gap(len: f64) -> Piece {
    Piece { start: GAP_AT - len, end: GAP_AT }
}

const GAP_AT: f64 = -1000.0;

impl Piece {
    pub fn is_gap(&self) -> bool {
        self.end <= GAP_AT
    }
}

/// A remix's pieces (in its own time, gaps where it has nothing) that line up
/// with the song after `edit`, from the ones that lined up before (`map`,
/// covering the song before the edit).
pub fn compose(map: &[Piece], edit: &[Piece]) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let push = |out: &mut Vec<Piece>, p: Piece| match out.last_mut() {
        Some(last) if !p.is_gap() && !last.is_gap() && (last.end - p.start).abs() < 1e-9 => last.end = p.end,
        Some(last) if p.is_gap() && last.is_gap() => *last = gap(last.len() + p.len()),
        _ => out.push(p),
    };
    for e in edit {
        let mut at = e.start;
        for (m, d) in map.iter().zip(placed(map)) {
            let (s, t) = (e.start.max(d), e.end.min(d + m.len()));
            if t - s > 1e-9 {
                if s > at + 1e-9 {
                    push(&mut out, gap(s - at));
                }
                push(&mut out, Piece { start: m.start + s - d, end: m.start + t - d });
                at = t;
            }
        }
        if e.end > at + 1e-9 {
            push(&mut out, gap(e.end - at));
        }
    }
    out
}

/// Move everything timed on the song with the edit: sections, regions (one
/// cut in two gets a new id from `new_id`), chords, lyrics, beats,
/// downbeats and MIDI notes, and the song's length. Takes are the app's
/// (their audio is spliced).
pub fn apply(p: &mut Project, edit: &Edit, new_id: &mut dyn FnMut() -> String) {
    let pieces = &edit.pieces;
    let old_grid = BeatGrid::of(p);
    *p.sections_mut() = edit.sections.clone();
    p.duration = duration(pieces);
    let mut regions = Vec::new();
    for r in p.regions().clone() {
        for (k, (s, e)) in map_span(pieces, r.start, r.end).into_iter().filter(|(s, e)| e - s > 0.05).enumerate() {
            let mut r = r.clone();
            if k > 0 {
                r.id = new_id();
            }
            r.start = s;
            r.end = e;
            regions.push(r);
        }
    }
    *p.regions_mut() = regions;
    let spans = |list: &[Section]| -> Vec<Section> {
        list.iter()
            .flat_map(|c| map_span(pieces, c.start, c.end).into_iter().map(move |(s, e)| Section { start: s, end: e, label: c.label.clone() }))
            .filter(|c| c.end - c.start > 0.05)
            .collect::<Vec<_>>()
    };
    let spans = |list: &[Section]| -> Vec<Section> {
        let mut out = spans(list);
        out.sort_by(|a, b| a.start.total_cmp(&b.start));
        out
    };
    let points = |list: &[f64]| -> Vec<f64> {
        let mut out: Vec<f64> = list.iter().flat_map(|&t| map_point(pieces, t)).collect();
        out.sort_by(|a, b| a.total_cmp(b));
        // a beat each side of a cut can land together: keep one
        out.dedup_by(|b, a| *b - *a < 0.05);
        out
    };
    if let Some(a) = p.analysis.as_mut() {
        if let Some(c) = a.chords.as_mut() {
            *c = spans(c);
        }
        if let Some(s) = a.sections.as_mut() {
            *s = spans(s);
        }
        if let Some(lines) = a.lyrics.as_mut() {
            // a line goes wherever its middle is heard, whole
            let mut out = Vec::new();
            for l in lines.iter() {
                let mid = 0.5 * (l.start + l.end);
                for (pc, d) in pieces.iter().zip(placed(pieces)) {
                    if mid >= pc.start && mid < pc.end {
                        let shift = d - pc.start;
                        let (lo, hi) = (d, d + pc.len());
                        let mut l = l.clone();
                        l.start = (l.start + shift).max(lo);
                        l.end = (l.end + shift).min(hi);
                        out.push(l);
                    }
                }
            }
            out.sort_by(|a, b| a.start.total_cmp(&b.start));
            *lines = out;
        }
        if let Some(b) = a.beats.as_mut() {
            *b = points(b);
        }
        if let Some(b) = a.downbeats.as_mut() {
            *b = points(b);
        }
    }
    let new_grid = BeatGrid::of(p);
    for t in p.tracks.iter_mut() {
        let Some(notes) = t.notes.as_mut() else { continue };
        let mut out = Vec::new();
        for n in notes.iter() {
            for at in map_point(pieces, old_grid.time_of(n.start)) {
                let mut n = n.clone();
                n.start = BeatGrid::snap(new_grid.beat_at(at), 64.0);
                out.push(n);
            }
        }
        out.sort_by(|a, b| a.start.total_cmp(&b.start));
        *notes = out;
    }
}

/// The song's sections of a kind ("verse", "chorus", ...) a word names.
fn kind_of(label: &str) -> String {
    label.to_lowercase().chars().filter(|c| c.is_alphabetic()).collect()
}

/// Section kinds by the words people use for them (English and Dutch).
const KINDS: [(&str, &str); 14] = [
    ("intro", "intro"), ("verse", "verse"), ("couplet", "verse"), ("prechorus", "prechorus"), ("chorus", "chorus"),
    ("refrein", "chorus"), ("hook", "chorus"), ("bridge", "bridge"), ("brug", "bridge"), ("interlude", "interlude"),
    ("break", "break"), ("solo", "solo"), ("outro", "outro"), ("drop", "drop"),
];

/// A section reference in some words: (kind, ordinal: 1-based, -1 the last).
fn section_ref(words: &[String]) -> Option<(&'static str, Option<i32>)> {
    let found = section_refs(words);
    // "drop" is also a verb ("drop the second verse"): another section named wins
    found.iter().find(|(k, _)| *k != "drop").or(found.first()).copied()
}

fn section_refs(words: &[String]) -> Vec<(&'static str, Option<i32>)> {
    let mut found = Vec::new();
    let ordinal = |w: &str| -> Option<i32> {
        Some(match w {
            "first" | "1st" | "eerste" | "opening" => 1,
            "second" | "2nd" | "tweede" => 2,
            "third" | "3rd" | "derde" => 3,
            "fourth" | "4th" | "vierde" => 4,
            "fifth" | "5th" | "vijfde" => 5,
            "last" | "final" | "laatste" => -1,
            _ => return None,
        })
    };
    for (i, w) in words.iter().enumerate() {
        // "pre-chorus" / "pre chorus" are one word
        let joined = if w == "pre" { words.get(i + 1).map(|n| format!("pre{n}")) } else { None };
        let w = joined.as_deref().unwrap_or(w.as_str());
        let Some((_, kind)) = KINDS.iter().find(|(k, _)| w == *k || w.strip_suffix('s') == Some(k)) else { continue };
        let before = i.checked_sub(1).and_then(|j| ordinal(&words[j])).or_else(|| i.checked_sub(2).and_then(|j| ordinal(&words[j])));
        let after = words.get(i + if joined.is_some() { 2 } else { 1 }).and_then(|n| n.parse::<i32>().ok());
        found.push((*kind, before.or(after)));
    }
    found
}

/// Find a referenced section among the song's: Err says why not (none of
/// that kind, or which of several).
fn find_section(sections: &[Section], kind: &str, ordinal: Option<i32>, default_last: bool) -> Result<usize, String> {
    let mut of: Vec<usize> = (0..sections.len()).filter(|&i| kind_of(&sections[i].label).starts_with(kind)).collect();
    if of.is_empty() && kind == "bridge" {
        // songs often call it something else
        of = (0..sections.len()).filter(|&i| ["interlude", "break", "solo"].iter().any(|k| kind_of(&sections[i].label).starts_with(k))).collect();
    }
    if of.is_empty() && kind == "chorus" {
        of = (0..sections.len()).filter(|&i| kind_of(&sections[i].label).starts_with("hook")).collect();
    }
    if of.is_empty() {
        return Err(format!("the song has no {kind}"));
    }
    match ordinal {
        Some(-1) => Ok(*of.last().unwrap()),
        Some(k) if k >= 1 => of.get(k as usize - 1).copied().ok_or_else(|| format!("the song has only {} of the {kind}", of.len())),
        _ if of.len() == 1 || default_last => Ok(*of.last().unwrap()),
        _ => Err(format!("which {kind}? The song has {}: say \"the first {kind}\", \"the second {kind}\" or \"the last {kind}\"", of.len())),
    }
}

/// How many seconds "30 s longer", "a minute longer", "add 20 seconds" asks for.
fn seconds_in(words: &[String]) -> Option<f64> {
    for (i, w) in words.iter().enumerate() {
        let unit = |u: &str| -> Option<f64> {
            match u {
                "s" | "sec" | "secs" | "second" | "seconds" | "seconden" => Some(1.0),
                "m" | "min" | "mins" | "minute" | "minutes" | "minuut" | "minuten" => Some(60.0),
                "bars" | "bar" | "maten" | "maat" => Some(0.0),
                _ => None,
            }
        };
        // "30s" is one word
        let (num, rest) = w.split_at(w.find(|c: char| !c.is_ascii_digit()).unwrap_or(w.len()));
        let Ok(n) = num.parse::<f64>() else {
            if (w == "a" || w == "one" || w == "een") && words.get(i + 1).is_some_and(|u| unit(u).is_some_and(|f| f == 60.0)) {
                return Some(60.0);
            }
            continue;
        };
        let f = if rest.is_empty() { words.get(i + 1).and_then(|u| unit(u)) } else { unit(rest) };
        if let Some(f) = f.filter(|f| *f > 0.0) {
            return Some(n * f);
        }
    }
    None
}

/// Read a Describe as an arrangement edit: "repeat the chorus", "cut the
/// second verse", "move the bridge after the last chorus", "make it 30 s
/// longer", "add an outro". None: not about the arrangement. `selected`: the
/// section selected (for "repeat this", "delete it").
pub fn arrange_intent(text: &str, sections: &[Section], selected: Option<usize>) -> Option<Result<Arrange, String>> {
    let t = text.to_lowercase();
    let words: Vec<String> = t.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect();
    let has = |ws: &[&str]| ws.iter().any(|w| words.iter().any(|x| x == w));
    // about a sound or a part, not the song's order ("remove the echo in the chorus")
    if crate::is_effect_text(&t.replace("chorus", "")) || has(&["vocals", "vocal", "voice", "drums", "bass", "guitar", "piano", "keys", "synth", "lyrics", "stem", "lane", "midi"]) {
        return None;
    }
    let reference = section_ref(&words);
    let this = selected.filter(|_| has(&["this", "it", "dit", "deze", "here", "selected"]));
    let longer = has(&["longer", "extend", "lengthen", "langer", "verleng", "verlengen"]);
    let add = has(&["add", "voeg", "append", "new"]);
    // the song longer at its end
    if (longer && reference.is_none() && this.is_none()) || (add && reference.is_some_and(|(k, _)| k == "outro") && !sections.iter().any(|s| kind_of(&s.label).starts_with("outro"))) {
        return Some(Ok(Arrange::Extend(seconds_in(&words).unwrap_or(30.0).clamp(5.0, 120.0))));
    }
    let dup = longer || has(&["repeat", "duplicate", "double", "again", "copy", "herhaal", "herhalen", "nogmaals", "twice"]) || (add && has(&["another", "extra", "more", "nog"]));
    let del = has(&["cut", "delete", "remove", "drop", "skip", "lose", "weg", "verwijder", "schrap", "without"]);
    let mov = has(&["move", "put", "place", "verplaats", "zet"]);
    if !(dup || del || mov) {
        return None;
    }
    // the subject before "after"/"before", the target after it
    let split = words.iter().position(|w| matches!(w.as_str(), "after" | "before" | "na" | "voor" | "behind" | "achter"));
    let (subject, target) = match split {
        Some(i) => (&words[..i], Some((&words[i + 1..], matches!(words[i].as_str(), "after" | "na" | "behind" | "achter")))),
        None => (&words[..], None),
    };
    let subject_ref = section_ref(subject);
    let section = match (subject_ref, this) {
        (Some((kind, ord)), _) => {
            // "repeat the chorus": the one selected if it is one, else the last
            let sel_is = selected.filter(|&i| ord.is_none() && sections.get(i).is_some_and(|s| kind_of(&s.label).starts_with(kind)));
            match sel_is {
                Some(i) => i,
                None => match find_section(sections, kind, ord, dup && !mov) {
                    Ok(i) => i,
                    Err(e) => return Some(Err(e)),
                },
            }
        }
        (None, Some(i)) => i,
        (None, None) if selected.is_some() && reference.is_none() => selected.unwrap(),
        _ => return None,
    };
    let n = sections.len();
    let end = has(&["end", "eind", "einde"]) && target.is_none();
    let start = has(&["start", "beginning", "begin", "front"]) && target.is_none();
    let to = match target {
        Some((words, after)) => match section_ref(words) {
            Some((kind, ord)) => match find_section(sections, kind, ord, true) {
                Ok(i) => Some(if after { i + 1 } else { i }),
                Err(e) => return Some(Err(e)),
            },
            None if words.iter().any(|w| w == "this" || w == "it") && selected.is_some() => selected.map(|i| if after { i + 1 } else { i }),
            None => None,
        },
        None if end && (mov || dup) => Some(n),
        None if start && (mov || dup) => Some(0),
        None => None,
    };
    let op = if mov {
        match to {
            Some(to) => Op::Move { section, to },
            None => return Some(Err(format!("move the {} where? Say \"after the ...\" or \"before the ...\"", sections[section].label))),
        }
    } else if dup {
        Op::Duplicate { section, to: to.unwrap_or(section + 1) }
    } else {
        Op::Delete { section }
    };
    Some(Ok(Arrange::Op(op)))
}

/// Says what an edit does ("Repeated the chorus", ...), for the status line.
pub fn says(op: &Op, sections: &[Section]) -> String {
    let name = |i: usize| sections.get(i).map(|s| s.label.clone()).unwrap_or_default();
    let place = |to: usize| match to {
        0 => "at the start".to_string(),
        t if t >= sections.len() => "at the end".to_string(),
        t => format!("before the {}", name(t)),
    };
    match *op {
        Op::Move { section, to } if to == 0 || to >= sections.len() => format!("Moved the {} to the {}", name(section), if to == 0 { "start" } else { "end" }),
        Op::Move { section, to } => format!("Moved the {} {}", name(section), place(to)),
        Op::Duplicate { section, to } if to == section + 1 => format!("Repeated the {}", name(section)),
        Op::Duplicate { section, to } => format!("Repeated the {} {}", name(section), place(to)),
        Op::Delete { section } => format!("Cut the {}", name(section)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Analysis, LyricLine, Note, Region, Track};

    fn sec(start: f64, end: f64, label: &str) -> Section {
        Section { start, end, label: label.into() }
    }

    /// A 40 s song: intro, verse, chorus, verse, chorus (8 s each), beats every
    /// half second, a bar every 2 s.
    fn song() -> Project {
        let mut p = Project::import("t", "original.wav", 40.0, 48000);
        *p.sections_mut() = vec![sec(0.01, 8.0, "intro"), sec(8.0, 16.0, "verse"), sec(16.0, 24.0, "chorus"), sec(24.0, 32.0, "verse"), sec(32.0, 40.0, "chorus")];
        p.analysis = Some(Analysis {
            tempo: Some(120.0),
            beats: Some((0..80).map(|i| i as f64 * 0.5).collect()),
            downbeats: Some((0..20).map(|i| i as f64 * 2.0).collect()),
            chords: Some(vec![sec(14.0, 18.0, "Am"), sec(18.0, 20.0, "F")]),
            lyrics: Some(vec![
                LyricLine { start: 9.0, end: 12.0, text: "first verse".into() },
                LyricLine { start: 17.0, end: 20.0, text: "chorus line".into() },
                LyricLine { start: 23.5, end: 25.0, text: "over the cut".into() },
            ]),
            ..Default::default()
        });
        p.regions_mut().push(Region { id: "r1".into(), track: "original".into(), start: 20.0, end: 28.0, kind: "mute".into(), gain_db: None, note: None });
        p.tracks.push(Track {
            id: "midi1".into(),
            name: "Keys".into(),
            file: String::new(),
            gain_db: None,
            mute: None,
            solo: None,
            fx: None,
            // beat 32 = 16 s (the chorus), beat 40 = 20 s
            notes: Some(vec![Note { start: 32.0, len: 2.0, pitch: 60, vel: 0.8 }, Note { start: 40.0, len: 1.0, pitch: 64, vel: 0.8 }]),
            instrument: None,
        });
        p
    }

    fn ids() -> impl FnMut() -> String {
        let mut n = 1;
        move || {
            n += 1;
            format!("r{n}")
        }
    }

    #[test]
    fn sections_are_cut_on_downbeats() {
        let secs = [sec(0.0, 7.8, "a"), sec(7.8, 16.2, "b"), sec(16.2, 20.0, "c")];
        assert_eq!(cut_points(&secs, &[0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 16.0], 20.0), vec![0.0, 8.0, 16.0, 20.0]);
        assert_eq!(cut_points(&secs, &[0.0, 9.0], 20.0), vec![0.0, 7.8, 16.2, 20.0], "too far from a downbeat: stays");
    }

    #[test]
    fn a_duplicate_plays_the_section_twice() {
        let p = song();
        let e = plan(&p, &Op::Duplicate { section: 2, to: 3 }).unwrap();
        assert_eq!(e.pieces, vec![Piece { start: 0.0, end: 24.0 }, Piece { start: 16.0, end: 40.0 }]);
        let labels: Vec<&str> = e.sections.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["intro", "verse", "chorus", "chorus", "verse", "chorus"]);
        assert_eq!((e.sections[3].start, e.sections[3].end), (24.0, 32.0));
        assert_eq!(e.sections[0].start, 0.01, "the song's first section keeps its start");
        assert_eq!(duration(&e.pieces), 48.0);
    }

    #[test]
    fn move_and_delete_reorder_the_pieces() {
        let p = song();
        let e = plan(&p, &Op::Move { section: 1, to: 5 }).unwrap();
        assert_eq!(e.pieces, vec![Piece { start: 0.0, end: 8.0 }, Piece { start: 16.0, end: 40.0 }, Piece { start: 8.0, end: 16.0 }]);
        let e = plan(&p, &Op::Delete { section: 3 }).unwrap();
        assert_eq!(e.pieces, vec![Piece { start: 0.0, end: 24.0 }, Piece { start: 32.0, end: 40.0 }]);
        assert_eq!(e.sections.len(), 4);
        assert!(plan(&p, &Op::Move { section: 1, to: 2 }).is_err(), "already there");
    }

    #[test]
    fn times_map_through_the_pieces() {
        let pieces = [Piece { start: 0.0, end: 24.0 }, Piece { start: 16.0, end: 40.0 }];
        assert_eq!(map_point(&pieces, 10.0), vec![10.0]);
        assert_eq!(map_point(&pieces, 18.0), vec![18.0, 26.0], "a duplicated time is heard twice");
        assert_eq!(map_span(&pieces, 20.0, 28.0), vec![(20.0, 24.0), (28.0, 36.0)]);
        assert_eq!(map_span(&pieces, 0.0, 40.0), vec![(0.0, 24.0), (24.0, 48.0)]);
        let cut = [Piece { start: 0.0, end: 8.0 }, Piece { start: 16.0, end: 40.0 }];
        assert!(map_point(&cut, 10.0).is_empty(), "a deleted time is gone");
        // pieces that follow each other in the old song carry a span on as one
        let split = [Piece { start: 0.0, end: 8.0 }, Piece { start: 8.0, end: 16.0 }];
        assert_eq!(map_span(&split, 4.0, 12.0), vec![(4.0, 12.0)]);
    }

    #[test]
    fn take_audio_runs_follow_the_cuts() {
        // a section take over the chorus with a quarter second of margin each side
        let pieces = [Piece { start: 0.0, end: 24.0 }, Piece { start: 16.0, end: 40.0 }];
        let r = runs(&pieces, 15.75, 24.25, 0.25);
        assert_eq!(r, vec![(15.75, vec![Piece { start: 15.75, end: 24.0 }, Piece { start: 16.0, end: 24.25 }])], "the chorus twice in a row: one run, cut like the lanes");
        // the chorus copied to the end: the take twice, keeping its margin past each cut
        let to_end = [Piece { start: 0.0, end: 40.0 }, Piece { start: 16.0, end: 24.0 }];
        let r = runs(&to_end, 15.75, 24.25, 0.25);
        assert_eq!(r, vec![(15.75, vec![Piece { start: 15.75, end: 24.25 }]), (39.75, vec![Piece { start: 15.75, end: 24.25 }])]);
        // a lane take over the whole song: one run through every piece
        let r = runs(&pieces, 0.0, 40.0, 0.0);
        assert_eq!(r, vec![(0.0, pieces.to_vec())]);
        // moved: the verse goes to the end, a take over verse and chorus splits
        let moved = [Piece { start: 0.0, end: 8.0 }, Piece { start: 16.0, end: 40.0 }, Piece { start: 8.0, end: 16.0 }];
        let r = runs(&moved, 10.0, 20.0, 0.0);
        assert_eq!(r, vec![(8.0, vec![Piece { start: 16.0, end: 20.0 }]), (34.0, vec![Piece { start: 10.0, end: 16.0 }])]);
    }

    #[test]
    fn remixes_follow_the_song_through_every_edit() {
        let first = [Piece { start: 0.0, end: 24.0 }, Piece { start: 16.0, end: 40.0 }];
        let m = compose(&[Piece { start: 0.0, end: 40.0 }], &first);
        assert_eq!(m, first.to_vec());
        // then the first 8 s of the new song are cut
        let m = compose(&m, &[Piece { start: 8.0, end: 48.0 }]);
        assert_eq!(m, vec![Piece { start: 8.0, end: 24.0 }, Piece { start: 16.0, end: 40.0 }]);
        // a remix a little shorter than the song: the song keeps its time, with a gap
        let short = [Piece { start: 0.0, end: 39.9 }, gap(0.1)];
        let m = compose(&short, &[Piece { start: 0.0, end: 40.0 }, Piece { start: 30.0, end: 50.0 }]);
        assert_eq!(m.len(), 4);
        assert_eq!((m[0], m[2]), (Piece { start: 0.0, end: 39.9 }, Piece { start: 30.0, end: 39.9 }));
        assert!(m[1].is_gap() && (m[1].len() - 0.1).abs() < 1e-9);
        assert!(m[3].is_gap() && (m[3].len() - 10.1).abs() < 1e-9, "past its end and the song's old end: nothing");
        assert!((duration(&m) - 60.0).abs() < 1e-9, "every second of the song is in the map");
    }

    #[test]
    fn everything_timed_moves_with_a_duplicate() {
        let mut p = song();
        let e = plan(&p, &Op::Duplicate { section: 2, to: 3 }).unwrap();
        apply(&mut p, &e, &mut ids());
        assert_eq!(p.duration, 48.0);
        let a = p.analysis.as_ref().unwrap();
        // the chord over the cut is cut with it, and the chorus's chords come twice
        let chords: Vec<(f64, f64, &str)> = a.chords.as_ref().unwrap().iter().map(|c| (c.start, c.end, c.label.as_str())).collect();
        assert_eq!(chords, [(14.0, 18.0, "Am"), (18.0, 20.0, "F"), (24.0, 26.0, "Am"), (26.0, 28.0, "F")]);
        let lyrics: Vec<(f64, f64, &str)> = a.lyrics.as_ref().unwrap().iter().map(|l| (l.start, l.end, l.text.as_str())).collect();
        assert_eq!(lyrics, [(9.0, 12.0, "first verse"), (17.0, 20.0, "chorus line"), (25.0, 28.0, "chorus line"), (31.5, 33.0, "over the cut")], "a line goes where its middle goes");
        let beats = a.beats.as_ref().unwrap();
        assert_eq!(beats.len(), 96, "80 beats and the chorus's 16 again");
        assert!(beats.windows(2).all(|w| (w[1] - w[0] - 0.5).abs() < 1e-9), "still every half second");
        assert_eq!(a.downbeats.as_ref().unwrap().len(), 24);
        // the region over the chorus's end and the verse: in two, the copy with a new id
        let r: Vec<(&str, f64, f64)> = p.regions().iter().map(|r| (r.id.as_str(), r.start, r.end)).collect();
        assert_eq!(r, [("r1", 20.0, 24.0), ("r2", 28.0, 36.0)]);
        // the notes in the chorus are played in both
        let notes: Vec<f64> = p.tracks[1].notes.as_ref().unwrap().iter().map(|n| n.start).collect();
        assert_eq!(notes, [32.0, 40.0, 48.0, 56.0]);
    }

    #[test]
    fn a_cut_section_takes_its_notes_and_lines_along() {
        let mut p = song();
        let e = plan(&p, &Op::Delete { section: 1 }).unwrap();
        apply(&mut p, &e, &mut ids());
        assert_eq!(p.duration, 32.0);
        let a = p.analysis.as_ref().unwrap();
        assert_eq!(a.lyrics.as_ref().unwrap()[0].text, "chorus line", "the verse's words went with it");
        assert_eq!(a.lyrics.as_ref().unwrap()[0].start, 9.0);
        let notes: Vec<f64> = p.tracks[1].notes.as_ref().unwrap().iter().map(|n| n.start).collect();
        assert_eq!(notes, [16.0, 24.0], "the chorus's notes moved up 8 s (16 beats)");
        assert_eq!(p.sections()[1].label, "chorus");
        assert_eq!(p.sections()[1].start, 8.0);
    }

    fn labels(list: &[&str]) -> Vec<Section> {
        list.iter().enumerate().map(|(i, l)| sec(i as f64 * 10.0, i as f64 * 10.0 + 10.0, l)).collect()
    }

    #[test]
    fn describe_reads_arrangement_edits() {
        // It's Autumn's sections
        let s = labels(&["intro", "verse", "chorus", "verse", "chorus", "interlude", "chorus"]);
        let read = |t: &str| arrange_intent(t, &s, None);
        assert_eq!(read("repeat the chorus"), Some(Ok(Arrange::Op(Op::Duplicate { section: 6, to: 7 }))), "the last chorus, again after itself");
        assert_eq!(read("cut the second verse"), Some(Ok(Arrange::Op(Op::Delete { section: 3 }))));
        assert_eq!(read("move the bridge after the last chorus"), Some(Ok(Arrange::Op(Op::Move { section: 5, to: 7 }))), "no bridge: the interlude");
        assert_eq!(read("move the first chorus before the intro"), Some(Ok(Arrange::Op(Op::Move { section: 2, to: 0 }))));
        assert_eq!(read("repeat the first chorus at the end"), Some(Ok(Arrange::Op(Op::Duplicate { section: 2, to: 7 }))));
        assert_eq!(read("make it 30 s longer"), Some(Ok(Arrange::Extend(30.0))));
        assert_eq!(read("make the song a minute longer"), Some(Ok(Arrange::Extend(60.0))));
        assert_eq!(read("add an outro"), Some(Ok(Arrange::Extend(30.0))));
        assert_eq!(read("make the chorus longer"), Some(Ok(Arrange::Op(Op::Duplicate { section: 6, to: 7 }))));
        assert!(matches!(read("delete the verse"), Some(Err(e)) if e.contains("which verse")), "two verses: ask which");
        assert!(matches!(read("cut the drop"), Some(Err(e)) if e.contains("no drop")));
        assert_eq!(read("remove the echo in the chorus"), None, "an effect, not the arrangement");
        assert_eq!(read("cut the vocals in the second verse"), None, "a lane");
        assert_eq!(read("1960s beat pop, warm tape"), None);
        assert_eq!(read("knip het tweede couplet weg"), Some(Ok(Arrange::Op(Op::Delete { section: 3 }))));
        // a section selected: "this"
        assert_eq!(arrange_intent("repeat this", &s, Some(1)), Some(Ok(Arrange::Op(Op::Duplicate { section: 1, to: 2 }))));
        assert_eq!(arrange_intent("delete it", &s, Some(5)), Some(Ok(Arrange::Op(Op::Delete { section: 5 }))));
        assert_eq!(arrange_intent("repeat the chorus", &s, Some(2)), Some(Ok(Arrange::Op(Op::Duplicate { section: 2, to: 3 }))), "the selected chorus");
    }
}
