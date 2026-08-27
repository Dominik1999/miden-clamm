//! Bit-equality tests for `amm::math::swap_math::compute_swap_step` against the Rust
//! oracle (`amm_math::swap_math::compute_swap_step`), exact-in and exact-out paths,
//! including panic parity.

mod common;

use amm_math::swap_math::{compute_swap_step as oracle_step, FEE_PIPS_DENOMINATOR};
use amm_math::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};
use common::*;
use rand::Rng;

fn driver() -> miden_processor::Program {
    program(
        "use amm::math::swap_math\nuse miden::core::sys\n\nbegin\n    repeat.18 adv_push end\n    exec.swap_math::compute_swap_step\n    exec.sys::truncate_stack\nend\n",
    )
}

/// Sign-magnitude encoding of the oracle's i128 amount_remaining.
fn encode_amount(amount_remaining: i128) -> (u128, bool) {
    (amount_remaining.unsigned_abs(), amount_remaining >= 0)
}

fn check(prog: &miden_processor::Program, cur: u128, tgt: u128, l: u128, rem: i128, fee: u32) {
    let (amount_abs, exact_in) = encode_amount(rem);
    // Top-first stack: [cur(4), tgt(4), l(4), amount_abs(4), exact_in, fee].
    let mut s = Vec::new();
    s.extend_from_slice(&u128_to_limbs(cur));
    s.extend_from_slice(&u128_to_limbs(tgt));
    s.extend_from_slice(&u128_to_limbs(l));
    s.extend_from_slice(&u128_to_limbs(amount_abs));
    s.push(exact_in as u64);
    s.push(fee as u64);
    let advice = advice_for_stack(&s);

    let oracle = catch(move || oracle_step(cur, tgt, l, rem, fee));
    let result = execute(library(), prog, &advice);
    match oracle {
        Some((next, amount_in, amount_out, fee_amount)) => {
            let stack = result.unwrap_or_else(|e| {
                panic!("oracle succeeded, MASM must too (cur={cur} tgt={tgt} l={l} rem={rem} fee={fee}): {e:?}")
            });
            let got = (
                limbs_to_u128(&stack[0..4]),
                limbs_to_u128(&stack[4..8]),
                limbs_to_u128(&stack[8..12]),
                limbs_to_u128(&stack[12..16]),
            );
            assert_eq!(
                got,
                (next, amount_in, amount_out, fee_amount),
                "swap step mismatch for cur={cur} tgt={tgt} l={l} rem={rem} fee={fee}"
            );
        }
        None => assert!(
            result.is_err(),
            "oracle panicked, MASM must fail: cur={cur} tgt={tgt} l={l} rem={rem} fee={fee}"
        ),
    }
}

fn random_price(r: &mut impl Rng) -> u128 {
    let span = MAX_SQRT_RATIO - MIN_SQRT_RATIO;
    let bits = r.random_range(0..=64);
    MIN_SQRT_RATIO + ((r.random::<u128>() >> bits) % span)
}

#[test]
fn swap_step_matches_oracle_on_structured_cases() {
    let prog = driver();
    let q96: u128 = 1 << 96;

    let cases: &[(u128, u128, u128, i128, u32)] = &[
        // zero liquidity reaches the target with zero amounts
        (q96, q96 / 2, 0, 1_000_000i128, 3000),
        // tiny exact-in amount: everything becomes fee
        (q96, q96 / 2, 10u128.pow(20), 1, 500),
        // exact-in reaching the target
        (q96, q96 - (q96 >> 20), 10u128.pow(24), i128::MAX, 3000),
        // exact-in not reaching the target, both directions
        (q96, q96 / 2, 10u128.pow(24), 10i128.pow(12), 3000),
        (q96, q96 * 2, 10u128.pow(24), 10i128.pow(12), 3000),
        // exact-out, capped and uncapped, both directions
        (q96, q96 / 2, 10u128.pow(24), -1_000_000_000, 500),
        (q96, q96 * 2, 10u128.pow(24), -1_000_000_000, 500),
        (q96, q96 / 2, 10u128.pow(18), i128::MIN, 10000), // huge request: reaches target
        // zero amount remaining, both modes
        (q96, q96 / 2, 10u128.pow(18), 0, 3000),
        // fee 0 and fee just below the denominator
        (q96, q96 / 2, 10u128.pow(18), 1_000_000i128, 0),
        (q96, q96 / 2, 10u128.pow(18), 1_000_000i128, FEE_PIPS_DENOMINATOR - 1),
        // domain boundaries
        (MIN_SQRT_RATIO, MAX_SQRT_RATIO, 10u128.pow(18), 10i128.pow(15), 3000),
        (MAX_SQRT_RATIO, MIN_SQRT_RATIO, 10u128.pow(18), 10i128.pow(15), 3000),
        (MIN_SQRT_RATIO, MIN_SQRT_RATIO, 1, 1, 1),
        // extreme liquidity
        (q96, q96 * 2, u128::MAX, 10i128.pow(18), 3000),
        (q96, q96 / 2, 1, 10i128.pow(18), 3000),
        // panic parity: fee at the denominator; price out of range
        (q96, q96 / 2, 1, 1, FEE_PIPS_DENOMINATOR),
        (1, q96, 1, 1, 3000),
    ];
    for &(cur, tgt, l, rem, fee) in cases {
        check(&prog, cur, tgt, l, rem, fee);
    }
}

#[test]
fn swap_step_matches_oracle_on_random_inputs() {
    let prog = driver();
    let mut r = rng(0x50A9_0001);

    for _ in 0..512 {
        let cur = random_price(&mut r);
        let tgt = random_price(&mut r);
        let l = match r.random_range(0..6) {
            0 => 0,
            1 => 1,
            2 => u128::MAX,
            _ => random_u128(&mut r, 128),
        };
        let magnitude = match r.random_range(0..6) {
            0 => 0u128,
            1 => 1,
            2 => u64::MAX as u128,
            _ => random_u128(&mut r, 127),
        };
        let rem: i128 = if r.random_bool(0.5) {
            magnitude as i128
        } else {
            -(magnitude as i128)
        };
        let fee: u32 = match r.random_range(0..5) {
            0 => 0,
            1 => 500,
            2 => 3000,
            3 => 10000,
            _ => r.random_range(0..FEE_PIPS_DENOMINATOR),
        };
        check(&prog, cur, tgt, l, rem, fee);
    }
}
