//! Tick math verification: constant derivation, monotonicity, round-trips,
//! and a dense relative-error sweep against the U1024 reference.

mod common;

use amm_math::tick_math::*;
use common::*;
use proptest::prelude::*;

/// (a) The hard-coded per-bit constants must equal the values derived from
/// first principles with U1024 fixed point. On mismatch, prints the correct
/// table to paste into `tick_math.rs`.
#[test]
fn tick_constants_match_u1024_derivation() {
    let derived = derive_tick_constants();
    if derived != TICK_BIT_CONSTANTS {
        println!("TICK_BIT_CONSTANTS is wrong; correct table:");
        for (i, c) in derived.iter().enumerate() {
            println!("    0x{c:032x}, // bit {i}");
        }
        panic!("TICK_BIT_CONSTANTS do not match the U1024 derivation");
    }
}

#[test]
fn min_max_ratio_constants_match_function() {
    assert_eq!(get_sqrt_ratio_at_tick(MIN_TICK), MIN_SQRT_RATIO);
    assert_eq!(get_sqrt_ratio_at_tick(MAX_TICK), MAX_SQRT_RATIO);
}

/// Sample ticks: all boundaries, a dense band around zero, and a sweep of
/// the whole range.
fn sampled_ticks() -> Vec<i32> {
    let mut ticks: Vec<i32> = vec![MIN_TICK, MIN_TICK + 1, MIN_TICK + 2, MAX_TICK - 2, MAX_TICK - 1, MAX_TICK];
    ticks.extend(-1000..=1000);
    let mut t = MIN_TICK;
    while t <= MAX_TICK {
        ticks.push(t);
        t += 1000;
    }
    ticks.push(MAX_TICK);
    ticks.sort_unstable();
    ticks.dedup();
    ticks
}

/// (b) Strict monotonicity across every sampled tick and its successor.
#[test]
fn monotonic_over_sampled_and_boundary_ticks() {
    for &t in sampled_ticks().iter() {
        if t < MAX_TICK {
            assert!(
                get_sqrt_ratio_at_tick(t) < get_sqrt_ratio_at_tick(t + 1),
                "ratio not strictly increasing at tick {t}"
            );
        }
    }
}

/// (c) Round-trip and ±1-ulp bracket property on all sampled ticks.
#[test]
fn round_trip_and_ulp_brackets() {
    for &t in sampled_ticks().iter() {
        let x = get_sqrt_ratio_at_tick(t);
        if t < MAX_TICK {
            assert_eq!(get_tick_at_sqrt_ratio(x), t, "round-trip at tick {t}");
            // One ulp below the next tick's ratio still maps to t.
            let next = get_sqrt_ratio_at_tick(t + 1);
            assert_eq!(get_tick_at_sqrt_ratio(next - 1), t, "upper bracket at tick {t}");
        }
        // One ulp below this tick's ratio maps to t - 1.
        if t > MIN_TICK && x > MIN_SQRT_RATIO {
            assert_eq!(get_tick_at_sqrt_ratio(x - 1), t - 1, "lower bracket at tick {t}");
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn monotonic_random_pairs(t1 in MIN_TICK..=MAX_TICK, t2 in MIN_TICK..=MAX_TICK) {
        prop_assume!(t1 < t2);
        prop_assert!(get_sqrt_ratio_at_tick(t1) < get_sqrt_ratio_at_tick(t2));
    }

    #[test]
    fn tick_at_ratio_bracket_random(x in MIN_SQRT_RATIO..MAX_SQRT_RATIO) {
        let t = get_tick_at_sqrt_ratio(x);
        prop_assert!(get_sqrt_ratio_at_tick(t) <= x);
        prop_assert!(x < get_sqrt_ratio_at_tick(t + 1));
    }

    #[test]
    fn round_trip_random_ticks(t in MIN_TICK..MAX_TICK) {
        prop_assert_eq!(get_tick_at_sqrt_ratio(get_sqrt_ratio_at_tick(t)), t);
    }
}

/// (d) Dense error sweep vs the exact U1024 reference: every 1000 ticks,
/// all of [-1000, 1000], and the boundaries. Prints the maximum observed
/// relative error (for the phase report) and asserts the bound.
///
/// Two figures are tracked, because Q64.96 has a representation floor:
/// - `raw`: |lib - exact| / exact. At deep negative ticks the output is
///   ~2^64, so a *correctly rounded* result is still up to 1 ulp
///   (2^-64 relative) away from the exact real value; no algorithm in this
///   format can do better. Asserted < 2^-63.
/// - `algorithmic`: the error beyond the unavoidable 1-ulp final-rounding
///   allowance, i.e. max(0, |lib - exact| - 1 ulp) / exact. This measures
///   the quality of the bit-decomposition itself and is asserted < 2^-90.
/// - `raw over ticks >= 0` (outputs have >= 96 fractional bits, so
///   quantization is negligible): asserted < 2^-90.
#[test]
fn max_relative_error_sweep_vs_u1024_reference() {
    let powers = inv_sqrt_powers();
    const SCALE: usize = 100; // errors are reported as err * 2^SCALE

    let mut max_raw: (u128, i32) = (0, 0);
    let mut max_raw_nonneg: (u128, i32) = (0, 0);
    let mut max_alg: (u128, i32) = (0, 0);

    for &t in sampled_ticks().iter() {
        let lib = U1024::from(get_sqrt_ratio_at_tick(t));
        let exact = ref_sqrt_ratio_qf(&powers, t);
        // Common units of 2^-(96+F): lib * 2^F vs exact * 2^96.
        let lhs = lib << F;
        let rhs = exact << 96;
        let diff = if lhs >= rhs { lhs - rhs } else { rhs - lhs };

        let raw = ((diff << SCALE) / rhs).low_u128();
        if raw > max_raw.0 {
            max_raw = (raw, t);
        }
        if t >= 0 && raw > max_raw_nonneg.0 {
            max_raw_nonneg = (raw, t);
        }

        // Subtract one Q64.96 ulp (= 2^F in these units).
        let ulp = U1024::one() << F;
        let alg_diff = if diff > ulp { diff - ulp } else { U1024::zero() };
        let alg = ((alg_diff << SCALE) / rhs).low_u128();
        if alg > max_alg.0 {
            max_alg = (alg, t);
        }
    }

    let to_f = |x: u128| x as f64 / 2f64.powi(SCALE as i32);
    println!(
        "tick sweep max relative error (raw, incl. Q64.96 quantization): {:.3e} (~2^{:.2}) at tick {}",
        to_f(max_raw.0),
        to_f(max_raw.0).log2(),
        max_raw.1
    );
    println!(
        "tick sweep max relative error (raw, ticks >= 0):                {:.3e} (~2^{:.2}) at tick {}",
        to_f(max_raw_nonneg.0),
        to_f(max_raw_nonneg.0).log2(),
        max_raw_nonneg.1
    );
    println!(
        "tick sweep max relative error (beyond 1-ulp final rounding):    {:.3e} (~2^{:.2}) at tick {}",
        to_f(max_alg.0),
        to_f(max_alg.0).log2(),
        max_alg.1
    );

    // 2^-90 at SCALE=100 is 2^10; 2^-63 is 2^37.
    assert!(max_alg.0 < 1 << 10, "algorithmic error >= 2^-90");
    assert!(max_raw_nonneg.0 < 1 << 10, "raw error over ticks >= 0 is >= 2^-90");
    assert!(max_raw.0 < 1 << 37, "raw error >= 2^-63 (worse than 1 ulp at the format floor)");
}
