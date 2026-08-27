//! `a * b / d` with exact 256-bit (and wider, crate-internal) intermediates.
//!
//! Equivalent of Uniswap v3's `FullMath.mulDiv` / `mulDivRoundingUp`,
//! built on the limb helpers in [`crate::wide`] instead of uint256.

use crate::wide;

/// `floor(a * b / d)` with an exact 256-bit intermediate product.
///
/// Rounding: **down** (toward zero). Callers that owe value to the pool
/// must use [`mul_div_ceil`] instead; this floor variant is the
/// pool-favoring choice when computing what the pool pays out.
///
/// Panics if `d == 0` or the quotient does not fit in `u128`
/// (desired Miden behavior: the tx fails).
pub fn mul_div_floor(a: u128, b: u128, d: u128) -> u128 {
    assert!(d != 0, "mul_div: division by zero");
    let prod = wide::mul_u128(a, b);
    let (q, _r) = wide::div_rem(&prod, &wide::limbs_from_u128(d));
    assert!(wide::sig_limbs(&q) <= 2, "mul_div: quotient overflow");
    wide::limbs_to_u128(&q)
}

/// `ceil(a * b / d)` with an exact 256-bit intermediate product.
///
/// Rounding: **up**. This is the pool-favoring choice when computing what
/// a user owes the pool (mirrors `FullMath.mulDivRoundingUp`).
///
/// Panics if `d == 0` or the quotient does not fit in `u128`.
pub fn mul_div_ceil(a: u128, b: u128, d: u128) -> u128 {
    assert!(d != 0, "mul_div: division by zero");
    let prod = wide::mul_u128(a, b);
    let (mut q, r) = wide::div_rem(&prod, &wide::limbs_from_u128(d));
    if wide::sig_limbs(&r) != 0 {
        wide::add_one(&mut q);
    }
    assert!(wide::sig_limbs(&q) <= 2, "mul_div: quotient overflow");
    wide::limbs_to_u128(&q)
}

/// Wide-dividend division used by `sqrt_price_math`, where dividends are
/// up to 384-bit products (e.g. `(liquidity << 96) * delta`) and divisors
/// may exceed `u128` (exact-denominator next-price formulas).
///
/// Returns the quotient as limbs, rounded up when `round_up` and the
/// remainder is non-zero, down otherwise. Panics if `d == 0`.
pub(crate) fn div_wide(n: &[u64], d: &[u64], round_up: bool) -> [u64; wide::MAX_LIMBS] {
    let (mut q, r) = wide::div_rem(n, d);
    if round_up && wide::sig_limbs(&r) != 0 {
        wide::add_one(&mut q);
    }
    q
}

/// [`div_wide`] narrowed to `u128`. Panics additionally if the quotient
/// does not fit in `u128` (price/amount overflow — the tx fails).
pub(crate) fn div_wide_u128(n: &[u64], d: &[u64], round_up: bool) -> u128 {
    let q = div_wide(n, d, round_up);
    assert!(wide::sig_limbs(&q) <= 2, "mul_div: quotient overflow");
    wide::limbs_to_u128(&q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic() {
        assert_eq!(mul_div_floor(7, 3, 2), 10);
        assert_eq!(mul_div_ceil(7, 3, 2), 11);
        assert_eq!(mul_div_floor(u128::MAX, u128::MAX, u128::MAX), u128::MAX);
        assert_eq!(mul_div_ceil(u128::MAX, u128::MAX, u128::MAX), u128::MAX);
        assert_eq!(mul_div_floor(0, u128::MAX, 5), 0);
        assert_eq!(mul_div_ceil(0, u128::MAX, 5), 0);
        // Exact division: ceil == floor.
        assert_eq!(mul_div_ceil(1 << 100, 1 << 20, 1 << 60), 1 << 60);
    }

    #[test]
    #[should_panic(expected = "division by zero")]
    fn zero_divisor_panics() {
        let _ = mul_div_floor(1, 1, 0);
    }

    #[test]
    #[should_panic(expected = "quotient overflow")]
    fn overflow_panics() {
        let _ = mul_div_floor(u128::MAX, u128::MAX, 1);
    }

    #[test]
    #[should_panic(expected = "quotient overflow")]
    fn ceil_overflow_at_boundary_panics() {
        // floor fits exactly in u128::MAX but ceil would need one more.
        let _ = mul_div_ceil(u128::MAX, 3, 2);
    }
}
