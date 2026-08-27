//! Tests for the four PRODUCTION AMM notes: the P2IDE-style two-path scripts
//! with reclaim/refund branching, driven through a pool whose
//! `AuthNetworkAccount` allowlist holds exactly the four note-script roots.
//! Run against BOTH backends: the Rust build (`amm-note-*` + Rust-SDK
//! basic-wallet senders) and the MASM build (kernel-read pool + MASM notes
//! whose reclaim path targets the STANDARD BasicWallet `receive_asset`, so
//! senders are plain standard wallets).
//!
//! On the Rust backend the sender wallets carry the Rust-SDK `basic-wallet`
//! component (`contracts/basic-wallet`): those notes' reclaim path is a
//! cross-context `call` against the MAST root of that package's
//! `receive_asset`, which the standard MASM BasicWallet does not expose.
//! The MASM backend needs no such component — see
//! `masm_reclaim_works_with_standard_basic_wallet`.
//!
//! Expected values are computed natively with amm-math through `PoolSim`
//! and asserted to match the on-chain result exactly, as in the Phase 2
//! suite.

use integration::pool::testbed::{
    assert_p2id_output, consume_note, read_map, read_value, try_consume, vault_balance,
    Backend, PoolTestbed, WALLET_FUND,
};
use integration::pool::{
    position_key, u128_to_word, u256_to_words, PoolSim, POS_LIQUIDITY, POS_TOKENS_OWED, TICK_OFF,
};
use miden_client::{Felt, Word};
use miden_testing::MockChain;

const FEE_PIPS: u32 = 3000;
const SPACING: i32 = 60;
const DEADLINE: u32 = 1000;
/// Short deadline used by the reclaim tests (must exceed the block height
/// reached after pool setup, and be reachable via `prove_until_block`).
const RECLAIM_DEADLINE: u32 = 15;

/// Serial-derivation salts (guest constants).
const SALT_SWAP_OUT: u32 = 0;
const SALT_SWAP_REFUND: u32 = 1;
const SALT_MINT_REFUND: u32 = 2;
const SALT_COLLECT: u32 = 3;

const L_NARROW: u128 = 1_000_000_000_000; // 1e12

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
        (sim.tick + TICK_OFF) as u64,
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
    assert_eq!(read_value(mock_chain, pool, "fee_growth_global0_lo")?, fg0_lo);
    assert_eq!(read_value(mock_chain, pool, "fee_growth_global0_hi")?, fg0_hi);
    assert_eq!(read_value(mock_chain, pool, "fee_growth_global1_lo")?, fg1_lo);
    assert_eq!(read_value(mock_chain, pool, "fee_growth_global1_hi")?, fg1_hi);
    Ok(())
}

/// Expects that consuming `note` with `account` fails; nothing is committed.
async fn expect_note_failure(
    mock_chain: &MockChain,
    account: miden_client::account::AccountId,
    note: miden_client::note::NoteId,
    label: &str,
) -> anyhow::Result<()> {
    let result = try_consume(mock_chain, account, note).await;
    anyhow::ensure!(result.is_err(), "{label}: transaction unexpectedly succeeded");
    Ok(())
}

/// Test 1: happy path through each production note (pool executes):
/// mint (with excess -> P2ID refund), swap in both directions (exact
/// min_out), burn, collect -- identical end state to the Phase 2 harness
/// equivalents, computed by the same PoolSim.
async fn production_lifecycle_happy_paths_scenario(backend: Backend) -> anyhow::Result<()> {
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
    assert!(collect0 > principal0 && collect1 > principal1, "fees must accrue");

    let excess = 1000u64;
    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(
        lp,
        -120,
        120,
        L_NARROW,
        owed0 as u64 + excess,
        owed1 as u64 + excess,
        DEADLINE,
    )?;
    let swap_a = tb.add_swap_note(trader, 0, in0, out_a.amount_out as u64, trader, DEADLINE)?;
    let swap_b = tb.add_swap_note(trader, 1, in1, out_b.amount_out as u64, trader, DEADLINE)?;
    let burn_note = tb.add_burn_note(lp, -120, 120, L_NARROW)?;
    let collect_note = tb.add_collect_note(lp, -120, 120)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    // Mint: position recorded under the sender key, excess refunded.
    let mint_exec = consume_note(&mut mock_chain, pool, mint_note.id()).await?;
    let liq_key = position_key(lp.suffix(), Felt::from(lp.prefix()), -120, 120, POS_LIQUIDITY);
    assert_eq!(
        read_map(&mock_chain, pool, "positions", liq_key)?,
        u128_to_word(L_NARROW),
        "position liquidity mismatch"
    );
    assert_p2id_output(
        &mint_exec,
        &mint_note,
        SALT_MINT_REFUND,
        lp,
        &[(h.token0, excess), (h.token1, excess)],
    )?;

    // Swaps: exact output notes + exact post-state.
    let exec_a = consume_note(&mut mock_chain, pool, swap_a.id()).await?;
    assert_p2id_output(&exec_a, &swap_a, SALT_SWAP_OUT, trader, &[(h.token1, out_a.amount_out as u64)])?;
    let exec_b = consume_note(&mut mock_chain, pool, swap_b.id()).await?;
    assert_p2id_output(&exec_b, &swap_b, SALT_SWAP_OUT, trader, &[(h.token0, out_b.amount_out as u64)])?;

    // Burn: no notes, tokensOwed = principal + fees.
    let burn_exec = consume_note(&mut mock_chain, pool, burn_note.id()).await?;
    assert_eq!(burn_exec.output_notes().iter().count(), 0, "burn must not emit notes");
    let owed_key = position_key(lp.suffix(), Felt::from(lp.prefix()), -120, 120, POS_TOKENS_OWED);
    let owed_word = read_map(&mock_chain, pool, "positions", owed_key)?;
    assert_eq!(owed_word[0].as_canonical_u64(), collect0, "tokensOwed0 mismatch");
    assert_eq!(owed_word[1].as_canonical_u64(), collect1, "tokensOwed1 mismatch");

    // Collect: single P2ID payout, owed zeroed.
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

    // Exact final pool state + vault conservation.
    assert_pool_state(&mock_chain, pool, &sim)?;
    let expect0 = owed0 as i128 + excess as i128 + in0 as i128
        - out_b.amount_out as i128
        - collect0 as i128
        - excess as i128;
    let expect1 = owed1 as i128 + excess as i128 + in1 as i128
        - out_a.amount_out as i128
        - collect1 as i128
        - excess as i128;
    assert_eq!(vault_balance(&mock_chain, pool, h.token0)? as i128, expect0);
    assert_eq!(vault_balance(&mock_chain, pool, h.token1)? as i128, expect1);
    Ok(())
}

/// Test 2: sender reclaim after the deadline. The SENDER's wallet consumes
/// the swap note past `deadline_height`: assets return to the sender's
/// vault, pool state and vault are untouched.
async fn swap_reclaim_after_deadline_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let amount_in: u64 = 1_000_000_000;
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, owed0 as u64, owed1 as u64, DEADLINE)?;
    let swap_note = tb.add_swap_note(trader, 0, amount_in, 0, trader, RECLAIM_DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;

    // Advance past the deadline, then consume WITH THE SENDER's wallet.
    mock_chain.prove_until_block(RECLAIM_DEADLINE)?;
    let executed = consume_note(&mut mock_chain, trader, swap_note.id()).await?;
    assert_eq!(executed.output_notes().iter().count(), 0, "reclaim must not emit notes");

    // Assets are back in the sender's vault.
    assert_eq!(
        vault_balance(&mock_chain, trader, h.token0)?,
        WALLET_FUND + amount_in,
        "reclaimed input asset missing from sender vault"
    );
    assert_eq!(vault_balance(&mock_chain, trader, h.token1)?, WALLET_FUND);

    // Pool untouched: state still equals the post-mint sim, vault holds
    // exactly the mint amounts.
    assert_pool_state(&mock_chain, pool, &sim)?;
    assert_eq!(vault_balance(&mock_chain, pool, h.token0)?, owed0 as u64);
    assert_eq!(vault_balance(&mock_chain, pool, h.token1)?, owed1 as u64);
    Ok(())
}

/// Test 3: reclaim BEFORE the deadline fails; the note stays consumable
/// and the same sender reclaims successfully once the deadline passes.
async fn swap_reclaim_too_early_fails_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;

    let trader = tb.trader.id();
    let amount_in: u64 = 1_000_000_000;
    let swap_note = tb.add_swap_note(trader, 0, amount_in, 0, trader, RECLAIM_DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;

    // Well before the deadline: the reclaim path must panic.
    expect_note_failure(&mock_chain, trader, swap_note.id(), "reclaim too early").await?;
    // Still unconsumed: a second early attempt fails identically.
    expect_note_failure(&mock_chain, trader, swap_note.id(), "reclaim too early retry").await?;

    // The note survives the failed attempts: past the deadline it succeeds.
    mock_chain.prove_until_block(RECLAIM_DEADLINE)?;
    consume_note(&mut mock_chain, trader, swap_note.id()).await?;
    assert_eq!(
        vault_balance(&mock_chain, trader, h.token0)?,
        WALLET_FUND + amount_in
    );
    Ok(())
}

/// Test 4: reclaim by a third account (not the sender, not the pool)
/// fails even after the deadline; the rightful sender can still reclaim.
async fn swap_reclaim_by_third_party_fails_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let amount_in: u64 = 1_000_000_000;
    let swap_note = tb.add_swap_note(trader, 0, amount_in, 0, trader, RECLAIM_DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;

    mock_chain.prove_until_block(RECLAIM_DEADLINE)?;
    // The LP (a wallet with the same component, but not the sender) tries
    // to steal the reclaim.
    expect_note_failure(&mock_chain, lp, swap_note.id(), "third-party reclaim").await?;
    assert_eq!(vault_balance(&mock_chain, lp, h.token0)?, WALLET_FUND);

    // The rightful sender still can.
    consume_note(&mut mock_chain, trader, swap_note.id()).await?;
    assert_eq!(
        vault_balance(&mock_chain, trader, h.token0)?,
        WALLET_FUND + amount_in
    );
    Ok(())
}

/// Test 5: a note whose storage pool_id names a DIFFERENT account than the
/// executing pool fails (wrong pool or unauthorized consumer) and leaves
/// the pool untouched.
async fn wrong_pool_swap_note_fails_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, owed0 as u64, owed1 as u64, DEADLINE)?;
    // Storage points at the trader's account id, not the pool's.
    let wrong_pool_note =
        tb.add_swap_note_with_pool_id(trader, trader, 0, 1_000_000_000, 0, trader, DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;
    expect_note_failure(&mock_chain, pool, wrong_pool_note.id(), "wrong-pool note").await?;

    // Pool untouched.
    assert_pool_state(&mock_chain, pool, &sim)?;
    assert_eq!(vault_balance(&mock_chain, pool, h.token0)?, owed0 as u64);
    assert_eq!(vault_balance(&mock_chain, pool, h.token1)?, owed1 as u64);
    Ok(())
}

/// Test 6: pre-deadline slippage violation through the production swap
/// note fails and leaves the note consumable; after an opposite-direction
/// swap moves the price, the IDENTICAL note succeeds (retry semantics:
/// fill-if-price-recovers).
async fn slippage_retry_succeeds_after_price_recovery_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;

    let amount_retry: u64 = 1_000_000_000; // token0 in
    let amount_bump: u64 = 5_000_000_000; // token1 in, pushes price up

    // Sim A: what the retry swap would yield at the INITIAL price.
    let out_initial = {
        let mut s = PoolSim::new(FEE_PIPS, SPACING, 0);
        s.mint(-120, 120, L_NARROW);
        s.swap(amount_retry, true).amount_out
    };

    // Main sim: mint, bump (one_for_zero), then the retry swap.
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);
    let out_bump = sim.swap(amount_bump, false);
    let out_retry = sim.swap(amount_retry, true);
    assert!(
        out_retry.amount_out > out_initial,
        "test setup: the bump swap must improve the retry swap's output"
    );

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, owed0 as u64, owed1 as u64, DEADLINE)?;
    // min_out is only achievable AFTER the bump swap executes.
    let retry_note = tb.add_swap_note(
        trader,
        0,
        amount_retry,
        out_retry.amount_out as u64,
        trader,
        DEADLINE,
    )?;
    let bump_note = tb.add_swap_note(
        trader,
        1,
        amount_bump,
        out_bump.amount_out as u64,
        trader,
        DEADLINE,
    )?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;

    // Pre-deadline slippage violation: fails, note stays unconsumed.
    expect_note_failure(&mock_chain, pool, retry_note.id(), "slippage violation").await?;
    expect_note_failure(&mock_chain, pool, retry_note.id(), "slippage violation retry").await?;

    // The opposite-direction swap moves the price...
    consume_note(&mut mock_chain, pool, bump_note.id()).await?;

    // ...and the IDENTICAL note now succeeds with the exact sim output.
    let executed = consume_note(&mut mock_chain, pool, retry_note.id()).await?;
    assert_p2id_output(
        &executed,
        &retry_note,
        SALT_SWAP_OUT,
        trader,
        &[(h.token1, out_retry.amount_out as u64)],
    )?;
    assert_pool_state(&mock_chain, pool, &sim)?;
    Ok(())
}

/// Test 7: post-deadline POOL execution of the production swap note:
/// consume-and-refund via P2ID to the sender, no swap math, pool state
/// unchanged (the hybrid failure semantics, driven through the production
/// note).
async fn expired_swap_pool_execution_refunds_sender_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let amount_in: u64 = 1_000_000_000;
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, owed0 as u64, owed1 as u64, DEADLINE)?;
    // deadline 0: expired from genesis.
    let expired_swap = tb.add_swap_note(trader, 0, amount_in, 0, trader, 0)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;
    let executed = consume_note(&mut mock_chain, pool, expired_swap.id()).await?;

    // Refund P2ID back to the SENDER with the exact input asset.
    assert_p2id_output(
        &executed,
        &expired_swap,
        SALT_SWAP_REFUND,
        trader,
        &[(h.token0, amount_in)],
    )?;

    // No swap math ran.
    assert_pool_state(&mock_chain, pool, &sim)?;
    assert_eq!(vault_balance(&mock_chain, pool, h.token0)?, owed0 as u64);
    assert_eq!(vault_balance(&mock_chain, pool, h.token1)?, owed1 as u64);
    Ok(())
}

/// Test 8: burn/collect notes consumed by the SENDER are a cleanup no-op:
/// the notes are consumed, but pool state, position, and wallet vaults are
/// untouched.
async fn burn_collect_sender_cleanup_noop_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let lp = tb.lp.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, owed0 as u64, owed1 as u64, DEADLINE)?;
    let burn_note = tb.add_burn_note(lp, -120, 120, L_NARROW)?;
    let collect_note = tb.add_collect_note(lp, -120, 120)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;

    // The LP consumes its own burn and collect notes as cleanup. The
    // consuming transactions commit (the chain accepts the nullifiers),
    // which is the consumption evidence -- MockChain does not re-check
    // nullifier spends when merely re-executing a transaction context, so
    // a "second consumption fails" probe would be meaningless here.
    let burn_exec = consume_note(&mut mock_chain, lp, burn_note.id()).await?;
    assert_eq!(burn_exec.output_notes().iter().count(), 0);
    assert_eq!(burn_exec.input_notes().num_notes(), 1, "burn note must be an input");
    let collect_exec = consume_note(&mut mock_chain, lp, collect_note.id()).await?;
    assert_eq!(collect_exec.output_notes().iter().count(), 0);
    assert_eq!(collect_exec.input_notes().num_notes(), 1, "collect note must be an input");

    // Pool + position untouched: the position still holds its liquidity,
    // nothing is owed, pool state still equals the post-mint sim.
    let liq_key = position_key(lp.suffix(), Felt::from(lp.prefix()), -120, 120, POS_LIQUIDITY);
    assert_eq!(
        read_map(&mock_chain, pool, "positions", liq_key)?,
        u128_to_word(L_NARROW),
        "position liquidity must be untouched"
    );
    let owed_key = position_key(lp.suffix(), Felt::from(lp.prefix()), -120, 120, POS_TOKENS_OWED);
    assert_eq!(read_map(&mock_chain, pool, "positions", owed_key)?, Word::default());
    assert_pool_state(&mock_chain, pool, &sim)?;
    // The LP's vault is unchanged (the no-op moved no assets).
    assert_eq!(vault_balance(&mock_chain, lp, h.token0)?, WALLET_FUND);
    assert_eq!(vault_balance(&mock_chain, lp, h.token1)?, WALLET_FUND);
    Ok(())
}

/// Test 9: network-note construction sanity. A swap note built exactly as
/// a Phase 4 network deployment would build it -- `NoteType::Public` plus
/// the `NetworkAccountTarget` attachment word
/// `[target_suffix, target_prefix, exec_hint, 0]` (scheme 2) targeting the
/// pool -- builds fine and is consumed by the pool unchanged (MockChain
/// ignores the attachment).
async fn network_swap_note_builds_and_executes_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let amount_in: u64 = 1_000_000_000;
    let outcome = sim.swap(amount_in, true);

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, owed0 as u64, owed1 as u64, DEADLINE)?;
    let net_swap = tb.add_swap_note_network(
        trader,
        0,
        amount_in,
        outcome.amount_out as u64,
        trader,
        DEADLINE,
    )?;

    // The attachment is present on the built note with the exact
    // standardized layout: scheme 2, one word
    // [target_suffix, target_prefix, exec_hint, 0].
    use miden_standards::note::{NetworkAccountTarget, NoteExecutionHint};
    let pool_id = tb.pool.id();
    let expected_attachment: miden_client::note::NoteAttachment =
        NetworkAccountTarget::new(pool_id, NoteExecutionHint::always())
            .map_err(|e| anyhow::anyhow!("NetworkAccountTarget::new: {e:?}"))?
            .into();
    let attachments = net_swap.attachments();
    assert_eq!(attachments.num_attachments(), 1, "exactly one attachment expected");
    let attachment = attachments.get(0).expect("attachment 0 must exist");
    assert_eq!(
        attachment.attachment_scheme().as_u16(),
        2,
        "NetworkAccountTarget must use attachment scheme 2"
    );
    assert_eq!(attachment, &expected_attachment, "attachment mismatch");
    assert_eq!(attachment.num_words(), 1, "attachment must be one word");
    let elems = attachment.as_elements();
    assert_eq!(elems[0], pool_id.suffix());
    assert_eq!(elems[1], Felt::from(pool_id.prefix()));
    assert_eq!(elems[2], Felt::from(NoteExecutionHint::always()));
    assert_eq!(elems[3].as_canonical_u64(), 0);

    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;
    let executed = consume_note(&mut mock_chain, pool, net_swap.id()).await?;

    assert_pool_state(&mock_chain, pool, &sim)?;
    assert_p2id_output(
        &executed,
        &net_swap,
        SALT_SWAP_OUT,
        trader,
        &[(h.token1, outcome.amount_out as u64)],
    )?;
    Ok(())
}

#[tokio::test]
async fn production_lifecycle_happy_paths() -> anyhow::Result<()> {
    production_lifecycle_happy_paths_scenario(Backend::RustProduction).await
}

#[tokio::test]
async fn production_lifecycle_happy_paths_masm() -> anyhow::Result<()> {
    production_lifecycle_happy_paths_scenario(Backend::Masm).await
}

#[tokio::test]
async fn swap_reclaim_after_deadline() -> anyhow::Result<()> {
    swap_reclaim_after_deadline_scenario(Backend::RustProduction).await
}

#[tokio::test]
async fn swap_reclaim_after_deadline_masm() -> anyhow::Result<()> {
    swap_reclaim_after_deadline_scenario(Backend::Masm).await
}

#[tokio::test]
async fn swap_reclaim_too_early_fails() -> anyhow::Result<()> {
    swap_reclaim_too_early_fails_scenario(Backend::RustProduction).await
}

#[tokio::test]
async fn swap_reclaim_too_early_fails_masm() -> anyhow::Result<()> {
    swap_reclaim_too_early_fails_scenario(Backend::Masm).await
}

#[tokio::test]
async fn swap_reclaim_by_third_party_fails() -> anyhow::Result<()> {
    swap_reclaim_by_third_party_fails_scenario(Backend::RustProduction).await
}

#[tokio::test]
async fn swap_reclaim_by_third_party_fails_masm() -> anyhow::Result<()> {
    swap_reclaim_by_third_party_fails_scenario(Backend::Masm).await
}

#[tokio::test]
async fn wrong_pool_swap_note_fails() -> anyhow::Result<()> {
    wrong_pool_swap_note_fails_scenario(Backend::RustProduction).await
}

#[tokio::test]
async fn wrong_pool_swap_note_fails_masm() -> anyhow::Result<()> {
    wrong_pool_swap_note_fails_scenario(Backend::Masm).await
}

#[tokio::test]
async fn slippage_retry_succeeds_after_price_recovery() -> anyhow::Result<()> {
    slippage_retry_succeeds_after_price_recovery_scenario(Backend::RustProduction).await
}

#[tokio::test]
async fn slippage_retry_succeeds_after_price_recovery_masm() -> anyhow::Result<()> {
    slippage_retry_succeeds_after_price_recovery_scenario(Backend::Masm).await
}

#[tokio::test]
async fn expired_swap_pool_execution_refunds_sender() -> anyhow::Result<()> {
    expired_swap_pool_execution_refunds_sender_scenario(Backend::RustProduction).await
}

#[tokio::test]
async fn expired_swap_pool_execution_refunds_sender_masm() -> anyhow::Result<()> {
    expired_swap_pool_execution_refunds_sender_scenario(Backend::Masm).await
}

#[tokio::test]
async fn burn_collect_sender_cleanup_noop() -> anyhow::Result<()> {
    burn_collect_sender_cleanup_noop_scenario(Backend::RustProduction).await
}

#[tokio::test]
async fn burn_collect_sender_cleanup_noop_masm() -> anyhow::Result<()> {
    burn_collect_sender_cleanup_noop_scenario(Backend::Masm).await
}

#[tokio::test]
async fn network_swap_note_builds_and_executes() -> anyhow::Result<()> {
    network_swap_note_builds_and_executes_scenario(Backend::RustProduction).await
}

#[tokio::test]
async fn network_swap_note_builds_and_executes_masm() -> anyhow::Result<()> {
    network_swap_note_builds_and_executes_scenario(Backend::Masm).await
}

/// MASM-port win: reclaim works for a STANDARD miden-standards BasicWallet
/// sender. The MASM testbed's wallets are created with
/// `add_existing_wallet_with_assets` (standard BasicWallet component, no
/// Rust-SDK wallet), and the MASM swap note's Path B moves the assets through
/// the standard `receive_asset` root — which the Rust production notes could
/// not serve (their reclaim `call`s the Rust-SDK wallet's root).
#[tokio::test]
async fn masm_reclaim_works_with_standard_basic_wallet() -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(Backend::Masm, FEE_PIPS, SPACING, 0)?;
    assert_eq!(tb.backend, Backend::Masm);

    let trader = tb.trader.id();
    let amount_in: u64 = 1_000_000_000;
    let swap_note = tb.add_swap_note(trader, 0, amount_in, 0, trader, RECLAIM_DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;

    // Too early: the standard wallet cannot reclaim before the deadline.
    let early = try_consume(&mock_chain, trader, swap_note.id()).await;
    anyhow::ensure!(early.is_err(), "early reclaim must fail");

    // Past the deadline the STANDARD wallet reclaims the asset.
    mock_chain.prove_until_block(RECLAIM_DEADLINE)?;
    let executed = consume_note(&mut mock_chain, trader, swap_note.id()).await?;
    assert_eq!(executed.output_notes().iter().count(), 0, "reclaim must not emit notes");
    assert_eq!(
        vault_balance(&mock_chain, trader, h.token0)?,
        WALLET_FUND + amount_in,
        "reclaimed input asset missing from standard-wallet vault"
    );
    Ok(())
}
