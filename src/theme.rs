//! The subject and its mutations (spec §6.2).
//!
//! A theme is 2-4 bars of (slot, scale-degree) pairs generated from the
//! identity seed. Each commit applies mutation operators chosen by its
//! own hash: interval nudge, rhythmic displacement, ornament
//! insert/remove, in-scale note swap, contour inversion (rare).
//! Magnitude is weighted by diff size, so a branch's musical distance
//! from its origin tracks its code distance.

use crate::model::{Key, Scale};
use crate::rng::Rng;
use crate::theory::{midi_to_steps, pick_positions, MelodyWalk, CONTOURS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeNote {
    /// Absolute slot on the 16th grid across the whole theme.
    pub slot: u32,
    /// Absolute scale-step index (theory::steps_to_midi converts).
    pub degree: i32,
    /// Metric weight: higher survives density thinning longer.
    pub weight: u8,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub bars: u32,
    /// Sorted by slot at all times.
    pub notes: Vec<ThemeNote>,
    center: i32,
    lo: i32,
    hi: i32,
}

impl Theme {
    /// Generate the subject from the identity seed (spec §6.2:
    /// a 2-to-4-bar theme; the exposition states it verbatim).
    pub fn generate(rng: &mut Rng, key: Key, scale: Scale, center_midi: u8) -> Theme {
        let bars = 2 + rng.below(3) as u32;
        let contour = CONTOURS[rng.below(CONTOURS.len() as u64) as usize];
        let mut walk = MelodyWalk::new(key, scale, center_midi);
        let center = midi_to_steps(key, scale, center_midi);
        let mut notes = Vec::new();
        let total_slots = bars * 16;
        for bar in 0..bars {
            let density = 5 + rng.below(3) as u32; // 5..=7 notes per bar
            for slot in pick_positions(rng, density, bar == bars - 1) {
                let abs = bar * 16 + slot;
                let pull = contour.target(abs as f64 / total_slots as f64);
                let pitch = walk.step(rng, pull);
                notes.push(ThemeNote {
                    slot: abs,
                    degree: midi_to_steps(key, scale, pitch),
                    weight: slot_weight(slot),
                });
            }
        }
        let n = scale.degrees().len() as i32;
        Theme {
            bars,
            notes,
            center,
            lo: center - (n * 3) / 2,
            hi: center + (n * 3) / 2,
        }
    }

    /// Notes of one bar as (slot-in-bar, degree, weight), sorted.
    pub fn bar(&self, bar: u32) -> Vec<(u32, i32, u8)> {
        let lo = bar * 16;
        self.notes
            .iter()
            .filter(|n| n.slot >= lo && n.slot < lo + 16)
            .map(|n| (n.slot - lo, n.degree, n.weight))
            .collect()
    }

    /// Apply `ops` mutation operators. Operator choice and targets come
    /// entirely from `rng`, which the caller seeds from the commit hash.
    pub fn mutate(&mut self, rng: &mut Rng, ops: u32) {
        for _ in 0..ops {
            match rng.weighted(&[25, 25, 20, 25, 5]) {
                0 => self.interval_nudge(rng),
                1 => self.displace(rng),
                2 => self.ornament(rng),
                3 => self.swap(rng),
                _ => self.invert(), // contour inversion, rare
            }
        }
        for n in &mut self.notes {
            n.degree = n.degree.clamp(self.lo, self.hi);
        }
        self.notes.sort_by_key(|n| n.slot);
    }

    fn pick(&self, rng: &mut Rng) -> usize {
        rng.below(self.notes.len() as u64) as usize
    }

    fn interval_nudge(&mut self, rng: &mut Rng) {
        if self.notes.is_empty() {
            return;
        }
        let i = self.pick(rng);
        self.notes[i].degree += if rng.weighted(&[50, 50]) == 0 { 1 } else { -1 };
    }

    fn displace(&mut self, rng: &mut Rng) {
        if self.notes.is_empty() {
            return;
        }
        let i = self.pick(rng);
        let delta: i64 = if rng.weighted(&[50, 50]) == 0 { 1 } else { -1 };
        let cur = self.notes[i].slot as i64;
        let bar = cur / 16;
        let cand = cur + delta;
        // Stay inside the same bar; never land on an occupied slot.
        if cand / 16 != bar || cand < 0 {
            return;
        }
        let cand = cand as u32;
        if self.notes.iter().any(|n| n.slot == cand) {
            return;
        }
        self.notes[i].slot = cand;
        self.notes[i].weight = slot_weight(cand % 16);
    }

    fn ornament(&mut self, rng: &mut Rng) {
        if self.notes.is_empty() {
            return;
        }
        let bar_notes = self.notes.len() as u32 / self.bars;
        if bar_notes > 7 || (bar_notes > 3 && rng.weighted(&[50, 50]) == 1) {
            // Remove: drop the lightest note (rightmost among ties).
            if let Some((i, _)) = self
                .notes
                .iter()
                .enumerate()
                .min_by_key(|(i, n)| (n.weight, std::cmp::Reverse(*i)))
                && self.notes.len() > 3 {
                    self.notes.remove(i);
                }
            return;
        }
        // Insert: a light neighbor pickup one slot before an existing note.
        let i = self.pick(rng);
        let anchor = self.notes[i];
        if anchor.slot == 0 {
            return;
        }
        let slot = anchor.slot - 1;
        if slot / 16 != anchor.slot / 16 || self.notes.iter().any(|n| n.slot == slot) {
            return;
        }
        self.notes.push(ThemeNote {
            slot,
            degree: anchor.degree + if rng.weighted(&[50, 50]) == 0 { 1 } else { -1 },
            weight: 0,
        });
    }

    fn swap(&mut self, rng: &mut Rng) {
        if self.notes.len() < 2 {
            return;
        }
        let i = self.pick(rng);
        let j = self.pick(rng);
        if i != j {
            let (di, dj) = (self.notes[i].degree, self.notes[j].degree);
            self.notes[i].degree = dj;
            self.notes[j].degree = di;
        }
    }

    fn invert(&mut self) {
        // Mirror around the theme center: the melody upside down.
        for n in &mut self.notes {
            n.degree = 2 * self.center - n.degree;
        }
    }
}

pub fn slot_weight(slot_in_bar: u32) -> u8 {
    if slot_in_bar == 0 {
        3
    } else if slot_in_bar.is_multiple_of(4) {
        2
    } else {
        1
    }
}

/// How many mutation operators a commit applies: at least one, growing
/// logarithmically with diff size (spec §6.2: magnitude weighted by
/// diff size), capped so one giant vendored-code commit cannot erase
/// the theme.
pub fn ops_for_diff(diff_magnitude: u32) -> u32 {
    (1 + (diff_magnitude as u64 + 1).ilog2() / 3).min(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> (Theme, Key, Scale) {
        let key = Key { root: 2 };
        let scale = Scale::MajorPentatonic;
        let mut rng = Rng::new(99).child("theme");
        (Theme::generate(&mut rng, key, scale, 62), key, scale)
    }

    #[test]
    fn subject_is_two_to_four_bars() {
        let (t, _, _) = theme();
        assert!((2..=4).contains(&t.bars));
        assert!(!t.notes.is_empty());
        // Sorted, unique slots.
        for w in t.notes.windows(2) {
            assert!(w[0].slot < w[1].slot);
        }
    }

    #[test]
    fn mutation_is_deterministic() {
        let (mut a, _, _) = theme();
        let (mut b, _, _) = theme();
        let mut ra = Rng::new(0xABCD).child("mutation");
        let mut rb = Rng::new(0xABCD).child("mutation");
        a.mutate(&mut ra, 4);
        b.mutate(&mut rb, 4);
        assert_eq!(a.notes, b.notes);
    }

    #[test]
    fn mutation_changes_the_theme() {
        let (mut a, _, _) = theme();
        let before = a.notes.clone();
        let mut r = Rng::new(0x1234).child("mutation");
        a.mutate(&mut r, 3);
        assert_ne!(before, a.notes);
    }

    #[test]
    fn heavy_mutation_keeps_theme_playable() {
        let (mut t, _, _) = theme();
        let r = Rng::new(5).child("mutation");
        for i in 0..200 {
            let mut cr = r.child_u64(i);
            t.mutate(&mut cr, 5);
        }
        assert!(t.notes.len() >= 3, "theme decayed to nothing");
        for n in &t.notes {
            assert!(n.slot < t.bars * 16);
        }
        // Slots stay unique.
        for w in t.notes.windows(2) {
            assert!(w[0].slot < w[1].slot, "colliding slots after mutation");
        }
    }

    #[test]
    fn ops_scale_with_diff() {
        assert_eq!(ops_for_diff(0), 1);
        assert!(ops_for_diff(50) <= ops_for_diff(5000));
        assert_eq!(ops_for_diff(u32::MAX), 5);
    }
}
