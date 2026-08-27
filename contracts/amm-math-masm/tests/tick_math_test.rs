//! Bit-equality and bracket-property tests for `amm::math::tick_math` against the Rust
//! oracle (`amm_math::tick_math`).
//!
//! Coverage:
//! - constant parity: the MASM per-bit table (parsed from source) == TICK_BIT_CONSTANTS;
//! - forward mapping: every single-bit tick (both signs), boundaries, dense stride and
//!   random ticks, all bit-equal in the VM;
//! - reverse mapping (log2 algorithm): dense tick sweep + random ratios + +/-1-ulp
//!   probes around every sampled boundary, MASM result == the oracle's binary-search
//!   result in the VM; plus an exhaustive native check of the identical algorithm
//!   (host mirror) over EVERY tick boundary +/-1 ulp in the supported range.

mod common;

use amm_math::tick_math::{
    get_sqrt_ratio_at_tick as oracle_forward, get_tick_at_sqrt_ratio as oracle_reverse,
    MAX_SQRT_RATIO, MAX_TICK, MIN_SQRT_RATIO, MIN_TICK, TICK_BIT_CONSTANTS,
};
use common::*;
use rand::Rng;
use primitive_types::U256;

const TICK_OFFSET: i64 = 524_288; // 2^19, DESIGN.md pool encoding

fn off(tick: i32) -> u64 {
    (tick as i64 + TICK_OFFSET) as u64
}

fn forward_driver() -> miden_processor::Program {
    program(
        "use amm::math::tick_math\nuse miden::core::sys\n\nbegin\n    adv_push\n    exec.tick_math::get_sqrt_ratio_at_tick\n    exec.sys::truncate_stack\nend\n",
    )
}

fn reverse_driver() -> miden_processor::Program {
    // Four adv_push instructions leave the FIRST advice value deepest, so the advice
    // vector carries the limbs most-significant-first to put limb 0 on top.
    program(
        "use amm::math::tick_math\nuse miden::core::sys\n\nbegin\n    adv_push adv_push adv_push adv_push\n    exec.tick_math::get_tick_at_sqrt_ratio\n    exec.sys::truncate_stack\nend\n",
    )
}

fn masm_forward(prog: &miden_processor::Program, tick: i32) -> u128 {
    let stack = execute(library(), prog, &[off(tick)]).expect("forward driver must execute");
    limbs_to_u128(&stack[..4])
}

fn masm_reverse(prog: &miden_processor::Program, x: u128) -> i32 {
    let limbs = u128_to_limbs(x);
    let advice = [limbs[3], limbs[2], limbs[1], limbs[0]];
    let stack = execute(library(), prog, &advice).expect("reverse driver must execute");
    (stack[0] as i64 - TICK_OFFSET) as i32
}

// CONSTANT PARITY
// ================================================================================================

/// Parses `const NAME = 0x...` / `const NAME = 123` definitions out of the MASM source.
fn parse_masm_constants(source: &str) -> std::collections::BTreeMap<String, u64> {
    let mut map = std::collections::BTreeMap::new();
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("const ") else {
            continue;
        };
        let Some((name, value)) = rest.split_once('=') else {
            continue;
        };
        let value = value.trim().split(&[' ', '#'][..]).next().unwrap_or("");
        let parsed = if let Some(hex) = value.strip_prefix("0x") {
            u64::from_str_radix(hex, 16).ok()
        } else {
            value.parse::<u64>().ok()
        };
        if let Some(v) = parsed {
            map.insert(name.trim().to_string(), v);
        }
    }
    map
}

#[test]
fn tick_constants_match_rust_oracle() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("asm/tick_math.masm"),
    )
    .expect("tick_math.masm must be readable");
    let consts = parse_masm_constants(&source);

    // The 19-entry per-bit table, limb for limb.
    for (i, &c) in TICK_BIT_CONSTANTS.iter().enumerate() {
        let limbs = u128_to_limbs(c);
        for (l, &expected) in limbs.iter().enumerate() {
            let name = format!("TICK_C{i:02}_L{l}");
            assert_eq!(
                consts.get(&name).copied(),
                Some(expected),
                "MASM constant {name} must equal Rust TICK_BIT_CONSTANTS[{i}] limb {l}"
            );
        }
    }

    // Domain constants.
    for (name, expected) in [
        ("TICK_OFFSET", TICK_OFFSET as u64),
        ("OFF_MIN_TICK", off(MIN_TICK)),
        ("OFF_MAX_TICK", off(MAX_TICK)),
        ("OFF_MAX_TICK_M1", off(MAX_TICK - 1)),
    ] {
        assert_eq!(consts.get(name).copied(), Some(expected), "MASM constant {name}");
    }
    for (prefix, value) in [("MIN_SQRT", MIN_SQRT_RATIO), ("MAX_SQRT", MAX_SQRT_RATIO)] {
        let limbs = u128_to_limbs(value);
        for (l, &expected) in limbs.iter().enumerate() {
            let name = format!("{prefix}_L{l}");
            assert_eq!(consts.get(&name).copied(), Some(expected), "MASM constant {name}");
        }
    }

    // Reverse-mapping constants: K and the offset-folded A1/A2, recomputed exactly.
    let k = U256::from(255738958999603826347141u128);
    let c1 = U256::from(3402992956809132418596140100660247210u128);
    let c2 = U256::from(291339464771989622907027621153398088495u128);
    let off_term = U256::from(TICK_OFFSET) << 128;
    let log_term = (U256::one() << 69) * k;
    let a1 = off_term - c1 - log_term;
    let a2 = off_term + c2 - log_term;

    let limb = |v: U256, i: usize| ((v >> (32 * i)) & U256::from(0xFFFF_FFFFu64)).as_u64();
    for i in 0..3 {
        assert_eq!(consts.get(&format!("LOG_K_L{i}")).copied(), Some(limb(k, i)));
    }
    for i in 0..5 {
        assert_eq!(consts.get(&format!("LOG_A1_L{i}")).copied(), Some(limb(a1, i)));
        assert_eq!(consts.get(&format!("LOG_A2_L{i}")).copied(), Some(limb(a2, i)));
    }
}

// FORWARD MAPPING
// ================================================================================================

#[test]
fn forward_matches_oracle_on_structured_ticks() {
    let prog = forward_driver();
    let mut ticks: Vec<i32> = vec![0, 1, -1, MIN_TICK, MAX_TICK, MIN_TICK + 1, MAX_TICK - 1];
    // Every single-bit magnitude, both signs: pins each per-bit constant behaviorally.
    for i in 0..19 {
        let m = 1i32 << i;
        if m <= MAX_TICK {
            ticks.push(m);
            ticks.push(-m);
        }
    }
    // All-bits patterns.
    ticks.push(443_635);
    ticks.push(-443_635);
    ticks.push(0b1010101010101010101 % (MAX_TICK + 1));
    for tick in ticks {
        assert_eq!(
            masm_forward(&prog, tick),
            oracle_forward(tick),
            "forward mismatch at tick {tick}"
        );
    }
}

#[test]
fn forward_matches_oracle_on_dense_and_random_ticks() {
    let prog = forward_driver();
    let mut r = rng(0x71C4_0001);

    // Dense stride sweep across the full range.
    let mut count = 0;
    let mut tick = MIN_TICK;
    while tick <= MAX_TICK {
        assert_eq!(
            masm_forward(&prog, tick),
            oracle_forward(tick),
            "forward mismatch at tick {tick}"
        );
        tick += 3517; // prime stride, ~252 evaluations
        count += 1;
    }
    assert!(count > 200);

    // Random ticks.
    for _ in 0..512 {
        let tick = r.random_range(MIN_TICK..=MAX_TICK);
        assert_eq!(
            masm_forward(&prog, tick),
            oracle_forward(tick),
            "forward mismatch at tick {tick}"
        );
    }
}

#[test]
fn forward_rejects_out_of_range_ticks() {
    let prog = forward_driver();
    for bad in [off(MIN_TICK) - 1, off(MAX_TICK) + 1, 0, u32::MAX as u64] {
        assert!(
            execute(library(), &prog, &[bad]).is_err(),
            "tick_off {bad} must be rejected"
        );
    }
}

// REVERSE MAPPING (log2 algorithm)
// ================================================================================================

/// Host-side mirror of the MASM log2 reverse mapping, integer-for-integer identical.
/// Used for the exhaustive native bracket check below (the in-VM tests verify the MASM
/// itself against the oracle on dense samples).
fn mirror_reverse(x: u128) -> i32 {
    assert!((MIN_SQRT_RATIO..MAX_SQRT_RATIO).contains(&x));
    let m = 127 - x.leading_zeros(); // msb index, in [64, 127]
    let mut r: u128 = x << (127 - m); // = (x << 32) >> (msb_ratio - 127), normalized mantissa
    let mut log_off: u128 = ((m as u128) - 64) << 64;
    for k in (50..=63).rev() {
        let sq = U256::from(r) * U256::from(r);
        if (sq >> 255) == U256::one() {
            r = ((sq >> 128) & U256::from(u128::MAX)).as_u128();
            log_off |= 1u128 << k;
        } else {
            r = ((sq >> 127) & U256::from(u128::MAX)).as_u128();
        }
    }
    let k = U256::from(255738958999603826347141u128);
    let c1 = U256::from(3402992956809132418596140100660247210u128);
    let c2 = U256::from(291339464771989622907027621153398088495u128);
    let off_term = U256::from(TICK_OFFSET) << 128;
    let log_term = (U256::one() << 69) * k;
    let a1 = off_term - c1 - log_term;
    let a2 = off_term + c2 - log_term;

    let p = U256::from(log_off) * k;
    let t_hi_off = ((p + a2) >> 128).as_u64().min(off(MAX_TICK));
    let t_lo_off = ((p + a1) >> 128)
        .as_u64()
        .max(off(MIN_TICK))
        .min(off(MAX_TICK - 1));
    let t_hi = (t_hi_off as i64 - TICK_OFFSET) as i32;
    let t_lo = (t_lo_off as i64 - TICK_OFFSET) as i32;
    if t_lo == t_hi {
        t_lo
    } else if oracle_forward(t_hi) <= x {
        t_hi
    } else {
        t_lo
    }
}

/// Exhaustive native validation: for EVERY tick boundary in the supported range, the
/// mirror of the MASM algorithm satisfies the exact bracket property at the boundary
/// ratio and one ulp on either side. Runs natively (no VM), so the full 2.6M-probe
/// sweep is fast; the in-VM tests below pin MASM == mirror == oracle on dense samples.
#[test]
fn reverse_mirror_is_exact_on_every_boundary() {
    let mut ratio_t = oracle_forward(MIN_TICK);
    for t in MIN_TICK..MAX_TICK {
        let ratio_next = oracle_forward(t + 1);
        // x = ratio(t): bracket says result == t.
        assert_eq!(mirror_reverse(ratio_t), t, "boundary ratio({t})");
        // x = ratio(t) + 1 (still < ratio(t+1) unless degenerate): result == t.
        if ratio_t + 1 < ratio_next {
            assert_eq!(mirror_reverse(ratio_t + 1), t, "ratio({t})+1");
        }
        // x = ratio(t+1) - 1 >= ratio(t): result == t.
        if ratio_next - 1 >= ratio_t {
            assert_eq!(mirror_reverse(ratio_next - 1), t, "ratio({})-1", t + 1);
        }
        ratio_t = ratio_next;
    }
}

#[test]
fn reverse_matches_oracle_on_dense_boundaries_in_vm() {
    let prog = reverse_driver();

    // Sampled ticks: stride sweep + all single-bit magnitudes + range edges.
    let mut ticks: Vec<i32> = vec![MIN_TICK, MIN_TICK + 1, -1, 0, 1, MAX_TICK - 1, MAX_TICK];
    let mut t = MIN_TICK;
    while t <= MAX_TICK {
        ticks.push(t);
        t += 9973; // prime stride, ~89 boundary groups
    }
    for i in 0..19 {
        let m = 1i32 << i;
        if m <= MAX_TICK {
            ticks.push(m);
            ticks.push(-m);
        }
    }

    for tick in ticks {
        let boundary = oracle_forward(tick);
        for probe in [boundary.wrapping_sub(1), boundary, boundary + 1] {
            if !(MIN_SQRT_RATIO..MAX_SQRT_RATIO).contains(&probe) {
                continue;
            }
            let expected = oracle_reverse(probe);
            let got = masm_reverse(&prog, probe);
            assert_eq!(got, expected, "reverse mismatch at x={probe} (boundary tick {tick})");
        }
    }
}

#[test]
fn reverse_matches_oracle_on_random_ratios_in_vm() {
    let prog = reverse_driver();
    let mut r = rng(0x71C4_0002);
    let span = MAX_SQRT_RATIO - MIN_SQRT_RATIO;
    for _ in 0..512 {
        // Mix uniform ratios with ratios biased toward the low end (log-uniform-ish).
        let x = if r.random_bool(0.5) {
            MIN_SQRT_RATIO + (r.random::<u128>() % span)
        } else {
            let bits = r.random_range(0..64);
            MIN_SQRT_RATIO + ((r.random::<u128>() >> bits) % span)
        };
        let expected = oracle_reverse(x);
        let got = masm_reverse(&prog, x);
        assert_eq!(got, expected, "reverse mismatch at x={x}");
    }
}

#[test]
fn reverse_rejects_out_of_domain_ratios() {
    let prog = reverse_driver();
    for bad in [0u128, MIN_SQRT_RATIO - 1, MAX_SQRT_RATIO, u128::MAX] {
        let limbs = u128_to_limbs(bad);
        let advice = [limbs[3], limbs[2], limbs[1], limbs[0]];
        assert!(
            execute(library(), &prog, &advice).is_err(),
            "ratio {bad} must be rejected"
        );
    }
}
