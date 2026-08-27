//! Bit-equality tests for `amm::math::sqrt_price_math` against the Rust oracle
//! (`amm_math::sqrt_price_math`), including panic parity across the full valid domain
//! and out-of-domain inputs.

mod common;

use amm_math::sqrt_price_math as oracle;
use amm_math::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};
use common::*;
use rand::Rng;

fn amount_delta_driver(proc_name: &str) -> miden_processor::Program {
    program(&format!(
        "use amm::math::sqrt_price_math\nuse miden::core::sys\n\nbegin\n    repeat.13 adv_push end\n    exec.sqrt_price_math::{proc_name}\n    exec.sys::truncate_stack\nend\n"
    ))
}

fn next_price_driver(proc_name: &str) -> miden_processor::Program {
    program(&format!(
        "use amm::math::sqrt_price_math\nuse miden::core::sys\n\nbegin\n    repeat.13 adv_push end\n    exec.sqrt_price_math::{proc_name}\n    exec.sys::truncate_stack\nend\n"
    ))
}

/// Top-first stack: [first(4), second(4), third(4), flag].
fn inputs(first: u128, second: u128, third: u128, flag: bool) -> Vec<u64> {
    let mut s = Vec::new();
    s.extend_from_slice(&u128_to_limbs(first));
    s.extend_from_slice(&u128_to_limbs(second));
    s.extend_from_slice(&u128_to_limbs(third));
    s.push(flag as u64);
    advice_for_stack(&s)
}

fn check_u128_result(
    prog: &miden_processor::Program,
    advice: &[u64],
    oracle_result: Option<u128>,
    context: &str,
) {
    let result = execute(library(), prog, advice);
    match oracle_result {
        Some(expected) => {
            let stack = result.unwrap_or_else(|e| {
                panic!("oracle succeeded, MASM must too ({context}): {e:?}")
            });
            assert_eq!(limbs_to_u128(&stack[..4]), expected, "mismatch ({context})");
        }
        None => assert!(result.is_err(), "oracle panicked, MASM must fail ({context})"),
    }
}

/// Random sqrt price across the whole supported domain, log-biased so both ends are hit.
fn random_price(r: &mut impl Rng) -> u128 {
    let span = MAX_SQRT_RATIO - MIN_SQRT_RATIO;
    let bits = r.random_range(0..=64);
    MIN_SQRT_RATIO + ((r.random::<u128>() >> bits) % span)
}

fn random_liquidity(r: &mut impl Rng) -> u128 {
    match r.random_range(0..8) {
        0 => 0,
        1 => 1,
        2 => u128::MAX,
        3 => u64::MAX as u128,
        _ => random_u128(r, 128),
    }
}

fn random_amount(r: &mut impl Rng) -> u128 {
    match r.random_range(0..8) {
        0 => 0,
        1 => 1,
        2 => u64::MAX as u128,
        3 => u128::MAX,
        _ => random_u128(r, 128),
    }
}

#[test]
fn amount_deltas_match_oracle() {
    let a0d = amount_delta_driver("get_amount0_delta");
    let a1d = amount_delta_driver("get_amount1_delta");
    let mut r = rng(0x59A7_0001);

    let mut structured: Vec<(u128, u128, u128, bool)> = vec![
        (1 << 96, 1 << 97, 0, true),
        (1 << 96, 1 << 96, u64::MAX as u128, true),
        (MIN_SQRT_RATIO, MAX_SQRT_RATIO, 1, true),
        (MIN_SQRT_RATIO, MAX_SQRT_RATIO, u128::MAX, false),
        (MIN_SQRT_RATIO, MIN_SQRT_RATIO + 1, u128::MAX, true),
        (MAX_SQRT_RATIO - 1, MAX_SQRT_RATIO, u128::MAX, false),
        // out-of-range prices: panic parity
        (1, 1 << 96, 1, true),
        (1 << 96, u128::MAX, 1, true),
        (0, 0, 0, false),
    ];
    for _ in 0..512 {
        structured.push((
            random_price(&mut r),
            random_price(&mut r),
            random_liquidity(&mut r),
            r.random_bool(0.5),
        ));
    }

    for (sa, sb, l, rup) in structured {
        let advice = inputs(sa, sb, l, rup);
        check_u128_result(
            &a0d,
            &advice,
            catch(move || oracle::get_amount0_delta(sa, sb, l, rup)),
            &format!("amount0 sa={sa} sb={sb} l={l} rup={rup}"),
        );
        check_u128_result(
            &a1d,
            &advice,
            catch(move || oracle::get_amount1_delta(sa, sb, l, rup)),
            &format!("amount1 sa={sa} sb={sb} l={l} rup={rup}"),
        );
    }
}

#[test]
fn next_prices_match_oracle() {
    let from_in = next_price_driver("get_next_sqrt_price_from_input");
    let from_out = next_price_driver("get_next_sqrt_price_from_output");
    let mut r = rng(0x59A7_0002);

    let mut cases: Vec<(u128, u128, u128, bool)> = vec![
        (1 << 96, 10u128.pow(18), 0, true),
        (1 << 96, 10u128.pow(18), 0, false),
        (1 << 96, 0, 1, true),  // zero liquidity: panic parity
        (1 << 96, 1, u64::MAX as u128, false), // output beyond reserves
        (1 << 96, 1000, 2000, true),           // price underflow (output path)
        (MIN_SQRT_RATIO, 1, 1, true),
        (MAX_SQRT_RATIO, u128::MAX, u128::MAX, false),
        (MAX_SQRT_RATIO, 1, u128::MAX, false),
        (1, 1, 1, true), // out-of-range price: panic parity
    ];
    for _ in 0..512 {
        cases.push((
            random_price(&mut r),
            random_liquidity(&mut r),
            random_amount(&mut r),
            r.random_bool(0.5),
        ));
    }

    for (p, l, amt, zfo) in cases {
        let advice = inputs(p, l, amt, zfo);
        check_u128_result(
            &from_in,
            &advice,
            catch(move || oracle::get_next_sqrt_price_from_input(p, l, amt, zfo)),
            &format!("from_input p={p} l={l} amt={amt} zfo={zfo}"),
        );
        check_u128_result(
            &from_out,
            &advice,
            catch(move || oracle::get_next_sqrt_price_from_output(p, l, amt, zfo)),
            &format!("from_output p={p} l={l} amt={amt} zfo={zfo}"),
        );
    }
}
