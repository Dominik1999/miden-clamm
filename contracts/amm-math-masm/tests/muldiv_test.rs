//! Bit-equality tests for `amm::math::muldiv` against the Rust oracle
//! (`amm_math::muldiv`), including panic parity (division by zero, quotient overflow).

mod common;

use common::*;

fn driver(proc_name: &str) -> miden_processor::Program {
    program(&format!(
        "use amm::math::muldiv\nuse miden::core::sys\n\nbegin\n    repeat.12 adv_push end\n    exec.muldiv::{proc_name}\n    exec.sys::truncate_stack\nend\n"
    ))
}

fn stack_inputs(a: u128, b: u128, d: u128) -> Vec<u64> {
    // Top-first: [a limbs, b limbs, d limbs].
    let mut s = Vec::new();
    s.extend_from_slice(&u128_to_limbs(a));
    s.extend_from_slice(&u128_to_limbs(b));
    s.extend_from_slice(&u128_to_limbs(d));
    advice_for_stack(&s)
}

fn check(
    floor_prog: &miden_processor::Program,
    ceil_prog: &miden_processor::Program,
    a: u128,
    b: u128,
    d: u128,
) {
    let advice = stack_inputs(a, b, d);
    for (prog, oracle) in [
        (floor_prog, catch(move || amm_math::muldiv::mul_div_floor(a, b, d))),
        (ceil_prog, catch(move || amm_math::muldiv::mul_div_ceil(a, b, d))),
    ] {
        let result = execute(library(), prog, &advice);
        match oracle {
            Some(expected) => {
                let stack = result.expect("oracle succeeded, MASM must too");
                assert_eq!(
                    limbs_to_u128(&stack[..4]),
                    expected,
                    "mul_div mismatch for a={a} b={b} d={d}"
                );
            }
            None => {
                assert!(result.is_err(), "oracle panicked, MASM must fail: a={a} b={b} d={d}");
            }
        }
    }
}

#[test]
fn mul_div_matches_oracle_on_random_inputs() {
    let floor_prog = driver("mul_div_floor");
    let ceil_prog = driver("mul_div_ceil");
    let mut r = rng(0x30D1_0001);

    for _ in 0..512 {
        let a = random_u128(&mut r, 128);
        let b = random_u128(&mut r, 128);
        let d = random_u128(&mut r, 128).max(1);
        check(&floor_prog, &ceil_prog, a, b, d);
    }
}

#[test]
fn mul_div_matches_oracle_on_edge_cases() {
    let floor_prog = driver("mul_div_floor");
    let ceil_prog = driver("mul_div_ceil");
    let m = u128::MAX;

    let cases: &[(u128, u128, u128)] = &[
        (7, 3, 2),
        (m, m, m),
        (0, m, 5),
        (1 << 100, 1 << 20, 1 << 60),
        (m, 1, 1),
        (m, m, 1),      // overflow: both must fail
        (m, 3, 2),      // floor fits exactly; ceil overflows
        (m - 1, m, m),  // floor = m - 1, ceil = m
        (1, 1, m),
        (m, m - 1, m),
        (1 << 127, 2, 1),   // overflow boundary
        (1 << 127, 2, 2),   // exactly 2^127
        ((1 << 96) + 12345, (1 << 96) - 1, 997),
    ];
    for &(a, b, d) in cases {
        check(&floor_prog, &ceil_prog, a, b, d);
    }

    // Division by zero: both must fail (oracle panics).
    check(&floor_prog, &ceil_prog, 1, 1, 0);
    check(&floor_prog, &ceil_prog, 0, 0, 0);
}
