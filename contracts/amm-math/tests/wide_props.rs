//! Property tests for the limb arithmetic in `wide` and `muldiv` against
//! primitive-types / uint big integers.

mod common;

use amm_math::{muldiv, wide};
use common::*;
use primitive_types::U512;
use proptest::collection::vec;
use proptest::prelude::*;

/// Limb values biased toward carry/borrow/normalization boundaries.
fn limb() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => any::<u64>(),
        1 => Just(0u64),
        1 => Just(u64::MAX),
        1 => Just(1u64 << 63),
        1 => Just((1u64 << 63) - 1),
        1 => Just(1u64),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn mul_u128_matches_u512(a in any::<u128>(), b in any::<u128>()) {
        let got = u512_from_limbs(&wide::mul_u128(a, b));
        prop_assert_eq!(got, u512(a) * u512(b));
    }

    #[test]
    fn mul_limbs_matches_u512(a in vec(limb(), 1..=4), b in vec(limb(), 1..=4)) {
        let mut out = [0u64; 8];
        wide::mul_limbs(&a, &b, &mut out);
        prop_assert_eq!(u512_from_limbs(&out), u512_from_limbs(&a) * u512_from_limbs(&b));
    }

    #[test]
    fn div_rem_matches_u512(u in vec(limb(), 1..=8), v in vec(limb(), 1..=4)) {
        prop_assume!(v.iter().any(|&l| l != 0));
        let (q, r) = wide::div_rem(&u, &v);
        let (uu, vv) = (u512_from_limbs(&u), u512_from_limbs(&v));
        prop_assert_eq!(u512_from_limbs(&q), uu / vv);
        prop_assert_eq!(u512_from_limbs(&r), uu % vv);
    }

    /// The workhorse shape: ~384-bit dividends over u128 divisors
    /// (`liquidity << 96` intermediates).
    #[test]
    fn div_rem_384_by_128(u in vec(limb(), 6..=6), v in any::<u128>()) {
        prop_assume!(v != 0);
        let (q, r) = wide::div_rem(&u, &wide::limbs_from_u128(v));
        let (uu, vv) = (u512_from_limbs(&u), u512(v));
        prop_assert_eq!(u512_from_limbs(&q), uu / vv);
        prop_assert_eq!(u512_from_limbs(&r), uu % vv);
    }

    /// Divisor high limbs pinned to normalization boundaries to stress the
    /// qhat estimation/correction paths.
    #[test]
    fn div_rem_qhat_boundaries(
        u in vec(limb(), 3..=8),
        v_lo in limb(),
        v_hi in prop_oneof![
            Just(1u64 << 63), Just((1u64 << 63) - 1), Just(u64::MAX),
            Just(u64::MAX - 1), Just(1u64), any::<u64>(),
        ],
    ) {
        let v = [v_lo, v_hi];
        prop_assume!(v_hi != 0 || v_lo != 0);
        let (q, r) = wide::div_rem(&u, &v);
        let (uu, vv) = (u512_from_limbs(&u), u512_from_limbs(&v));
        prop_assert_eq!(u512_from_limbs(&q), uu / vv);
        prop_assert_eq!(u512_from_limbs(&r), uu % vv);
    }

    #[test]
    fn mul_div_floor_ceil_match_reference(a in any::<u128>(), b in any::<u128>(), d in any::<u128>()) {
        prop_assume!(d != 0);
        let num = u512(a) * u512(b);
        let (q, r) = num.div_mod(u512(d));
        prop_assume!(fits_u128(q) && fits_u128(if r.is_zero() { q } else { q + U512::one() }));
        prop_assert_eq!(u512(muldiv::mul_div_floor(a, b, d)), q);
        let ceil = if r.is_zero() { q } else { q + U512::one() };
        prop_assert_eq!(u512(muldiv::mul_div_ceil(a, b, d)), ceil);
        // Rounding sanity: floor <= ceil <= floor + 1.
        prop_assert!(ceil - q <= U512::one());
    }

    #[test]
    fn limb_helpers_roundtrip(x in any::<u128>()) {
        prop_assert_eq!(wide::limbs_to_u128(&wide::limbs_from_u128(x)), x);
    }
}

/// Deterministic Knuth-D edge cases, including the classic add-back
/// trigger (also asserted branch-level in the crate's unit tests).
#[test]
fn div_rem_targeted_edges() {
    let cases: &[(&[u64], &[u64])] = &[
        // Hacker's Delight add-back case scaled to 64-bit digits.
        (&[3, 0, 1 << 63], &[1, 1 << 63]),
        // qhat initially b (must clamp to b-1).
        (&[0, u64::MAX - 1, u64::MAX], &[u64::MAX, u64::MAX]),
        (&[u64::MAX; 8], &[1, 1 << 63]),
        (&[u64::MAX; 8], &[u64::MAX, u64::MAX, u64::MAX, u64::MAX]),
        (&[0, 0, 0, 0, 0, 1], &[1, 1]),
        (&[1, 0, 0, 0, 0, 0, 0, 1 << 63], &[1, 0, 0, 1 << 63]),
        (&[0, 0, 1], &[1, u64::MAX]),
    ];
    for (u, v) in cases {
        let (q, r) = wide::div_rem(u, v);
        let (uu, vv) = (u512_from_limbs(u), u512_from_limbs(v));
        assert_eq!(u512_from_limbs(&q), uu / vv, "quotient for {u:?} / {v:?}");
        assert_eq!(u512_from_limbs(&r), uu % vv, "remainder for {u:?} / {v:?}");
    }
}
