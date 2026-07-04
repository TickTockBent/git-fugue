//! Deterministic PRNG: SplitMix64, seeded hierarchically.
//!
//! One RNG per scope, derived from a parent seed plus a label
//! (identity seed -> section seed -> unit seed). No global state,
//! no HashMap iteration order, ever.

#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng { state: seed }
    }

    /// Derive a child RNG for a named scope. Same parent + same label
    /// always yields the same child stream.
    pub fn child(&self, label: &str) -> Rng {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
        for b in label.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Rng::new(splitmix(self.state ^ h))
    }

    /// Derive a child RNG from a numeric key (e.g. a content hash).
    pub fn child_u64(&self, key: u64) -> Rng {
        Rng::new(splitmix(self.state ^ key))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        splitmix(self.state)
    }

    /// Uniform in [0, n). n must be > 0.
    pub fn below(&mut self, n: u64) -> u64 {
        // Multiply-shift; bias is negligible for our small ranges.
        ((self.next_u64() as u128 * n as u128) >> 64) as u64
    }

    /// Uniform in [lo, hi] inclusive.
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        debug_assert!(lo <= hi);
        lo + self.below((hi - lo + 1) as u64) as i64
    }

    /// Uniform float in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Weighted choice: returns an index into `weights`.
    pub fn weighted(&mut self, weights: &[u32]) -> usize {
        let total: u64 = weights.iter().map(|w| *w as u64).sum();
        debug_assert!(total > 0);
        let mut x = self.below(total);
        for (i, w) in weights.iter().enumerate() {
            if x < *w as u64 {
                return i;
            }
            x -= *w as u64;
        }
        weights.len() - 1
    }
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// FNV-1a over arbitrary bytes; used for identifier hashing.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_streams() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn child_scopes_independent() {
        let root = Rng::new(7);
        let mut c1 = root.child("section:src");
        let mut c2 = root.child("section:tests");
        assert_ne!(c1.next_u64(), c2.next_u64());
        // Re-derivation is stable.
        let mut c1b = root.child("section:src");
        assert_eq!(Rng::new(7).child("section:src").next_u64(), c1b.next_u64());
    }

    #[test]
    fn weighted_in_bounds() {
        let mut r = Rng::new(1);
        for _ in 0..1000 {
            let i = r.weighted(&[60, 30, 10]);
            assert!(i < 3);
        }
    }
}
