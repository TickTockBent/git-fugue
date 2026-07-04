//! The musical constraint system (spec §4).
//!
//! Data never maps directly to notes: everything here resolves inside
//! pre-validated structures. The RNG selects; these rules constrain.

use crate::model::{Key, Scale};
use crate::rng::Rng;

/// Contour templates (spec §4). Each returns a target position in
/// [-1, 1] for progress x in [0, 1]: where the melody *wants* to be
/// within its register at that point in the phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contour {
    Arch,
    Ramp,
    Valley,
    Plateau,
    Zigzag,
}

pub const CONTOURS: [Contour; 5] = [
    Contour::Arch,
    Contour::Ramp,
    Contour::Valley,
    Contour::Plateau,
    Contour::Zigzag,
];

impl Contour {
    pub fn target(self, x: f64) -> f64 {
        match self {
            Contour::Arch => 1.0 - (2.0 * x - 1.0).powi(2) * 2.0, // rise to +1, fall
            Contour::Ramp => 2.0 * x - 1.0,
            Contour::Valley => (2.0 * x - 1.0).powi(2) * 2.0 - 1.0,
            Contour::Plateau => 0.2,
            Contour::Zigzag => {
                if ((x * 4.0) as u32).is_multiple_of(2) {
                    0.6
                } else {
                    -0.6
                }
            }
        }
    }

    /// Used by liner notes; kept even while nothing prints it yet.
    #[allow(dead_code)]
    pub fn name(self) -> &'static str {
        match self {
            Contour::Arch => "arch",
            Contour::Ramp => "ramp",
            Contour::Valley => "valley",
            Contour::Plateau => "plateau",
            Contour::Zigzag => "zigzag",
        }
    }
}

/// A melodic random walk over scale degrees, constrained by
/// voice-leading rules: 60% step, 30% third, 10% larger leap; leaps
/// resolve by step in the opposite direction; range clamped to three
/// octaves; pitches always quantized to the scale (spec §4).
pub struct MelodyWalk {
    key: Key,
    scale: Scale,
    /// Absolute position in "scale steps" (degree index across octaves).
    pos: i32,
    lo: i32,
    hi: i32,
    /// Pending forced resolution after a leap: -1, 0, or +1 direction.
    resolve: i32,
}

impl MelodyWalk {
    /// `center_midi` is the preferred register center; range spans
    /// roughly 1.5 octaves either side (3-octave usable range).
    pub fn new(key: Key, scale: Scale, center_midi: u8) -> Self {
        let n = scale.degrees().len() as i32;
        let center = midi_to_steps(key, scale, center_midi);
        let span = (n * 3) / 2; // ~1.5 octaves in scale steps
        MelodyWalk {
            key,
            scale,
            pos: center,
            lo: center - span,
            hi: center + span,
            resolve: 0,
        }
    }

    /// Advance the walk one note. `pull` in [-1, 1] is the contour's
    /// target position within the register; the walk is biased toward
    /// it. Returns a MIDI pitch, always in scale.
    pub fn step(&mut self, rng: &mut Rng, pull: f64) -> u8 {
        let interval: i32 = if self.resolve != 0 {
            let d = self.resolve;
            self.resolve = 0;
            d // forced stepwise resolution
        } else {
            let size = match rng.weighted(&[60, 30, 10]) {
                0 => 1,
                1 => 2,
                _ => rng.range(3, 5) as i32,
            };
            let target = self.lo as f64 + (pull + 1.0) / 2.0 * (self.hi - self.lo) as f64;
            let toward_target = if (target - self.pos as f64) >= 0.0 { 1 } else { -1 };
            // 70% toward the contour target, 30% against.
            let dir = if rng.weighted(&[70, 30]) == 0 {
                toward_target
            } else {
                -toward_target
            };
            if size >= 3 {
                self.resolve = -dir; // leap resolves by step, opposite way
            }
            size * dir
        };
        self.pos += interval;
        if self.pos > self.hi {
            self.pos = self.hi - (self.pos - self.hi); // reflect
            self.resolve = 0;
        }
        if self.pos < self.lo {
            self.pos = self.lo + (self.lo - self.pos);
            self.resolve = 0;
        }
        self.pos = self.pos.clamp(self.lo, self.hi);
        steps_to_midi(self.key, self.scale, self.pos)
    }

}

/// Convert an absolute scale-step index to a MIDI pitch.
pub fn steps_to_midi(key: Key, scale: Scale, steps: i32) -> u8 {
    let degs = scale.degrees();
    let n = degs.len() as i32;
    let oct = steps.div_euclid(n);
    let idx = steps.rem_euclid(n) as usize;
    let pitch = key.root as i32 + degs[idx] as i32 + 12 * oct;
    pitch.clamp(0, 127) as u8
}

/// Nearest scale-step index at or below a MIDI pitch.
pub fn midi_to_steps(key: Key, scale: Scale, midi: u8) -> i32 {
    let degs = scale.degrees();
    let n = degs.len() as i32;
    let rel = midi as i32 - key.root as i32;
    let oct = rel.div_euclid(12);
    let pc = rel.rem_euclid(12) as u8;
    let mut best = 0i32;
    for (i, d) in degs.iter().enumerate() {
        if *d <= pc {
            best = i as i32;
        }
    }
    oct * n + best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Key {
        Key { root: 9 } // A
    }

    #[test]
    fn walk_stays_in_scale() {
        let scale = Scale::MajorPentatonic;
        let mut walk = MelodyWalk::new(key(), scale, 69);
        let mut rng = Rng::new(123);
        for i in 0..500 {
            let pull = Contour::Arch.target(i as f64 / 500.0);
            let p = walk.step(&mut rng, pull);
            let pc = (p as i32 - key().root as i32).rem_euclid(12) as u8;
            assert!(
                scale.degrees().contains(&pc),
                "pitch {p} (pc {pc}) not in scale"
            );
        }
    }

    #[test]
    fn walk_stays_in_register() {
        let mut walk = MelodyWalk::new(key(), Scale::Dorian, 69);
        let mut rng = Rng::new(7);
        for _ in 0..1000 {
            let p = walk.step(&mut rng, 0.0);
            assert!(p >= 69 - 24 && p <= 69 + 24, "pitch {p} left register");
        }
    }

    #[test]
    fn roundtrip_steps_midi() {
        // Stay inside the 0..=127 MIDI range: the clamp in
        // steps_to_midi is a safety net, not part of the mapping.
        for scale in [Scale::MajorPentatonic, Scale::Aeolian] {
            for s in 0..40 {
                let m = steps_to_midi(key(), scale, s);
                assert_eq!(midi_to_steps(key(), scale, m), s);
            }
        }
    }
}
