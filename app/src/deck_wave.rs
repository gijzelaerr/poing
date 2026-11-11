//! A deck's waveform in live mode: the song scrolling under a fixed playhead
//! (four seconds either side), the stems each in their colour, with the beat
//! grid, the hot cues, the sections and the loop; under it the whole song
//! with where the deck is. It reads the deck the audio thread publishes. A
//! click on either part sends the deck there.

use crate::audio::PEAK_BLOCK;
use crate::decks::{Cmd, DeckSong, DECKS};
use crate::state::EditorState;
use makepad_widgets::*;
use std::sync::atomic::Ordering;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.DeckWaveBase = #(DeckWave::register_widget(vm))

    mod.widgets.DeckWave = set_type_default() do mod.widgets.DeckWaveBase{
        width: Fill
        height: 150
        draw_bg +: {color: #x0f1013}
        draw_text +: {
            color: #xeceef2
            text_style: theme.font_regular{font_size: 8.0}
        }
    }
}

/// Seconds of the song across the scrolling part.
const SPAN_S: f64 = 8.0;
/// The whole-song strip's height, and the gap above it.
const OVERVIEW_H: f64 = 22.0;
const GAP: f64 = 6.0;
/// The stems' colours (vocals, drums, bass, other).
const COLORS: [u32; 4] = [0xffb454, 0x5ad1e6, 0xc38bff, 0x6fcf7a];

fn rgba(hex: u32, a: f32) -> Vec4f {
    vec4(((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0, a)
}

/// The song time a click at `at` asks for, in a waveform drawn in `rect`
/// with the playhead at `t` of a song `seconds` long: the scrolling part is
/// seconds either side of the middle, the strip under it the whole song.
/// None in the gap between them, or past the song's ends.
pub fn time_at(rect: Rect, at: DVec2, t: f64, seconds: f64) -> Option<f64> {
    let w = rect.size.x.floor().max(1.0);
    let h = (rect.size.y - OVERVIEW_H - GAP).max(20.0);
    let (x, y) = (at.x - rect.pos.x, at.y - rect.pos.y);
    if x < 0.0 || x > w || y < 0.0 {
        return None;
    }
    let to = if y < h {
        t + (x - w * 0.5) * SPAN_S / w
    } else if y >= h + GAP && y <= h + GAP + OVERVIEW_H {
        x / w * seconds
    } else {
        return None;
    };
    (0.0..=seconds).contains(&to).then_some(to)
}

/// The lanes of a deck's song that make sound: (stem index, level, the stem
/// it mutes with). A whole-song take stands in for all of them (as "other").
pub fn lanes(song: &DeckSong) -> Vec<(usize, f32, usize)> {
    if let Some(k) = song.mix.song_take() {
        return vec![(k.stem, 1.0, 3)];
    }
    (0..song.mix.gains.len().min(song.stems.len()))
        .filter(|i| song.mix.gains[*i] > 0.0 && !song.stems[*i].peaks.is_empty())
        .map(|i| (i, song.mix.gains[i], song.role.get(i).copied().unwrap_or(3).min(3) as usize))
        .collect()
}

/// How far the lanes swing over song time [t0, t1) (0 when nothing is there):
/// the largest of their peaks, each at its level, for the stems `shown` picks.
pub fn swing(song: &DeckSong, lanes: &[(usize, f32, usize)], t0: f64, t1: f64, shown: &dyn Fn(usize) -> bool) -> f32 {
    let sr = song.sr as f64;
    let mut most = 0.0f32;
    for &(stem, gain, role) in lanes {
        if !shown(role) {
            continue;
        }
        let peaks = &song.stems[stem].peaks;
        let b0 = (t0.max(0.0) * sr) as usize / PEAK_BLOCK;
        let b1 = (((t1.max(0.0) * sr) as usize).div_ceil(PEAK_BLOCK)).max(b0 + 1).min(peaks.len());
        for &(lo, hi) in peaks.get(b0..b1).unwrap_or(&[]) {
            most = most.max(lo.abs().max(hi.abs()) * gain);
        }
    }
    most
}

#[derive(Script, ScriptHook, Widget)]
pub struct DeckWave {
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
    /// Which deck: 0 or 1.
    #[live]
    deck: f64,
    #[rust]
    area: Area,
    #[rust]
    next_frame: NextFrame,
    /// The whole-song strip, a level per pixel column, for the song and width it was made for.
    #[rust]
    overview: Vec<f32>,
    #[rust]
    overview_of: (usize, usize),
    /// Where it was last drawn.
    #[rust]
    rect: Rect,
}

impl DeckWave {
    fn rect(&mut self, cx: &mut Cx2d, x: f64, y: f64, w: f64, h: f64, color: Vec4f) {
        self.draw_rect.color = color;
        self.draw_rect.draw_abs(cx, Rect { pos: dvec2(x, y), size: dvec2(w.max(1.0), h.max(1.0)) });
    }

    fn text(&mut self, cx: &mut Cx2d, x: f64, y: f64, color: Vec4f, s: &str) {
        self.draw_text.color = color;
        self.draw_text.draw_abs(cx, dvec2(x, y), s);
    }

    fn draw_deck(&mut self, cx: &mut Cx2d, st: &EditorState, rect: Rect) {
        let decks = &st.shared.decks;
        let d = (self.deck as usize).min(DECKS - 1);
        let Some(song) = decks.song(d) else { return };
        let deck = &decks.deck[d];
        let t = deck.pos.get();
        let (x0, w) = (rect.pos.x, rect.size.x.floor().max(1.0));
        let h = (rect.size.y - OVERVIEW_H - GAP).max(20.0);
        let (top, mid) = (rect.pos.y, rect.pos.y + h * 0.5);
        let pps = w / SPAN_S;
        let x_of = |at: f64| x0 + w * 0.5 + (at - t) * pps;
        let (from, to) = (t - SPAN_S * 0.5, t + SPAN_S * 0.5);
        let lanes = lanes(&song);
        // the loudest lane fills the height
        let loudest = lanes.iter().map(|&(s, g, _)| song.stems[s].peak_abs * g).fold(1e-6f32, f32::max);
        let scale = h * 0.47 / loudest as f64;

        // the loop, behind everything
        if deck.loop_on.load(Ordering::Relaxed) {
            let a = deck.loop_start.get();
            let b = song.grid.time_at(song.grid.beat_at(a) + deck.loop_beats.get() as f64);
            let (xa, xb) = (x_of(a).max(x0), x_of(b).min(x0 + w));
            if xb > xa {
                self.rect(cx, xa, top, xb - xa, h, rgba(0x4f8cff, 0.22));
            }
        }
        // the beats: every fourth one a little brighter
        let mut k = song.grid.beat_at(from).ceil();
        while song.grid.time_at(k) < to {
            let at = song.grid.time_at(k);
            if at >= 0.0 && at <= song.seconds() {
                let bar = (k as i64).rem_euclid(4) == 0;
                self.rect(cx, x_of(at).floor(), top, 1.0, h, rgba(0xffffff, if bar { 0.22 } else { 0.09 }));
            }
            k += 1.0;
        }
        // the stems, a column a pixel; a muted stem stays as a shadow
        for px in 0..w as usize {
            let t0 = from + px as f64 / pps;
            if t0 < 0.0 || t0 >= song.seconds() {
                continue;
            }
            // the widest first, so each stem shows where it is the quieter one too
            let mut cols = [0usize, 1, 2, 3].map(|role| (swing(&song, &lanes, t0, t0 + 1.0 / pps, &|r| r == role), role));
            cols.sort_by(|a, b| b.0.total_cmp(&a.0));
            for (v, role) in cols {
                if v <= 0.0 {
                    continue;
                }
                let half = (v as f64 * scale).min(h * 0.5);
                let alpha = if deck.muted(role) { 0.12 } else { 0.9 };
                self.rect(cx, x0 + px as f64, mid - half, 1.0, half * 2.0, rgba(COLORS[role], alpha));
            }
        }
        // the sections' names along the bottom, the hot cues along the top
        for (at, name) in &song.sections {
            if *at >= from && *at < to {
                self.rect(cx, x_of(*at).floor(), top, 1.0, h, rgba(0xffffff, 0.3));
                self.text(cx, x_of(*at) + 4.0, top + h - 13.0, rgba(0xeceef2, 0.75), name);
            }
        }
        for (i, hot) in song.hot.iter().enumerate() {
            let Some((at, _)) = hot else { continue };
            if *at >= from && *at < to {
                let x = x_of(*at).floor();
                self.rect(cx, x, top, 2.0, h, rgba(0xff5f6d, 0.9));
                self.rect(cx, x, top, 13.0, 13.0, rgba(0xff5f6d, 0.9));
                self.text(cx, x + 3.0, top + 1.0, rgba(0x0f1013, 1.0), &((b'A' + i as u8) as char).to_string());
            }
        }
        let cue = deck.cue.get();
        if cue >= from && cue < to {
            self.rect(cx, x_of(cue).floor(), top + h - 6.0, 2.0, 6.0, rgba(0xffd166, 1.0));
        }
        // the playhead
        self.rect(cx, x0 + (w * 0.5).floor() - 1.0, top, 2.0, h, rgba(0xffffff, 0.95));

        // the whole song, and where the deck is in it
        let oy = top + h + GAP;
        // (by the deck's load count: another song may get the same address)
        let key = (deck.gen.load(Ordering::Relaxed) as usize, w as usize);
        if self.overview_of != key {
            let per = song.seconds() / w;
            self.overview = (0..w as usize).map(|px| swing(&song, &lanes, px as f64 * per, (px + 1) as f64 * per, &|_| true)).collect();
            self.overview_of = key;
        }
        let most = self.overview.iter().fold(1e-6f32, |m, v| m.max(*v));
        let at = if song.seconds() > 0.0 { (t / song.seconds()).clamp(0.0, 1.0) * w } else { 0.0 };
        for px in 0..w as usize {
            let half = (self.overview[px] / most) as f64 * OVERVIEW_H * 0.5;
            let color = if (px as f64) < at { rgba(0x8a90a0, 0.45) } else { rgba(0xc9cedb, 0.9) };
            self.rect(cx, x0 + px as f64, oy + OVERVIEW_H * 0.5 - half, 1.0, half * 2.0, color);
        }
        for hot in song.hot.iter().flatten() {
            self.rect(cx, x0 + (hot.0 / song.seconds().max(1e-6) * w).floor(), oy, 2.0, 5.0, rgba(0xff5f6d, 0.9));
        }
        self.rect(cx, x0 + at.floor(), oy, 2.0, OVERVIEW_H, rgba(0xffffff, 0.95));

        // it scrolls while the deck moves: playing, or under the hand
        if decks.on() && (deck.playing.load(Ordering::Relaxed) || deck.touch.load(Ordering::Relaxed)) {
            self.next_frame = cx.new_next_frame();
        }
    }
}

impl Widget for DeckWave {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.next_frame.is_event(event).is_some() {
            self.area.redraw(cx);
        }
        if let Hit::FingerDown(fe) = event.hits(cx, self.area) {
            let Some(st) = scope.data_mut().get_mut::<EditorState>() else { return };
            let decks = &st.shared.decks;
            let d = (self.deck as usize).min(DECKS - 1);
            let Some(song) = decks.song(d) else { return };
            if let Some(to) = time_at(self.rect, fe.abs, decks.deck[d].pos.get(), song.seconds()) {
                decks.deck[d].send(Cmd::Jump(to));
                self.area.redraw(cx);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle_with_area(&mut self.area, walk);
        self.rect = rect;
        self.draw_bg.draw_abs(cx, rect);
        if let Some(st) = scope.data_mut().get_mut::<EditorState>() {
            self.draw_deck(cx, st, rect);
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{MixParams, Stem, Stems};
    use crate::decks::Grid;
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_click_on_the_waveform_is_a_time_in_the_song() {
        // 800 wide, 150 high: the scrolling part is the top 122, the strip the bottom 22
        let rect = Rect { pos: dvec2(100.0, 50.0), size: dvec2(800.0, 150.0) };
        let at = |x: f64, y: f64| time_at(rect, dvec2(100.0 + x, 50.0 + y), 60.0, 200.0);
        assert_eq!(at(400.0, 30.0), Some(60.0), "the middle is where the deck is");
        assert_eq!(at(600.0, 30.0), Some(62.0), "a quarter of the width is two seconds");
        assert_eq!(at(0.0, 30.0), Some(56.0));
        assert_eq!(at(400.0, 125.0), None, "the gap between the two");
        assert_eq!(at(400.0, 140.0), Some(100.0), "the strip is the whole song");
        assert_eq!(at(800.0, 140.0), Some(200.0));
        assert_eq!(at(900.0, 30.0), None);
        // near the song's start the left of the scrolling part is before it
        assert_eq!(time_at(rect, dvec2(110.0, 80.0), 1.0, 200.0), None);
    }

    #[test]
    fn the_waveform_shows_the_lanes_that_sound_at_their_levels() {
        let sr = 48000;
        // a second of 0.5 then a second of 0.1 on the vocals; drums at 0.8 all through, at half level; a muted lane
        let mut v = vec![0.5f32; sr as usize];
        v.extend(vec![0.1f32; sr as usize]);
        let lane = |v: Vec<f32>| Arc::new(Stem::new(v.clone(), v));
        let stems: Stems = Arc::new(vec![lane(v), lane(vec![0.8; 2 * sr as usize]), lane(vec![0.9; 2 * sr as usize])]);
        let song = DeckSong {
            name: "test".into(),
            stems,
            mix: MixParams { version: 1, gains: vec![1.0, 0.5, 0.0], ..Default::default() },
            sr,
            frames: 2 * sr as usize,
            grid: Grid::new(vec![0.0, 0.5, 1.0, 1.5, 2.0]),
            bpm: 120.0,
            hot: [None, None, None, None],
            sections: Vec::new(),
            role: vec![0, 1, 3],
            rack: Mutex::new(None),
            missing: Vec::new(),
        };
        let lanes = lanes(&song);
        assert_eq!(lanes.iter().map(|l| (l.0, l.2)).collect::<Vec<_>>(), [(0, 0), (1, 1)], "the muted lane is left out");
        let vocals = |t0, t1| swing(&song, &lanes, t0, t1, &|r| r == 0);
        assert!((vocals(0.2, 0.3) - 0.5).abs() < 1e-6 && (vocals(1.5, 1.6) - 0.1).abs() < 1e-6);
        assert!((swing(&song, &lanes, 0.2, 0.3, &|r| r == 1) - 0.4).abs() < 1e-6, "the drums at half their 0.8");
        assert!((swing(&song, &lanes, 1.5, 1.6, &|_| true) - 0.4).abs() < 1e-6, "the loudest of them");
        assert_eq!(swing(&song, &lanes, 5.0, 5.1, &|_| true), 0.0, "past the end");
    }
}
