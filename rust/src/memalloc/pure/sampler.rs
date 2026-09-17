//! Byte-based Poisson sampling of allocations.
//!
//! Ported from `cpp/_memalloc_heap.cpp`, whose header comment derives the
//! approach from tcmalloc. Summarised:
//!
//! We want every allocated byte to have the same chance of appearing in the
//! profile, so we sample on bytes rather than on allocations. For an average
//! of one sample per `R` bytes, count bytes allocated in `C` and draw a target
//! `T` from an exponential distribution with mean `R`; when `C >= T`, take a
//! sample, reset `C`, and redraw `T`.
//!
//! Reporting the sampled allocation's own size would badly under-represent the
//! heap, since most sampled allocations are small. Each sample instead stands
//! for roughly `R` bytes. The exact correction is `W = R + (C - T)`, which can
//! be rewritten `C + (R - T)`; since `T` averages `R`, the C++ drops the
//! `(R - T)` term and uses `C` directly as the weight. This port keeps that
//! simplification so the numbers do not move.

use crate::memalloc::pure::rng::MinstdRand;

/// Seed used when the sampling interval is 0.
///
/// `2^32 / phi`. Matches the C++ fallback, which exists because the interval
/// doubles as the seed and 0 is a degenerate seed.
const FALLBACK_SEED: u32 = 0x9e37_79b9;

/// Sampling state: the interval, the byte counter, and the next target.
pub struct Sampler {
    /// Mean sampling interval in bytes, `R` above.
    interval: u32,
    rng: MinstdRand,
    /// Next target in bytes, `T` above.
    target: u64,
    /// Bytes allocated since the last sample, `C` above.
    allocated: u64,
}

impl Sampler {
    /// Create a sampler with mean interval `interval` bytes.
    ///
    /// The interval seeds the RNG, exactly as the C++ does, so a process with
    /// a given configuration draws a reproducible sequence.
    pub fn new(interval: u32) -> Self {
        Self::with_seed(interval, interval)
    }

    /// Create a sampler with an explicit RNG seed.
    ///
    /// Only for tests and for an operator override; `new` is what production
    /// uses.
    pub fn with_seed(interval: u32, seed: u32) -> Self {
        let mut rng = MinstdRand::new(if seed == 0 { FALLBACK_SEED } else { seed });
        let target = next_target(&mut rng, interval);
        Self {
            interval,
            rng,
            target,
            allocated: 0,
        }
    }

    /// Account for an allocation of `size` bytes.
    ///
    /// Returns `Some(weight)` when this allocation should be sampled, where
    /// `weight` is the number of bytes this sample stands for. The caller
    /// decides whether it can actually record the sample and calls
    /// [`Sampler::reset`] if it does -- mirroring the C++, where a full
    /// allocation map declines the sample without resetting the counter.
    pub fn on_alloc(&mut self, size: usize) -> Option<u64> {
        self.allocated = self.allocated.saturating_add(size as u64);
        if self.allocated < self.target {
            return None;
        }
        Some(self.allocated)
    }

    /// Bytes accumulated since the last reset.
    pub fn allocated(&self) -> u64 {
        self.allocated
    }

    /// Clear the byte counter and draw a new target. Called after a sample is
    /// successfully recorded, and after a fork.
    pub fn reset(&mut self) {
        self.allocated = 0;
        self.target = next_target(&mut self.rng, self.interval);
    }

    /// Mean sampling interval in bytes.
    pub fn interval(&self) -> u32 {
        self.interval
    }

    #[cfg(test)]
    pub fn target(&self) -> u64 {
        self.target
    }
}

/// Draw the next sampling target from an exponential distribution with mean
/// `interval`, by inverse transform: `-mean * ln(U)` for `U` uniform on
/// `(0, 1)`.
///
/// The C++ used `std::exponential_distribution`, whose algorithm is
/// implementation-defined (libstdc++ and libc++ disagree), so the draw
/// sequence is not reproducible across standard libraries anyway. Inverse
/// transform is distributionally identical and has no dependencies.
fn next_target(rng: &mut MinstdRand, interval: u32) -> u64 {
    // Widen before the +1 so an interval of u32::MAX cannot wrap to 0, which
    // would make the sampling rate infinite. (The C++ had the same fix.)
    let mean = f64::from(interval) + 1.0;
    let draw = -mean * rng.next_unit().ln();

    // Rust saturates on float-to-int casts, so unlike the C++ this needs no
    // explicit clamp to avoid undefined behaviour. The bound is kept because
    // the interval is capped at u32::MAX and letting a draw exceed it would
    // silently stall sampling.
    if draw >= f64::from(u32::MAX) {
        return u64::from(u32::MAX);
    }
    // PANIC-OK: bounded above by the branch and below by `draw > 0` (`ln` of a
    // value in (0,1) is negative and `mean` is positive); casts saturate
    // regardless.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bounded = draw as u64;
    bounded
}

/// Estimate how many allocations a sample of `size` bytes with weight
/// `weight` stands for.
///
/// Zero-byte allocations are legal and can be sampled (if an allocation during
/// sampling pushes past the threshold, the next allocation is sampled and may
/// be 0 bytes), so the size is floored at 1 to avoid dividing by zero.
/// (`traceback_t::init_sample` in `cpp/_memalloc_tb.cpp`.)
pub fn scaled_count(size: usize, weight: u64) -> u64 {
    let adjusted = if size > 0 { size } else { 1 };
    // PANIC-OK: `adjusted >= 1`, so the division is well defined; the cast
    // saturates.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let count = (weight as f64 / adjusted as f64) as u64;
    count
}

#[cfg(test)]
mod tests {
    // Test code is not on the hook path, so the panic wall does not apply.
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::arithmetic_side_effects,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation
    )]

    use super::{MinstdRand, Sampler, scaled_count};

    #[test]
    fn small_allocations_accumulate_until_the_target() {
        let mut s = Sampler::with_seed(4096, 1);
        let target = s.target();
        let mut total = 0u64;
        loop {
            total += 1;
            match s.on_alloc(1) {
                Some(weight) => {
                    assert_eq!(weight, total);
                    assert!(total >= target, "fired early at {total} < {target}");
                    break;
                }
                None => assert!(total < target, "missed the target at {total}"),
            }
            assert!(total < 1 << 30, "never fired");
        }
    }

    #[test]
    fn an_allocation_larger_than_the_target_fires_immediately() {
        let mut s = Sampler::with_seed(512, 7);
        let weight = s.on_alloc(1 << 30).expect("should fire");
        assert_eq!(weight, 1 << 30);
    }

    #[test]
    fn declining_a_sample_keeps_the_counter() {
        // Mirrors the C++ behaviour when the allocation map is full: the
        // caller does not reset, so the counter keeps growing.
        let mut s = Sampler::with_seed(64, 3);
        let first = loop {
            if let Some(w) = s.on_alloc(8) {
                break w;
            }
        };
        let second = s.on_alloc(8).expect("should still be over the target");
        assert!(second > first, "{second} !> {first}");
        assert_eq!(s.allocated(), second);
    }

    #[test]
    fn reset_clears_the_counter_and_redraws() {
        let mut s = Sampler::with_seed(1024, 11);
        while s.on_alloc(64).is_none() {}
        s.reset();
        assert_eq!(s.allocated(), 0);
    }

    /// The whole point of the sampler: total sampled weight must approximate
    /// total bytes allocated. A factor-of-R error here would silently skew
    /// every profile while still looking plausible.
    ///
    /// The estimator is unbiased but noisy, so each case drives enough bytes
    /// to expect a useful number of samples, and the tolerance is derived from
    /// how many actually landed. Spacing between samples is exponential, so
    /// the relative error of a sum of `n` of them falls off as `1/sqrt(n)`;
    /// four standard errors gives a bound that is tight but not flaky.
    #[test]
    fn total_weight_approximates_total_bytes() {
        // (sampling interval, mean allocation size). Deliberately spans mean
        // sizes far below, near, and far above the interval. Paired so that
        // driving a few hundred intervals' worth of bytes stays cheap.
        const CASES: &[(u32, usize)] = &[
            (1, 1),
            (1, 64),
            (64, 1),
            (64, 64),
            (64, 4096),
            (4096, 8),
            (4096, 4096),
            (4096, 1 << 16),
            (512 * 1024, 1024),
            (512 * 1024, 512 * 1024),
            (512 * 1024, 1 << 22),
            (16 * 1024 * 1024, 1 << 15),
            (16 * 1024 * 1024, 1 << 24),
        ];
        // Miri is ~100x slower and the job has a 20 minute budget, so there
        // it runs a handful of cheap cases: the arithmetic paths are the same,
        // and convergence is what native runs are for.
        const CASES_MIRI: &[(u32, usize)] = &[(1, 1), (64, 64), (4096, 512), (512 * 1024, 1 << 16)];
        let (cases, want_samples): (&[(u32, usize)], u64) = if cfg!(miri) {
            (CASES_MIRI, 20)
        } else {
            (CASES, 400)
        };

        for &(interval, mean_size) in cases {
            let mut s = Sampler::with_seed(interval, 0xABCD);
            let mut rng = MinstdRand::new(0x1234);
            let budget = u64::from(interval).max(1).saturating_mul(want_samples);

            let mut allocated = 0u64;
            let mut sampled = 0u64;
            let mut samples = 0u64;
            while allocated < budget {
                // Uniform in 1..=2*mean_size, so the mean is mean_size.
                let span = (mean_size as u32).saturating_mul(2).max(1);
                let size = (rng.next_u32() % span).max(1) as usize;
                allocated += size as u64;
                if let Some(weight) = s.on_alloc(size) {
                    sampled += weight;
                    samples += 1;
                    s.reset();
                }
            }

            assert!(
                samples > 0,
                "interval {interval} mean size {mean_size}: no samples at all"
            );
            let ratio = sampled as f64 / allocated as f64;
            let tolerance = (4.0 / (samples as f64).sqrt()).clamp(0.10, 1.5);
            assert!(
                (ratio - 1.0).abs() <= tolerance,
                "interval {interval} mean size {mean_size}: sampled {sampled} \
                 of {allocated} bytes over {samples} samples \
                 (ratio {ratio:.4}, tolerance {tolerance:.4})"
            );
        }
    }

    /// The interval bounds must not divide by zero, overflow, or stall.
    ///
    /// A drawn target of 0 is legitimate: it means "sample this allocation",
    /// and with an interval of 0 it is the common case. So this asserts the
    /// target stays in range, not that it is non-zero.
    #[test]
    fn extreme_intervals_are_safe() {
        for interval in [0u32, 1, u32::MAX] {
            let mut s = Sampler::with_seed(interval, interval);
            for _ in 0..1_000 {
                assert!(
                    s.target() <= u64::from(u32::MAX),
                    "interval {interval} target {} out of range",
                    s.target()
                );
                if s.on_alloc(1).is_some() {
                    s.reset();
                }
            }
        }
    }

    /// An interval of 0 means "sample everything"; it must not divide by zero
    /// or stall.
    #[test]
    fn zero_interval_samples_frequently() {
        let mut s = Sampler::with_seed(0, 0);
        let mut fired = 0;
        for _ in 0..1_000 {
            if s.on_alloc(1).is_some() {
                fired += 1;
                s.reset();
            }
        }
        assert!(fired > 100, "only fired {fired} times out of 1000");
    }

    /// With a 4 GiB interval, single-byte allocations must simply accumulate.
    #[test]
    fn a_huge_interval_does_not_oversample() {
        let mut s = Sampler::with_seed(u32::MAX, 1);
        let mut fired = 0;
        for _ in 0..10_000 {
            if s.on_alloc(1).is_some() {
                fired += 1;
                s.reset();
            }
        }
        assert_eq!(fired, 0, "fired {fired} times with a 4 GiB interval");
        assert_eq!(s.allocated(), 10_000);
    }

    #[test]
    fn scaled_count_floors_zero_sized_allocations() {
        assert_eq!(scaled_count(0, 100), 100);
        assert_eq!(scaled_count(1, 100), 100);
        assert_eq!(scaled_count(10, 100), 10);
        assert_eq!(scaled_count(100, 100), 1);
        assert_eq!(scaled_count(1000, 100), 0);
        assert_eq!(scaled_count(0, 0), 0);
    }
}
