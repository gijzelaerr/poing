//! The arrangement view: ruler, sections, chords, takes, one lane per stem
//! (header with M/S and effect chips, waveform, regions), the master strip
//! and the playhead. Immediate-mode shapes and text; all mouse and keyboard
//! editing of the arrangement happens here.

use crate::audio::{MASTER_METER, METER_LANES, PEAK_BLOCK};
use crate::model::REGION_KINDS;
use crate::state::{EditorState, Selection};
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.DrawShapeBase = #(DrawShape::script_component(vm))
    set_type_default() do #(DrawShape::script_shader(vm)){
        ..mod.draw.DrawQuad
    }

    mod.widgets.DawTimelineBase = #(DawTimeline::register_widget(vm))

    mod.widgets.DawTimeline = set_type_default() do mod.widgets.DawTimelineBase{
        width: Fill
        height: Fill
        draw_bg +: {color: #x0f1013}
        // A rounded box with a vertical gradient and an optional border.
        draw_shape +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                // `sdf.box` draws twice the radius it is given
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, self.radius * 0.5)
                sdf.fill_keep(mix(self.color, self.color2, self.pos.y))
                if self.border_size > 0.0 {
                    sdf.stroke(self.border_color, self.border_size)
                }
                return sdf.result
            }
        }
        draw_text +: {
            color: #xeceef2
            text_style: theme.font_regular{font_size: 8.5}
        }
        draw_bold +: {
            color: #xeceef2
            text_style: theme.font_bold{font_size: 9.5}
        }
        draw_clock +: {
            color: #xeceef2
            text_style: theme.font_bold{font_size: 13}
        }
    }
}

/// A rounded, shaded shape (see the pixel shader above).
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawShape {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    color2: Vec4f,
    #[live]
    border_color: Vec4f,
    #[live]
    radius: f32,
    #[live]
    border_size: f32,
}

const HEADER_W: f64 = 196.0;
const RULER_H: f64 = 30.0;
const SECT_H: f64 = 28.0;
const CHORD_H: f64 = 20.0;
const TAKES_H: f64 = 30.0;
const MASTER_H: f64 = 34.0;
const GAP: f64 = 8.0;
const LANE_GAP: f64 = 3.0;
const EDGE_PX: f64 = 6.0;

const ACCENT: u32 = 0x0a84ff;
const TEXT: u32 = 0xeceef2;
const TEXT_2: u32 = 0x8d939e;
const CARD: u32 = 0x16181d;
const HEADER: u32 = 0x1b1e24;
const HAIRLINE: u32 = 0x262a31;

#[derive(Clone, Copy, Default)]
struct Geo {
    rect: Rect,
    x0: f64,
    w: f64,
    sect_y: f64,
    chord_y: f64,
    takes_y: f64,
    lanes_y: f64,
    lane_h: f64,
    n: usize,
    master_y: f64,
}

#[derive(Clone, Copy, Default, Debug)]
enum Drag {
    #[default]
    None,
    Seek,
    Pending { track: usize, t0: f64, x0: f64 },
    NewRegion { region: usize, anchor: f64 },
    MoveRegion { region: usize, grab: f64 },
    ResizeRegion { region: usize, left: bool },
    SectionEdge { index: usize },
    /// Moving a note: where in it it was grabbed (beats).
    NoteMove { lane: usize, idx: usize, grab: f64 },
    /// Changing a note's length by its right edge.
    NoteSize { lane: usize, idx: usize },
    /// Dragging a lane's dB value sideways sets its level.
    Gain { lane: usize, x0: f64, g0: f64 },
}

#[derive(Script, ScriptHook, Widget)]
pub struct DawTimeline {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_rect: DrawColor,
    #[live]
    draw_shape: DrawShape,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_bold: DrawText,
    #[live]
    draw_clock: DrawText,
    #[rust]
    area: Area,
    #[rust]
    geo: Geo,
    #[rust]
    drag: Drag,
    #[rust]
    next_frame: NextFrame,
    /// Take chips as drawn: (rect, take id; None is the original).
    #[rust]
    chips: Vec<(Rect, Option<String>)>,
    /// Where each lane starts (and the last one ends): lanes differ in height.
    #[rust]
    tops: Vec<f64>,
    /// MIDI lanes' pitch ranges as drawn: (lane, lowest, highest).
    #[rust]
    midi_view: Vec<(usize, u8, u8)>,
    /// Effect chips as drawn: (rect, lane or master, index in its chain).
    #[rust]
    fx_chips: Vec<(Rect, Option<usize>, usize)>,
}

fn rgba(hex: u32, a: f32) -> Vec4f {
    vec4(((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0, a)
}

/// The same colour a little darker (for gradients).
fn shade(c: Vec4f, f: f32) -> Vec4f {
    vec4(c.x * f, c.y * f, c.z * f, c.w)
}

/// A lane's colour, by what it is (a kept layer by the instrument it names).
fn track_color(id: &str, name: &str, i: usize) -> u32 {
    const OTHERS: [u32; 6] = [0xff375f, 0x5e5ce6, 0xffd60a, 0x64d2ff, 0xbf5af2, 0x30d158];
    let kind = name.to_lowercase();
    let id = if id.starts_with("layer-") {
        ["drums", "bass", "vocals", "other"].into_iter().find(|k| kind.starts_with(k)).unwrap_or(id)
    } else {
        id
    };
    match id {
        "original" => 0x8e8e93,
        "vocals" => 0xbf5af2,
        "drums" => 0x64d2ff,
        "bass" => 0x30d158,
        "other" => 0xff9f0a,
        _ => OTHERS[i % OTHERS.len()],
    }
}

fn region_color(kind: &str) -> u32 {
    match kind {
        "bad" => 0xff453a,
        "regenerate" => 0xff9f0a,
        "too_loud" => 0xff375f,
        "too_quiet" => 0x64d2ff,
        "mute" => 0x8e8e93,
        _ => 0x30d158,
    }
}

fn section_color(label: &str) -> u32 {
    let l = label.to_lowercase();
    if l.starts_with("chorus") {
        0xb4602c
    } else if l.starts_with("verse") {
        0x23706b
    } else if l.starts_with("bridge") {
        0x6a4fa0
    } else if l.starts_with("inst") || l.starts_with("drop") || l.starts_with("break") || l.starts_with("interlude") {
        0x3f7a34
    } else {
        0x3a404c
    }
}

fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u32, t % 60.0)
}

/// At most `n` characters, with an ellipsis when cut.
fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}

/// Region kind index for the inspector dropdown.
pub fn kind_index(kind: &str) -> usize {
    REGION_KINDS.iter().position(|k| *k == kind).unwrap_or(0)
}

impl DawTimeline {
    fn x_of(&self, st: &EditorState, t: f64) -> f64 {
        self.geo.x0 + (t - st.scroll_t) * st.pps
    }
    fn t_of(&self, st: &EditorState, x: f64) -> f64 {
        (st.scroll_t + (x - self.geo.x0) / st.pps).clamp(0.0, st.duration())
    }
    /// Visible horizontal span of [t0, t1], clamped to the waveform area.
    fn span(&self, st: &EditorState, t0: f64, t1: f64) -> Option<(f64, f64)> {
        let a = self.x_of(st, t0).max(self.geo.x0);
        let b = self.x_of(st, t1).min(self.geo.x0 + self.geo.w);
        (b > a).then_some((a, b - a))
    }
    fn lane_y(&self, i: usize) -> f64 {
        self.tops.get(i).copied().unwrap_or(self.geo.lanes_y + i as f64 * self.geo.lane_h)
    }
    /// Height of lane i (an open MIDI lane is taller: its piano roll).
    fn lane_hh(&self, i: usize) -> f64 {
        match (self.tops.get(i), self.tops.get(i + 1)) {
            (Some(a), Some(b)) => b - a,
            _ => self.geo.lane_h,
        }
    }
    fn lane_at(&self, y: f64) -> Option<usize> {
        if y < self.geo.lanes_y {
            return None;
        }
        (0..self.geo.n).find(|&i| y >= self.lane_y(i) && y < self.lane_y(i) + self.lane_hh(i))
    }
    fn in_ruler(&self, y: f64) -> bool {
        y >= self.geo.rect.pos.y && y < self.geo.sect_y
    }
    fn in_chords(&self, y: f64) -> bool {
        y >= self.geo.chord_y && y < self.geo.takes_y
    }
    fn in_sections(&self, y: f64) -> bool {
        y >= self.geo.sect_y && y < self.geo.sect_y + SECT_H
    }
    fn mute_box(&self, lane: usize) -> Rect {
        Rect { pos: dvec2(self.geo.rect.pos.x + HEADER_W - 58.0, self.lane_y(lane) + 9.0), size: dvec2(22.0, 18.0) }
    }
    /// The lane's dB value in its header: drag it to set the level.
    fn gain_box(&self, lane: usize) -> Rect {
        Rect { pos: dvec2(self.geo.rect.pos.x + 26.0, self.lane_y(lane) + 24.0), size: dvec2(80.0, 16.0) }
    }
    fn solo_box(&self, lane: usize) -> Rect {
        Rect { pos: dvec2(self.geo.rect.pos.x + HEADER_W - 32.0, self.lane_y(lane) + 9.0), size: dvec2(22.0, 18.0) }
    }
    fn clamp_scroll(&self, st: &mut EditorState) {
        let visible = self.geo.w / st.pps;
        st.scroll_t = st.scroll_t.clamp(0.0, (st.duration() - visible).max(0.0));
    }

    /// (region index, part) under the pointer in a lane: 0 body, 1 left edge, 2 right edge
    fn region_at(&self, st: &EditorState, lane: usize, x: f64) -> Option<(usize, u8)> {
        let id = &st.project.tracks[lane].id;
        for (i, r) in st.project.regions().iter().enumerate().rev() {
            if &r.track != id {
                continue;
            }
            let (a, b) = (self.x_of(st, r.start), self.x_of(st, r.end));
            if (x - a).abs() <= EDGE_PX {
                return Some((i, 1));
            }
            if (x - b).abs() <= EDGE_PX {
                return Some((i, 2));
            }
            if x > a && x < b {
                return Some((i, 0));
            }
        }
        None
    }

    fn section_edge_at(&self, st: &EditorState, x: f64) -> Option<usize> {
        let secs = st.project.sections();
        (1..secs.len()).find(|&i| (self.x_of(st, secs[i].start) - x).abs() <= EDGE_PX)
    }

    fn rect(&mut self, cx: &mut Cx2d, x: f64, y: f64, w: f64, h: f64, color: Vec4f) {
        self.draw_rect.color = color;
        self.draw_rect.draw_abs(cx, Rect { pos: dvec2(x, y), size: dvec2(w.max(1.0), h.max(1.0)) });
    }
    /// A rounded box shading from `top` to `bottom`.
    fn shape(&mut self, cx: &mut Cx2d, r: Rect, radius: f64, top: Vec4f, bottom: Vec4f) {
        self.shape_bordered(cx, r, radius, top, bottom, vec4(0.0, 0.0, 0.0, 0.0), 0.0);
    }
    #[allow(clippy::too_many_arguments)]
    fn shape_bordered(&mut self, cx: &mut Cx2d, r: Rect, radius: f64, top: Vec4f, bottom: Vec4f, border: Vec4f, border_size: f64) {
        self.draw_shape.color = top;
        self.draw_shape.color2 = bottom;
        self.draw_shape.border_color = border;
        self.draw_shape.border_size = border_size as f32;
        self.draw_shape.radius = radius as f32;
        self.draw_shape.draw_abs(cx, r);
    }
    fn text(&mut self, cx: &mut Cx2d, x: f64, y: f64, color: Vec4f, s: &str) {
        self.draw_text.color = color;
        self.draw_text.draw_abs(cx, dvec2(x, y), s);
    }
    fn bold(&mut self, cx: &mut Cx2d, x: f64, y: f64, color: Vec4f, s: &str) {
        self.draw_bold.color = color;
        self.draw_bold.draw_abs(cx, dvec2(x, y), s);
    }
    /// A pill with a label; returns its rect.
    fn pill(&mut self, cx: &mut Cx2d, x: f64, y: f64, h: f64, label: &str, fill: Vec4f, ink: Vec4f) -> Rect {
        let w = label.chars().count() as f64 * 5.6 + 16.0;
        let r = Rect { pos: dvec2(x, y), size: dvec2(w, h) };
        self.shape(cx, r, h * 0.5, fill, shade(fill, 0.92));
        self.text(cx, x + 8.0, y + (h - 11.0) * 0.5, ink, label);
        r
    }

    /// A level meter: dBFS from -60 to 0 across w, green, then yellow from
    /// -12 dB, red from -3 dB.
    fn meter_bar(&mut self, cx: &mut Cx2d, x: f64, y: f64, w: f64, level: f32) {
        self.shape(cx, Rect { pos: dvec2(x, y), size: dvec2(w, 4.0) }, 2.0, rgba(0x2a2e37, 1.0), rgba(0x2a2e37, 1.0));
        let db = 20.0 * level.max(1e-6).log10();
        let frac = ((db + 60.0) / 60.0).clamp(0.0, 1.0) as f64;
        if frac > 0.0 {
            let c = if db > -3.0 { 0xff453a } else if db > -12.0 { 0xffd60a } else { 0x30d158 };
            self.shape(cx, Rect { pos: dvec2(x, y), size: dvec2((w * frac).max(4.0), 4.0) }, 2.0, rgba(c, 1.0), rgba(c, 0.85));
        }
    }

    /// A lane's waveform: the peaks under each pixel column, drawn twice
    /// (the full swing faint, its inner half solid) for depth.
    fn wave(&mut self, cx: &mut Cx2d, st: &EditorState, stem: usize, ly: f64, lane_h: f64, color: Vec4f) {
        let peak = st.stems.get(stem).map_or(1.0, |s| s.peak_abs);
        self.wave_span(cx, st, stem, ly, lane_h, color, 0.0, (0.0, st.duration()), peak);
    }

    /// A stem's waveform over song time [from, to], the stem starting at song
    /// time `at`, scaled so `peak` fills the lane (a take uses its lane's).
    #[allow(clippy::too_many_arguments)]
    fn wave_span(&mut self, cx: &mut Cx2d, st: &EditorState, stem: usize, ly: f64, lane_h: f64, color: Vec4f, at: f64, span: (f64, f64), peak: f32) {
        let Some(s) = st.stems.get(stem).cloned() else { return };
        let scale = (lane_h * 0.42) / peak.max(1e-6) as f64;
        let mid = ly + lane_h * 0.5;
        let sr = st.sr() as f64;
        let (x0, w) = (self.geo.x0, self.geo.w);
        let faint = vec4(color.x, color.y, color.z, color.w * 0.45);
        for px in 0..(w as usize) {
            let t0 = st.scroll_t + px as f64 / st.pps;
            if t0 >= st.duration() || t0 >= span.1 {
                break;
            }
            if t0 < span.0 || t0 < at {
                continue;
            }
            let b0 = ((t0 - at) * sr) as usize / PEAK_BLOCK;
            let b1 = ((((t0 - at) + 1.0 / st.pps) * sr) as usize / PEAK_BLOCK).max(b0 + 1).min(s.peaks.len());
            if b0 >= b1 {
                break;
            }
            let (mut mn, mut mx) = (f32::MAX, f32::MIN);
            for &(a, b) in &s.peaks[b0..b1] {
                mn = mn.min(a);
                mx = mx.max(b);
            }
            let (top, bot) = (mid - mx as f64 * scale, mid - mn as f64 * scale);
            self.rect(cx, x0 + px as f64, top, 1.0, (bot - top).max(1.0), faint);
            let (it, ib) = (mid - mx as f64 * scale * 0.5, mid - mn as f64 * scale * 0.5);
            self.rect(cx, x0 + px as f64, it, 1.0, (ib - it).max(1.0), color);
        }
    }

    /// A MIDI lane's notes on pitch rows; open (tall) it is a piano roll with
    /// row shading, C labels and beat lines.
    fn draw_notes(&mut self, cx: &mut Cx2d, st: &EditorState, lane: usize, ly: f64, lh: f64, col: Vec4f) {
        let notes = st.project.tracks[lane].notes.clone().unwrap_or_default();
        let grid = poing_model::BeatGrid::of(&st.project);
        let open = lh > 200.0;
        let (mut lo, mut hi) = notes.iter().fold((127u8, 0u8), |(a, b), n| (a.min(n.pitch), b.max(n.pitch)));
        if notes.is_empty() {
            (lo, hi) = (48, 72);
        }
        let want = if open { 24 } else { 12 };
        let span = (hi as i32 - lo as i32 + 4).max(want);
        let mid = (lo as i32 + hi as i32) / 2;
        let lo = (mid - span / 2).clamp(0, 127 - span) as u8;
        let hi = (lo as i32 + span).min(127) as u8;
        self.midi_view.retain(|(l, _, _)| *l != lane);
        self.midi_view.push((lane, lo, hi));
        let (x0, w) = (self.geo.x0, self.geo.w);
        let top = ly + 4.0;
        let rh = (lh - 8.0) / (hi - lo + 1) as f64;
        if open {
            for p in lo..=hi {
                let y = top + (hi - p) as f64 * rh;
                if [1, 3, 6, 8, 10].contains(&(p % 12)) {
                    self.rect(cx, x0, y, w, rh, rgba(0x000000, 0.18));
                }
                if p % 12 == 0 && rh >= 7.0 {
                    self.rect(cx, x0, y + rh - 1.0, w, 1.0, rgba(HAIRLINE, 1.0));
                    self.text(cx, x0 + 3.0, y + rh - 12.0, rgba(TEXT_2, 0.8), &format!("C{}", p as i32 / 12 - 1));
                }
            }
            // beat lines, bars stronger
            let (b0, b1) = (grid.beat_at(st.scroll_t).floor() as i64, grid.beat_at(st.scroll_t + w / st.pps).ceil() as i64);
            if (b1 - b0) as f64 * 4.0 < w {
                for b in b0..=b1 {
                    let x = self.x_of(st, grid.time_of(b as f64));
                    if x >= x0 && x <= x0 + w {
                        let bar = (b as f64 - grid.bar_offset).rem_euclid(grid.per_bar as f64) == 0.0;
                        self.rect(cx, x, top, 1.0, lh - 8.0, rgba(if bar { 0x3a3f49 } else { 0x23262d }, 1.0));
                    }
                }
            }
        }
        let sel = st.note_sel.filter(|(l, _)| *l == lane).map(|(_, i)| i);
        for (k, n) in notes.iter().enumerate() {
            let (a, b) = (self.x_of(st, grid.time_of(n.start)), self.x_of(st, grid.time_of(n.start + n.len)));
            if b < x0 || a > x0 + w || n.pitch < lo || n.pitch > hi {
                continue;
            }
            let (a, b) = (a.max(x0), b.min(x0 + w));
            let y = top + (hi - n.pitch) as f64 * rh;
            // closed, rows are thin: notes keep 3 px so the part reads at a glance
            let h = (rh - 1.0).max(if open { 2.0 } else { 3.0 });
            let r = Rect { pos: dvec2(a, y + rh * 0.5 - h * 0.5), size: dvec2((b - a - 1.0).max(2.0), h) };
            let shown = vec4(col.x, col.y, col.z, col.w * (0.55 + 0.45 * n.vel as f32));
            if sel == Some(k) {
                self.shape_bordered(cx, r, 2.0, shown, shade(shown, 0.85), rgba(0xffffff, 1.0), 1.5);
            } else {
                self.shape(cx, r, 2.0, shown, shade(shown, 0.85));
            }
        }
    }

    /// The pitch row under y in MIDI lane lane.
    fn pitch_at(&self, lane: usize, y: f64) -> u8 {
        let Some(&(_, lo, hi)) = self.midi_view.iter().find(|(l, _, _)| *l == lane) else { return 60 };
        let (top, lh) = (self.lane_y(lane) + 4.0, self.lane_hh(lane) - LANE_GAP - 8.0);
        let rh = lh / (hi - lo + 1) as f64;
        let row = ((y - top) / rh).floor().clamp(0.0, (hi - lo) as f64) as u8;
        hi - row
    }

    /// A press on a MIDI lane: grab a note (its body moves it, its right edge
    /// sizes it) or draw a new one on the 1/16 grid.
    fn note_down(&mut self, st: &mut EditorState, lane: usize, x: f64, y: f64) {
        let grid = poing_model::BeatGrid::of(&st.project);
        let beat = grid.beat_at(self.t_of(st, x));
        let pitch = self.pitch_at(lane, y);
        let notes = st.project.tracks[lane].notes.clone().unwrap_or_default();
        let hit = notes.iter().enumerate().rev().find(|(_, n)| {
            let (a, b) = (self.x_of(st, grid.time_of(n.start)), self.x_of(st, grid.time_of(n.start + n.len)));
            n.pitch == pitch && x >= a - 2.0 && x <= b + 2.0
        });
        st.checkpoint();
        st.select(Selection::Track(lane));
        match hit {
            Some((idx, n)) => {
                let right = self.x_of(st, grid.time_of(n.start + n.len));
                st.note_sel = Some((lane, idx));
                self.drag = if x > right - EDGE_PX { Drag::NoteSize { lane, idx } } else { Drag::NoteMove { lane, idx, grab: beat - n.start } };
            }
            None => {
                let n = poing_model::Note { start: ((beat * 4.0).floor() / 4.0).max(0.0), len: st.note_len, pitch, vel: 0.8 };
                let notes = st.project.tracks[lane].notes.get_or_insert_with(Vec::new);
                notes.push(n);
                let idx = notes.len() - 1;
                st.note_sel = Some((lane, idx));
                self.drag = Drag::NoteSize { lane, idx };
            }
        }
    }

    /// A row of effect chips from (x, y); the chain's own "+" is not drawn
    /// (effects are added by Describe).
    fn fx_chips_row(&mut self, cx: &mut Cx2d, st: &EditorState, lane: Option<usize>, x: f64, y: f64, max_x: f64) {
        let chain: Vec<(String, bool, bool)> = st
            .project
            .chain(lane)
            .iter()
            .map(|f| (cut(&f.name, 14), f.bypassed(), st.fx_errors.contains_key(&f.id)))
            .collect();
        let mut cx_ = x;
        for (i, (name, bypassed, broken)) in chain.into_iter().enumerate() {
            let selected = st.sel == Selection::Fx(lane, i);
            let fill = if selected {
                rgba(ACCENT, 1.0)
            } else if broken {
                rgba(0xff453a, 0.35)
            } else {
                rgba(0x2a2e37, if bypassed { 0.5 } else { 1.0 })
            };
            let ink = if selected { rgba(0xffffff, 1.0) } else { rgba(TEXT, if bypassed { 0.45 } else { 0.9 }) };
            let w = name.chars().count() as f64 * 5.6 + 16.0;
            if cx_ + w > max_x {
                break;
            }
            let r = self.pill(cx, cx_, y, 16.0, &name, fill, ink);
            self.fx_chips.push((r, lane, i));
            cx_ += r.size.x + 4.0;
        }
    }

    fn draw_all(&mut self, cx: &mut Cx2d, st: &mut EditorState, rect: Rect) {
        let n = st.project.tracks.len();
        let chords = st.project.analysis.as_ref().and_then(|a| a.chords.clone()).unwrap_or_default();
        let lyrics = st.project.analysis.as_ref().and_then(|a| a.lyrics.clone()).unwrap_or_default();
        let chord_y = rect.pos.y + RULER_H + SECT_H;
        let takes_y = chord_y + if chords.is_empty() { 0.0 } else { CHORD_H };
        let lanes_y = takes_y + if st.project.takes().is_empty() { 0.0 } else { TAKES_H } + GAP;
        let song_take = st.song_take().map(|(stem, t)| (stem, t.id.clone()));
        // While a song take plays it is the only lane: the stems are not what you hear.
        let n = if song_take.is_some() { 0 } else { n };
        // Active layers get a lane each, under the stems.
        let layers: Vec<(usize, poing_model::Take)> = if song_take.is_some() {
            Vec::new()
        } else {
            st.active
                .iter()
                .filter_map(|id| st.project.take_index(id))
                .filter(|&i| st.project.takes()[i].scope == "layer")
                .map(|i| (st.project.tracks.len() + i, st.project.takes()[i].clone()))
                .collect()
        };
        let avail = rect.size.y - (lanes_y - rect.pos.y) - MASTER_H - GAP;
        let count = (n + layers.len()).max(1) as f64;
        // A selected MIDI lane opens into a piano roll; the others share the rest.
        let open = match st.sel {
            Selection::Track(i) if i < n && st.project.tracks[i].is_midi() => Some(i),
            _ => None,
        };
        let open_h = (avail * 0.55).clamp(220.0, 420.0);
        let lane_h = match open {
            Some(_) if count > 1.0 => ((avail - open_h) / (count - 1.0)).clamp(36.0, 150.0),
            _ => (avail / count).clamp(44.0, 150.0),
        };
        self.tops.clear();
        let mut y = lanes_y;
        for i in 0..(n + layers.len()) {
            self.tops.push(y);
            y += if open == Some(i) { open_h } else { lane_h };
        }
        self.tops.push(y);
        let master_y = rect.pos.y + rect.size.y - MASTER_H;
        self.geo = Geo {
            rect,
            x0: rect.pos.x + HEADER_W,
            w: (rect.size.x - HEADER_W - 8.0).max(10.0),
            sect_y: rect.pos.y + RULER_H,
            chord_y,
            takes_y,
            lanes_y,
            lane_h,
            n,
            master_y,
        };
        if st.pps <= 0.0 && st.duration() > 0.0 {
            st.pps = self.geo.w / st.duration();
            st.scroll_t = 0.0;
        }
        let playing = st.shared.playing();
        let ph = st.playhead();
        if playing {
            let visible = self.geo.w / st.pps;
            if ph > st.scroll_t + visible * 0.92 || ph < st.scroll_t {
                st.scroll_t = ph - visible * 0.08;
                self.clamp_scroll(st);
            }
            self.next_frame = cx.new_next_frame();
        }
        let (x0, w) = (self.geo.x0, self.geo.w);
        let left = rect.pos.x;
        // meters: take the peaks the audio thread left, let the shown level fall back
        let mut moving = false;
        for i in 0..=METER_LANES {
            let v = f32::from_bits(st.shared.meters[i].swap(0, std::sync::atomic::Ordering::Relaxed));
            let shown = &mut st.meter_show[i];
            *shown = v.max(*shown * 0.88);
            moving |= *shown > 1e-4;
        }
        if moving {
            self.next_frame = cx.new_next_frame();
        }
        let white = rgba(TEXT, 1.0);
        let dim = rgba(TEXT_2, 1.0);
        let faint = rgba(TEXT_2, 0.7);
        self.fx_chips.clear();

        // header column and clock
        self.rect(cx, left, rect.pos.y, HEADER_W - 4.0, rect.size.y, rgba(0x121317, 1.0));
        self.draw_clock.color = white;
        self.draw_clock.draw_abs(cx, dvec2(left + 14.0, rect.pos.y + 6.0), &fmt_time(ph));
        self.text(cx, left + 100.0, rect.pos.y + 11.0, faint, &format!("/ {}", fmt_time(st.duration())));

        // the loop: a band on the ruler and a faint wash over the lanes
        if let Some((a, b)) = st.loop_range {
            if let Some((x, lw)) = self.span(st, a, b) {
                self.shape(cx, Rect { pos: dvec2(x, rect.pos.y + 4.0), size: dvec2(lw, RULER_H - 8.0) }, 4.0, rgba(0xffd60a, 0.28), rgba(0xffd60a, 0.18));
                self.rect(cx, x, rect.pos.y + RULER_H, lw, master_y - rect.pos.y - RULER_H, rgba(0xffd60a, 0.04));
                if lw > 40.0 {
                    self.text(cx, x + 6.0, rect.pos.y + 1.0, rgba(0xffd60a, 1.0), "loop");
                }
            }
        }

        // ruler: a tick every 1/2/5/10/15/30/60 s, whichever is >= 70 px apart
        let step = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0].into_iter().find(|s| s * st.pps >= 70.0).unwrap_or(60.0);
        let mut t = (st.scroll_t / step).floor() * step;
        while t <= st.scroll_t + w / st.pps {
            let x = self.x_of(st, t);
            if x >= x0 {
                self.rect(cx, x, rect.pos.y + RULER_H - 7.0, 1.0, 7.0, rgba(HAIRLINE, 1.0));
                self.text(cx, x + 4.0, rect.pos.y + 10.0, faint, fmt_time(t).trim_end_matches(".0"));
            }
            t += step;
        }

        // sections
        let sy = self.geo.sect_y;
        self.text(cx, left + 14.0, sy + 8.0, dim, "Sections");
        for i in 0..st.project.sections().len() {
            let s = st.project.sections()[i].clone();
            if let Some((x, sw)) = self.span(st, s.start, s.end) {
                let c = rgba(section_color(&s.label), 1.0);
                let r = Rect { pos: dvec2(x + 1.0, sy + 3.0), size: dvec2((sw - 2.0).max(2.0), SECT_H - 6.0) };
                if st.sel == Selection::Section(i) {
                    self.shape_bordered(cx, r, 6.0, shade(c, 1.15), c, rgba(0xffffff, 0.9), 1.5);
                } else {
                    self.shape(cx, r, 6.0, c, shade(c, 0.82));
                }
                if sw > 36.0 {
                    self.bold(cx, x + 8.0, sy + 8.0, white, &cut(&s.label, ((sw - 14.0) / 6.5) as usize));
                }
            }
        }

        // background job progress: a thin accent line under the sections
        if let Some((_, p)) = st.analysing.clone() {
            self.shape(cx, Rect { pos: dvec2(x0, sy + SECT_H - 2.0), size: dvec2(w, 2.0) }, 1.0, rgba(HAIRLINE, 1.0), rgba(HAIRLINE, 1.0));
            let pw = (w * p.clamp(0.0, 1.0) as f64).max(2.0);
            self.shape(cx, Rect { pos: dvec2(x0, sy + SECT_H - 2.0), size: dvec2(pw, 2.0) }, 1.0, rgba(ACCENT, 1.0), rgba(ACCENT, 1.0));
            self.next_frame = cx.new_next_frame();
        }

        // chords: names on a quiet line, the one playing in the accent
        if !chords.is_empty() {
            self.text(cx, left + 14.0, chord_y + 4.0, dim, "Chords");
            let cur = chords.iter().position(|c| ph >= c.start && ph < c.end);
            for (i, c) in chords.iter().enumerate() {
                if c.label == "N" {
                    continue;
                }
                if let Some((x, cw)) = self.span(st, c.start, c.end) {
                    let on = cur == Some(i);
                    if cw >= 18.0 {
                        self.rect(cx, x, chord_y + 5.0, 1.0, CHORD_H - 10.0, rgba(HAIRLINE, 1.0));
                    }
                    if on {
                        self.shape(cx, Rect { pos: dvec2(x + 1.0, chord_y + 2.0), size: dvec2((cw - 2.0).max(2.0), CHORD_H - 4.0) }, 4.0, rgba(ACCENT, 0.25), rgba(ACCENT, 0.2));
                    }
                    if cw > c.label.chars().count() as f64 * 6.0 + 8.0 {
                        self.text(cx, x + 5.0, chord_y + 4.0, if on { white } else { dim }, &c.label);
                    }
                }
            }
        }

        // takes: Original, Take 1 · vocals, ... click to switch (A/B)
        self.chips.clear();
        if !st.project.takes().is_empty() {
            self.text(cx, left + 14.0, takes_y + 9.0, dim, "Takes");
            let mut x = x0;
            let original = if st.project.is_draft() { "Version 1" } else { "Original" };
            let combined = st.project.takes().iter().filter(|t| t.engine == "copy").count();
            // sections combined from remixes share one chip (each shows on the lanes)
            let mut names: Vec<(String, Option<String>)> = std::iter::once((original.to_string(), None))
                .chain(st.project.takes().iter().filter(|t| t.engine != "copy").map(|t| {
                    let n = t.id.trim_start_matches('t');
                    let label = match (t.scope.as_str(), t.track.as_deref()) {
                        _ if crate::state::named_take(t) => t.prompt.clone(),
                        ("lane" | "layer", Some(track)) => format!("Take {n} · {track}"),
                        ("section", _) => format!("Take {n} · {:.0}s", t.start),
                        _ => format!("Take {n}"),
                    };
                    (label, Some(t.id.clone()))
                }))
                .collect();
            if combined > 0 {
                names.push((format!("Combined · {combined} section{}", if combined == 1 { "" } else { "s" }), Some(crate::state::COMBINED.into())));
            }
            for (label, id) in names {
                let on = match &id {
                    None => st.active.is_empty(),
                    Some(id) if id == crate::state::COMBINED => st.combined_on(),
                    Some(id) => st.take_on(id),
                };
                let (fill, ink) = if on { (rgba(ACCENT, 1.0), rgba(0xffffff, 1.0)) } else { (rgba(0x23262d, 1.0), rgba(TEXT, 0.85)) };
                let r = self.pill(cx, x, takes_y + 5.0, 20.0, &label, fill, ink);
                self.chips.push((r, id));
                x += r.size.x + 6.0;
            }
        }

        // a song take: one lane with its waveform
        if let Some((stem, id)) = &song_take {
            let h = (master_y - GAP - lanes_y).max(44.0);
            let card = Rect { pos: dvec2(left + 6.0, lanes_y), size: dvec2(rect.size.x - 12.0, h) };
            self.shape(cx, card, 8.0, rgba(CARD, 1.0), rgba(CARD, 1.0));
            self.shape(cx, Rect { pos: dvec2(left + 6.0, lanes_y), size: dvec2(HEADER_W - 12.0, h) }, 8.0, rgba(HEADER, 1.0), rgba(HEADER, 1.0));
            self.shape(cx, Rect { pos: dvec2(left + 16.0, lanes_y + 14.0), size: dvec2(8.0, 8.0) }, 8.0, rgba(ACCENT, 1.0), rgba(ACCENT, 1.0));
            let name = st.project.take_index(id).map(|k| &st.project.takes()[k]).filter(|t| crate::state::named_take(t)).map(|t| t.prompt.clone());
            self.bold(cx, left + 30.0, lanes_y + 10.0, white, &name.unwrap_or_else(|| format!("Take {}", id.trim_start_matches('t'))));
            self.text(cx, left + 30.0, lanes_y + 28.0, dim, "the whole song");
            self.wave(cx, st, *stem, lanes_y, h.min(280.0), rgba(ACCENT, 0.9));
        }

        // lanes: a card each, header on the left
        let gains = st.project.track_gains();
        for i in 0..n {
            let ly = self.lane_y(i);
            let tr = st.project.tracks[i].clone();
            let col = rgba(track_color(&tr.id, &tr.name, i), 1.0);
            let audible = gains[i] > 0.0;
            let lh = self.lane_hh(i) - LANE_GAP;
            let selected = st.sel == Selection::Track(i);
            self.shape(cx, Rect { pos: dvec2(left + 6.0, ly), size: dvec2(rect.size.x - 12.0, lh) }, 8.0, rgba(CARD, 1.0), rgba(0x14161a, 1.0));
            let head = Rect { pos: dvec2(left + 6.0, ly), size: dvec2(HEADER_W - 12.0, lh) };
            if selected {
                self.shape_bordered(cx, head, 8.0, rgba(0x232833, 1.0), rgba(0x1f232c, 1.0), rgba(ACCENT, 0.9), 1.5);
            } else {
                self.shape(cx, head, 8.0, rgba(HEADER, 1.0), rgba(0x181a1f, 1.0));
            }
            let dot = if audible { col } else { vec4(col.x, col.y, col.z, 0.35) };
            self.shape(cx, Rect { pos: dvec2(left + 16.0, ly + 14.0), size: dvec2(8.0, 8.0) }, 8.0, dot, dot);
            self.bold(cx, left + 30.0, ly + 10.0, if audible { white } else { dim }, &cut(&tr.name, 16));
            self.text(cx, left + 30.0, ly + 27.0, dim, &format!("{:+.1} dB", tr.gain()));
            let (mb, sb) = (self.mute_box(i), self.solo_box(i));
            let (mf, mi) = if tr.muted() { (rgba(0xff453a, 1.0), rgba(0xffffff, 1.0)) } else { (rgba(0x2a2e37, 1.0), rgba(TEXT, 0.8)) };
            self.shape(cx, mb, 5.0, mf, shade(mf, 0.9));
            self.text(cx, mb.pos.x + 7.0, mb.pos.y + 3.5, mi, "M");
            let (sf, si) = if tr.soloed() { (rgba(0xffd60a, 1.0), rgba(0x111111, 1.0)) } else { (rgba(0x2a2e37, 1.0), rgba(TEXT, 0.8)) };
            self.shape(cx, sb, 5.0, sf, shade(sf, 0.9));
            self.text(cx, sb.pos.x + 7.5, sb.pos.y + 3.5, si, "S");
            if i < METER_LANES {
                let level = st.meter_show[i];
                self.meter_bar(cx, left + 30.0, ly + 42.0, HEADER_W - 66.0, level);
            }
            if lh >= 66.0 {
                self.fx_chips_row(cx, st, Some(i), left + 16.0, ly + lh - 20.0, left + HEADER_W - 10.0);
            }
            if tr.is_midi() {
                self.draw_notes(cx, st, i, ly, lh, if audible { col } else { vec4(col.x, col.y, col.z, 0.3) });
            } else {
                self.wave(cx, st, i, ly, lh, if audible { col } else { vec4(col.x, col.y, col.z, 0.25) });
            }
            // A lane take that is on: its own audio over its range, on the lane's scale.
            let lane_peak = st.stems.get(i).map_or(1.0, |s| s.peak_abs);
            let takes_here: Vec<(usize, f64, f64)> = st
                .active
                .iter()
                .filter_map(|id| st.project.take_index(id))
                .filter(|&k| st.project.takes()[k].scope == "lane" && st.project.takes()[k].track.as_deref() == Some(tr.id.as_str()))
                .map(|k| (st.project.tracks.len() + k, st.project.takes()[k].start, st.project.takes()[k].end))
                .collect();
            for (stem, start, end) in takes_here {
                if let Some((x, tw)) = self.span(st, start, end) {
                    self.rect(cx, x, ly + 1.0, tw, lh - 2.0, rgba(CARD, 1.0));
                    let bright = vec4(col.x.min(1.0), col.y.min(1.0), col.z.min(1.0), if audible { 1.0 } else { 0.3 });
                    self.wave_span(cx, st, stem, ly, lh, bright, start, (start, end), lane_peak);
                }
            }
        }

        // layers: one lane each, at their blend
        for (j, (stem, t)) in layers.iter().enumerate() {
            let ly = self.lane_y(n + j);
            let lh = self.lane_hh(n + j) - LANE_GAP;
            self.shape(cx, Rect { pos: dvec2(left + 6.0, ly), size: dvec2(rect.size.x - 12.0, lh) }, 8.0, rgba(0x14202d, 1.0), rgba(0x121a24, 1.0));
            self.shape(cx, Rect { pos: dvec2(left + 6.0, ly), size: dvec2(HEADER_W - 12.0, lh) }, 8.0, rgba(0x18263a, 1.0), rgba(0x15202f, 1.0));
            let what = t.track.as_deref().unwrap_or("layer");
            self.bold(cx, left + 16.0, ly + 10.0, white, &format!("+ {what}"));
            let level = format!("Take {}  ·  {:.0}%", t.id.trim_start_matches('t'), t.blend() * 100.0);
            self.text(cx, left + 16.0, ly + 27.0, dim, &level);
            self.wave(cx, st, *stem, ly, lh, rgba(ACCENT, 0.9));
        }

        // lyrics along the bottom of the vocals lane
        if let Some(v) = st.project.track_index("vocals").filter(|&v| song_take.is_none() && v < n) {
            let y = self.lane_y(v) + self.lane_hh(v) - LANE_GAP - 17.0;
            let mut right = f64::MIN;
            for l in &lyrics {
                let x = self.x_of(st, l.start).max(x0);
                if x >= x0 + w || self.x_of(st, l.end) < x0 || x < right {
                    continue;
                }
                let on = ph >= l.start && ph < l.end;
                self.text(cx, x + 4.0, y, if on { white } else { faint }, l.text.trim());
                right = x + l.text.trim().chars().count() as f64 * 5.5 + 12.0;
            }
        }

        // regions over the lanes
        for ri in 0..st.project.regions().len() {
            if song_take.is_some() {
                break;
            }
            let r = st.project.regions()[ri].clone();
            let Some(lane) = st.project.track_index(&r.track) else { continue };
            let ly = self.lane_y(lane);
            if let Some((x, rw)) = self.span(st, r.start, r.end) {
                let c = rgba(region_color(&r.kind), 1.0);
                let body = Rect { pos: dvec2(x, ly + 3.0), size: dvec2(rw, self.lane_hh(lane) - LANE_GAP - 6.0) };
                let sel = st.sel == Selection::Region(ri);
                let border = if sel { rgba(0xffffff, 0.95) } else { vec4(c.x, c.y, c.z, 0.7) };
                self.shape_bordered(cx, body, 6.0, vec4(c.x, c.y, c.z, 0.16), vec4(c.x, c.y, c.z, 0.10), border, if sel { 1.5 } else { 1.0 });
                let label = match r.kind.as_str() {
                    "bad" | "regenerate" | "mute" => r.kind.clone(),
                    _ => format!("{} {:+.1}dB", r.kind, r.gain()),
                };
                if rw > 40.0 {
                    self.pill(cx, x + 4.0, ly + 6.0, 16.0, &cut(&label, ((rw - 24.0) / 5.6) as usize), c, rgba(0x111111, 1.0));
                }
            }
        }

        // section and lane takes glow over the lanes they replace
        let replacing: Vec<poing_model::Take> = st
            .active
            .iter()
            .filter_map(|id| st.project.take_index(id))
            .map(|i| st.project.takes()[i].clone())
            .filter(|t| t.scope == "section" || t.scope == "lane")
            .collect();
        for t in replacing.iter().filter(|_| n > 0) {
            let (y, h) = match t.track.as_deref().and_then(|id| st.project.track_index(id)).filter(|_| t.scope == "lane") {
                Some(lane) => (self.lane_y(lane), self.lane_hh(lane) - LANE_GAP),
                None => (lanes_y, self.lane_y(n) - lanes_y - LANE_GAP),
            };
            if let Some((x, tw)) = self.span(st, t.start, t.end) {
                let r = Rect { pos: dvec2(x, y), size: dvec2(tw, h) };
                self.shape_bordered(cx, r, 8.0, rgba(ACCENT, 0.12), rgba(ACCENT, 0.06), rgba(ACCENT, 0.9), 1.5);
                if tw > 60.0 {
                    let label = if crate::state::named_take(t) { t.prompt.clone() } else { format!("Take {}", t.id.trim_start_matches('t')) };
                    self.pill(cx, x + 6.0, y + 6.0, 16.0, &label, rgba(ACCENT, 1.0), rgba(0xffffff, 1.0));
                }
            }
        }

        // master strip: the mix's own effects
        let my = master_y;
        self.shape(cx, Rect { pos: dvec2(left + 6.0, my), size: dvec2(rect.size.x - 12.0, MASTER_H - 6.0) }, 8.0, rgba(0x15171b, 1.0), rgba(0x15171b, 1.0));
        self.bold(cx, left + 16.0, my + 8.0, dim, "Master");
        let level = st.meter_show[MASTER_METER];
        self.meter_bar(cx, left + 74.0, my + 12.0, HEADER_W - 92.0, level);
        self.fx_chips_row(cx, st, None, x0, my + 6.0, x0 + w);
        if st.project.chain(None).is_empty() {
            self.text(cx, x0, my + 8.0, faint, "Select a lane and describe an effect (\"tape saturation\", \"a warm plate reverb\") to add one");
        }

        // playhead: an accent line with a cap in the ruler
        let px = self.x_of(st, ph);
        if px >= x0 && px <= x0 + w {
            self.rect(cx, px, rect.pos.y + RULER_H - 6.0, 1.5, master_y - rect.pos.y - RULER_H + 6.0, rgba(ACCENT, 0.95));
            self.shape(cx, Rect { pos: dvec2(px - 5.0, rect.pos.y + RULER_H - 12.0), size: dvec2(11.0, 8.0) }, 3.0, rgba(ACCENT, 1.0), rgba(ACCENT, 1.0));
        }
    }
}

/// A keyboard shortcut on the song; true when the key was one.
pub fn shortcut(st: &mut EditorState, ke: &KeyEvent) -> bool {
    let cmd = ke.modifiers.control || ke.modifiers.logo;
    match ke.key_code {
        KeyCode::Space => st.toggle_play(),
        KeyCode::Delete | KeyCode::Backspace => st.notice = st.delete_selected(),
        KeyCode::KeyZ if cmd => st.notice = st.undo(ke.modifiers.shift),
        KeyCode::KeyY if cmd => st.notice = st.undo(true),
        KeyCode::Home => st.seek(0.0),
        KeyCode::End => st.seek(st.duration()),
        // L: loop the selected section or region
        KeyCode::KeyL if !cmd => st.notice = Some(st.toggle_loop()),
        // F: fit the song in view
        KeyCode::KeyF if !cmd => st.pps = 0.0,
        // B: split the section at the playhead (a blade)
        KeyCode::KeyB if !cmd => {
            let t = st.playhead();
            st.split_section_at(t);
        }
        // M / S: mute or solo the selected lane
        KeyCode::KeyM | KeyCode::KeyS if !cmd => {
            let Selection::Track(i) = st.sel else { return false };
            st.checkpoint();
            let t = &mut st.project.tracks[i];
            if ke.key_code == KeyCode::KeyM {
                t.mute = Some(!t.muted());
            } else {
                t.solo = Some(!t.soloed());
            }
            st.push_mix();
            st.sel_changed = true;
        }
        KeyCode::KeyS if cmd => {
            st.notice = Some(match st.save() {
                Ok(()) => "Saved".into(),
                Err(e) => format!("Could not save: {e}"),
            });
        }
        // Escape: select nothing
        KeyCode::Escape => st.select(Selection::None),
        _ => return false,
    }
    true
}

impl Widget for DawTimeline {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.next_frame.is_event(event).is_some() {
            self.area.redraw(cx);
        }
        let Some(st) = scope.data_mut().get_mut::<EditorState>() else { return };
        match event.hits(cx, self.area) {
            Hit::FingerDown(fe) => {
                cx.set_key_focus(self.area);
                let (x, y) = (fe.abs.x, fe.abs.y);
                let in_wave = x >= self.geo.x0;
                if let Some((_, lane, i)) = self.fx_chips.iter().find(|(r, _, _)| r.contains(fe.abs)).copied() {
                    st.select(Selection::Fx(lane, i));
                } else if self.in_ruler(y) {
                    if fe.tap_count >= 2 {
                        // double-click the ruler: fit the song
                        st.pps = 0.0;
                    } else {
                        st.seek(self.t_of(st, x));
                        self.drag = Drag::Seek;
                    }
                } else if self.in_sections(y) {
                    if let Some(edge) = self.section_edge_at(st, x) {
                        st.checkpoint();
                        self.drag = Drag::SectionEdge { index: edge };
                    } else if in_wave {
                        let t = self.t_of(st, x);
                        if let Some(i) = st.project.sections().iter().position(|s| t >= s.start && t < s.end) {
                            let start = st.project.sections()[i].start;
                            st.select(Selection::Section(i));
                            st.seek(start);
                        }
                    }
                } else if y >= self.geo.takes_y && y < self.geo.lanes_y {
                    if let Some((_, id)) = self.chips.iter().find(|(r, _)| r.contains(fe.abs)).cloned() {
                        st.select(Selection::None);
                        match id {
                            None => st.all_takes_off(),
                            Some(id) if id == crate::state::COMBINED => st.toggle_combined(),
                            Some(id) => st.toggle_take(&id),
                        }
                    }
                } else if self.in_chords(y) {
                    if in_wave {
                        st.seek(self.t_of(st, x));
                        self.drag = Drag::Seek;
                    }
                } else if let Some(lane) = self.lane_at(y).filter(|&l| in_wave && st.project.tracks[l].is_midi()) {
                    self.note_down(st, lane, x, y);
                } else if let Some(lane) = self.lane_at(y) {
                    if !in_wave {
                        if self.gain_box(lane).contains(fe.abs) {
                            st.checkpoint();
                            if fe.tap_count >= 2 {
                                // double-click: back to 0 dB
                                st.project.tracks[lane].gain_db = Some(0.0);
                                st.push_mix();
                            } else {
                                self.drag = Drag::Gain { lane, x0: fe.abs.x, g0: st.project.tracks[lane].gain() };
                            }
                        } else if self.mute_box(lane).contains(fe.abs) {
                            st.checkpoint();
                            let t = &mut st.project.tracks[lane];
                            t.mute = Some(!t.muted());
                            st.push_mix();
                        } else if self.solo_box(lane).contains(fe.abs) {
                            st.checkpoint();
                            let t = &mut st.project.tracks[lane];
                            t.solo = Some(!t.soloed());
                            st.push_mix();
                        }
                        st.select(Selection::Track(lane));
                    } else if let Some((ri, part)) = self.region_at(st, lane, x) {
                        st.checkpoint();
                        st.select(Selection::Region(ri));
                        self.drag = match part {
                            1 => Drag::ResizeRegion { region: ri, left: true },
                            2 => Drag::ResizeRegion { region: ri, left: false },
                            _ => Drag::MoveRegion { region: ri, grab: self.t_of(st, x) - st.project.regions()[ri].start },
                        };
                    } else {
                        self.drag = Drag::Pending { track: lane, t0: self.t_of(st, x), x0: x };
                    }
                } else if y >= self.geo.master_y && !in_wave {
                    st.select(Selection::None);
                }
                self.area.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                let x = fe.abs.x;
                let t = self.t_of(st, x);
                match self.drag {
                    Drag::Seek => st.seek(t),
                    Drag::Pending { track, t0, x0 } if (x - x0).abs() > 4.0 => {
                        st.checkpoint();
                        let ri = st.add_region(track, t0, t);
                        st.select(Selection::Region(ri));
                        self.drag = Drag::NewRegion { region: ri, anchor: t0 };
                    }
                    Drag::NewRegion { region, anchor } => {
                        if let Some(r) = st.project.regions_mut().get_mut(region) {
                            r.start = anchor.min(t);
                            r.end = anchor.max(t);
                        }
                        st.push_mix();
                    }
                    Drag::MoveRegion { region, grab } => {
                        let dur = st.duration();
                        if let Some(r) = st.project.regions_mut().get_mut(region) {
                            let len = r.end - r.start;
                            r.start = (t - grab).clamp(0.0, dur - len);
                            r.end = r.start + len;
                        }
                        st.push_mix();
                        st.sel_changed = true;
                    }
                    Drag::ResizeRegion { region, left } => {
                        if let Some(r) = st.project.regions_mut().get_mut(region) {
                            if left {
                                r.start = t.min(r.end - 0.05);
                            } else {
                                r.end = t.max(r.start + 0.05);
                            }
                        }
                        st.push_mix();
                        st.sel_changed = true;
                    }
                    Drag::Gain { lane, x0, g0 } => {
                        // a tenth of a dB per pixel, in half-dB steps
                        let g = ((g0 + (x - x0) * 0.1) * 2.0).round() / 2.0;
                        st.project.tracks[lane].gain_db = Some(g.clamp(-36.0, 12.0));
                        st.push_mix();
                        st.sel_changed = true;
                    }
                    Drag::NoteMove { lane, idx, grab } => {
                        let grid = poing_model::BeatGrid::of(&st.project);
                        let beat = grid.beat_at(t);
                        let pitch = self.pitch_at(lane, fe.abs.y);
                        if let Some(n) = st.project.tracks[lane].notes.as_mut().and_then(|v| v.get_mut(idx)) {
                            n.start = poing_model::BeatGrid::snap(beat - grab, 4.0).max(0.0);
                            n.pitch = pitch;
                        }
                        st.dirty = true;
                    }
                    Drag::NoteSize { lane, idx } => {
                        let grid = poing_model::BeatGrid::of(&st.project);
                        let beat = grid.beat_at(t);
                        if let Some(n) = st.project.tracks[lane].notes.as_mut().and_then(|v| v.get_mut(idx)) {
                            n.len = (poing_model::BeatGrid::snap(beat, 4.0) - n.start).max(0.25);
                            st.note_len = n.len;
                        }
                        st.dirty = true;
                    }
                    Drag::SectionEdge { index } => {
                        let secs = st.project.sections_mut();
                        let t = t.clamp(secs[index - 1].start + 0.5, secs[index].end - 0.5);
                        secs[index - 1].end = t;
                        secs[index].start = t;
                        st.dirty = true;
                    }
                    _ => {}
                }
                self.area.redraw(cx);
            }
            Hit::FingerUp(_) => {
                match self.drag {
                    // a click, not a drag: seek and select the track
                    Drag::Pending { track, t0, .. } => {
                        st.seek(t0);
                        st.select(Selection::Track(track));
                    }
                    Drag::NewRegion { .. } => st.sel_changed = true,
                    // a note edit is heard from the next rack (built once, on release)
                    Drag::NoteMove { lane, .. } | Drag::NoteSize { lane, .. } => {
                        let notes = st.project.tracks[lane].notes.clone().unwrap_or_default();
                        st.set_notes(lane, notes);
                    }
                    _ => {}
                }
                self.drag = Drag::None;
                self.area.redraw(cx);
            }
            Hit::FingerScroll(se) => {
                let delta = if se.scroll.y.abs() > se.scroll.x.abs() { se.scroll.y } else { se.scroll.x };
                if se.modifiers.control || se.modifiers.logo {
                    // zoom around the pointer
                    let t = self.t_of(st, se.abs.x);
                    let min_pps = self.geo.w / st.duration().max(1.0);
                    st.pps = (st.pps * (-delta * 0.004).exp()).clamp(min_pps, 400.0);
                    st.scroll_t = t - (se.abs.x - self.geo.x0) / st.pps;
                } else {
                    st.scroll_t += delta / st.pps;
                }
                self.clamp_scroll(st);
                self.area.redraw(cx);
            }
            // Keys are the App's: shortcuts work wherever the focus is, except in a text field.
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle_with_area(&mut self.area, walk);
        if let Some(st) = scope.data_mut().get_mut::<EditorState>() {
            st.timeline_area = self.area;
        }
        self.draw_bg.draw_abs(cx, rect);
        if let Some(st) = scope.data_mut().get_mut::<EditorState>() {
            self.draw_all(cx, st, rect);
        }
        DrawStep::done()
    }
}
