//! Stage-2 deliverables for the MASM pool: component size (tx-deployability) and the
//! per-note cycle / throughput table (DESIGN Part 2 "throughput table" deliverable,
//! now measurable because the MASM port fits the 2^18 network-tx budget).
//!
//! Rust-build reference numbers (DESIGN, measured 2026-08-26): SWAP_NO_CROSS
//! 3,216,611 · SWAP_1_CROSS 3,971,269 · SWAP_5_CROSS 7,018,622 — 0 swaps per 2^18
//! network tx by construction.

use integration::pool::testbed::{
    assert_p2id_output, consume_note, Backend, PoolTestbed,
};
use integration::pool::PoolSim;

const FEE_PIPS: u32 = 3000;
const SPACING: i32 = 60;
const DEADLINE: u32 = 1000;
const L_NARROW: u128 = 1_000_000_000_000;
const L_BACKSTOP: u128 = 10_000_000_000_000;

/// ntx-builder per-network-tx cycle budget (CLI default, DESIGN Part 1f).
const NTX_CYCLE_BUDGET: usize = 1 << 18;

/// `ACCOUNT_UPDATE_MAX_SIZE` (256 KiB): the deployability bound the ~600 KB Rust
/// pool exceeded (DESIGN Part 5, Phase 4 finding 1).
const ACCOUNT_UPDATE_MAX_SIZE: usize = 262_144;

/// Rust-build per-note cycle references from DESIGN.
const RUST_SWAP_NO_CROSS: usize = 3_216_611;
const RUST_SWAP_1_CROSS: usize = 3_971_269;
const RUST_SWAP_5_CROSS: usize = 7_018_622;

#[test]
fn masm_component_size_within_deployment_bound() {
    let size = clamm_pool_masm::pool_library_size();
    println!(
        "MASM pool component library: {size} bytes serialized \
         (bound {ACCOUNT_UPDATE_MAX_SIZE}; Rust build ~600 KB, not tx-deployable)"
    );
    assert!(
        size < ACCOUNT_UPDATE_MAX_SIZE,
        "MASM pool component ({size} bytes) must stay tx-deployable"
    );
}

struct ShapeCycles {
    label: &'static str,
    cycles: usize,
    rust_reference: Option<usize>,
}

/// Measures every note shape's consumption cycles on the MASM backend, prints the
/// throughput table, and (exact-state, sim-checked) proves a multi-note batch swap
/// tx works.
#[tokio::test]
async fn masm_cycles_and_throughput_table() -> anyhow::Result<()> {
    let mut shapes: Vec<ShapeCycles> = Vec::new();

    // ---- testbed A: mint / refund / no-cross / 1-cross / burn / collect ----
    {
        let mut tb = PoolTestbed::for_backend(Backend::Masm, FEE_PIPS, SPACING, 0)?;
        let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
        let (n0, n1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
        sim.mint(-120, 120, L_NARROW);
        let (b0, b1) = sim.amounts_for_liquidity(-6000, 6000, L_BACKSTOP, true);
        sim.mint(-6000, 6000, L_BACKSTOP);

        let amount_small: u64 = 1_000_000_000; // stays inside the narrow range
        let amount_cross: u64 = 80_000_000_000; // crosses -120 into the backstop
        let out_small = sim.swap(amount_small, true);
        assert_eq!(out_small.crossings, 0, "setup: no-cross swap must not cross");
        let out_cross = sim.swap(amount_cross, true);
        assert_eq!(out_cross.crossings, 1, "setup: 1-cross swap must cross once");
        sim.burn(-120, 120, L_NARROW);
        let (c0, c1) = sim.collect(-120, 120);
        // Both swaps are token0-in and the 1-cross swap leaves the price below the
        // narrow range, so the collected token1 side may be zero.
        assert!(c0 > 0);

        let lp = tb.lp.id();
        let trader = tb.trader.id();
        let mint_narrow = tb.add_mint_note(lp, -120, 120, L_NARROW, n0 as u64, n1 as u64, DEADLINE)?;
        let mint_backstop =
            tb.add_mint_note(lp, -6000, 6000, L_BACKSTOP, b0 as u64, b1 as u64, DEADLINE)?;
        // deadline 0: consumed as a pure refund, no swap math.
        let refund_swap = tb.add_swap_note(trader, 0, amount_small, 0, trader, 0)?;
        let swap_small =
            tb.add_swap_note(trader, 0, amount_small, out_small.amount_out as u64, trader, DEADLINE)?;
        let swap_cross =
            tb.add_swap_note(trader, 0, amount_cross, out_cross.amount_out as u64, trader, DEADLINE)?;
        let burn_note = tb.add_burn_note(lp, -120, 120, L_NARROW)?;
        let collect_note = tb.add_collect_note(lp, -120, 120)?;
        let (mut mock_chain, h) = tb.build()?;
        let pool = h.pool.id();

        let mut measure = |label: &'static str,
                           executed: &miden_client::transaction::ExecutedTransaction,
                           rust_reference: Option<usize>|
         -> anyhow::Result<()> {
            let m = executed.measurements();
            anyhow::ensure!(m.note_execution.len() == 1);
            shapes.push(ShapeCycles {
                label,
                cycles: m.note_execution[0].1,
                rust_reference,
            });
            Ok(())
        };

        let exec = consume_note(&mut mock_chain, pool, mint_narrow.id()).await?;
        measure("mint", &exec, None)?;
        consume_note(&mut mock_chain, pool, mint_backstop.id()).await?;
        let exec = consume_note(&mut mock_chain, pool, refund_swap.id()).await?;
        measure("deadline-refund", &exec, None)?;
        let exec = consume_note(&mut mock_chain, pool, swap_small.id()).await?;
        measure(
            "swap no-cross (ends IN-RANGE, reverse mapping in situ)",
            &exec,
            Some(RUST_SWAP_NO_CROSS),
        )?;
        let exec = consume_note(&mut mock_chain, pool, swap_cross.id()).await?;
        measure("swap 1-cross", &exec, Some(RUST_SWAP_1_CROSS))?;
        let exec = consume_note(&mut mock_chain, pool, burn_note.id()).await?;
        measure("burn", &exec, None)?;
        let exec = consume_note(&mut mock_chain, pool, collect_note.id()).await?;
        measure("collect", &exec, None)?;

        // The scenario stays exact: final payout checked against the sim.
        let mut expected_payout = vec![(h.token0, c0)];
        if c1 > 0 {
            expected_payout.push((h.token1, c1));
        }
        assert_p2id_output(&exec, &collect_note, 3, lp, &expected_payout)?;
    }

    // ---- testbed B: 5-cross swap over a ladder of narrow ranges ----
    {
        let mut tb = PoolTestbed::for_backend(Backend::Masm, FEE_PIPS, SPACING, 0)?;
        let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
        let ranges: [(i32, i32, u128); 6] = [
            (-120, 120, L_NARROW),
            (-240, -120, L_NARROW),
            (-360, -240, L_NARROW),
            (-480, -360, L_NARROW),
            (-600, -480, L_NARROW),
            (-6000, 6000, L_BACKSTOP),
        ];
        let lp = tb.lp.id();
        let trader = tb.trader.id();
        let mut mint_notes = Vec::new();
        for (lower, upper, liq) in ranges {
            let (a0, a1) = sim.amounts_for_liquidity(lower, upper, liq, true);
            sim.mint(lower, upper, liq);
            mint_notes.push(tb.add_mint_note(lp, lower, upper, liq, a0 as u64, a1 as u64, DEADLINE)?);
        }
        let amount_in: u64 = 400_000_000_000;
        let outcome = sim.swap(amount_in, true);
        assert_eq!(outcome.crossings, 5, "setup: swap must cross five ticks");
        let swap_note =
            tb.add_swap_note(trader, 0, amount_in, outcome.amount_out as u64, trader, DEADLINE)?;
        let (mut mock_chain, h) = tb.build()?;
        let pool = h.pool.id();
        for note in &mint_notes {
            consume_note(&mut mock_chain, pool, note.id()).await?;
        }
        let exec = consume_note(&mut mock_chain, pool, swap_note.id()).await?;
        assert_p2id_output(&exec, &swap_note, 0, trader, &[(h.token1, outcome.amount_out as u64)])?;
        let m = exec.measurements();
        anyhow::ensure!(m.note_execution.len() == 1);
        shapes.push(ShapeCycles {
            label: "swap 5-cross",
            cycles: m.note_execution[0].1,
            rust_reference: Some(RUST_SWAP_5_CROSS),
        });
    }

    // ---- batch: two no-cross swaps consumed in ONE MockChain transaction ----
    // (also yields the measured per-tx kernel overhead used by the table below)
    let (batch_overhead, batch_note_cycles, batch_trace_len) = {
        let mut tb = PoolTestbed::for_backend(Backend::Masm, FEE_PIPS, SPACING, 0)?;
        let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
        let (n0, n1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
        sim.mint(-120, 120, L_NARROW);
        let amount: u64 = 1_000_000_000;
        let out_a = sim.swap(amount, true);
        let out_b = sim.swap(amount, true);

        let lp = tb.lp.id();
        let trader = tb.trader.id();
        let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, n0 as u64, n1 as u64, DEADLINE)?;
        let swap_a = tb.add_swap_note(trader, 0, amount, out_a.amount_out as u64, trader, DEADLINE)?;
        let swap_b = tb.add_swap_note(trader, 0, amount, out_b.amount_out as u64, trader, DEADLINE)?;
        let (mut mock_chain, h) = tb.build()?;
        let pool = h.pool.id();

        consume_note(&mut mock_chain, pool, mint_note.id()).await?;
        let executed = mock_chain
            .build_tx_context(pool, &[swap_a.id(), swap_b.id()], &[])?
            .build()?
            .execute()
            .await?;
        mock_chain.add_pending_executed_transaction(&executed)?;
        mock_chain.prove_next_block()?;

        // Exact end state after both swaps.
        assert_eq!(
            integration::pool::testbed::read_value(&mock_chain, pool, "sqrt_price")?,
            integration::pool::u128_to_word(sim.sqrt_price),
            "batch end price mismatch"
        );
        assert_eq!(
            integration::pool::testbed::vault_balance(&mock_chain, pool, h.token0)?,
            n0 as u64 + 2 * amount
        );

        let m = executed.measurements();
        anyhow::ensure!(m.note_execution.len() == 2, "batch must consume two notes");
        let per_note: Vec<usize> = m.note_execution.iter().map(|(_, c)| *c).collect();
        let overhead = m.total_cycles() - per_note.iter().sum::<usize>();
        (overhead, per_note, m.trace_length())
    };

    // ---- report ----
    println!();
    println!("== MASM pool per-note-consumption cycles (MockChain, per-note segment) ==");
    println!(
        "{:<52} {:>10} {:>13} {:>9}",
        "note shape", "cycles", "rust (DESIGN)", "speedup"
    );
    for shape in &shapes {
        let (rust, speedup) = match shape.rust_reference {
            Some(r) => (
                r.to_string(),
                format!("{:.0}x", r as f64 / shape.cycles.max(1) as f64),
            ),
            None => ("-".to_string(), "-".to_string()),
        };
        println!(
            "{:<52} {:>10} {:>13} {:>9}",
            shape.label, shape.cycles, rust, speedup
        );
    }
    println!();
    println!(
        "batch tx: 2 no-cross swaps in ONE tx -> per-note cycles {:?}, kernel overhead {} \
         cycles, trace length {}",
        batch_note_cycles, batch_overhead, batch_trace_len
    );
    println!();
    println!(
        "== throughput: swaps per 2^18-cycle network tx (batch-measured kernel overhead \
         {batch_overhead} cycles; per-note extrapolation beyond the 2-note batch) =="
    );
    let mut throughput = std::collections::BTreeMap::new();
    for shape in &shapes {
        if !shape.label.starts_with("swap") && shape.label != "deadline-refund" {
            continue;
        }
        let per_tx = (NTX_CYCLE_BUDGET.saturating_sub(batch_overhead)) / shape.cycles.max(1);
        println!("{:<52} {:>3} per 2^18 tx (vs 0 in Rust)", shape.label, per_tx);
        throughput.insert(shape.label, per_tx);
    }

    // Hard gates: the port's reason to exist.
    let get = |label: &str| {
        shapes
            .iter()
            .find(|s| s.label.starts_with(label))
            .map(|s| s.cycles)
            .unwrap()
    };
    assert!(
        get("swap no-cross") + batch_overhead < NTX_CYCLE_BUDGET,
        "an in-range swap (incl. reverse mapping) must fit a default network tx"
    );
    assert!(
        get("swap 1-cross") + batch_overhead < NTX_CYCLE_BUDGET,
        "a 1-cross swap must fit a default network tx"
    );
    assert!(
        throughput["swap no-cross (ends IN-RANGE, reverse mapping in situ)"] >= 2,
        "at least two in-range swaps must fit one default network tx"
    );
    Ok(())
}
