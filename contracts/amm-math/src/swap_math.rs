//! Faithful port of Uniswap v3 `SwapMath.computeSwapStep`.

use crate::muldiv;
use crate::sqrt_price_math;
use crate::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};

/// Fee denominator: fees are expressed in hundredths of a bip (pips),
/// e.g. 500, 3000, 10000.
pub const FEE_PIPS_DENOMINATOR: u32 = 1_000_000;

/// Computes one step of a swap within a single price segment.
///
/// Inputs:
/// - `sqrt_ratio_current`, `sqrt_ratio_target`: Q64.96 sqrt prices; the
///   direction is `zero_for_one` iff `current >= target`. Both must be in
///   the supported range.
/// - `liquidity`: in-range liquidity (u128).
/// - `amount_remaining`: `>= 0` means **exact input** (fee taken from the
///   input first); `< 0` means **exact output**.
/// - `fee_pips`: fee in hundredths of a bip; must be `< 1_000_000`.
///
/// Returns `(sqrt_ratio_next, amount_in, amount_out, fee_amount)` where
/// `sqrt_ratio_next` never moves past `sqrt_ratio_target`, `amount_in` and
/// `fee_amount` are owed to the pool, and `amount_out` is paid by the pool.
///
/// Rounding (all directions favor the pool, mirroring Uniswap):
/// - `amount_in` is rounded **up**, `amount_out` rounded **down**;
/// - `fee_amount` is rounded **up** (`mulDivRoundingUp`), except in the
///   exact-input case where the target is not reached: there the entire
///   unconsumed remainder is taken as fee
///   (`fee_amount = amount_remaining - amount_in`, Uniswap's special case);
/// - exact-output never delivers more than requested (`amount_out` is
///   capped at `-amount_remaining`; the rounding surplus stays with the
///   pool).
///
/// Panics on out-of-range prices, `fee_pips >= 1_000_000`, or arithmetic
/// under/overflow (the tx fails).
pub fn compute_swap_step(
    sqrt_ratio_current: u128,
    sqrt_ratio_target: u128,
    liquidity: u128,
    amount_remaining: i128,
    fee_pips: u32,
) -> (u128, u128, u128, u128) {
    assert!(
        (MIN_SQRT_RATIO..=MAX_SQRT_RATIO).contains(&sqrt_ratio_current)
            && (MIN_SQRT_RATIO..=MAX_SQRT_RATIO).contains(&sqrt_ratio_target),
        "swap_math: sqrt price out of supported range"
    );
    assert!(
        fee_pips < FEE_PIPS_DENOMINATOR,
        "swap_math: fee_pips out of range"
    );

    let zero_for_one = sqrt_ratio_current >= sqrt_ratio_target;
    let exact_in = amount_remaining >= 0;

    let sqrt_ratio_next;
    let mut amount_in = 0u128;
    let mut amount_out = 0u128;

    if exact_in {
        let amount_remaining_less_fee = muldiv::mul_div_floor(
            amount_remaining as u128,
            (FEE_PIPS_DENOMINATOR - fee_pips) as u128,
            FEE_PIPS_DENOMINATOR as u128,
        );
        amount_in = if zero_for_one {
            sqrt_price_math::get_amount0_delta(sqrt_ratio_target, sqrt_ratio_current, liquidity, true)
        } else {
            sqrt_price_math::get_amount1_delta(sqrt_ratio_current, sqrt_ratio_target, liquidity, true)
        };
        sqrt_ratio_next = if amount_remaining_less_fee >= amount_in {
            sqrt_ratio_target
        } else {
            sqrt_price_math::get_next_sqrt_price_from_input(
                sqrt_ratio_current,
                liquidity,
                amount_remaining_less_fee,
                zero_for_one,
            )
        };
    } else {
        let amount_out_requested = amount_remaining.unsigned_abs();
        amount_out = if zero_for_one {
            sqrt_price_math::get_amount1_delta(sqrt_ratio_target, sqrt_ratio_current, liquidity, false)
        } else {
            sqrt_price_math::get_amount0_delta(sqrt_ratio_current, sqrt_ratio_target, liquidity, false)
        };
        sqrt_ratio_next = if amount_out_requested >= amount_out {
            sqrt_ratio_target
        } else {
            sqrt_price_math::get_next_sqrt_price_from_output(
                sqrt_ratio_current,
                liquidity,
                amount_out_requested,
                zero_for_one,
            )
        };
    }

    let max = sqrt_ratio_target == sqrt_ratio_next;

    if zero_for_one {
        if !(max && exact_in) {
            amount_in = sqrt_price_math::get_amount0_delta(
                sqrt_ratio_next,
                sqrt_ratio_current,
                liquidity,
                true,
            );
        }
        if !(max && !exact_in) {
            amount_out = sqrt_price_math::get_amount1_delta(
                sqrt_ratio_next,
                sqrt_ratio_current,
                liquidity,
                false,
            );
        }
    } else {
        if !(max && exact_in) {
            amount_in = sqrt_price_math::get_amount1_delta(
                sqrt_ratio_current,
                sqrt_ratio_next,
                liquidity,
                true,
            );
        }
        if !(max && !exact_in) {
            amount_out = sqrt_price_math::get_amount0_delta(
                sqrt_ratio_current,
                sqrt_ratio_next,
                liquidity,
                false,
            );
        }
    }

    // Exact output: never deliver more than requested (rounding surplus
    // stays with the pool).
    if !exact_in {
        let requested = amount_remaining.unsigned_abs();
        if amount_out > requested {
            amount_out = requested;
        }
    }

    let fee_amount = if exact_in && sqrt_ratio_next != sqrt_ratio_target {
        // Target not reached in exact-in: the whole unconsumed remainder is
        // the fee (Uniswap special case; pool-favoring).
        (amount_remaining as u128) - amount_in
    } else {
        muldiv::mul_div_ceil(
            amount_in,
            fee_pips as u128,
            (FEE_PIPS_DENOMINATOR - fee_pips) as u128,
        )
    };

    (sqrt_ratio_next, amount_in, amount_out, fee_amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q96: u128 = 1 << 96;

    #[test]
    fn zero_liquidity_reaches_target_with_zero_amounts() {
        let (next, ain, aout, fee) = compute_swap_step(Q96, Q96 / 2, 0, 1_000_000, 3000);
        assert_eq!(next, Q96 / 2);
        assert_eq!((ain, aout, fee), (0, 0, 0));
    }

    #[test]
    fn exact_in_all_consumed_as_fee_when_amount_too_small() {
        // amount_remaining_less_fee floors to 0 -> price does not move,
        // everything becomes fee.
        let (next, ain, aout, fee) = compute_swap_step(Q96, Q96 / 2, 10u128.pow(20), 1, 500);
        assert_eq!(next, Q96);
        assert_eq!(ain, 0);
        assert_eq!(aout, 0);
        assert_eq!(fee, 1);
    }

    #[test]
    #[should_panic(expected = "fee_pips out of range")]
    fn fee_at_denominator_panics() {
        let _ = compute_swap_step(Q96, Q96 / 2, 1, 1, FEE_PIPS_DENOMINATOR);
    }
}
