//! Tick <-> sqrt-price conversion (Uniswap v3 `TickMath`, Q64.96 in `u128`).
//!
//! Supported tick range is ±443,636 (half of Uniswap's ±887,272), chosen so
//! every sqrt ratio fits Q64.96 in `u128`: ratios span (2^64, 2^128).

use crate::wide;

/// Lowest supported tick.
pub const MIN_TICK: i32 = -443_636;
/// Highest supported tick.
pub const MAX_TICK: i32 = 443_636;

/// `get_sqrt_ratio_at_tick(MIN_TICK)` — the smallest representable sqrt
/// price (Q64.96). Verified against the function in tests.
pub const MIN_SQRT_RATIO: u128 = 18_447_090_764_788_882_728; // 0x100013b504ea15d28
/// `get_sqrt_ratio_at_tick(MAX_TICK)` — the largest representable sqrt
/// price (Q64.96). Verified against the function in tests.
pub const MAX_SQRT_RATIO: u128 = 340_275_971_719_517_849_884_101_479_065_584_693_834; // 0xfffec4b135bb7f32a81b33b5fb40724a

/// Per-bit multiplicative constants: `C[i] = round(2^128 / sqrt(1.0001)^(2^i))`
/// as Q0.128 fractions. 19 constants cover `|tick| <= 443,636 < 2^19`.
///
/// Do not edit by hand: the table is derived (not transcribed) — the
/// `tick_constants_match_u1024_derivation` test recomputes each value
/// exactly with U1024 fixed-point arithmetic and asserts equality,
/// printing the correct table on mismatch.
pub const TICK_BIT_CONSTANTS: [u128; 19] = [
    0xfffcb933bd6fad37aa2d162d1a594001, // bit 0: round(2^128 / sqrt(1.0001)^(2^0))
    0xfff97272373d413259a46990580e213a, // bit 1
    0xfff2e50f5f656932ef12357cf3c7fdcc, // bit 2
    0xffe5caca7e10e4e61c3624eaa0941cd0, // bit 3
    0xffcb9843d60f6159c9db58835c926644, // bit 4
    0xff973b41fa98c081472e6896dfb254c0, // bit 5
    0xff2ea16466c96a3843ec78b326b52861, // bit 6
    0xfe5dee046a99a2a811c461f1969c3053, // bit 7
    0xfcbe86c7900a88aedcffc83b479aa3a4, // bit 8
    0xf987a7253ac413176f2b074cf7815e54, // bit 9
    0xf3392b0822b70005940c7a398e4b70f3, // bit 10
    0xe7159475a2c29b7443b29c7fa6e889d9, // bit 11
    0xd097f3bdfd2022b8845ad8f792aa5825, // bit 12
    0xa9f746462d870fdf8a65dc1f90e061e5, // bit 13
    0x70d869a156d2a1b890bb3df62baf32f7, // bit 14
    0x31be135f97d08fd981231505542fcfa6, // bit 15
    0x09aa508b5b7a84e1c677de54f3e99bc9, // bit 16
    0x005d6af8dedb81196699c329225ee604, // bit 17
    0x00002216e584f5fa1ea926041bedfe98, // bit 18
];

/// Returns `sqrt(1.0001^tick) * 2^96` as Q64.96 in `u128`.
///
/// Algorithm (Uniswap `TickMath.getSqrtRatioAtTick`): start from
/// `ratio = 2^128` (Q128.128 "1.0"); for each set bit `i` of `|tick|`
/// multiply by [`TICK_BIT_CONSTANTS`]`[i]` taking the high 256 bits of the
/// 384-bit product (truncating, as Uniswap does); if `tick > 0` invert via
/// `floor((2^256 - 1) / ratio)`; finally shift Q128.128 -> Q64.96 by 32
/// bits, rounding **up** if any shifted-out bit is set (Uniswap semantics;
/// also the pool-favoring direction for the price grid).
///
/// Rounding: the result is within ~2^-90 relative error of the exact
/// value, always representable; the final shift rounds up.
///
/// Panics if `tick` is outside `[MIN_TICK, MAX_TICK]`.
pub fn get_sqrt_ratio_at_tick(tick: i32) -> u128 {
    assert!(
        (MIN_TICK..=MAX_TICK).contains(&tick),
        "tick_math: tick out of range"
    );
    let abs_tick = tick.unsigned_abs();

    // ratio = 2^128 in Q128.128 (little-endian limbs).
    let mut ratio: [u64; 4] = [0, 0, 1, 0];
    for (i, &c) in TICK_BIT_CONSTANTS.iter().enumerate() {
        if (abs_tick >> i) & 1 == 1 {
            let mut prod = [0u64; 6];
            wide::mul_limbs(&ratio, &wide::limbs_from_u128(c), &mut prod);
            // >> 128: keep the high four limbs (truncation).
            ratio = [prod[2], prod[3], prod[4], prod[5]];
        }
    }

    if tick > 0 {
        // ratio = floor((2^256 - 1) / ratio).
        let (q, _r) = wide::div_rem(&[u64::MAX; 4], &ratio);
        ratio = [q[0], q[1], q[2], q[3]];
    }

    // Q128.128 -> Q64.96: shift right by 32, rounding up.
    assert!(
        ratio[3] == 0 && ratio[2] >> 32 == 0,
        "tick_math: sqrt ratio overflow"
    );
    let round_up = ratio[0] & 0xFFFF_FFFF != 0;
    let mut x = (ratio[0] >> 32) as u128 | (ratio[1] as u128) << 32 | (ratio[2] as u128) << 96;
    if round_up {
        x += 1;
    }
    x
}

/// Returns the unique tick `t` such that
/// `get_sqrt_ratio_at_tick(t) <= sqrt_ratio_x96 < get_sqrt_ratio_at_tick(t + 1)`.
///
/// Implemented as a binary search over [`get_sqrt_ratio_at_tick`]
/// (correctness first; a log2-based fast path can come later in MASM).
/// The result is exact with respect to this crate's tick->ratio mapping,
/// so no rounding direction applies.
///
/// Panics if `sqrt_ratio_x96` is outside `[MIN_SQRT_RATIO, MAX_SQRT_RATIO)`
/// (Uniswap's half-open input domain).
pub fn get_tick_at_sqrt_ratio(sqrt_ratio_x96: u128) -> i32 {
    assert!(
        (MIN_SQRT_RATIO..MAX_SQRT_RATIO).contains(&sqrt_ratio_x96),
        "tick_math: sqrt ratio out of range"
    );
    let mut lo = MIN_TICK;
    let mut hi = MAX_TICK;
    // Invariant: ratio(lo) <= x < ratio(hi).
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if get_sqrt_ratio_at_tick(mid) <= sqrt_ratio_x96 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_zero_is_exactly_q96_one() {
        assert_eq!(get_sqrt_ratio_at_tick(0), 1u128 << 96);
    }

    #[test]
    fn boundary_constants_match_function() {
        assert_eq!(get_sqrt_ratio_at_tick(MIN_TICK), MIN_SQRT_RATIO);
        assert_eq!(get_sqrt_ratio_at_tick(MAX_TICK), MAX_SQRT_RATIO);
        // Format invariants: ratios span (2^64, 2^128).
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(MIN_SQRT_RATIO > 1 << 64);
            assert!(MAX_SQRT_RATIO < u128::MAX);
        }
    }

    #[test]
    #[should_panic(expected = "tick out of range")]
    fn tick_below_min_panics() {
        let _ = get_sqrt_ratio_at_tick(MIN_TICK - 1);
    }

    #[test]
    #[should_panic(expected = "tick out of range")]
    fn tick_above_max_panics() {
        let _ = get_sqrt_ratio_at_tick(MAX_TICK + 1);
    }

    #[test]
    #[should_panic(expected = "sqrt ratio out of range")]
    fn ratio_below_min_panics() {
        let _ = get_tick_at_sqrt_ratio(MIN_SQRT_RATIO - 1);
    }

    #[test]
    #[should_panic(expected = "sqrt ratio out of range")]
    fn ratio_at_max_panics() {
        let _ = get_tick_at_sqrt_ratio(MAX_SQRT_RATIO);
    }
}
