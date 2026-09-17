//! `std::minstd_rand`, reimplemented.
//!
//! The C++ profiler draws heap sampling intervals from a per-instance
//! `std::minstd_rand` (`cpp/_memalloc_heap.cpp`). Keeping the same engine
//! keeps the sampling sequence identical across the rewrite, and because
//! `std::minstd_rand` is fully specified by the C++ standard the port can be
//! pinned bit-for-bit by a golden vector.
//!
//! A per-instance engine was chosen over `rand()` deliberately: all state
//! lives in the object, so there are no global locks and it is fork-safe.

/// Multiplier of `std::minstd_rand`.
const A: u64 = 48_271;

/// Modulus of `std::minstd_rand`, `2^31 - 1`.
const M: u64 = 2_147_483_647;

/// Lehmer / Park-Miller generator, equivalent to
/// `std::linear_congruential_engine<uint_fast32_t, 48271, 0, 2147483647>`.
///
/// The state is always in `1..M`, so [`MinstdRand::next_u32`] never returns 0.
/// That matters for [`next_sample_size`](crate::memalloc::pure::sampler): the
/// inverse-transform draw takes a logarithm of the state, which must not be
/// zero.
pub struct MinstdRand {
    state: u32,
}

impl MinstdRand {
    /// Seed the engine, normalising exactly as libstdc++ does: reduce modulo
    /// `M`, and substitute 1 if that leaves 0. A zero state is a fixed point
    /// of the recurrence (the sequence would be all zeros), so it must be
    /// avoided.
    pub fn new(seed: u32) -> Self {
        // PANIC-OK: `% M` on a widened value cannot overflow or divide by
        // zero; the result is below `M` so the cast back is lossless.
        #[allow(
            clippy::arithmetic_side_effects,
            clippy::modulo_arithmetic,
            clippy::cast_possible_truncation
        )]
        let reduced = (u64::from(seed) % M) as u32;
        Self {
            state: if reduced == 0 { 1 } else { reduced },
        }
    }

    /// Advance the engine and return the new state, in `1..M`.
    pub fn next_u32(&mut self) -> u32 {
        // PANIC-OK: `state < M` and `A * M` fits in u64, so the multiply cannot
        // overflow; `M` is a non-zero constant; the remainder is below `M` so
        // the cast back is lossless.
        #[allow(
            clippy::arithmetic_side_effects,
            clippy::modulo_arithmetic,
            clippy::cast_possible_truncation
        )]
        {
            self.state = ((u64::from(self.state) * A) % M) as u32;
        }
        self.state
    }

    /// Uniform in `(0, 1)`: the state divided by the modulus.
    ///
    /// Never 0 (the state is never 0) and never 1 (the state is never `M`), so
    /// callers may take its logarithm.
    pub fn next_unit(&mut self) -> f64 {
        f64::from(self.next_u32()) / M as f64
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation
    )]

    use super::{M, MinstdRand};

    /// `std::minstd_rand` is required by the C++ standard to produce
    /// 399268537 as its 10000th value when seeded with 1. Pinning it here
    /// proves the port is bit-identical to the engine the C++ used.
    #[test]
    fn matches_the_cxx_standard_golden_vector() {
        let mut rng = MinstdRand::new(1);
        let mut last = 0;
        for _ in 0..10_000 {
            last = rng.next_u32();
        }
        assert_eq!(last, 399_268_537);
    }

    #[test]
    fn state_stays_in_range_and_never_repeats_immediately() {
        let mut rng = MinstdRand::new(12345);
        let mut prev = 0;
        for _ in 0..10_000 {
            let v = rng.next_u32();
            assert!(v >= 1, "state reached 0");
            assert!(u64::from(v) < M, "state reached the modulus");
            assert_ne!(v, prev, "engine stalled");
            prev = v;
        }
    }

    /// A zero state is a fixed point, so seeding must never produce one.
    #[test]
    fn degenerate_seeds_are_normalised() {
        for seed in [0, M as u32, u32::MAX, 1, 2] {
            let mut rng = MinstdRand::new(seed);
            for _ in 0..100 {
                assert!(rng.next_u32() >= 1, "seed {seed} degenerated to 0");
            }
        }
    }

    #[test]
    fn same_seed_gives_same_sequence() {
        let draws = |seed| {
            let mut rng = MinstdRand::new(seed);
            (0..64).map(|_| rng.next_u32()).collect::<Vec<_>>()
        };
        assert_eq!(draws(7), draws(7));
        assert_ne!(draws(7), draws(8));
    }

    #[test]
    fn next_unit_is_strictly_inside_zero_to_one() {
        let mut rng = MinstdRand::new(99);
        for _ in 0..10_000 {
            let u = rng.next_unit();
            assert!(u > 0.0 && u < 1.0, "u = {u}");
            assert!(u.ln().is_finite(), "ln(u) not finite for u = {u}");
        }
    }
}
