//! Pure deterministic Pseudo-Random Number Generator controlled exclusively by a single 64-bit seed.
//! Zero OS entropy (`getrandom` / `ThreadRng` strictly forbidden in simulation paths).
//! Combines SplitMix64 seeding with a 64-bit xoshiro256** state engine for statistically
//! high-quality, platform-independent, bit-reproducible fault schedules.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicPrng {
    seed: u64,
    s: [u64; 4],
    draws: u64,
}

impl DeterministicPrng {
    #[inline]
    pub fn new(seed: u64) -> Self {
        let mut sm = seed;
        let s0 = Self::splitmix64(&mut sm);
        let s1 = Self::splitmix64(&mut sm);
        let s2 = Self::splitmix64(&mut sm);
        let s3 = Self::splitmix64(&mut sm);
        Self {
            seed,
            s: [s0, s1, s2, s3],
            draws: 0,
        }
    }

    #[inline]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    #[inline]
    pub const fn draws(&self) -> u64 {
        self.draws
    }

    /// Deterministically derive an independent child PRNG stream tagged by `stream_tag`.
    #[inline]
    pub fn fork_stream(&mut self, stream_tag: u64) -> Self {
        let mixed = self.next_u64() ^ stream_tag.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        Self::new(mixed)
    }

    #[inline]
    fn splitmix64(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// xoshiro256** core generator — 100% deterministic across all CPU architectures.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.draws = self.draws.wrapping_add(1);
        let result = (self.s[1].wrapping_mul(5)).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;

        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];

        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);

        result
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Unbiased inclusive integer draw in `[min_incl, max_incl]` via Lemire's rejection method.
    #[inline]
    pub fn range_u64(&mut self, min_incl: u64, max_incl: u64) -> u64 {
        if min_incl >= max_incl {
            return min_incl;
        }
        let span = max_incl - min_incl;
        if span == u64::MAX {
            return self.next_u64();
        }
        let bound = span + 1;
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let r = self.next_u64();
            let prod = u128::from(r) * u128::from(bound);
            if (prod as u64) >= threshold {
                return min_incl + (prod >> 64) as u64;
            }
        }
    }

    #[inline]
    pub fn range_usize(&mut self, len: usize) -> usize {
        if len <= 1 {
            return 0;
        }
        self.range_u64(0, (len - 1) as u64) as usize
    }

    /// Returns `true` with probability `ppm / 1_000_000` (parts-per-million, integer-exact).
    #[inline]
    pub fn chance_ppm(&mut self, ppm: u32) -> bool {
        if ppm == 0 {
            return false;
        }
        if ppm >= 1_000_000 {
            return true;
        }
        self.range_u64(0, 999_999) < u64::from(ppm)
    }

    /// Fisher-Yates deterministic in-place shuffle.
    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        let len = slice.len();
        if len <= 1 {
            return;
        }
        for i in (1..len).rev() {
            let j = self.range_usize(i + 1);
            slice.swap(i, j);
        }
    }

    /// Heavy-tailed integer network latency generator (no floats!) modeling realistic tail spikes.
    /// With 90% probability draws uniform `[min_ns, min_ns + jitter_ns]`;
    /// with 10% probability injects a power-of-two tail spike bounded by `max_spike_ns`.
    pub fn bimodal_tail_latency_ns(
        &mut self,
        min_ns: u64,
        jitter_ns: u64,
        tail_ppm: u32,
        max_spike_ns: u64,
    ) -> u64 {
        let base = min_ns.saturating_add(self.range_u64(0, jitter_ns));
        if self.chance_ppm(tail_ppm) {
            let shift = self.range_u64(1, 7) as u32;
            let spike = base.saturating_mul(1u64 << shift);
            spike.min(max_spike_ns.max(base))
        } else {
            base
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_prng_is_bit_exact_for_identical_seed() {
        let mut a = DeterministicPrng::new(0xDEAD_BEEF_CAFE_BABE);
        let mut b = DeterministicPrng::new(0xDEAD_BEEF_CAFE_BABE);
        for _ in 0..10_000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_eq!(a.draws(), 10_000);
    }

    #[test]
    fn range_u64_respects_bounds_and_ppm_extremes() {
        let mut rng = DeterministicPrng::new(42);
        assert!(!rng.chance_ppm(0));
        assert!(rng.chance_ppm(1_000_000));
        for _ in 0..1_000 {
            let v = rng.range_u64(15, 29);
            assert!((15..=29).contains(&v));
        }
    }
}
