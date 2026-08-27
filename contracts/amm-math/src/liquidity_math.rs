//! Port of Uniswap v3 `LiquidityMath`.

/// Applies a signed liquidity delta to a liquidity amount.
///
/// No rounding is involved (integer add/sub); the function panics on
/// underflow (`delta` more negative than `l`) or overflow past
/// `u128::MAX`, mirroring Uniswap's checked `addDelta`. A panic is the
/// desired Miden behavior: the tx fails.
pub fn add_delta(l: u128, delta: i128) -> u128 {
    if delta >= 0 {
        l.checked_add(delta as u128)
            .expect("liquidity_math: liquidity overflow")
    } else {
        l.checked_sub(delta.unsigned_abs())
            .expect("liquidity_math: liquidity underflow")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic() {
        assert_eq!(add_delta(1, 0), 1);
        assert_eq!(add_delta(1, -1), 0);
        assert_eq!(add_delta(1, 1), 2);
        assert_eq!(add_delta(0, i128::MAX), i128::MAX as u128);
        assert_eq!(add_delta(u128::MAX - 1, 1), u128::MAX);
        assert_eq!(add_delta(u128::MAX, i128::MIN), u128::MAX - (1u128 << 127));
    }

    #[test]
    #[should_panic(expected = "liquidity overflow")]
    fn overflow_panics() {
        let _ = add_delta(u128::MAX, 1);
    }

    #[test]
    #[should_panic(expected = "liquidity underflow")]
    fn underflow_panics() {
        let _ = add_delta(0, -1);
    }
}
