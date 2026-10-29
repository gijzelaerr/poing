//! Make a written MIDI part sound played: what a player does that a grid does
//! not. Timing a little off the grid (chord notes not all at once), legato
//! notes that overlap the next one (sample libraries play that as a legato
//! transition), velocities that vary, and a long note that swells and tapers
//! on the dynamics controller instead of sitting flat, with a slow drift in
//! expression. Seeded, so the same part humanises the same way again.

use crate::{Cc, Note};

/// How a part is humanised.
#[derive(Clone, Copy, Debug)]
pub struct Humanize {
    /// 0..1: how far from the grid (1 is a loose player, 0.5 a tight one).
    pub amount: f64,
    /// Seconds per beat of the song (timing is felt in milliseconds, not beats).
    pub seconds_per_beat: f64,
    pub seed: u64,
}

/// A small deterministic generator (SplitMix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Roughly normal, mean 0, sd 1 (sum of four uniforms).
    fn normal(&mut self) -> f64 {
        ((0..4).map(|_| self.next()).sum::<f64>() - 2.0) * 1.732
    }
}

/// The dynamics (controller 1) of a curve at `beat`, if it has any.
fn curve_at(cc: &[Cc], beat: f64) -> Option<f64> {
    let pts: Vec<&Cc> = cc.iter().filter(|c| c.num == 1).collect();
    let first = pts.first()?;
    if beat <= first.beat {
        return Some(first.value);
    }
    for w in pts.windows(2) {
        if beat <= w[1].beat {
            let f = (beat - w[0].beat) / (w[1].beat - w[0].beat).max(1e-9);
            return Some(w[0].value + (w[1].value - w[0].value) * f);
        }
    }
    pts.last().map(|p| p.value)
}

/// Humanise `notes` and their controller curves in place.
pub fn humanize(notes: &mut Vec<Note>, cc: &mut Vec<Cc>, h: Humanize) {
    let mut rng = Rng(h.seed ^ 0x5EED);
    let amount = h.amount.clamp(0.0, 1.0);
    let ms = |ms: f64| ms / 1000.0 / h.seconds_per_beat.max(1e-3);
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    let starts: Vec<f64> = notes.iter().map(|n| n.start).collect();
    let ends: Vec<f64> = notes.iter().map(|n| n.start + n.len).collect();
    for (i, n) in notes.iter_mut().enumerate() {
        // legato: a note the next one starts where it ends overlaps it a little
        let joined = starts.iter().zip(&ends).enumerate().any(|(j, (s, _))| j != i && (s - ends[i]).abs() < ms(60.0));
        if joined && n.len >= 0.5 {
            n.len += ms(70.0 + 40.0 * rng.next());
        }
        // a bowed or blown note speaks a little after its beat, more for long ones
        let late = if n.len >= 1.0 { 12.0 } else { 4.0 };
        n.start = (n.start + ms(amount * (late + 9.0 * rng.normal()))).max(0.0);
        n.vel = (n.vel * (1.0 + amount * 0.12 * rng.normal())).clamp(0.05, 1.0);
    }
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    // long notes breathe: in a little under the curve, up past its middle, tapering off
    if cc.iter().any(|c| c.num == 1) {
        let mut add = Vec::new();
        let mut longs: Vec<&Note> = notes.iter().filter(|n| n.len >= 2.0).collect();
        longs.dedup_by(|b, a| (b.start - a.start).abs() < 0.5);
        for n in longs {
            let base = |b: f64| curve_at(cc, b).unwrap_or(0.6);
            let swell = amount * (0.06 + 0.04 * rng.next());
            for (f, d) in [(0.0, -0.6), (0.45, 1.0), (0.85, 0.3), (1.0, -0.4)] {
                let b = n.start + n.len * f;
                add.push(Cc { beat: b, num: 1, value: (base(b) + swell * d).clamp(0.0, 1.0) });
            }
        }
        cc.extend(add);
        cc.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        // close points of the same controller: the later one wins
        cc.dedup_by(|b, a| b.num == a.num && (b.beat - a.beat).abs() < 0.05);
    }
    // expression drifts slowly around 0.85, as a player's bow pressure does
    if !notes.is_empty() && !cc.iter().any(|c| c.num == 11) {
        let (a, b) = (notes[0].start, notes.iter().map(|n| n.start + n.len).fold(0.0, f64::max));
        let mut beat = a;
        while beat <= b {
            cc.push(Cc { beat, num: 11, value: (0.85 + amount * 0.06 * rng.normal()).clamp(0.6, 1.0) });
            beat += 4.0 + 4.0 * rng.next();
        }
        cc.sort_by(|x, y| x.beat.total_cmp(&y.beat));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part() -> (Vec<Note>, Vec<Cc>) {
        let n = |start, len, pitch| Note { start, len, pitch, vel: 0.7 };
        // a three-note chord held 4 beats, then a legato line of two-beat notes
        let notes = vec![n(0.0, 4.0, 57), n(0.0, 4.0, 60), n(0.0, 4.0, 64), n(4.0, 2.0, 69), n(6.0, 2.0, 71), n(8.0, 2.0, 72)];
        let cc = vec![Cc { beat: 0.0, num: 1, value: 0.5 }, Cc { beat: 10.0, num: 1, value: 0.7 }];
        (notes, cc)
    }

    #[test]
    fn a_humanised_part_breathes_and_is_off_the_grid() {
        let (mut notes, mut cc) = part();
        let h = Humanize { amount: 0.6, seconds_per_beat: 0.49, seed: 7 };
        humanize(&mut notes, &mut cc, h);
        let chord: Vec<f64> = notes.iter().filter(|n| n.start < 1.0).map(|n| n.start).collect();
        assert_eq!(chord.len(), 3);
        assert!(chord.iter().any(|s| (s - chord[0]).abs() > 1e-4), "chord notes are not all at once: {chord:?}");
        assert!(chord.iter().all(|s| s.abs() < 0.1), "but within ~50 ms of the beat: {chord:?}");
        // the legato line overlaps: each note ends after the next starts
        let line: Vec<&Note> = notes.iter().filter(|n| n.pitch >= 69).collect();
        assert!(line.windows(2).all(|w| w[0].start + w[0].len > w[1].start), "legato overlaps");
        assert!(notes.iter().any(|n| (n.vel - 0.7).abs() > 1e-3), "velocities vary");
        // long notes swell: the dynamics rise inside the held chord
        let inside: Vec<f64> = cc.iter().filter(|c| c.num == 1 && c.beat > 0.5 && c.beat < 3.5).map(|c| c.value).collect();
        assert!(inside.iter().any(|v| *v > 0.53), "a swell over the curve's 0.5-0.56: {inside:?}");
        assert!(cc.iter().any(|c| c.num == 11), "expression drifts");
        assert!(cc.windows(2).all(|w| w[0].beat <= w[1].beat));
    }

    #[test]
    fn the_same_seed_humanises_the_same_way() {
        let (mut a, mut ca) = part();
        let (mut b, mut cb) = part();
        let h = Humanize { amount: 0.5, seconds_per_beat: 0.5, seed: 3 };
        humanize(&mut a, &mut ca, h);
        humanize(&mut b, &mut cb, h);
        assert_eq!((a, ca), (b, cb));
    }
}
