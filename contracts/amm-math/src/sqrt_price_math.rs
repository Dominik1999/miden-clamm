//! Port of Uniswap v3 `SqrtPriceMath` on `u128`/limb arithmetic, keeping
//! Uniswap's rounding directions (every rounding favors the pool).
//!
//! Divergence from Uniswap sizing (DESIGN.md Part 3): sqrt prices are
//! Q64.96 in `u128` over ticks ±443,636 and liquidity is `u128`. All wide
//! intermediates (up to ~352 bits) go through [`crate::wide`]; where
//! Uniswap needs a uint256-overflow fallback formula, we simply compute the
//! exact-denominator formula on limbs.

use crate::muldiv::{div_wide, div_wide_u128};
use crate::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};
use crate::wide;

fn assert_price_in_range(sqrt_price_x96: u128) {
    assert!(
        (MIN_SQRT_RATIO..=MAX_SQRT_RATIO).contains(&sqrt_price_x96),
        "sqrt_price_math: sqrt price out of supported range"
    );
}

/// `liquidity << 96` as 4 little-endian limbs (up to 224 bits).
fn shl96(liquidity: u128) -> [u64; 4] {
    let [l0, l1] = wide::limbs_from_u128(liquidity);
    [0, l0 << 32, (l0 >> 32) | (l1 << 32), l1 >> 32]
}

/// Amount of token0 spanned by `[min(a,b), max(a,b)]` at `liquidity`:
/// `amount0 = L * 2^96 * (sqrt_b - sqrt_a) / (sqrt_b * sqrt_a)`.
///
/// Computed exactly as Uniswap does, in two division steps:
/// `mul_div(L << 96, sqrt_b - sqrt_a, sqrt_b)` then `/ sqrt_a`, with
/// round-up applied at **both** steps when `round_up` (matching
/// `FullMath.mulDivRoundingUp` + `UnsafeMath.divRoundingUp`).
///
/// Rounding: **up** when `round_up` (what a user owes the pool), **down**
/// otherwise (what the pool pays out) — both directions favor the pool.
///
/// Panics if either price is outside the supported range or the result
/// does not fit `u128` (possible only for liquidity far beyond any u64
/// token balance; the tx fails).
pub fn get_amount0_delta(sqrt_a: u128, sqrt_b: u128, liquidity: u128, round_up: bool) -> u128 {
    assert_price_in_range(sqrt_a);
    assert_price_in_range(sqrt_b);
    let (lo, hi) = if sqrt_a <= sqrt_b {
        (sqrt_a, sqrt_b)
    } else {
        (sqrt_b, sqrt_a)
    };
    if liquidity == 0 || lo == hi {
        return 0;
    }
    let numerator1 = shl96(liquidity);
    let mut numerator = [0u64; 6];
    wide::mul_limbs(&numerator1, &wide::limbs_from_u128(hi - lo), &mut numerator);
    let step1 = div_wide(&numerator, &wide::limbs_from_u128(hi), round_up);
    div_wide_u128(&step1, &wide::limbs_from_u128(lo), round_up)
}

/// Amount of token1 spanned by `[min(a,b), max(a,b)]` at `liquidity`:
/// `amount1 = L * (sqrt_b - sqrt_a) / 2^96` (i.e. `mul_div(L, delta, Q96)`).
///
/// Rounding: **up** when `round_up` (owed to the pool), **down** otherwise
/// (paid by the pool) — both directions favor the pool.
///
/// Panics if either price is outside the supported range or the result
/// does not fit `u128`.
pub fn get_amount1_delta(sqrt_a: u128, sqrt_b: u128, liquidity: u128, round_up: bool) -> u128 {
    assert_price_in_range(sqrt_a);
    assert_price_in_range(sqrt_b);
    let (lo, hi) = if sqrt_a <= sqrt_b {
        (sqrt_a, sqrt_b)
    } else {
        (sqrt_b, sqrt_a)
    };
    let p = wide::mul_u128(liquidity, hi - lo);
    // Divide by 2^96 == shift right by 96 over the limbs.
    assert!(p[3] >> 32 == 0, "sqrt_price_math: amount1 overflow");
    let q = ((p[1] >> 32) | (p[2] << 32)) as u128 | (((p[2] >> 32) | (p[3] << 32)) as u128) << 64;
    let rem_nonzero = p[0] != 0 || p[1] & 0xFFFF_FFFF != 0;
    if round_up && rem_nonzero {
        q.checked_add(1).expect("sqrt_price_math: amount1 overflow")
    } else {
        q
    }
}

/// Next sqrt price after adding (`add == true`) or removing (`add == false`)
/// `amount` of **token0**. Price moves down when adding, up when removing.
///
/// Formula: `sqrt_q = L * 2^96 * sqrt_p / (L * 2^96 ± amount * sqrt_p)`,
/// always computed with the **exact denominator** on limbs (Uniswap's
/// uint256-overflow fallback is unnecessary here) and the quotient rounded
/// **up** — Uniswap's direction: the price moves as little as possible, so
/// adding token0 never consumes more than `amount`, and removing token0
/// always frees at least `amount`.
///
/// Panics on `amount * sqrt_p >= L << 96` when removing (insufficient
/// reserves) and on results that do not fit `u128` (the tx fails).
fn get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_p: u128,
    liquidity: u128,
    amount: u128,
    add: bool,
) -> u128 {
    if amount == 0 {
        return sqrt_p;
    }
    let numerator1 = shl96(liquidity);
    let product = wide::mul_u128(amount, sqrt_p);
    let mut numerator = [0u64; 6];
    wide::mul_limbs(&numerator1, &wide::limbs_from_u128(sqrt_p), &mut numerator);

    let mut denominator = [0u64; 5];
    if add {
        denominator[..4].copy_from_slice(&product);
        wide::add_assign(&mut denominator, &numerator1);
    } else {
        assert!(
            wide::cmp_limbs(&numerator1, &product) == core::cmp::Ordering::Greater,
            "sqrt_price_math: amount0 removal exceeds reserves"
        );
        denominator[..4].copy_from_slice(&numerator1);
        wide::sub_assign(&mut denominator, &product);
    }
    div_wide_u128(&numerator, &denominator, true)
}

/// Next sqrt price after adding (`add == true`) or removing (`add == false`)
/// `amount` of **token1**. Price moves up when adding, down when removing.
///
/// Formula: `sqrt_q = sqrt_p ± amount * 2^96 / L`, with the quotient
/// rounded **down** when adding and **up** when removing — Uniswap's
/// direction: the price moves as little as possible, so adding token1
/// never consumes more than `amount`, and removing token1 always frees at
/// least `amount`.
///
/// Panics on price under/overflow (the tx fails).
fn get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_p: u128,
    liquidity: u128,
    amount: u128,
    add: bool,
) -> u128 {
    let [a0, a1] = wide::limbs_from_u128(amount);
    let shifted = [0, a0 << 32, (a0 >> 32) | (a1 << 32), a1 >> 32]; // amount << 96
    let liq = wide::limbs_from_u128(liquidity);
    if add {
        let quotient = div_wide_u128(&shifted, &liq, false);
        sqrt_p
            .checked_add(quotient)
            .expect("sqrt_price_math: price overflow")
    } else {
        let quotient = div_wide_u128(&shifted, &liq, true);
        assert!(sqrt_p > quotient, "sqrt_price_math: price underflow");
        sqrt_p - quotient
    }
}

/// Next sqrt price given an **input** amount of token0 (`zero_for_one`) or
/// token1 (`!zero_for_one`).
///
/// Rounding (Uniswap `getNextSqrtPriceFromInput`): the returned price is
/// always rounded so that the swap consumes **at most** `amount_in` — for
/// `zero_for_one` the exact-denominator formula's quotient is rounded up
/// (price falls as little as possible), for `!zero_for_one` the added
/// quotient is rounded down (price rises as little as possible). Both
/// favor the pool.
///
/// The result may lie outside the supported tick range for extreme inputs
/// and is range-checked by consumers ([`crate::swap_math`] caps moves at a
/// target price, keeping results in range).
///
/// Panics if `sqrt_p` is out of range, `liquidity == 0`, or the result
/// under/overflows `u128`.
pub fn get_next_sqrt_price_from_input(
    sqrt_p: u128,
    liquidity: u128,
    amount_in: u128,
    zero_for_one: bool,
) -> u128 {
    assert_price_in_range(sqrt_p);
    assert!(liquidity > 0, "sqrt_price_math: zero liquidity");
    if zero_for_one {
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_p, liquidity, amount_in, true)
    } else {
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_p, liquidity, amount_in, true)
    }
}

/// Next sqrt price given an **output** amount of token1 (`zero_for_one`)
/// or token0 (`!zero_for_one`).
///
/// Rounding (Uniswap `getNextSqrtPriceFromOutput`): the returned price is
/// always rounded so that the swap delivers **at least** `amount_out` —
/// for `zero_for_one` the subtracted quotient is rounded up (price falls
/// far enough), for `!zero_for_one` the exact-denominator quotient is
/// rounded up (price rises far enough). The caller caps the delivered
/// amount at the request, so the surplus stays with the pool.
///
/// Panics if `sqrt_p` is out of range, `liquidity == 0`, the requested
/// output exceeds the reserves implied by `liquidity`, or the result
/// under/overflows `u128`.
pub fn get_next_sqrt_price_from_output(
    sqrt_p: u128,
    liquidity: u128,
    amount_out: u128,
    zero_for_one: bool,
) -> u128 {
    assert_price_in_range(sqrt_p);
    assert!(liquidity > 0, "sqrt_price_math: zero liquidity");
    if zero_for_one {
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_p, liquidity, amount_out, false)
    } else {
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_p, liquidity, amount_out, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const Q96: u128 = 1 << 96;

    #[test]
    fn amount_deltas_zero_cases() {
        assert_eq!(get_amount0_delta(Q96, Q96 * 2, 0, true), 0);
        assert_eq!(get_amount1_delta(Q96, Q96 * 2, 0, true), 0);
        assert_eq!(get_amount0_delta(Q96, Q96, u64::MAX as u128, true), 0);
        assert_eq!(get_amount1_delta(Q96, Q96, u64::MAX as u128, true), 0);
    }

    #[test]
    fn amount_deltas_are_symmetric_in_prices() {
        let (a, b, l) = (Q96, Q96 * 3 / 2, 1_000_000_000_000u128);
        assert_eq!(
            get_amount0_delta(a, b, l, true),
            get_amount0_delta(b, a, l, true)
        );
        assert_eq!(
            get_amount1_delta(a, b, l, false),
            get_amount1_delta(b, a, l, false)
        );
    }

    #[test]
    fn next_price_zero_amount_is_identity() {
        let l = 10u128.pow(18);
        assert_eq!(get_next_sqrt_price_from_input(Q96, l, 0, true), Q96);
        assert_eq!(get_next_sqrt_price_from_input(Q96, l, 0, false), Q96);
        assert_eq!(get_next_sqrt_price_from_output(Q96, l, 0, true), Q96);
        assert_eq!(get_next_sqrt_price_from_output(Q96, l, 0, false), Q96);
    }

    #[test]
    #[should_panic(expected = "zero liquidity")]
    fn next_price_zero_liquidity_panics() {
        let _ = get_next_sqrt_price_from_input(Q96, 0, 1, true);
    }

    #[test]
    #[should_panic(expected = "out of supported range")]
    fn out_of_range_price_panics() {
        let _ = get_amount0_delta(1, Q96, 1, true);
    }

    #[test]
    #[should_panic(expected = "removal exceeds reserves")]
    fn amount0_removal_beyond_reserves_panics() {
        // Removing more token0 than the position can ever hold.
        let _ = get_next_sqrt_price_from_output(Q96, 1, u64::MAX as u128, false);
    }

    #[test]
    #[should_panic(expected = "price underflow")]
    fn amount1_removal_beyond_reserves_panics() {
        // quotient = ceil(2000 * 2^96 / 1000) = 2 * 2^96 > sqrt_p = 2^96.
        let _ = get_next_sqrt_price_from_output(Q96, 1000, 2000, true);
    }
}
