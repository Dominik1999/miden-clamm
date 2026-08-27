//! Bit-equality tests for `amm::math::wide` against the Rust oracle (`amm_math::wide`).
//!
//! The division driver exercises the full internal surface: short division, Knuth D
//! with qhat correction and the add-back branch, u < v, and zero dividends.

mod common;

use common::*;

const U_LIMBS: usize = 12; // 384-bit dividend capacity (spill limb at index 12)
const V_LIMBS: usize = 9; // 288-bit divisor capacity

const U_PTR: u32 = 0x1000;
const V_PTR: u32 = 0x1020;
const Q_PTR: u32 = 0x1040;

/// Driver: loads a 12-limb dividend and a 9-limb divisor from the advice stack,
/// runs div_rem_ptr, and returns [q0..q11, rem_nonzero] (q0 on top).
fn div_driver_source() -> String {
    let mut src = String::from("use amm::math::wide\nuse miden::core::sys\n\nbegin\n");
    src.push_str(&format!("    push.{U_PTR}\n"));
    src.push_str(&format!(
        "    repeat.{}\n        adv_push dup.1 mem_store add.1\n    end\n    drop\n",
        U_LIMBS
    ));
    src.push_str(&format!("    push.{V_PTR}\n"));
    src.push_str(&format!(
        "    repeat.{}\n        adv_push dup.1 mem_store add.1\n    end\n    drop\n",
        V_LIMBS
    ));
    src.push_str(&format!(
        "    push.{V_LIMBS} push.{V_PTR} push.{U_LIMBS} push.{U_PTR} push.{Q_PTR}\n"
    ));
    src.push_str("    exec.wide::div_rem_ptr\n");
    for i in (0..U_LIMBS).rev() {
        src.push_str(&format!("    push.{} mem_load\n", Q_PTR + i as u32));
    }
    src.push_str("    exec.sys::truncate_stack\nend\n");
    src
}

/// Runs the MASM division and returns (q_limbs_u32[12], rem_nonzero).
fn masm_div(program: &miden_processor::Program, u: &[u64; U_LIMBS], v: &[u64; V_LIMBS]) -> (Vec<u64>, bool) {
    let mut advice: Vec<u64> = Vec::new();
    advice.extend_from_slice(u);
    advice.extend_from_slice(v);
    let stack = execute(library(), program, &advice).expect("division driver must execute");
    // stack (top first): [q0..q11, rem_nonzero, ...]
    let q = stack[..U_LIMBS].to_vec();
    let rem_nonzero = stack[U_LIMBS] == 1;
    (q, rem_nonzero)
}

/// Oracle: Rust wide::div_rem over u64 limbs.
fn oracle_div(u: &[u64; U_LIMBS], v: &[u64; V_LIMBS]) -> (Vec<u64>, bool) {
    let u64_u = u32_to_u64_limbs(u); // 6 u64 limbs
    let mut v_padded = v.to_vec();
    v_padded.push(0); // 10 u32 limbs -> 5 u64 limbs
    let u64_v = u32_to_u64_limbs(&v_padded);
    let (q, r) = amm_math::wide::div_rem(&u64_u, &u64_v);
    let q_u32 = u64_to_u32_limbs(&q[..6]);
    let rem_nonzero = amm_math::wide::sig_limbs(&r) != 0;
    (q_u32, rem_nonzero)
}

fn check_case(program: &miden_processor::Program, u: &[u64; U_LIMBS], v: &[u64; V_LIMBS]) {
    let (masm_q, masm_rem) = masm_div(program, u, v);
    let (oracle_q, oracle_rem) = oracle_div(u, v);
    assert_eq!(
        masm_q, oracle_q,
        "quotient mismatch for u={u:?} v={v:?}"
    );
    assert_eq!(
        masm_rem, oracle_rem,
        "remainder flag mismatch for u={u:?} v={v:?}"
    );
}

fn random_limbs<const N: usize>(r: &mut impl rand::Rng, max_sig: usize) -> [u64; N] {
    let sig = r.random_range(0..=max_sig.min(N));
    let mut out = [0u64; N];
    for slot in out.iter_mut().take(sig) {
        *slot = r.random_range(0..=0xFFFF_FFFFu64);
    }
    // Bias: make the top selected limb non-zero half the time to hit exact widths.
    if sig > 0 && r.random_bool(0.5) {
        out[sig - 1] = r.random_range(1..=0xFFFF_FFFFu64);
    }
    out
}

#[test]
fn division_matches_oracle_on_random_inputs() {
    let program = program(&div_driver_source());
    let mut r = rng(0xD1D1_0001);

    let mut cases = 0usize;
    while cases < 512 {
        let u: [u64; U_LIMBS] = random_limbs(&mut r, U_LIMBS);
        let v: [u64; V_LIMBS] = random_limbs(&mut r, V_LIMBS);
        if v.iter().all(|&l| l == 0) {
            continue; // division by zero panics; covered separately
        }
        check_case(&program, &u, &v);
        cases += 1;
    }
}

#[test]
fn division_matches_oracle_on_adversarial_cases() {
    let program = program(&div_driver_source());

    let max = 0xFFFF_FFFFu64;
    let mut cases: Vec<(Vec<u64>, Vec<u64>)> = Vec::new();

    // Rust oracle's own adversarial suite (u64 limbs, converted to u32 limbs).
    // add-back trigger: u = 2^255, v = 2^191 + 1.
    cases.push((
        u64_to_u32_limbs(&[0, 0, 0, 1u64 << 63, 0, 0]),
        u64_to_u32_limbs(&[1u64, 0, 1u64 << 63]).into_iter().chain([0, 0, 0]).collect(),
    ));
    // qhat correction boundary shapes.
    cases.push((
        u64_to_u32_limbs(&[u64::MAX, u64::MAX, u64::MAX, u64::MAX, 0, 0]),
        u64_to_u32_limbs(&[1, 1u64 << 63]).into_iter().chain([0; 5]).collect(),
    ));
    cases.push((
        u64_to_u32_limbs(&[u64::MAX, u64::MAX, u64::MAX, u64::MAX, 0, 0]),
        u64_to_u32_limbs(&[u64::MAX, u64::MAX]).into_iter().chain([0; 5]).collect(),
    ));
    cases.push((
        u64_to_u32_limbs(&[0, u64::MAX, u64::MAX - 1, 0, 0, 0]),
        u64_to_u32_limbs(&[u64::MAX, u64::MAX]).into_iter().chain([0; 5]).collect(),
    ));
    cases.push((
        u64_to_u32_limbs(&[3, 0, 1u64 << 63, 0, 0, 0]),
        u64_to_u32_limbs(&[1, 1u64 << 63]).into_iter().chain([0; 5]).collect(),
    ));
    cases.push((
        u64_to_u32_limbs(&[0, 0, 1u64 << 63, 1u64 << 63, 0, 0]),
        u64_to_u32_limbs(&[1, 1u64 << 63]).into_iter().chain([0; 5]).collect(),
    ));
    cases.push((
        u64_to_u32_limbs(&[u64::MAX, 0, 0, 1, 0, 0]),
        u64_to_u32_limbs(&[u64::MAX, 1]).into_iter().chain([0; 5]).collect(),
    ));
    cases.push((
        u64_to_u32_limbs(&[0, 0, 0, 0, 0, u64::MAX]),
        u64_to_u32_limbs(&[u64::MAX, u64::MAX >> 1]).into_iter().chain([0; 5]).collect(),
    ));
    cases.push((
        u64_to_u32_limbs(&[5, 4, 3, 2, 1, 6]),
        u64_to_u32_limbs(&[7, 0, 1]).into_iter().chain([0; 3]).collect(),
    ));

    // u32-digit add-back trigger: u = 2^127, v = 2^95 + 1 (3 significant u32 limbs).
    cases.push((
        vec![0, 0, 0, 1 << 31, 0, 0, 0, 0, 0, 0, 0, 0],
        vec![1, 0, 1 << 31, 0, 0, 0, 0, 0, 0],
    ));
    // Trivial paths.
    cases.push((vec![0; 12], vec![1, 0, 0, 0, 0, 0, 0, 0, 0]));
    cases.push((vec![42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], vec![42, 0, 0, 0, 0, 0, 0, 0, 0]));
    cases.push((vec![41, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], vec![42, 0, 0, 0, 0, 0, 0, 0, 0]));
    // u < v with equal limb counts.
    cases.push((
        vec![1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        vec![2, 1, 0, 0, 0, 0, 0, 0, 0],
    ));
    // Max dividend over small divisors (short-division stress).
    cases.push((vec![max; 12], vec![3, 0, 0, 0, 0, 0, 0, 0, 0]));
    cases.push((vec![max; 12], vec![max, 0, 0, 0, 0, 0, 0, 0, 0]));
    // Max dividend over max divisor.
    cases.push((vec![max; 12], vec![max; 9]));
    // Divisor top limb 1 after normalization boundary.
    cases.push((vec![max; 12], vec![0, 0, 0, 0, 0, 0, 0, 0, 1]));

    for (u, v) in cases {
        let mut ua = [0u64; U_LIMBS];
        ua.copy_from_slice(&u[..U_LIMBS]);
        let mut va = [0u64; V_LIMBS];
        va.copy_from_slice(&v[..V_LIMBS]);
        check_case(&program, &ua, &va);
    }
}

#[test]
fn division_by_zero_fails() {
    let program = program(&div_driver_source());
    let mut advice = vec![0u64; U_LIMBS + V_LIMBS];
    advice[0] = 7; // u = 7, v = 0
    let result = execute(library(), &program, &advice);
    assert!(result.is_err(), "division by zero must fail");
}

#[test]
fn multiplication_matches_oracle() {
    // Driver: 8-limb a times 4-limb b -> 12-limb product (the widest shape the
    // library uses; smaller shapes are sub-cases of the same loop).
    const A_PTR: u32 = 0x2000;
    const B_PTR: u32 = 0x2010;
    const OUT_PTR: u32 = 0x2020;
    let mut src = String::from("use amm::math::wide\nuse miden::core::sys\n\nbegin\n");
    src.push_str(&format!("    push.{A_PTR}\n"));
    src.push_str("    repeat.8\n        adv_push dup.1 mem_store add.1\n    end\n    drop\n");
    src.push_str(&format!("    push.{B_PTR}\n"));
    src.push_str("    repeat.4\n        adv_push dup.1 mem_store add.1\n    end\n    drop\n");
    src.push_str(&format!(
        "    push.4 push.{B_PTR} push.8 push.{A_PTR} push.{OUT_PTR}\n"
    ));
    src.push_str("    exec.wide::mul_limbs_ptr\n");
    for i in (0..12).rev() {
        src.push_str(&format!("    push.{} mem_load\n", OUT_PTR + i as u32));
    }
    src.push_str("    exec.sys::truncate_stack\nend\n");
    let program = program(&src);

    let mut r = rng(0xD1D1_0002);
    for case in 0..256 {
        let a: [u64; 8] = if case == 0 {
            [0xFFFF_FFFF; 8]
        } else {
            random_limbs(&mut r, 8)
        };
        let b: [u64; 4] = if case == 0 {
            [0xFFFF_FFFF; 4]
        } else {
            random_limbs(&mut r, 4)
        };

        let mut advice: Vec<u64> = Vec::new();
        advice.extend_from_slice(&a);
        advice.extend_from_slice(&b);
        let stack = execute(library(), &program, &advice).expect("mul driver must execute");
        let masm_out = &stack[..12];

        let a64 = u32_to_u64_limbs(&a);
        let b64 = u32_to_u64_limbs(&b);
        let mut out64 = [0u64; 6];
        amm_math::wide::mul_limbs(&a64, &b64, &mut out64);
        let oracle_out = u64_to_u32_limbs(&out64);
        assert_eq!(masm_out, &oracle_out[..], "product mismatch for a={a:?} b={b:?}");
    }
}
