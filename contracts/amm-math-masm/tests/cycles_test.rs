//! Per-procedure cycle measurements of the MASM port, compared against the measured
//! Rust-build numbers from DESIGN.md (Phase 2 gates, MockChain in-VM benchmarks, net of
//! the note baseline): mul_div_floor 110,214 · get_sqrt_ratio_at_tick 196,434–319,817 ·
//! get_tick_at_sqrt_ratio (binary search) 4,748,056 · compute_swap_step 702,146.
//!
//! Cycles here are VM clock cycles (main trace rows before padding) of a driver
//! program, net of the same driver with the procedure call removed, measured with the
//! production execution path (FastProcessor + core-library event handlers).

mod common;

use amm_math::tick_math::{MAX_SQRT_RATIO, MIN_SQRT_RATIO};
use common::*;

const TICK_OFFSET: i64 = 524_288;

/// Rust-build reference cycle counts from DESIGN.md (measured 2026-08-26).
const RUST_MUL_DIV: usize = 110_214;
const RUST_FWD_TYPICAL: usize = 196_434;
const RUST_FWD_WORST: usize = 319_817;
const RUST_REVERSE: usize = 4_748_056;
const RUST_SWAP_STEP: usize = 702_146;

/// ntx-builder per-network-tx cycle budget (CLI default, DESIGN.md Part 1f).
const NTX_CYCLE_BUDGET: usize = 1 << 18;

struct Bench {
    label: &'static str,
    exec_line: &'static str,
    module: &'static str,
    n_inputs: usize,
    stack_top_first: Vec<u64>,
    rust_reference: Option<usize>,
}

fn measure(bench: &Bench) -> (usize, usize) {
    let call_src = format!(
        "use amm::math::{}\nuse miden::core::sys\n\nbegin\n    repeat.{} adv_push end\n    {}\n    exec.sys::truncate_stack\nend\n",
        bench.module, bench.n_inputs, bench.exec_line
    );
    let base_src = format!(
        "use miden::core::sys\n\nbegin\n    repeat.{} adv_push end\n    repeat.{} drop end\n    exec.sys::truncate_stack\nend\n",
        bench.n_inputs, bench.n_inputs
    );
    let advice = advice_for_stack(&bench.stack_top_first);
    let (_, call_cycles) = execute_with_cycles(library(), &program(&call_src), &advice)
        .unwrap_or_else(|e| panic!("bench {} must execute: {e:?}", bench.label));
    let (_, base_cycles) = execute_with_cycles(library(), &program(&base_src), &advice)
        .expect("baseline must execute");
    (call_cycles.saturating_sub(base_cycles), call_cycles)
}

fn u128_stack(values: &[u128]) -> Vec<u64> {
    let mut s = Vec::new();
    for &v in values {
        s.extend_from_slice(&u128_to_limbs(v));
    }
    s
}

#[test]
fn cycle_benchmarks() {
    let q96: u128 = 1 << 96;
    let mid_ratio = amm_math::tick_math::get_sqrt_ratio_at_tick(12_345);
    let typical_l = 10u128.pow(24);

    let mut swap_stack = u128_stack(&[
        q96,                       // current
        q96 / 2,                   // target
        typical_l,                 // liquidity
        10u128.pow(12),            // |amount_remaining|
    ]);
    swap_stack.push(1); // exact_in
    swap_stack.push(3000); // fee_pips

    let mut a0d_stack = u128_stack(&[q96, q96 * 2, typical_l]);
    a0d_stack.push(1);
    let mut from_in_stack = u128_stack(&[q96, typical_l, 10u128.pow(12)]);
    from_in_stack.push(1);

    let benches = vec![
        Bench {
            label: "mul_div_floor",
            exec_line: "exec.muldiv::mul_div_floor",
            module: "muldiv",
            n_inputs: 12,
            stack_top_first: u128_stack(&[u128::MAX - 12345, u128::MAX / 7, u128::MAX / 3]),
            rust_reference: Some(RUST_MUL_DIV),
        },
        Bench {
            label: "mul_div_ceil",
            exec_line: "exec.muldiv::mul_div_ceil",
            module: "muldiv",
            n_inputs: 12,
            stack_top_first: u128_stack(&[u128::MAX - 12345, u128::MAX / 7, u128::MAX / 3]),
            rust_reference: None,
        },
        Bench {
            label: "get_sqrt_ratio_at_tick(12345)",
            exec_line: "exec.tick_math::get_sqrt_ratio_at_tick",
            module: "tick_math",
            n_inputs: 1,
            stack_top_first: vec![(12_345i64 + TICK_OFFSET) as u64],
            rust_reference: Some(RUST_FWD_TYPICAL),
        },
        Bench {
            label: "get_sqrt_ratio_at_tick(+443636)",
            exec_line: "exec.tick_math::get_sqrt_ratio_at_tick",
            module: "tick_math",
            n_inputs: 1,
            stack_top_first: vec![(443_636i64 + TICK_OFFSET) as u64],
            rust_reference: Some(RUST_FWD_WORST),
        },
        Bench {
            label: "get_sqrt_ratio_at_tick(-443636)",
            exec_line: "exec.tick_math::get_sqrt_ratio_at_tick",
            module: "tick_math",
            n_inputs: 1,
            stack_top_first: vec![(-443_636i64 + TICK_OFFSET) as u64],
            rust_reference: None,
        },
        Bench {
            label: "REVERSE TICK (mid ratio)",
            exec_line: "exec.tick_math::get_tick_at_sqrt_ratio",
            module: "tick_math",
            n_inputs: 4,
            stack_top_first: u128_stack(&[mid_ratio]),
            rust_reference: Some(RUST_REVERSE),
        },
        Bench {
            label: "REVERSE TICK (min ratio)",
            exec_line: "exec.tick_math::get_tick_at_sqrt_ratio",
            module: "tick_math",
            n_inputs: 4,
            stack_top_first: u128_stack(&[MIN_SQRT_RATIO]),
            rust_reference: Some(RUST_REVERSE),
        },
        Bench {
            label: "REVERSE TICK (max ratio - 1)",
            exec_line: "exec.tick_math::get_tick_at_sqrt_ratio",
            module: "tick_math",
            n_inputs: 4,
            stack_top_first: u128_stack(&[MAX_SQRT_RATIO - 1]),
            rust_reference: Some(RUST_REVERSE),
        },
        Bench {
            label: "get_amount0_delta",
            exec_line: "exec.sqrt_price_math::get_amount0_delta",
            module: "sqrt_price_math",
            n_inputs: 13,
            stack_top_first: a0d_stack,
            rust_reference: None,
        },
        Bench {
            label: "get_next_sqrt_price_from_input",
            exec_line: "exec.sqrt_price_math::get_next_sqrt_price_from_input",
            module: "sqrt_price_math",
            n_inputs: 13,
            stack_top_first: from_in_stack,
            rust_reference: None,
        },
        Bench {
            label: "compute_swap_step (exact-in)",
            exec_line: "exec.swap_math::compute_swap_step",
            module: "swap_math",
            n_inputs: 18,
            stack_top_first: swap_stack,
            rust_reference: Some(RUST_SWAP_STEP),
        },
        Bench {
            label: "fee_shl128_div_liquidity",
            exec_line: "exec.fee_growth::fee_shl128_div_liquidity",
            module: "fee_growth",
            n_inputs: 8,
            stack_top_first: u128_stack(&[u64::MAX as u128, typical_l]),
            rust_reference: None,
        },
        Bench {
            label: "liquidity_mul_delta_shr128",
            exec_line: "exec.fee_growth::liquidity_mul_delta_shr128",
            module: "fee_growth",
            n_inputs: 12,
            stack_top_first: {
                let mut s = u128_stack(&[u128::MAX / 3, u64::MAX as u128]); // delta lo, hi
                s.extend(u128_stack(&[typical_l]));
                s
            },
            rust_reference: None,
        },
    ];

    println!();
    println!("== amm-math-masm per-procedure cycle measurements (FastProcessor main trace) ==");
    println!("budget: 2^18 = {NTX_CYCLE_BUDGET} cycles per network tx (ntx-builder CLI default)");
    println!();
    println!(
        "{:<34} {:>12} {:>14} {:>10}",
        "procedure", "net cycles", "rust (DESIGN)", "speedup"
    );

    let mut results = Vec::new();
    for bench in &benches {
        let (net, _raw) = measure(bench);
        let speedup = bench
            .rust_reference
            .map(|r| format!("{:.0}x", r as f64 / net.max(1) as f64))
            .unwrap_or_else(|| "-".to_string());
        let rust = bench
            .rust_reference
            .map(|r| r.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!("{:<34} {:>12} {:>14} {:>10}", bench.label, net, rust, speedup);
        results.push((bench.label, net));
    }
    println!();

    let get = |label: &str| results.iter().find(|(l, _)| *l == label).unwrap().1;
    let swap = get("compute_swap_step (exact-in)");
    let reverse = get("REVERSE TICK (mid ratio)");
    println!(
        "swap step + reverse mapping: {} cycles -> ~{} per 2^18 network tx (vs 0 in Rust)",
        swap + reverse,
        NTX_CYCLE_BUDGET / (swap + reverse).max(1)
    );

    // Hard gates: the port's reason to exist. Each hot-path op must fit comfortably
    // in the ntx budget; the previously budget-breaking ops must beat Rust by >10x.
    assert!(get("mul_div_floor") < 20_000, "mul_div_floor too expensive");
    assert!(get("get_sqrt_ratio_at_tick(+443636)") < 60_000, "forward tick too expensive");
    assert!(get("REVERSE TICK (mid ratio)") < 100_000, "reverse tick too expensive");
    assert!(get("REVERSE TICK (min ratio)") < 100_000, "reverse tick (min) too expensive");
    assert!(get("REVERSE TICK (max ratio - 1)") < 100_000, "reverse tick (max) too expensive");
    assert!(get("compute_swap_step (exact-in)") < 100_000, "swap step too expensive");
    assert!(
        swap + reverse < NTX_CYCLE_BUDGET,
        "one swap step + reverse mapping must fit a default network tx"
    );
}
