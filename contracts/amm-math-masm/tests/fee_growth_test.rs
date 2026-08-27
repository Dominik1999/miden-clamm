//! Bit-equality tests for `amm::math::fee_growth` against the fee-growth helpers of
//! the clamm-pool Rust oracle (contracts/clamm-pool/src/lib.rs).
//!
//! The oracle helpers are reproduced here verbatim on top of `amm_math::wide` (the
//! clamm-pool crate is a cargo-miden guest crate and cannot be linked host-side); they
//! are byte-for-byte the same arithmetic as `fee_growth_increment` / `fees_owed` /
//! `u256_add` / `u256_sub` in the pool contract.

mod common;

use common::*;
use rand::Rng;

/// A u256 as 4 little-endian u64 limbs (the clamm-pool representation).
type U256 = [u64; 4];

// ---- clamm-pool oracle helpers (mirrored) ----

fn oracle_fee_growth_increment(fee: u128, liquidity: u128) -> U256 {
    assert!(liquidity > 0, "clamm: fee growth with zero liquidity");
    let f = amm_math::wide::limbs_from_u128(fee);
    let dividend = [0u64, 0, f[0], f[1]];
    let (q, _r) = amm_math::wide::div_rem(&dividend, &amm_math::wide::limbs_from_u128(liquidity));
    [q[0], q[1], q[2], q[3]]
}

fn oracle_fees_owed(delta: U256, liquidity: u128) -> u128 {
    let mut prod = [0u64; 6];
    amm_math::wide::mul_limbs(&delta, &amm_math::wide::limbs_from_u128(liquidity), &mut prod);
    (prod[2] as u128) | ((prod[3] as u128) << 64)
}

fn oracle_u256_add(a: U256, b: U256) -> U256 {
    let mut out = [0u64; 4];
    let mut carry = 0u64;
    for i in 0..4 {
        let (s1, c1) = a[i].overflowing_add(b[i]);
        let (s2, c2) = s1.overflowing_add(carry);
        out[i] = s2;
        carry = (c1 as u64) + (c2 as u64);
    }
    out
}

fn oracle_u256_sub(a: U256, b: U256) -> U256 {
    let mut out = [0u64; 4];
    let mut borrow = 0u64;
    for i in 0..4 {
        let (d1, b1) = a[i].overflowing_sub(b[i]);
        let (d2, b2) = d1.overflowing_sub(borrow);
        out[i] = d2;
        borrow = (b1 as u64) + (b2 as u64);
    }
    out
}

// ---- drivers ----

fn u256_to_u32_limbs(x: U256) -> Vec<u64> {
    u64_to_u32_limbs(&x)
}

fn random_u256(r: &mut impl Rng) -> U256 {
    let sig = r.random_range(0..=4);
    let mut out = [0u64; 4];
    for slot in out.iter_mut().take(sig) {
        *slot = r.random();
    }
    out
}

#[test]
fn fee_growth_increment_matches_oracle() {
    let prog = program(
        "use amm::math::fee_growth\nuse miden::core::sys\n\nbegin\n    repeat.8 adv_push end\n    exec.fee_growth::fee_shl128_div_liquidity\n    exec.sys::truncate_stack\nend\n",
    );
    let mut r = rng(0xFEE0_0001);

    let mut cases: Vec<(u128, u128)> = vec![
        (0, 1),
        (1, 1),
        (1, u128::MAX),
        (u64::MAX as u128, 1),
        (u64::MAX as u128, u128::MAX),
        (u128::MAX, 1),
        (u128::MAX, u128::MAX),
        (1 << 100, 1 << 20),
    ];
    for _ in 0..512 {
        cases.push((random_u128(&mut r, 128), random_u128(&mut r, 128).max(1)));
    }

    for (fee, l) in cases {
        // Top-first stack: [fee(4), l(4)].
        let mut s = Vec::new();
        s.extend_from_slice(&u128_to_limbs(fee));
        s.extend_from_slice(&u128_to_limbs(l));
        let stack = execute(library(), &prog, &advice_for_stack(&s))
            .expect("increment driver must execute");
        let expected = u256_to_u32_limbs(oracle_fee_growth_increment(fee, l));
        assert_eq!(&stack[..8], &expected[..], "increment mismatch fee={fee} l={l}");
    }

    // Zero liquidity: panic parity.
    let mut s = Vec::new();
    s.extend_from_slice(&u128_to_limbs(1));
    s.extend_from_slice(&u128_to_limbs(0));
    assert!(execute(library(), &prog, &advice_for_stack(&s)).is_err());
}

#[test]
fn fees_owed_matches_oracle() {
    let prog = program(
        "use amm::math::fee_growth\nuse miden::core::sys\n\nbegin\n    repeat.12 adv_push end\n    exec.fee_growth::liquidity_mul_delta_shr128\n    exec.sys::truncate_stack\nend\n",
    );
    let mut r = rng(0xFEE0_0002);

    let mut cases: Vec<(U256, u128)> = vec![
        ([0, 0, 0, 0], 0),
        ([u64::MAX; 4], u128::MAX),        // truncating cast: high bits dropped
        ([0, 0, 1, 0], 1),                 // exactly 1 << 128 -> owed 1
        ([u64::MAX, u64::MAX, 0, 0], u128::MAX), // fractional-only delta
        ([0, 0, u64::MAX, u64::MAX], 1),
    ];
    for _ in 0..512 {
        cases.push((random_u256(&mut r), random_u128(&mut r, 128)));
    }

    for (delta, l) in cases {
        // Top-first stack: [delta(8 u32 limbs), l(4)].
        let mut s = u256_to_u32_limbs(delta);
        s.extend_from_slice(&u128_to_limbs(l));
        let stack = execute(library(), &prog, &advice_for_stack(&s))
            .expect("fees_owed driver must execute");
        let expected = oracle_fees_owed(delta, l);
        assert_eq!(
            limbs_to_u128(&stack[..4]),
            expected,
            "fees_owed mismatch delta={delta:?} l={l}"
        );
    }
}

#[test]
fn u256_wrapping_ops_match_oracle() {
    let add_prog = program(
        "use amm::math::fee_growth\nuse miden::core::sys\n\nbegin\n    repeat.16 adv_push end\n    exec.fee_growth::u256_wrapping_add\n    exec.sys::truncate_stack\nend\n",
    );
    let sub_prog = program(
        "use amm::math::fee_growth\nuse miden::core::sys\n\nbegin\n    repeat.16 adv_push end\n    exec.fee_growth::u256_wrapping_sub\n    exec.sys::truncate_stack\nend\n",
    );
    let mut r = rng(0xFEE0_0003);

    let mut cases: Vec<(U256, U256)> = vec![
        ([0; 4], [0; 4]),
        ([u64::MAX; 4], [u64::MAX; 4]),
        ([0; 4], [1, 0, 0, 0]), // sub wraps to 2^256 - 1
        ([1, 0, 0, 0], [u64::MAX; 4]),
    ];
    for _ in 0..256 {
        cases.push((random_u256(&mut r), random_u256(&mut r)));
    }

    for (a, b) in cases {
        // Core-lib stack contract: [b(8 limbs), a(8 limbs)] -> a op b.
        let mut s = u256_to_u32_limbs(b);
        s.extend(u256_to_u32_limbs(a));
        let advice = advice_for_stack(&s);

        let stack = execute(library(), &add_prog, &advice).expect("add driver must execute");
        assert_eq!(
            &stack[..8],
            &u256_to_u32_limbs(oracle_u256_add(a, b))[..],
            "u256 add mismatch a={a:?} b={b:?}"
        );

        let stack = execute(library(), &sub_prog, &advice).expect("sub driver must execute");
        assert_eq!(
            &stack[..8],
            &u256_to_u32_limbs(oracle_u256_sub(a, b))[..],
            "u256 sub mismatch a={a:?} b={b:?}"
        );
    }
}
