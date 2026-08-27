//! Property tests for `sqrt_price_math` against U512 references at full
//! precision, plus pool-favoring rounding invariants.

mod common;

use amm_math::sqrt_price_math::*;
use amm_math::tick_math::{get_sqrt_ratio_at_tick, MAX_SQRT_RATIO, MIN_SQRT_RATIO, MAX_TICK, MIN_TICK};
use common::*;
use primitive_types::U512;
use proptest::prelude::*;

const Q96: u128 = 1 << 96;

fn price() -> impl Strategy<Value = u128> {
    prop_oneof![
        2 => (MIN_TICK..=MAX_TICK).prop_map(get_sqrt_ratio_at_tick),
        3 => MIN_SQRT_RATIO..=MAX_SQRT_RATIO,
        1 => Just(MIN_SQRT_RATIO),
        1 => Just(MAX_SQRT_RATIO),
        1 => Just(Q96),
    ]
}

fn liquidity() -> impl Strategy<Value = u128> {
    prop_oneof![
        4 => 1u128..=u64::MAX as u128,
        3 => 1u128..=u128::MAX,
        1 => Just(1u128),
        1 => Just(u64::MAX as u128),
        1 => Just(u128::MAX - 1),
        1 => Just(u128::MAX),
    ]
}

fn amount() -> impl Strategy<Value = u128> {
    prop_oneof![
        4 => 0u128..=u64::MAX as u128,
        2 => 0u128..=u128::MAX,
        1 => Just(0u128),
        1 => Just(u64::MAX as u128),
    ]
}

fn cfg() -> ProptestConfig {
    ProptestConfig {
        cases: 512,
        max_global_rejects: 65536,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(cfg())]

    /// amount0: exact match vs the two-step U512 reference, symmetry in the
    /// price arguments, two-step floor == single-step floor (identity), and
    /// pool-favoring ceil bounds vs the exact rational.
    #[test]
    fn amount0_matches_reference_and_rational_bounds(a in price(), b in price(), l in liquidity()) {
        let ref_ceil = ref_amount0(a, b, l, true);
        prop_assume!(fits_u128(ref_ceil));
        let lib_floor = get_amount0_delta(a, b, l, false);
        let lib_ceil = get_amount0_delta(a, b, l, true);
        prop_assert_eq!(u512(lib_floor), ref_amount0(a, b, l, false));
        prop_assert_eq!(u512(lib_ceil), ref_ceil);
        // Symmetry a <-> b.
        prop_assert_eq!(lib_floor, get_amount0_delta(b, a, l, false));
        prop_assert_eq!(lib_ceil, get_amount0_delta(b, a, l, true));

        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        if l != 0 && lo != hi {
            let num = (u512(l) << 96) * u512(hi - lo);
            let den = u512(lo) * u512(hi);
            let single_floor = num / den;
            let single_ceil = div_round(num, den, true);
            // Nested floor division equals single floor division.
            prop_assert_eq!(u512(lib_floor), single_floor);
            // Round-up result covers the exact rational (pool never loses)
            // and overshoots by at most 2 (one per division step).
            prop_assert!(u512(lib_ceil) >= single_ceil);
            prop_assert!(u512(lib_ceil) <= single_ceil + U512::from(2u64));
        }
    }

    /// amount1: exact match vs U512 reference; ceil covers the exact
    /// rational and exceeds floor by at most 1.
    #[test]
    fn amount1_matches_reference_and_rational_bounds(a in price(), b in price(), l in liquidity()) {
        let ref_ceil = ref_amount1(a, b, l, true);
        prop_assume!(fits_u128(ref_ceil));
        let lib_floor = get_amount1_delta(a, b, l, false);
        let lib_ceil = get_amount1_delta(a, b, l, true);
        prop_assert_eq!(u512(lib_floor), ref_amount1(a, b, l, false));
        prop_assert_eq!(u512(lib_ceil), ref_ceil);
        prop_assert_eq!(lib_floor, get_amount1_delta(b, a, l, false));
        prop_assert!(lib_ceil - lib_floor <= 1);
    }

    /// Next price from input: matches the U512 exact-denominator reference,
    /// moves the right direction, and the (round-up) input actually needed
    /// to reach it never exceeds `amount_in` — the pool cannot be
    /// shortchanged by rounding.
    #[test]
    fn next_from_input_matches_reference_and_never_exceeds_amount(
        p in price(), l in liquidity(), x in amount(), zero_for_one in any::<bool>()
    ) {
        let reference = ref_next_from_input(p, l, x, zero_for_one).unwrap();
        prop_assume!(fits_u128(reference));
        let q = get_next_sqrt_price_from_input(p, l, x, zero_for_one);
        prop_assert_eq!(u512(q), reference);
        if zero_for_one {
            prop_assert!(q <= p);
            prop_assert!(ref_amount0(q, p, l, true) <= u512(x));
        } else {
            prop_assert!(q >= p);
            prop_assert!(ref_amount1(p, q, l, true) <= u512(x));
        }
    }

    /// Next price from output: matches the U512 reference, moves the right
    /// direction, and the (round-down) output freed by the move covers the
    /// full request — the pool never delivers less than priced.
    #[test]
    fn next_from_output_matches_reference_and_covers_request(
        p in price(), l in liquidity(), x in amount(), zero_for_one in any::<bool>()
    ) {
        let reference = ref_next_from_output(p, l, x, zero_for_one);
        prop_assume!(reference.is_some());
        let reference = reference.unwrap();
        prop_assume!(fits_u128(reference));
        let q = get_next_sqrt_price_from_output(p, l, x, zero_for_one);
        prop_assert_eq!(u512(q), reference);
        if zero_for_one {
            prop_assert!(q <= p);
            prop_assert!(ref_amount1(q, p, l, false) >= u512(x));
        } else {
            prop_assert!(q >= p);
            prop_assert!(ref_amount0(p, q, l, false) >= u512(x));
        }
    }

    /// Round-trip consistency: for a price move computed from an input, the
    /// floor-rounded amounts never exceed the ceil-rounded amounts.
    #[test]
    fn floor_never_exceeds_ceil(a in price(), b in price(), l in liquidity()) {
        prop_assume!(fits_u128(ref_amount0(a, b, l, true)));
        prop_assume!(fits_u128(ref_amount1(a, b, l, true)));
        prop_assert!(get_amount0_delta(a, b, l, false) <= get_amount0_delta(a, b, l, true));
        prop_assert!(get_amount1_delta(a, b, l, false) <= get_amount1_delta(a, b, l, true));
    }
}

// ---------------------------------------------------------------------------
// Widest-case deterministic tests (u128 liquidity at its extremes)
// ---------------------------------------------------------------------------

#[test]
fn amount1_at_max_liquidity_exact_boundary() {
    // L = u128::MAX over exactly one Q96 of price range: amount1 = L.
    let got = get_amount1_delta(Q96, 2 * Q96, u128::MAX, true);
    assert_eq!(got, u128::MAX);
    assert_eq!(get_amount1_delta(Q96, 2 * Q96, u128::MAX, false), u128::MAX);
}

#[test]
fn amount1_near_max_delta_fits() {
    // Liquidity just under 2^96 across the full supported price range.
    let l = (1u128 << 96) - 1;
    let got = get_amount1_delta(MIN_SQRT_RATIO, MAX_SQRT_RATIO, l, true);
    assert_eq!(
        u512(got),
        ref_amount1(MIN_SQRT_RATIO, MAX_SQRT_RATIO, l, true)
    );
}

#[test]
fn amount0_at_max_liquidity_narrow_range() {
    let a = MAX_SQRT_RATIO - Q96;
    let b = MAX_SQRT_RATIO;
    let got = get_amount0_delta(a, b, u128::MAX, true);
    assert_eq!(u512(got), ref_amount0(a, b, u128::MAX, true));
    assert!(got > 0);
}

#[test]
#[should_panic(expected = "quotient overflow")]
fn amount0_at_max_liquidity_full_range_overflows_and_panics() {
    // L = u128::MAX across the full range yields ~2^160 token0: cannot be
    // represented; the tx must fail.
    let _ = get_amount0_delta(MIN_SQRT_RATIO, MAX_SQRT_RATIO, u128::MAX, true);
}

#[test]
fn next_from_input_at_max_liquidity_matches_reference() {
    for zero_for_one in [true, false] {
        let p = Q96;
        let x = u64::MAX as u128;
        let l = u128::MAX;
        let q = get_next_sqrt_price_from_input(p, l, x, zero_for_one);
        assert_eq!(u512(q), ref_next_from_input(p, l, x, zero_for_one).unwrap());
    }
}

#[test]
fn next_from_output_at_max_liquidity_matches_reference() {
    for zero_for_one in [true, false] {
        let p = Q96;
        let x = u64::MAX as u128;
        let l = u128::MAX;
        let q = get_next_sqrt_price_from_output(p, l, x, zero_for_one);
        assert_eq!(u512(q), ref_next_from_output(p, l, x, zero_for_one).unwrap());
    }
}
