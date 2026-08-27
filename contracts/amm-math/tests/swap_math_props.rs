//! Property tests for `swap_math::compute_swap_step` against a U512
//! reference port, plus port-independent conservation invariants.

mod common;

use amm_math::swap_math::compute_swap_step;
use amm_math::tick_math::{get_sqrt_ratio_at_tick, MAX_SQRT_RATIO, MIN_SQRT_RATIO, MAX_TICK, MIN_TICK};
use common::*;
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
        1 => Just(0u128),
        4 => 1u128..=u64::MAX as u128,
        3 => 1u128..=u128::MAX,
        1 => Just(1u128),
        1 => Just(u128::MAX),
    ]
}

fn amount_remaining() -> impl Strategy<Value = i128> {
    prop_oneof![
        4 => -(u64::MAX as i128)..=u64::MAX as i128,
        1 => any::<i128>(),
        1 => Just(0i128),
        1 => Just(1i128),
        1 => Just(-1i128),
    ]
}

fn fee() -> impl Strategy<Value = u32> {
    prop_oneof![
        1 => Just(0u32),
        2 => Just(500u32),
        2 => Just(3000u32),
        2 => Just(10000u32),
        2 => 0u32..1_000_000,
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

    #[test]
    fn swap_step_matches_reference_and_invariants(
        current in price(),
        target in price(),
        l in liquidity(),
        rem in amount_remaining(),
        fee_pips in fee(),
    ) {
        let reference = ref_compute_swap_step(current, target, l, rem, fee_pips);
        prop_assume!(reference.is_some());
        let (r_next, r_in, r_out, r_fee) = reference.unwrap();

        let (next, amount_in, amount_out, fee_amount) =
            compute_swap_step(current, target, l, rem, fee_pips);

        // Exact match with the full-precision reference port.
        prop_assert_eq!((next, amount_in, amount_out, fee_amount), (r_next, r_in, r_out, r_fee));

        // --- Port-independent invariants ---

        // Price moves toward the target and never past it.
        let (lo, hi) = if current <= target { (current, target) } else { (target, current) };
        prop_assert!(next >= lo && next <= hi, "price moved outside [current, target]");

        if rem >= 0 {
            // Exact input: consumed input + fee never exceeds the remaining
            // amount; when the target was not reached the entire remainder
            // is consumed (rest goes to fee).
            let rem_u = rem as u128;
            prop_assert!(fee_amount <= rem_u, "fee exceeds amount_remaining");
            let total = amount_in.checked_add(fee_amount).expect("in+fee overflow");
            prop_assert!(total <= rem_u, "amount_in + fee exceeds amount_remaining");
            if next != target {
                prop_assert_eq!(total, rem_u);
            }
        } else {
            // Exact output: never deliver more than requested; when the
            // target was not reached the request is met exactly.
            let requested = rem.unsigned_abs();
            prop_assert!(amount_out <= requested, "delivered more than requested");
            if next != target {
                prop_assert_eq!(amount_out, requested);
            }
        }

        // Fee is never below the exact pro-rata floor (pool never loses).
        prop_assert!(
            u512(fee_amount) >= u512(amount_in) * u512(fee_pips as u128) / u512((1_000_000 - fee_pips) as u128),
            "fee below pro-rata floor"
        );
    }

    /// Exact-in with a huge budget always reaches the target exactly.
    #[test]
    fn exact_in_large_budget_reaches_target(
        current in price(), target in price(), l in 1u128..=u64::MAX as u128, fee_pips in fee()
    ) {
        // Budget: enough for any in-range move at u64-scale liquidity.
        let rem: i128 = i128::MAX;
        let reference = ref_compute_swap_step(current, target, l, rem, fee_pips);
        prop_assume!(reference.is_some());
        let (next, _in, _out, _fee) = compute_swap_step(current, target, l, rem, fee_pips);
        prop_assert_eq!(next, target);
    }

    /// Fee of an exact-in swap consumes the input fully when the price
    /// cannot move (current == target).
    #[test]
    fn degenerate_segment_consumes_nothing(
        p in price(), l in liquidity(), rem in amount_remaining(), fee_pips in fee()
    ) {
        let reference = ref_compute_swap_step(p, p, l, rem, fee_pips);
        prop_assume!(reference.is_some());
        let (next, amount_in, amount_out, _fee) = compute_swap_step(p, p, l, rem, fee_pips);
        prop_assert_eq!(next, p);
        prop_assert_eq!(amount_in, 0);
        prop_assert_eq!(amount_out, 0);
    }
}

// ---------------------------------------------------------------------------
// Deterministic scenario tests
// ---------------------------------------------------------------------------

/// Uniswap-style scenario: exact-in that reaches the target with budget to
/// spare; amounts must match the target-capped deltas.
#[test]
fn exact_in_capped_at_target() {
    let current = Q96;
    let target = get_sqrt_ratio_at_tick(-100);
    let l = 10u128.pow(20);
    let (next, amount_in, amount_out, fee) =
        compute_swap_step(current, target, l, i128::MAX, 3000);
    assert_eq!(next, target);
    assert_eq!(u512(amount_in), ref_amount0(target, current, l, true));
    assert_eq!(u512(amount_out), ref_amount1(target, current, l, false));
    assert!(fee > 0);
    assert!(amount_in > amount_out); // price fell: token0 in is dearer
}

/// Exact-out that exhausts the segment: output is capped by what the
/// segment can deliver, not the request.
#[test]
fn exact_out_capped_at_target() {
    let current = Q96;
    let target = get_sqrt_ratio_at_tick(100);
    let l = 10u128.pow(12);
    let (next, _amount_in, amount_out, _fee) =
        compute_swap_step(current, target, l, -(1i128 << 100), 3000);
    assert_eq!(next, target);
    // Requested far more than the segment holds: delivered = full segment.
    assert_eq!(u512(amount_out), ref_amount0(current, target, l, false));
    assert!(amount_out < 1u128 << 100);
}

/// Zero fee: exact-in consumes the full remainder as input when the target
/// is not reached and charges no fee.
#[test]
fn zero_fee_exact_in() {
    let current = Q96;
    let target = get_sqrt_ratio_at_tick(-10_000);
    let l = 10u128.pow(24);
    let rem = 1_000_000i128;
    let (next, amount_in, _out, fee) = compute_swap_step(current, target, l, rem, 0);
    assert!(next > target && next < current);
    assert_eq!(fee, rem as u128 - amount_in);
    // With fee_pips == 0 the unconsumed remainder is only rounding dust.
    assert!(fee <= 1);
}
