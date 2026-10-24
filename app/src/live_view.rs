//! Live mode's screen: the two decks' readouts, loading songs onto them, the
//! controller map, and the `live` tool. The decks themselves are decks.rs.

use crate::decks::{DeckSong, Decks, DECKS, FX_BEATS, FX_NAMES};
use crate::deckmap::{Map, STEMS};
use crate::App;
use makepad_strict_json::{self as json, Value};
use makepad_widgets::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The beat FX are built once, the first time live mode starts.
static FX_BUILT: AtomicBool = AtomicBool::new(false);

fn clock(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0) as u64, t % 60.0)
}

fn db(v: f32) -> String {
    if v <= 1e-5 {
        "-inf".into()
    } else {
        format!("{:.0}", 20.0 * v.log10())
    }
}

/// An EQ knob as heard: kill, a dB value, or 0.
fn eq_text(v: f32) -> String {
    if v < 0.02 {
        "kill".into()
    } else if (v - 0.5).abs() < 0.01 {
        "0".into()
    } else {
        let g = if v <= 0.5 { (v * 2.0).powi(2) } else { 1.0 + (v - 0.5) * 2.0 * 0.995 };
        format!("{:+.0}", 20.0 * g.log10())
    }
}

fn filter_text(v: f32) -> &'static str {
    let c = v * 2.0 - 1.0;
    if c.abs() < 0.04 {
        "off"
    } else if c < 0.0 {
        "low-pass"
    } else {
        "high-pass"
    }
}

/// Deck 2's beat phase minus deck 1's (beats, -0.5..0.5).
pub fn phase_diff(decks: &Decks) -> f64 {
    (decks.deck[1].beat.get() - decks.deck[0].beat.get() + 0.5).rem_euclid(1.0) - 0.5
}

impl App {
    /// The controller map: the copy in Poing's config folder, else the built-in one.
    pub(crate) fn live_map(&mut self) -> Arc<Map> {
        if let Some(m) = &self.live_map {
            return m.clone();
        }
        let (map, from) = Map::load(crate::config_dir().as_deref());
        log!("poing: live: {from} (port `{}`, {} controls)", map.port, map.controls.len());
        let map = Arc::new(map);
        self.live_map = Some(map.clone());
        map
    }

    /// Put the song in front on deck `d`.
    fn load_deck(&mut self, d: usize) -> Option<String> {
        let st = self.state.as_ref()?;
        let song = Arc::new(DeckSong::of(st));
        let name = song.name.clone();
        if let Some(old) = st.shared.decks.load(d, song) {
            self.live_retired.push(old);
        }
        Some(name)
    }

    /// Live mode on or off. On: the editor's transport stops, empty decks get
    /// the song in front, the beat FX are built (once).
    pub(crate) fn show_live(&mut self, cx: &mut Cx, on: bool) {
        let Some(st) = self.state.as_ref() else { return };
        let shared = st.shared.clone();
        let sr = st.sr();
        if on {
            shared.set_playing(false);
            if !FX_BUILT.swap(true, Ordering::Relaxed) {
                match crate::decks::build_fx(sr) {
                    Ok(fx) => {
                        if let Ok(mut slot) = shared.decks.fx.lock() {
                            *slot = Some(Box::new(fx));
                        }
                    }
                    Err(e) => self.set_status(cx, &format!("The live effects do not compile: {e}")),
                }
            }
            shared.decks.set_ports(&self.live_ports.0, &self.live_ports.1);
            for d in 0..DECKS {
                if shared.decks.song(d).is_none() {
                    self.load_deck(d);
                }
            }
            let _ = self.live_map();
            shared.decks.on.store(true, Ordering::Relaxed);
            if self.live_pump.is_empty() {
                self.live_pump = cx.start_interval(0.05);
            }
        } else {
            shared.decks.on.store(false, Ordering::Relaxed);
            cx.stop_timer(self.live_pump);
            self.live_pump = Timer::empty();
        }
        self.live_shown = on;
        self.settings_shown = false;
        self.ui.view(cx, ids!(settings)).set_visible(cx, false);
        self.show_song_view(cx, true);
        self.refresh_live(cx);
    }

    /// Every 50 ms while live: the hardware Load button, songs the audio
    /// thread let go of, unmapped controller messages (logged), the readouts.
    pub(crate) fn pump_live(&mut self, cx: &mut Cx) {
        let Some(shared) = self.state.as_ref().map(|st| st.shared.clone()) else { return };
        for d in 0..DECKS {
            if shared.decks.deck[d].load.swap(false, Ordering::Relaxed) {
                if let Some(name) = self.load_deck(d) {
                    self.set_status(cx, &format!("Deck {}: {name}", d + 1));
                }
            }
        }
        if shared.decks.retired() {
            self.live_retired.clear();
        }
        while let Some(m) = shared.decks.unmapped.pop() {
            log!("poing: live: unmapped {:02X} {:02X} {:02X}", m[0], m[1], m[2]);
        }
        if self.live_shown {
            self.refresh_live(cx);
        }
    }

    fn refresh_live(&mut self, cx: &mut Cx) {
        let Some(st) = self.state.as_ref() else { return };
        let decks = &st.shared.decks;
        let ui = self.ui.clone();
        let ids = [
            [ids!(d0_title), ids!(d0_time), ids!(d0_info), ids!(d0_pads), ids!(d0_stems), ids!(d0_loop), ids!(d0_mix)],
            [ids!(d1_title), ids!(d1_time), ids!(d1_info), ids!(d1_pads), ids!(d1_stems), ids!(d1_loop), ids!(d1_mix)],
        ];
        for (d, deck) in decks.deck.iter().enumerate() {
            let song = decks.song(d);
            let t = deck.pos.get();
            let lines: [String; 7] = match &song {
                None => [format!("Deck {}", d + 1), "—".into(), "Press Load on the DUO to load the song in front".into(), String::new(), String::new(), String::new(), String::new()],
                Some(s) => {
                    let speed = deck.speed.get();
                    let playing = deck.playing.load(Ordering::Relaxed);
                    let shown_speed = if playing { speed } else { 1.0 + (deck.tempo.get() as f64 - 0.5) * 0.16 };
                    let section = s.section_at(t).map_or("", |(_, n)| n);
                    let mut info = format!("{section} · {:.1} bpm ({:+.1}%)", s.grid.bpm_at(t) * shown_speed, (shown_speed - 1.0) * 100.0);
                    info += if playing { " · playing" } else { " · stopped" };
                    if deck.sync.load(Ordering::Relaxed) {
                        info += " · SYNC";
                    }
                    let pads = (0..4).map(|i| match &s.hot[i] {
                        Some((_, name)) => format!("[{} {name}]", (b'A' + i as u8) as char),
                        None => format!("[{} —]", (b'A' + i as u8) as char),
                    });
                    let stems = STEMS.iter().enumerate().map(|(i, n)| format!("{} {n}", if deck.muted(i) { "○" } else { "●" }));
                    let beats = deck.loop_beats.get();
                    let size = if beats < 1.0 { format!("1/{:.0} beat", 1.0 / beats) } else { format!("{beats:.0} beats") };
                    let looping = if deck.loop_on.load(Ordering::Relaxed) { format!("Loop {size}, on") } else { format!("Loop {size}") };
                    let mix = format!(
                        "Trim {:+.0} dB · EQ H {} M {} L {} · Filter {} · Fader {:.0}% · Level {} dB",
                        (deck.trim.get() - 0.5) * 24.0,
                        eq_text(deck.eq_high.get()),
                        eq_text(deck.eq_mid.get()),
                        eq_text(deck.eq_low.get()),
                        filter_text(deck.color.get()),
                        deck.fader.get() * 100.0,
                        db(deck.meter.get())
                    );
                    [
                        format!("Deck {} · {}", d + 1, s.name),
                        format!("{} / {}", clock(t), clock(s.seconds())),
                        info,
                        format!("Pads {}", pads.collect::<Vec<_>>().join(" ")),
                        format!("Stems {}", stems.collect::<Vec<_>>().join("  ")),
                        looping,
                        mix,
                    ]
                }
            };
            for (id, text) in ids[d].iter().zip(lines.iter()) {
                ui.label(cx, *id).set_text(cx, text);
            }
        }
        ui.label(cx, ids!(live_bpm)).set_text(cx, &format!("{:.1}", decks.bpm.get()));
        let x = decks.xfader.get();
        let k = ((x * 10.0).round() as usize).min(10);
        ui.label(cx, ids!(live_xf)).set_text(cx, &format!("1  {}●{}  2", "-".repeat(k), "-".repeat(10 - k)));
        let beats = FX_BEATS[(decks.fx_beats.load(Ordering::Relaxed) as usize).min(FX_BEATS.len() - 1)];
        let fx = format!(
            "{} · {} beat · {:.0}% · {}",
            FX_NAMES[(decks.fx_kind.load(Ordering::Relaxed) as usize).min(1)],
            match beats {
                b if b == 0.25 => "1/4".to_string(),
                b if b == 0.5 => "1/2".to_string(),
                b if b == 0.75 => "3/4".to_string(),
                b => format!("{b:.0}"),
            },
            decks.fx_depth.get() * 100.0,
            if decks.fx_on.load(Ordering::Relaxed) { "on" } else { "off" }
        );
        ui.label(cx, ids!(live_fx)).set_text(cx, &fx);
        let port = if self.live_ports.0.is_empty() { "no OMNIS-DUO found: plug it in by USB (its PC port)" } else { "OMNIS-DUO connected" };
        ui.label(cx, ids!(live_status)).set_text(cx, port);
        ui.view(cx, ids!(live_view)).redraw(cx);
    }

    /// The `live` tool: turn live mode on or off, and both decks as they are.
    pub(crate) fn live_tool(&mut self, cx: &mut Cx, args: &[(String, Value)]) -> Result<(String, String), String> {
        if let Some(on) = crate::agent::get(args, "on").and_then(|v| v.as_bool()) {
            if on != self.live_shown {
                self.show_live(cx, on);
            }
        }
        let st = self.state.as_ref().ok_or("no song is open")?;
        let decks = &st.shared.decks;
        let f = |x: f64| Value::F64((x * 10000.0).round() / 10000.0);
        let deck = |d: usize| -> Value {
            let k = &decks.deck[d];
            let Some(s) = decks.song(d) else { return json::obj(vec![("loaded", Value::Bool(false))]) };
            let t = k.pos.get();
            let hot = s.hot.iter().map(|h| h.as_ref().map_or(Value::Null, |(at, n)| json::obj(vec![("name", json::s(n.clone())), ("t", f(*at))]))).collect();
            let stems = STEMS.iter().enumerate().map(|(i, n)| (*n, Value::Bool(!k.muted(i)))).collect();
            json::obj(vec![
                ("loaded", Value::Bool(true)),
                ("song", json::s(s.name.clone())),
                ("song_bpm", f(s.bpm)),
                ("playing", Value::Bool(k.playing.load(Ordering::Relaxed))),
                ("t", f(t)),
                ("beat", f(k.beat.get())),
                ("speed", f(k.speed.get())),
                ("bpm", f(s.grid.bpm_at(t) * k.speed.get())),
                ("tempo_fader", f(k.tempo.get() as f64)),
                ("sync", Value::Bool(k.sync.load(Ordering::Relaxed))),
                ("section", s.section_at(t).map_or(Value::Null, |(_, n)| json::s(n.to_string()))),
                ("cue", f(k.cue.get())),
                ("hot", Value::Arr(hot)),
                ("stems_playing", json::obj(stems)),
                ("loop", json::obj(vec![("on", Value::Bool(k.loop_on.load(Ordering::Relaxed))), ("beats", f(k.loop_beats.get() as f64))])),
                ("eq", json::obj(vec![("high", f(k.eq_high.get() as f64)), ("mid", f(k.eq_mid.get() as f64)), ("low", f(k.eq_low.get() as f64))])),
                ("color", f(k.color.get() as f64)),
                ("fader", f(k.fader.get() as f64)),
                ("level_db", json::s(db(k.meter.get()))),
            ])
        };
        let data = json::obj(vec![
            ("on", Value::Bool(decks.on())),
            ("controller", Value::Bool(!self.live_ports.0.is_empty())),
            ("bpm", f(decks.bpm.get())),
            ("xfader", f(decks.xfader.get() as f64)),
            (
                "fx",
                json::obj(vec![
                    ("on", Value::Bool(decks.fx_on.load(Ordering::Relaxed))),
                    ("kind", json::s(FX_NAMES[(decks.fx_kind.load(Ordering::Relaxed) as usize).min(1)].to_string())),
                    ("beats", f(FX_BEATS[(decks.fx_beats.load(Ordering::Relaxed) as usize).min(FX_BEATS.len() - 1)] as f64)),
                    ("depth", f(decks.fx_depth.get() as f64)),
                ]),
            ),
            ("phase_diff_beats", f(phase_diff(decks))),
            ("decks", Value::Arr(vec![deck(0), deck(1)])),
        ]);
        let line = if decks.on() { format!("live: {:.1} bpm, phase difference {:+.3} beat", decks.bpm.get(), phase_diff(decks)) } else { "live mode is off".into() };
        Ok((line, data.to_json()))
    }
}
