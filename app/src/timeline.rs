//! The arrangement view: ruler, sections lane, one lane per stem (header with
//! M/S, waveform, regions) and the playhead. Immediate-mode rects and text;
//! all mouse and keyboard editing of the arrangement happens here.

use crate::audio::PEAK_BLOCK;
use crate::model::REGION_KINDS;
use crate::state::{EditorState, Selection};
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.DawTimelineBase = #(DawTimeline::register_widget(vm))

    mod.widgets.DawTimeline = set_type_default() do mod.widgets.DawTimelineBase{
        width: Fill
        height: Fill
        draw_bg +: {color: #x16181d}
        draw_text +: {color: #xd8dce4}
    }
}

const HEADER_W: f64 = 200.0;
const RULER_H: f64 = 22.0;
const SECT_H: f64 = 26.0;
const GAP: f64 = 6.0;
const EDGE_PX: f64 = 6.0;

#[derive(Clone, Copy, Default)]
struct Geo {
    rect: Rect,
    x0: f64,
    w: f64,
    sect_y: f64,
    lanes_y: f64,
    lane_h: f64,
    n: usize,
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
    draw_text: DrawText,
    #[rust]
    area: Area,
    #[rust]
    geo: Geo,
    #[rust]
    drag: Drag,
    #[rust]
    next_frame: NextFrame,
}

fn rgba(hex: u32, a: f32) -> Vec4f {
    vec4(((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0, a)
}

const TRACK_COLORS: [u32; 8] = [0xe8a54b, 0xb39ddb, 0x4fc3f7, 0x26a69a, 0xef5350, 0x9ccc65, 0xf06292, 0xffd54f];

fn region_color(kind: &str) -> u32 {
    match kind {
        "bad" => 0xff5252,
        "regenerate" => 0xffab40,
        "too_loud" => 0xff4081,
        "too_quiet" => 0x40c4ff,
        "mute" => 0x9e9e9e,
        _ => 0x69f0ae,
    }
}

fn section_color(label: &str) -> u32 {
    let l = label.to_lowercase();
    if l.starts_with("chorus") {
        0xc0703a
    } else if l.starts_with("verse") {
        0x2f7f78
    } else if l.starts_with("bridge") {
        0x7b5ea7
    } else if l.starts_with("inst") || l.starts_with("drop") || l.starts_with("break") {
        0x4f8a3c
    } else {
        0x4a5568
    }
}

fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u32, t % 60.0)
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
        self.geo.lanes_y + i as f64 * self.geo.lane_h
    }
    fn lane_at(&self, y: f64) -> Option<usize> {
        if y < self.geo.lanes_y {
            return None;
        }
        let i = ((y - self.geo.lanes_y) / self.geo.lane_h) as usize;
        (i < self.geo.n).then_some(i)
    }
    fn in_ruler(&self, y: f64) -> bool {
        y >= self.geo.rect.pos.y && y < self.geo.sect_y
    }
    fn in_sections(&self, y: f64) -> bool {
        y >= self.geo.sect_y && y < self.geo.sect_y + SECT_H
    }
    fn mute_box(&self, lane: usize) -> Rect {
        Rect { pos: dvec2(self.geo.rect.pos.x + HEADER_W - 62.0, self.lane_y(lane) + 8.0), size: dvec2(24.0, 18.0) }
    }
    fn solo_box(&self, lane: usize) -> Rect {
        Rect { pos: dvec2(self.geo.rect.pos.x + HEADER_W - 34.0, self.lane_y(lane) + 8.0), size: dvec2(24.0, 18.0) }
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
    fn outline(&mut self, cx: &mut Cx2d, x: f64, y: f64, w: f64, h: f64, color: Vec4f) {
        self.rect(cx, x, y, w, 2.0, color);
        self.rect(cx, x, y + h - 2.0, w, 2.0, color);
        self.rect(cx, x, y, 2.0, h, color);
        self.rect(cx, x + w - 2.0, y, 2.0, h, color);
    }
    fn text(&mut self, cx: &mut Cx2d, x: f64, y: f64, color: Vec4f, s: &str) {
        self.draw_text.color = color;
        self.draw_text.draw_abs(cx, dvec2(x, y), s);
    }

    fn draw_all(&mut self, cx: &mut Cx2d, st: &mut EditorState, rect: Rect) {
        let n = st.project.tracks.len();
        let lanes_y = rect.pos.y + RULER_H + SECT_H + GAP;
        let lane_h = ((rect.size.y - (lanes_y - rect.pos.y)) / n.max(1) as f64).clamp(40.0, 140.0);
        self.geo = Geo {
            rect,
            x0: rect.pos.x + HEADER_W,
            w: (rect.size.x - HEADER_W).max(10.0),
            sect_y: rect.pos.y + RULER_H,
            lanes_y,
            lane_h,
            n,
        };
        if st.pps <= 0.0 && st.duration() > 0.0 {
            st.pps = self.geo.w / st.duration();
            st.scroll_t = 0.0;
        }
        self.draw_text.text_style.font_size = 8.5;
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
        let white = rgba(0xd8dce4, 1.0);
        let dim = rgba(0x8a93a6, 1.0);

        // ruler: a tick every 1/2/5/10/15/30/60 s, whichever is >= 60 px apart
        self.rect(cx, rect.pos.x, rect.pos.y, rect.size.x, RULER_H, rgba(0x0f1115, 1.0));
        let step = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0].into_iter().find(|s| s * st.pps >= 60.0).unwrap_or(60.0);
        let mut t = (st.scroll_t / step).floor() * step;
        while t <= st.scroll_t + w / st.pps {
            let x = self.x_of(st, t);
            if x >= x0 {
                self.rect(cx, x, rect.pos.y + 12.0, 1.0, RULER_H - 12.0, dim);
                self.text(cx, x + 3.0, rect.pos.y + 3.0, dim, fmt_time(t).trim_end_matches(".0"));
            }
            t += step;
        }
        let clock = format!("{}  /  {}", fmt_time(ph), fmt_time(st.duration()));
        self.text(cx, rect.pos.x + 8.0, rect.pos.y + 4.0, white, &clock);

        // sections
        let sy = self.geo.sect_y;
        self.text(cx, rect.pos.x + 8.0, sy + 7.0, dim, "Sections");
        for i in 0..st.project.sections().len() {
            let s = st.project.sections()[i].clone();
            if let Some((x, sw)) = self.span(st, s.start, s.end) {
                self.rect(cx, x, sy + 2.0, sw - 1.0, SECT_H - 4.0, rgba(section_color(&s.label), 0.85));
                if sw > 30.0 {
                    self.text(cx, x + 5.0, sy + 7.0, white, &s.label);
                }
                if st.sel == Selection::Section(i) {
                    self.outline(cx, x, sy + 2.0, sw - 1.0, SECT_H - 4.0, rgba(0xffffff, 0.9));
                }
            }
        }

        // lanes
        let gains = st.project.track_gains();
        for i in 0..n {
            let ly = self.lane_y(i);
            let tr = st.project.tracks[i].clone();
            let col = TRACK_COLORS[i % TRACK_COLORS.len()];
            let audible = gains[i] > 0.0;
            self.rect(cx, rect.pos.x, ly, rect.size.x, lane_h - 1.0, rgba(if i % 2 == 0 { 0x1c1f26 } else { 0x191b21 }, 1.0));
            let hsel = st.sel == Selection::Track(i);
            self.rect(cx, rect.pos.x, ly, HEADER_W - 2.0, lane_h - 1.0, rgba(if hsel { 0x2c3340 } else { 0x22262e }, 1.0));
            self.rect(cx, rect.pos.x, ly, 4.0, lane_h - 1.0, rgba(col, if audible { 1.0 } else { 0.3 }));
            self.text(cx, rect.pos.x + 12.0, ly + 9.0, if audible { white } else { dim }, &tr.name);
            self.text(cx, rect.pos.x + 12.0, ly + 27.0, dim, &format!("{:+.1} dB", tr.gain()));
            let (mb, sb) = (self.mute_box(i), self.solo_box(i));
            self.rect(cx, mb.pos.x, mb.pos.y, mb.size.x, mb.size.y, rgba(if tr.muted() { 0xe53935 } else { 0x343a46 }, 1.0));
            self.text(cx, mb.pos.x + 8.0, mb.pos.y + 3.0, white, "M");
            self.rect(cx, sb.pos.x, sb.pos.y, sb.size.x, sb.size.y, rgba(if tr.soloed() { 0xfdd835 } else { 0x343a46 }, 1.0));
            self.text(cx, sb.pos.x + 8.0, sb.pos.y + 3.0, if tr.soloed() { rgba(0x111111, 1.0) } else { white }, "S");

            // waveform: min/max of the peak blocks under each pixel column
            let stem = &st.stems[i];
            let scale = (lane_h * 0.46) / stem.peak_abs as f64;
            let mid = ly + lane_h * 0.5;
            let sr = st.sr() as f64;
            let wave_col = rgba(col, if audible { 0.85 } else { 0.25 });
            let mut cols: Vec<(f64, f64, f64)> = Vec::with_capacity(w as usize);
            for px in 0..(w as usize) {
                let t0 = st.scroll_t + px as f64 / st.pps;
                if t0 >= st.duration() {
                    break;
                }
                let b0 = (t0 * sr) as usize / PEAK_BLOCK;
                let b1 = (((t0 + 1.0 / st.pps) * sr) as usize / PEAK_BLOCK).max(b0 + 1).min(stem.peaks.len());
                if b0 >= b1 {
                    break;
                }
                let (mut mn, mut mx) = (f32::MAX, f32::MIN);
                for &(a, b) in &stem.peaks[b0..b1] {
                    mn = mn.min(a);
                    mx = mx.max(b);
                }
                cols.push((x0 + px as f64, mid - mx as f64 * scale, ((mx - mn) as f64 * scale).max(1.0)));
            }
            for (x, y, h) in cols {
                self.rect(cx, x, y, 1.0, h, wave_col);
            }
        }

        // regions over the lanes
        for ri in 0..st.project.regions().len() {
            let r = st.project.regions()[ri].clone();
            let Some(lane) = st.project.track_index(&r.track) else { continue };
            let ly = self.lane_y(lane);
            if let Some((x, rw)) = self.span(st, r.start, r.end) {
                let c = region_color(&r.kind);
                self.rect(cx, x, ly + 2.0, rw, lane_h - 5.0, rgba(c, 0.22));
                self.rect(cx, x, ly + 2.0, rw, 14.0, rgba(c, 0.75));
                let label = match r.kind.as_str() {
                    "bad" | "regenerate" | "mute" => r.kind.clone(),
                    _ => format!("{} {:+.1}dB", r.kind, r.gain()),
                };
                if rw > 24.0 {
                    self.text(cx, x + 4.0, ly + 3.0, rgba(0x111111, 1.0), &label);
                }
                if st.sel == Selection::Region(ri) {
                    self.outline(cx, x, ly + 2.0, rw, lane_h - 5.0, rgba(0xffffff, 0.95));
                }
            }
        }

        // playhead
        let px = self.x_of(st, ph);
        if px >= x0 && px <= x0 + w {
            self.rect(cx, px, rect.pos.y, 2.0, rect.size.y, rgba(0xffffff, 0.9));
        }
    }
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
                if self.in_ruler(y) {
                    st.seek(self.t_of(st, x));
                    self.drag = Drag::Seek;
                } else if self.in_sections(y) {
                    if let Some(edge) = self.section_edge_at(st, x) {
                        self.drag = Drag::SectionEdge { index: edge };
                    } else if in_wave {
                        let t = self.t_of(st, x);
                        if let Some(i) = st.project.sections().iter().position(|s| t >= s.start && t < s.end) {
                            let start = st.project.sections()[i].start;
                            st.select(Selection::Section(i));
                            st.seek(start);
                        }
                    }
                } else if let Some(lane) = self.lane_at(y) {
                    if !in_wave {
                        if self.mute_box(lane).contains(fe.abs) {
                            let t = &mut st.project.tracks[lane];
                            t.mute = Some(!t.muted());
                            st.push_mix();
                        } else if self.solo_box(lane).contains(fe.abs) {
                            let t = &mut st.project.tracks[lane];
                            t.solo = Some(!t.soloed());
                            st.push_mix();
                        }
                        st.select(Selection::Track(lane));
                    } else if let Some((ri, part)) = self.region_at(st, lane, x) {
                        st.select(Selection::Region(ri));
                        self.drag = match part {
                            1 => Drag::ResizeRegion { region: ri, left: true },
                            2 => Drag::ResizeRegion { region: ri, left: false },
                            _ => Drag::MoveRegion { region: ri, grab: self.t_of(st, x) - st.project.regions()[ri].start },
                        };
                    } else {
                        self.drag = Drag::Pending { track: lane, t0: self.t_of(st, x), x0: x };
                    }
                }
                self.area.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                let x = fe.abs.x;
                let t = self.t_of(st, x);
                match self.drag {
                    Drag::Seek => st.seek(t),
                    Drag::Pending { track, t0, x0 } if (x - x0).abs() > 4.0 => {
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
            Hit::KeyDown(ke) => {
                match ke.key_code {
                    KeyCode::Space => st.toggle_play(),
                    KeyCode::Delete | KeyCode::Backspace => st.delete_selected(),
                    KeyCode::Home => st.seek(0.0),
                    _ => {}
                }
                self.area.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle_with_area(&mut self.area, walk);
        self.draw_bg.draw_abs(cx, rect);
        if let Some(st) = scope.data_mut().get_mut::<EditorState>() {
            self.draw_all(cx, st, rect);
        }
        DrawStep::done()
    }
}
