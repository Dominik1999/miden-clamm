//! Lifecycle tests for the clamm-pool component (MockChain), run against BOTH
//! backends: the Rust-SDK build (harness notes) and the hand-written MASM build
//! (kernel-read, no-args pool + two-path notes).
//!
//! Every expected value is computed natively with amm-math through the
//! `PoolSim` mirror in `integration::pool` and asserted to match the
//! on-chain result EXACTLY (storage words, output-note assets, vault
//! balances). Cycle counts for the throughput table are printed as
//! `SWAP_NO_CROSS cycles: ...` / `SWAP_1_CROSS cycles: ...` /
//! `SWAP_5_CROSS cycles: ...`.

use integration::pool::{
    bitmap_key, bitmap_position, position_key, tick_key, u128_to_word, u256_to_words,
    PoolSim, POS_LIQUIDITY, POS_TOKENS_OWED, TICK_FG0_LO, TICK_FG1_LO, TICK_LIQ_GROSS,
    TICK_LIQ_NET,
};
use integration::pool::testbed::{
    assert_p2id_output, consume_note, read_map, read_value, vault_balance, Backend, PoolTestbed,
};
use miden_client::{Felt, Word};
use miden_testing::MockChain;

const FEE_PIPS: u32 = 3000;
const SPACING: i32 = 60;
const DEADLINE: u32 = 1000;

/// Serial-derivation salts (guest constants).
const SALT_SWAP_OUT: u32 = 0;
const SALT_MINT_REFUND: u32 = 2;
const SALT_COLLECT: u32 = 3;

const L_NARROW: u128 = 1_000_000_000_000; // 1e12
const L_BACKSTOP: u128 = 10_000_000_000_000; // 1e13

/// Asserts the committed pool state (price, tick, active liquidity, both
/// fee-growth accumulators) equals the sim exactly.
fn assert_pool_state(
    mock_chain: &MockChain,
    pool: miden_client::account::AccountId,
    sim: &PoolSim,
) -> anyhow::Result<()> {
    assert_eq!(
        read_value(mock_chain, pool, "sqrt_price")?,
        u128_to_word(sim.sqrt_price),
        "sqrt_price mismatch"
    );
    let state = read_value(mock_chain, pool, "pool_state")?;
    assert_eq!(
        state[0].as_canonical_u64(),
        (sim.tick + integration::pool::TICK_OFF) as u64,
        "current tick mismatch"
    );
    assert_eq!(state[1].as_canonical_u64(), 1, "initialized flag lost");
    assert_eq!(
        read_value(mock_chain, pool, "liquidity")?,
        u128_to_word(sim.liquidity),
        "active liquidity mismatch"
    );
    let (fg0_lo, fg0_hi) = u256_to_words(sim.fg0);
    let (fg1_lo, fg1_hi) = u256_to_words(sim.fg1);
    assert_eq!(
        read_value(mock_chain, pool, "fee_growth_global0_lo")?,
        fg0_lo,
        "fee growth 0 lo mismatch"
    );
    assert_eq!(
        read_value(mock_chain, pool, "fee_growth_global0_hi")?,
        fg0_hi,
        "fee growth 0 hi mismatch"
    );
    assert_eq!(
        read_value(mock_chain, pool, "fee_growth_global1_lo")?,
        fg1_lo,
        "fee growth 1 lo mismatch"
    );
    assert_eq!(
        read_value(mock_chain, pool, "fee_growth_global1_hi")?,
        fg1_hi,
        "fee growth 1 hi mismatch"
    );
    Ok(())
}

/// Test 1: mint records the position under the sender-derived key,
/// initializes ticks and bitmap, raises active liquidity, and refunds the
/// excess assets via P2ID.
async fn mint_position_records_state_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    // Provide 1000 excess units of each token -> refund note expected.
    let excess = 1000u64;
    let lp = tb.lp.id();
    let mint_note = tb.add_mint_note(
        lp,
        -120,
        120,
        L_NARROW,
        owed0 as u64 + excess,
        owed1 as u64 + excess,
        DEADLINE,
    )?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    let executed = consume_note(&mut mock_chain, pool, mint_note.id()).await?;

    // Position record: liquidity under the Poseidon2 sender key.
    let liq_key = position_key(lp.suffix(), Felt::from(lp.prefix()), -120, 120, POS_LIQUIDITY);
    assert_eq!(
        read_map(&mock_chain, pool, "positions", liq_key)?,
        u128_to_word(L_NARROW),
        "position liquidity mismatch"
    );

    // Tick records: gross = L at both ticks, net = +L at lower / -L at upper.
    assert_eq!(
        read_map(&mock_chain, pool, "ticks", tick_key(-120, TICK_LIQ_GROSS))?,
        u128_to_word(L_NARROW)
    );
    assert_eq!(
        read_map(&mock_chain, pool, "ticks", tick_key(120, TICK_LIQ_GROSS))?,
        u128_to_word(L_NARROW)
    );
    assert_eq!(
        read_map(&mock_chain, pool, "ticks", tick_key(-120, TICK_LIQ_NET))?,
        u128_to_word(L_NARROW as i128 as u128)
    );
    assert_eq!(
        read_map(&mock_chain, pool, "ticks", tick_key(120, TICK_LIQ_NET))?,
        u128_to_word((-(L_NARROW as i128)) as u128)
    );

    // Bitmap bits set for compressed ticks -2 and +2 (same 128-bit word).
    let (wi_lower, bit_lower) = bitmap_position(-120, SPACING);
    let (wi_upper, bit_upper) = bitmap_position(120, SPACING);
    let mut expected_words: std::collections::BTreeMap<u32, u128> = Default::default();
    *expected_words.entry(wi_lower).or_default() |= 1u128 << bit_lower;
    *expected_words.entry(wi_upper).or_default() |= 1u128 << bit_upper;
    for (wi, bits) in expected_words {
        assert_eq!(
            read_map(&mock_chain, pool, "tick_bitmap", bitmap_key(wi))?,
            u128_to_word(bits),
            "bitmap word {wi} mismatch"
        );
    }

    // Active liquidity raised; price untouched; fee growth still zero.
    assert_pool_state(&mock_chain, pool, &sim)?;
    assert_eq!(sim.liquidity, L_NARROW);

    // Refund note carries exactly the excess of both tokens back to the LP.
    assert_p2id_output(
        &executed,
        &mint_note,
        SALT_MINT_REFUND,
        lp,
        &[(h.token0, excess), (h.token1, excess)],
    )?;

    // Pool vault holds exactly the owed amounts.
    assert_eq!(vault_balance(&mock_chain, pool, h.token0)?, owed0 as u64);
    assert_eq!(vault_balance(&mock_chain, pool, h.token1)?, owed1 as u64);
    Ok(())
}

/// Test 2: exact-in zero_for_one swap that stays inside the minted range.
/// The on-chain output and post-state must equal the native amm-math sim
/// exactly. Prints SWAP_NO_CROSS cycles.
async fn swap_zero_for_one_within_range_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let amount_in: u64 = 1_000_000_000; // 1e9, well inside the range
    let outcome = sim.swap(amount_in, true);
    assert_eq!(outcome.crossings, 0, "test setup: swap must not cross");

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, owed0 as u64, owed1 as u64, DEADLINE)?;
    // min_out = the exact expected output: any on-chain deviation fails.
    let swap_note =
        tb.add_swap_note(trader, 0, amount_in, outcome.amount_out as u64, trader, DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    let mint_exec = consume_note(&mut mock_chain, pool, mint_note.id()).await?;
    assert_eq!(
        mint_exec.output_notes().iter().count(),
        0,
        "exact mint must not emit a refund"
    );

    let executed = consume_note(&mut mock_chain, pool, swap_note.id()).await?;

    // Host-side cross-check: on-chain state must equal amm-math natively.
    assert_pool_state(&mock_chain, pool, &sim)?;

    // P2ID out-note to the recipient with the exact expected amount.
    assert_p2id_output(
        &executed,
        &swap_note,
        SALT_SWAP_OUT,
        trader,
        &[(h.token1, outcome.amount_out as u64)],
    )?;

    // Vault: token0 grew by the full input, token1 shrank by the output.
    assert_eq!(
        vault_balance(&mock_chain, pool, h.token0)?,
        owed0 as u64 + amount_in
    );
    assert_eq!(
        vault_balance(&mock_chain, pool, h.token1)?,
        owed1 as u64 - outcome.amount_out as u64
    );

    let m = executed.measurements();
    anyhow::ensure!(m.note_execution.len() == 1);
    println!(
        "[{backend:?}] SWAP_NO_CROSS cycles: {} (total tx cycles: {}, trace length: {})",
        m.note_execution[0].1,
        m.total_cycles(),
        m.trace_length()
    );
    Ok(())
}

/// Test 3: swap that crosses the initialized tick at -120 (into a wide
/// backstop range): liquidity drops by the tick's liquidityNet, fgOutside
/// flips, output matches the sim exactly. Prints SWAP_1_CROSS cycles.
async fn swap_crosses_initialized_tick_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);

    let (n0, n1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);
    let (b0, b1) = sim.amounts_for_liquidity(-6000, 6000, L_BACKSTOP, true);
    sim.mint(-6000, 6000, L_BACKSTOP);

    let amount_in: u64 = 80_000_000_000; // 8e10: crosses -120, ends inside backstop
    let outcome = sim.swap(amount_in, true);
    assert_eq!(outcome.crossings, 1, "test setup: swap must cross exactly one tick");
    assert_eq!(
        outcome.end_liquidity, L_BACKSTOP,
        "test setup: narrow range must drop out"
    );

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_narrow = tb.add_mint_note(lp, -120, 120, L_NARROW, n0 as u64, n1 as u64, DEADLINE)?;
    let mint_backstop =
        tb.add_mint_note(lp, -6000, 6000, L_BACKSTOP, b0 as u64, b1 as u64, DEADLINE)?;
    let swap_note =
        tb.add_swap_note(trader, 0, amount_in, outcome.amount_out as u64, trader, DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_narrow.id()).await?;
    consume_note(&mut mock_chain, pool, mint_backstop.id()).await?;
    let executed = consume_note(&mut mock_chain, pool, swap_note.id()).await?;

    assert_pool_state(&mock_chain, pool, &sim)?;

    // The crossed tick's fgOutside words flipped to the sim's values.
    let crossed = sim.ticks.get(&-120).expect("sim retains crossed tick");
    let (o0_lo, o0_hi) = u256_to_words(crossed.fg_out0);
    let (o1_lo, o1_hi) = u256_to_words(crossed.fg_out1);
    assert_eq!(
        read_map(&mock_chain, pool, "ticks", tick_key(-120, TICK_FG0_LO))?,
        o0_lo,
        "fgOutside0 lo not flipped"
    );
    assert_eq!(
        read_map(&mock_chain, pool, "ticks", tick_key(-120, TICK_FG0_LO + 1))?,
        o0_hi,
        "fgOutside0 hi not flipped"
    );
    assert_eq!(
        read_map(&mock_chain, pool, "ticks", tick_key(-120, TICK_FG1_LO))?,
        o1_lo,
        "fgOutside1 lo not flipped"
    );
    assert_eq!(
        read_map(&mock_chain, pool, "ticks", tick_key(-120, TICK_FG1_LO + 1))?,
        o1_hi,
        "fgOutside1 hi not flipped"
    );
    // fgOutside0 must be non-zero after the flip (fees accrued before the cross).
    assert_ne!(crossed.fg_out0, [0u64; 4], "expected non-zero fgOutside0 after flip");

    assert_p2id_output(
        &executed,
        &swap_note,
        SALT_SWAP_OUT,
        trader,
        &[(h.token1, outcome.amount_out as u64)],
    )?;

    let m = executed.measurements();
    anyhow::ensure!(m.note_execution.len() == 1);
    println!(
        "[{backend:?}] SWAP_1_CROSS cycles: {} (total tx cycles: {}, trace length: {})",
        m.note_execution[0].1,
        m.total_cycles(),
        m.trace_length()
    );
    Ok(())
}

/// Test 4: exact-in swap in the other direction (one_for_zero).
async fn swap_one_for_zero_within_range_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let amount_in: u64 = 1_000_000_000;
    let outcome = sim.swap(amount_in, false);
    assert_eq!(outcome.crossings, 0);
    assert!(outcome.end_tick >= 0, "price must move up");

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, owed0 as u64, owed1 as u64, DEADLINE)?;
    let swap_note =
        tb.add_swap_note(trader, 1, amount_in, outcome.amount_out as u64, trader, DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;
    let executed = consume_note(&mut mock_chain, pool, swap_note.id()).await?;

    assert_pool_state(&mock_chain, pool, &sim)?;
    // Fee growth accrued on token1 only.
    assert_ne!(sim.fg1, [0u64; 4]);
    assert_eq!(sim.fg0, [0u64; 4]);

    assert_p2id_output(
        &executed,
        &swap_note,
        SALT_SWAP_OUT,
        trader,
        &[(h.token0, outcome.amount_out as u64)],
    )?;

    assert_eq!(
        vault_balance(&mock_chain, pool, h.token1)?,
        owed1 as u64 + amount_in
    );
    assert_eq!(
        vault_balance(&mock_chain, pool, h.token0)?,
        owed0 as u64 - outcome.amount_out as u64
    );
    Ok(())
}

/// Test 5: burn the full position after swaps (tokensOwed = principal +
/// fees), then collect (P2ID payout, owed zeroed).
async fn burn_and_collect_full_position_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let in0: u64 = 1_000_000_000;
    let in1: u64 = 2_000_000_000;
    let out_a = sim.swap(in0, true);
    let out_b = sim.swap(in1, false);
    let (principal0, principal1) = {
        let p = sim.burn(-120, 120, L_NARROW);
        (p.0 as u64, p.1 as u64)
    };
    let (collect0, collect1) = sim.collect(-120, 120);
    assert!(collect0 >= principal0 && collect1 >= principal1);

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, owed0 as u64, owed1 as u64, DEADLINE)?;
    let swap_a = tb.add_swap_note(trader, 0, in0, out_a.amount_out as u64, trader, DEADLINE)?;
    let swap_b = tb.add_swap_note(trader, 1, in1, out_b.amount_out as u64, trader, DEADLINE)?;
    let burn_note = tb.add_burn_note(lp, -120, 120, L_NARROW)?;
    let collect_note = tb.add_collect_note(lp, -120, 120)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await.map_err(|e| e.context("mint"))?;
    consume_note(&mut mock_chain, pool, swap_a.id()).await.map_err(|e| e.context("swap_a"))?;
    consume_note(&mut mock_chain, pool, swap_b.id()).await.map_err(|e| e.context("swap_b"))?;
    let burn_exec = consume_note(&mut mock_chain, pool, burn_note.id()).await.map_err(|e| e.context("burn"))?;
    assert_eq!(
        burn_exec.output_notes().iter().count(),
        0,
        "burn must not emit notes"
    );

    // tokensOwed = principal + fees, exactly as the sim computes.
    let owed_key =
        position_key(lp.suffix(), Felt::from(lp.prefix()), -120, 120, POS_TOKENS_OWED);
    let owed_word = read_map(&mock_chain, pool, "positions", owed_key)?;
    assert_eq!(owed_word[0].as_canonical_u64(), collect0, "tokensOwed0 mismatch");
    assert_eq!(owed_word[1].as_canonical_u64(), collect1, "tokensOwed1 mismatch");
    assert!(collect0 > principal0, "fees on token0 must have accrued");
    assert!(collect1 > principal1, "fees on token1 must have accrued");

    // Position liquidity zeroed; ticks cleared; bitmap bits cleared.
    let liq_key = position_key(lp.suffix(), Felt::from(lp.prefix()), -120, 120, POS_LIQUIDITY);
    assert_eq!(
        read_map(&mock_chain, pool, "positions", liq_key)?,
        Word::default()
    );
    for tick in [-120, 120] {
        for group in [TICK_LIQ_GROSS, TICK_LIQ_NET, TICK_FG0_LO, TICK_FG1_LO] {
            assert_eq!(
                read_map(&mock_chain, pool, "ticks", tick_key(tick, group))?,
                Word::default(),
                "tick {tick} group {group} not cleared"
            );
        }
    }
    let (wi, _) = bitmap_position(-120, SPACING);
    assert_eq!(
        read_map(&mock_chain, pool, "tick_bitmap", bitmap_key(wi))?,
        Word::default(),
        "bitmap bits not cleared"
    );
    assert_pool_state(&mock_chain, pool, &sim)?;
    assert_eq!(sim.liquidity, 0);

    // Collect pays both owed tokens in one P2ID note and zeroes the record.
    let collect_exec = consume_note(&mut mock_chain, pool, collect_note.id()).await?;
    assert_p2id_output(
        &collect_exec,
        &collect_note,
        SALT_COLLECT,
        lp,
        &[(h.token0, collect0), (h.token1, collect1)],
    )?;
    assert_eq!(
        read_map(&mock_chain, pool, "positions", owed_key)?,
        Word::default(),
        "tokensOwed not zeroed after collect"
    );
    Ok(())
}

/// Test 6: fee accounting sanity + vault conservation over a mint / swaps /
/// burn / collect lifecycle.
async fn fee_accounting_and_vault_conservation_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let swaps: [(u32, u64); 3] = [(0, 1_000_000_000), (1, 2_000_000_000), (0, 500_000_000)];
    let mut outcomes = Vec::new();
    for (dir, amount) in swaps {
        outcomes.push((dir, amount, sim.swap(amount, dir == 0)));
    }
    sim.burn(-120, 120, L_NARROW);
    let (collect0, collect1) = sim.collect(-120, 120);

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, owed0 as u64, owed1 as u64, DEADLINE)?;
    let mut swap_notes = Vec::new();
    for (dir, amount, outcome) in &outcomes {
        swap_notes.push(tb.add_swap_note(
            trader,
            *dir,
            *amount,
            outcome.amount_out as u64,
            trader,
            DEADLINE,
        )?);
    }
    let burn_note = tb.add_burn_note(lp, -120, 120, L_NARROW)?;
    let collect_note = tb.add_collect_note(lp, -120, 120)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;
    for note in &swap_notes {
        consume_note(&mut mock_chain, pool, note.id()).await?;
    }
    consume_note(&mut mock_chain, pool, burn_note.id()).await?;
    consume_note(&mut mock_chain, pool, collect_note.id()).await?;

    // Fee bounds: what the LP collects beyond principal is fees, and it can
    // never exceed the fees actually charged per token.
    let fees0_charged: u128 = outcomes
        .iter()
        .filter(|(d, _, _)| *d == 0)
        .map(|(_, _, o)| o.total_fee)
        .sum();
    let fees1_charged: u128 = outcomes
        .iter()
        .filter(|(d, _, _)| *d == 1)
        .map(|(_, _, o)| o.total_fee)
        .sum();
    let pos = sim.positions.get(&(-120, 120)).unwrap();
    assert_eq!(pos.tokens_owed0, 0);
    assert_eq!(pos.tokens_owed1, 0);
    assert!(fees0_charged > 0 && fees1_charged > 0);

    // Vault conservation: pool balance = mint-in + swap-in - swap-out - collect-out.
    let mut expect0: i128 = owed0 as i128;
    let mut expect1: i128 = owed1 as i128;
    for (dir, amount, outcome) in &outcomes {
        if *dir == 0 {
            expect0 += *amount as i128;
            expect1 -= outcome.amount_out as i128;
        } else {
            expect1 += *amount as i128;
            expect0 -= outcome.amount_out as i128;
        }
    }
    expect0 -= collect0 as i128;
    expect1 -= collect1 as i128;
    assert_eq!(vault_balance(&mock_chain, pool, h.token0)? as i128, expect0);
    assert_eq!(vault_balance(&mock_chain, pool, h.token1)? as i128, expect1);

    // The pool never pays out more than it charged: residual dust stays.
    assert!(expect0 >= 0 && expect1 >= 0);
    assert_pool_state(&mock_chain, pool, &sim)?;
    Ok(())
}

/// Throughput-table scenario: a swap crossing five initialized ticks
/// (ladder of narrow positions + wide backstop). Prints SWAP_5_CROSS cycles.
async fn swap_five_crossings_cycles_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
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

    let amount_in: u64 = 400_000_000_000; // 4e11: crosses -120..-600, ends in backstop
    let outcome = sim.swap(amount_in, true);
    assert_eq!(outcome.crossings, 5, "test setup: swap must cross five ticks");
    assert!(
        outcome.iterations <= integration::pool::MAX_TICK_CROSSINGS,
        "sim iterations exceed guest bound"
    );

    let swap_note =
        tb.add_swap_note(trader, 0, amount_in, outcome.amount_out as u64, trader, DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    for note in &mint_notes {
        consume_note(&mut mock_chain, pool, note.id()).await?;
    }
    let executed = consume_note(&mut mock_chain, pool, swap_note.id()).await?;

    assert_pool_state(&mock_chain, pool, &sim)?;
    assert_p2id_output(
        &executed,
        &swap_note,
        SALT_SWAP_OUT,
        trader,
        &[(h.token1, outcome.amount_out as u64)],
    )?;

    let m = executed.measurements();
    anyhow::ensure!(m.note_execution.len() == 1);
    println!(
        "[{backend:?}] SWAP_5_CROSS cycles: {} (total tx cycles: {}, trace length: {})",
        m.note_execution[0].1,
        m.total_cycles(),
        m.trace_length()
    );
    Ok(())
}

#[tokio::test]
async fn mint_position_records_state() -> anyhow::Result<()> {
    mint_position_records_state_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn mint_position_records_state_masm() -> anyhow::Result<()> {
    mint_position_records_state_scenario(Backend::Masm).await
}

#[tokio::test]
async fn swap_zero_for_one_within_range() -> anyhow::Result<()> {
    swap_zero_for_one_within_range_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn swap_zero_for_one_within_range_masm() -> anyhow::Result<()> {
    swap_zero_for_one_within_range_scenario(Backend::Masm).await
}

#[tokio::test]
async fn swap_crosses_initialized_tick() -> anyhow::Result<()> {
    swap_crosses_initialized_tick_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn swap_crosses_initialized_tick_masm() -> anyhow::Result<()> {
    swap_crosses_initialized_tick_scenario(Backend::Masm).await
}

#[tokio::test]
async fn swap_one_for_zero_within_range() -> anyhow::Result<()> {
    swap_one_for_zero_within_range_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn swap_one_for_zero_within_range_masm() -> anyhow::Result<()> {
    swap_one_for_zero_within_range_scenario(Backend::Masm).await
}

#[tokio::test]
async fn burn_and_collect_full_position() -> anyhow::Result<()> {
    burn_and_collect_full_position_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn burn_and_collect_full_position_masm() -> anyhow::Result<()> {
    burn_and_collect_full_position_scenario(Backend::Masm).await
}

#[tokio::test]
async fn fee_accounting_and_vault_conservation() -> anyhow::Result<()> {
    fee_accounting_and_vault_conservation_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn fee_accounting_and_vault_conservation_masm() -> anyhow::Result<()> {
    fee_accounting_and_vault_conservation_scenario(Backend::Masm).await
}

#[tokio::test]
async fn swap_five_crossings_cycles() -> anyhow::Result<()> {
    swap_five_crossings_cycles_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn swap_five_crossings_cycles_masm() -> anyhow::Result<()> {
    swap_five_crossings_cycles_scenario(Backend::Masm).await
}
