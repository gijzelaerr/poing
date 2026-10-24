//! The live controller's mapping, a data file (`app/live/omnis-duo.map`):
//! which MIDI message is which deck control, read into a table the MIDI
//! thread looks messages up in. See docs/LIVE.md for the format.

use std::path::Path;

/// The mapping Poing ships with.
pub const BUILT_IN: &str = include_str!("../live/omnis-duo.map");
/// Its name in Poing's config folder, where a copy overrides it.
pub const FILE: &str = "omnis-duo.map";

/// What a control does. Pads carry their cue (1..=4), loop length in beats
/// or stem; `Section` and `FxBeats` carry their direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    Play,
    Cue,
    Start,
    Sync,
    Load,
    HotCue(u8),
    Loop(f32),
    Mute(u8),
    Section(i8),
    LoopToggle,
    LoopSize,
    Touch,
    Scratch,
    Nudge,
    Tempo,
    Level,
    Trim,
    EqHigh,
    EqMid,
    EqLow,
    Color,
    Fader,
    XFader,
    FxOn,
    FxSelect,
    FxDepth,
    FxBeats(i8),
}

/// The four stems a deck mutes, in pad order.
pub const STEMS: [&str; 4] = ["vocals", "drums", "bass", "other"];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Button,
    Relative,
    Absolute,
    Output,
}

impl Act {
    fn kind(self) -> Kind {
        use Act::*;
        match self {
            LoopSize | Scratch | Nudge => Kind::Relative,
            Tempo | Trim | EqHigh | EqMid | EqLow | Color | Fader | XFader | FxDepth => Kind::Absolute,
            Level => Kind::Output,
            _ => Kind::Button,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    /// Status byte with its channel (a note-off is looked up as its note-on).
    pub status: u8,
    pub data1: u8,
    pub act: Act,
    /// 0 or 1 (0 for the master controls).
    pub deck: usize,
    pub hires: bool,
    pub invert: bool,
    pub led: bool,
}

/// What a message said, in the control's own terms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Msg {
    Down,
    Up,
    /// 0..1
    Value(f32),
    /// Steps of a relative control, + clockwise.
    Steps(i32),
}

#[derive(Clone, Debug)]
pub struct Map {
    /// MIDI ports whose name contains this (any case) play the decks.
    pub port: String,
    pub controls: Vec<Control>,
}

/// A 14-bit fader's last MSB, per channel and CC.
pub struct Decoder {
    msb: [[u8; 128]; 16],
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder { msb: [[0; 128]; 16] }
    }
}

fn hex(word: &str) -> Option<u8> {
    u8::from_str_radix(word, 16).ok()
}

impl Map {
    /// Parse a map; a bad line is an error naming it.
    pub fn parse(text: &str) -> Result<Map, String> {
        let mut map = Map { port: String::new(), controls: Vec::new() };
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let words: Vec<&str> = line.split_whitespace().collect();
            if words[0] == "port" {
                map.port = words[1..].join(" ");
                continue;
            }
            let bad = |why: &str| format!("{FILE} line {}: {why}: {line}", n + 1);
            let (Some(status), Some(data1)) = (hex(words[0]), words.get(1).and_then(|w| hex(w))) else {
                return Err(bad("expected two hex bytes first"));
            };
            if status < 0x80 || data1 > 0x7f {
                return Err(bad("not a MIDI status and data byte"));
            }
            let Some(&name) = words.get(2) else { return Err(bad("no action")) };
            let mut rest = words[3..].iter().copied().peekable();
            // the deck, for the actions that have one
            let master = matches!(name, "xfader" | "fx_on" | "fx_select" | "fx_depth" | "fx_beats");
            let deck = if master {
                0
            } else {
                match rest.next() {
                    Some("1") => 0,
                    Some("2") => 1,
                    _ => return Err(bad("the deck (1 or 2) is missing")),
                }
            };
            let mut arg = || rest.next().ok_or_else(|| bad("the argument is missing"));
            let act = match name {
                "play" => Act::Play,
                "cue" => Act::Cue,
                "start" => Act::Start,
                "sync" => Act::Sync,
                "load" => Act::Load,
                "hotcue" => Act::HotCue(arg()?.parse().ok().filter(|c| (1..=4).contains(c)).ok_or_else(|| bad("hot cues are 1 to 4"))?),
                "loop" => Act::Loop(arg()?.parse().ok().filter(|b: &f32| *b > 0.0 && *b <= 32.0).ok_or_else(|| bad("a loop is 1/32 to 32 beats"))?),
                "mute" => {
                    let stem = arg()?;
                    Act::Mute(STEMS.iter().position(|s| *s == stem).ok_or_else(|| bad("the stems are vocals, drums, bass and other"))? as u8)
                }
                "section" => Act::Section(if arg()?.starts_with('-') { -1 } else { 1 }),
                "loop_toggle" => Act::LoopToggle,
                "loop_size" => Act::LoopSize,
                "touch" => Act::Touch,
                "scratch" => Act::Scratch,
                "nudge" => Act::Nudge,
                "tempo" => Act::Tempo,
                "level" => Act::Level,
                "trim" => Act::Trim,
                "eq_high" => Act::EqHigh,
                "eq_mid" => Act::EqMid,
                "eq_low" => Act::EqLow,
                "color" => Act::Color,
                "fader" => Act::Fader,
                "xfader" => Act::XFader,
                "fx_on" => Act::FxOn,
                "fx_select" => Act::FxSelect,
                "fx_depth" => Act::FxDepth,
                "fx_beats" => Act::FxBeats(if arg()?.starts_with('-') { -1 } else { 1 }),
                _ => return Err(bad("unknown action")),
            };
            let mut c = Control { status: if status & 0xf0 == 0x80 { status | 0x10 } else { status }, data1, act, deck, hires: false, invert: false, led: true };
            for opt in rest {
                match opt {
                    "hires" => c.hires = true,
                    "invert" => c.invert = true,
                    "noled" => c.led = false,
                    _ => return Err(bad("unknown option (hires, invert, noled)")),
                }
            }
            map.controls.push(c);
        }
        if map.port.is_empty() {
            return Err(format!("{FILE}: no `port` line"));
        }
        Ok(map)
    }

    /// The map in Poing's config folder if there is one (falling back to the
    /// built-in one when it does not parse), else the built-in one; and what
    /// to log about it.
    pub fn load(config: Option<&Path>) -> (Map, String) {
        let built_in = || Map::parse(BUILT_IN).expect("the built-in map parses");
        let Some(path) = config.map(|d| d.join(FILE)).filter(|p| p.exists()) else {
            return (built_in(), "the built-in OMNIS-DUO map".into());
        };
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| Map::parse(&t)) {
            Ok(m) => (m, format!("the map in {}", path.display())),
            Err(e) => (built_in(), format!("the built-in map: {e}")),
        }
    }

    /// Is this the controller's port (by its name)?
    pub fn is_port(&self, name: &str) -> bool {
        name.to_lowercase().contains(&self.port.to_lowercase())
    }

    /// The control a message is for and what it said; None for a message the
    /// map does not know (or an LSB that only completed a fader).
    pub fn decode(&self, dec: &mut Decoder, d: [u8; 3]) -> Option<(&Control, Msg)> {
        let (st, d1, d2) = (d[0], d[1] & 0x7f, d[2] & 0x7f);
        let (status, value) = match st & 0xf0 {
            0x80 => (st | 0x10, 0),
            0x90 | 0xb0 => (st, d2),
            _ => return None,
        };
        let ch = (st & 0x0f) as usize;
        let find = |data1: u8| self.controls.iter().find(|c| c.status == status && c.data1 == data1);
        let (c, v14) = match find(d1) {
            Some(c) => {
                if c.hires {
                    dec.msb[ch][d1 as usize] = value;
                }
                (c, (value as u32) << 7)
            }
            // a 14-bit fader's LSB
            None => {
                let msb_cc = d1.checked_sub(0x20)?;
                let c = find(msb_cc).filter(|c| c.hires && status & 0xf0 == 0xb0)?;
                (c, ((dec.msb[ch][msb_cc as usize] as u32) << 7) | value as u32)
            }
        };
        let msg = match c.act.kind() {
            Kind::Output => return None,
            Kind::Button => {
                if value > 0 {
                    Msg::Down
                } else {
                    Msg::Up
                }
            }
            Kind::Relative => Msg::Steps(value as i32 - 0x40),
            Kind::Absolute => {
                let v = if c.hires { v14 as f32 / 16383.0 } else { value as f32 / 127.0 };
                Msg::Value(if c.invert { 1.0 - v } else { v })
            }
        };
        Some((c, msg))
    }

    /// The control lit by `act` on `deck`, if the map has one with an LED.
    pub fn led(&self, deck: usize, act: Act) -> Option<&Control> {
        self.controls.iter().find(|c| c.deck == deck && c.act == act && c.led)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> Map {
        Map::parse(BUILT_IN).unwrap()
    }

    fn act(m: &Map, d: [u8; 3]) -> Option<(usize, Act, Msg)> {
        let mut dec = Decoder::default();
        m.decode(&mut dec, d).map(|(c, msg)| (c.deck, c.act, msg))
    }

    #[test]
    fn the_built_in_map_knows_the_duo_s_documented_controls() {
        let m = map();
        assert!(m.is_port("OMNIS-DUO") && m.is_port("2- omnis-duo MIDI") && !m.is_port("Launchkey Mini MK3 MIDI"));
        // deck 1 on channel 1, deck 2 on channel 2; pads on channels 6 and 7
        assert_eq!(act(&m, [0x90, 0x00, 0x7f]), Some((0, Act::Play, Msg::Down)));
        assert_eq!(act(&m, [0x91, 0x00, 0x00]), Some((1, Act::Play, Msg::Up)));
        assert_eq!(act(&m, [0x81, 0x00, 0x40]), Some((1, Act::Play, Msg::Up)), "a note-off is a release");
        assert_eq!(act(&m, [0x91, 0x01, 0x7f]), Some((1, Act::Cue, Msg::Down)));
        for pad in 0..4u8 {
            assert_eq!(act(&m, [0x95, pad, 0x7f]), Some((0, Act::HotCue(pad + 1), Msg::Down)));
            assert_eq!(act(&m, [0x96, pad, 0x7f]), Some((1, Act::HotCue(pad + 1), Msg::Down)));
            assert_eq!(act(&m, [0x96, 0x08 + pad, 0x7f]), Some((1, Act::Mute(pad), Msg::Down)), "shift + pad {pad}");
        }
        assert_eq!(act(&m, [0x95, 0x06, 0x7f]), Some((0, Act::Loop(4.0), Msg::Down)));
        assert_eq!(act(&m, [0x90, 0x43, 0x7f]), Some((0, Act::LoopToggle, Msg::Down)));
        assert_eq!(act(&m, [0xb0, 0x2e, 0x41]), Some((0, Act::LoopSize, Msg::Steps(1))));
        assert_eq!(act(&m, [0xb1, 0x2e, 0x7f]), Some((1, Act::LoopSize, Msg::Steps(63))));
        assert_eq!(act(&m, [0x91, 0x0c, 0x7f]), Some((1, Act::Section(-1), Msg::Down)));
        assert_eq!(act(&m, [0x90, 0x0b, 0x7f]), Some((0, Act::Section(1), Msg::Down)));
        // the jog: touch, the top while touched, the side
        assert_eq!(act(&m, [0x90, 0x20, 0x7f]), Some((0, Act::Touch, Msg::Down)));
        assert_eq!(act(&m, [0xb0, 0x22, 0x3c]), Some((0, Act::Scratch, Msg::Steps(-4))));
        assert_eq!(act(&m, [0xb1, 0x21, 0x43]), Some((1, Act::Nudge, Msg::Steps(3))));
        assert_eq!(act(&m, [0x91, 0x1f, 0x7f]), Some((1, Act::Sync, Msg::Down)));
        // the mixer on channel 5
        assert_eq!(act(&m, [0xb4, 0x0b, 127]), Some((0, Act::XFader, Msg::Value(1.0))));
        assert_eq!(act(&m, [0xb4, 0x07, 0]), Some((1, Act::EqHigh, Msg::Value(0.0))));
        assert_eq!(act(&m, [0xb4, 0x04, 127]), Some((0, Act::EqLow, Msg::Value(1.0))));
        assert_eq!(act(&m, [0xb4, 0x0a, 127]), Some((1, Act::Color, Msg::Value(1.0))));
        assert_eq!(act(&m, [0xb4, 0x12, 127]), Some((1, Act::Fader, Msg::Value(1.0))));
        assert_eq!(act(&m, [0xb4, 0x72, 0x7f]), Some((0, Act::FxOn, Msg::Down)));
        assert_eq!(act(&m, [0xb4, 0x4d, 0x7f]), Some((0, Act::FxBeats(1), Msg::Down)));
        // unknown: nothing (the caller logs it); the meter is output only
        assert_eq!(act(&m, [0x96, 0x0c, 0x7f]), None);
        assert_eq!(act(&m, [0xb0, 0x02, 0x40]), None);
        assert!(m.led(1, Act::HotCue(2)).is_some_and(|c| c.status == 0x96 && c.data1 == 0x01));
        assert!(m.led(0, Act::Level).is_some_and(|c| c.status == 0xb0 && c.data1 == 0x02));
    }

    #[test]
    fn the_tempo_fader_is_14_bit() {
        let m = map();
        let mut dec = Decoder::default();
        let v = |dec: &mut Decoder, d| match m.decode(dec, d) {
            Some((c, Msg::Value(v))) if c.act == Act::Tempo => v,
            other => panic!("{other:?}"),
        };
        assert_eq!(v(&mut dec, [0xb0, 0x00, 0x40]), 8192.0 / 16383.0, "the MSB alone");
        let fine = v(&mut dec, [0xb0, 0x20, 0x7f]);
        assert_eq!(fine, (8192.0 + 127.0) / 16383.0, "its LSB, with the MSB before it");
        assert_eq!(v(&mut dec, [0xb1, 0x00, 0x7f]), 16256.0 / 16383.0, "deck 2 keeps its own MSB");
    }

    #[test]
    fn bad_lines_say_where() {
        assert!(Map::parse("port X\n90 00 fly 1\n").unwrap_err().contains("line 2"));
        assert!(Map::parse("port X\n90 00 play\n").unwrap_err().contains("deck"));
        assert!(Map::parse("port X\n95 00 hotcue 1 9\n").is_err());
        assert!(Map::parse("90 00 play 1\n").unwrap_err().contains("port"));
        let m = Map::parse("port X\n80 10 play 2 noled\nB4 0B xfader invert\n").unwrap();
        assert_eq!((m.controls[0].status, m.controls[0].led), (0x90, false));
        assert_eq!(act(&m, [0xb4, 0x0b, 127]), Some((0, Act::XFader, Msg::Value(0.0))));
    }
}
