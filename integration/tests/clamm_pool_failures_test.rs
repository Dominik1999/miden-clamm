//! Failure-path tests for the clamm-pool component (MockChain), run against BOTH
//! backends. In the MASM build the wrong-faucet rejection originates in the pool's
//! kernel-read asset validation rather than argument checking — the observable
//! outcome (tx fails, state untouched) is identical.
//!
//! Failed transactions never touch state, so each test asserts (a) the
//! execution errors and (b) the committed pool state is untouched where a
//! subsequent read is meaningful. The post-deadline swap (test 10) is the
//! one non-panicking path: the note IS consumed and a refund P2ID note is
//! emitted with no pool-state change.

use integration::pool::testbed::{
    assert_p2id_output, consume_note, read_value, vault_balance, Backend, PoolTestbed,
};
use integration::pool::{u128_to_word, PoolSim};
use miden_client::auth::AuthSchemeId;
use miden_testing::{Auth, MockChain};

const FEE_PIPS: u32 = 3000;
const SPACING: i32 = 60;
const DEADLINE: u32 = 1000;
const SALT_SWAP_REFUND: u32 = 1;

const L_NARROW: u128 = 1_000_000_000_000;
const L_BACKSTOP: u128 = 10_000_000_000_000;

/// Executes a note against the pool WITHOUT committing, expecting failure.
async fn expect_note_failure(
    mock_chain: &mut MockChain,
    pool: miden_client::account::AccountId,
    note: miden_client::note::NoteId,
    label: &str,
) -> anyhow::Result<()> {
    let result = mock_chain
        .build_tx_context(pool, &[note], &[])?
        .build()?
        .execute()
        .await;
    anyhow::ensure!(
        result.is_err(),
        "{label}: transaction unexpectedly succeeded"
    );
    Ok(())
}

/// Asserts the pool still sits at the initial tick-0 price.
fn assert_price_at_tick_zero(
    mock_chain: &MockChain,
    pool: miden_client::account::AccountId,
) -> anyhow::Result<()> {
    assert_eq!(
        read_value(mock_chain, pool, "sqrt_price")?,
        u128_to_word(amm_math::tick_math::get_sqrt_ratio_at_tick(0)),
        "pool price must be untouched"
    );
    Ok(())
}

/// Test 7: a swap note carrying an asset from a foreign faucet fails and
/// leaves the pool untouched.
async fn swap_with_wrong_faucet_fails_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (a0, a1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    // A third faucet unknown to the pool config.
    let evil_faucet = tb
        .builder
        .add_existing_basic_faucet(
            Auth::BasicAuth {
                auth_scheme: AuthSchemeId::Falcon512Poseidon2,
            },
            "EVL",
            9_000_000_000_000_000_000,
            None,
        )?
        .id();

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, a0 as u64, a1 as u64, DEADLINE)?;
    let evil_note = tb.add_swap_note_with_asset(
        trader,
        0,
        evil_faucet,
        1_000_000_000,
        0,
        trader,
        DEADLINE,
    )?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;
    expect_note_failure(&mut mock_chain, pool, evil_note.id(), "wrong-faucet swap").await?;

    // State untouched: price still at tick 0, vault holds only mint amounts.
    assert_price_at_tick_zero(&mock_chain, pool)?;
    assert_eq!(vault_balance(&mock_chain, pool, h.token0)?, a0 as u64);
    assert_eq!(vault_balance(&mock_chain, pool, h.token1)?, a1 as u64);
    Ok(())
}

/// Test 8a: burn from a non-owner sender fails (position key derives from
/// the kernel-read sender; the attacker addresses an empty position).
async fn burn_from_non_owner_fails_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (a0, a1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, a0 as u64, a1 as u64, DEADLINE)?;
    // The trader (not the LP) tries to burn the LP's position.
    let attack_burn = tb.add_burn_note(trader, -120, 120, L_NARROW)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;
    expect_note_failure(&mut mock_chain, pool, attack_burn.id(), "non-owner burn").await?;
    Ok(())
}

/// Test 8b: collect from a non-owner sender fails ("nothing to collect"
/// for the sender-derived position key).
async fn collect_from_non_owner_fails_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (a0, a1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);
    sim.burn(-120, 120, L_NARROW); // LP's tokensOwed is now non-zero

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, a0 as u64, a1 as u64, DEADLINE)?;
    let burn_note = tb.add_burn_note(lp, -120, 120, L_NARROW)?;
    // The trader (not the LP) tries to collect the LP's tokensOwed.
    let attack_collect = tb.add_collect_note(trader, -120, 120)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;
    consume_note(&mut mock_chain, pool, burn_note.id()).await?;
    expect_note_failure(&mut mock_chain, pool, attack_collect.id(), "non-owner collect").await?;
    Ok(())
}

/// Test 9: pre-deadline slippage violation (min_out unreachable) panics;
/// the note stays unconsumed and the pool untouched.
async fn slippage_violation_fails_before_deadline_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (a0, a1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, a0 as u64, a1 as u64, DEADLINE)?;
    // min_out = u64::MAX can never be met.
    let greedy_swap =
        tb.add_swap_note(trader, 0, 1_000_000_000, u64::MAX, trader, DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_note.id()).await?;
    expect_note_failure(&mut mock_chain, pool, greedy_swap.id(), "slippage violation").await?;

    // Note unconsumed: a second attempt fails the same way (it would be
    // gone if the first attempt had consumed it).
    expect_note_failure(&mut mock_chain, pool, greedy_swap.id(), "slippage retry").await?;
    assert_price_at_tick_zero(&mock_chain, pool)?;
    assert_eq!(vault_balance(&mock_chain, pool, h.token0)?, a0 as u64);
    Ok(())
}

/// Test 10: post-deadline swap note IS consumed: input refunded to the
/// sender via P2ID, no swap math, pool price unchanged.
async fn expired_swap_refunds_sender_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (a0, a1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let amount_in: u64 = 1_000_000_000;
    let mint_note = tb.add_mint_note(lp, -120, 120, L_NARROW, a0 as u64, a1 as u64, DEADLINE)?;
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

    // No swap math ran: price, fee growth, and vault are unchanged.
    assert_price_at_tick_zero(&mock_chain, pool)?;
    assert_eq!(
        read_value(&mock_chain, pool, "fee_growth_global0_lo")?,
        miden_client::Word::default()
    );
    assert_eq!(vault_balance(&mock_chain, pool, h.token0)?, a0 as u64);
    assert_eq!(vault_balance(&mock_chain, pool, h.token1)?, a1 as u64);
    Ok(())
}

/// Test 11: mint with a non-spacing-aligned or out-of-range tick fails.
async fn mint_with_bad_ticks_fails_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;

    let lp = tb.lp.id();
    // -100 is not a multiple of 60.
    let unaligned = tb.add_mint_note(lp, -100, 120, L_NARROW, 1_000_000, 1_000_000, DEADLINE)?;
    // -443_640 is aligned (60 * 7394) but below MIN_TICK = -443_636.
    let out_of_range =
        tb.add_mint_note(lp, -443_640, 120, L_NARROW, 1_000_000, 1_000_000, DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    expect_note_failure(&mut mock_chain, pool, unaligned.id(), "unaligned tick mint").await?;
    expect_note_failure(&mut mock_chain, pool, out_of_range.id(), "out-of-range tick mint")
        .await?;
    assert_price_at_tick_zero(&mock_chain, pool)?;
    Ok(())
}

/// Test 12: a swap that would exceed MAX_TICK_CROSSINGS iterations fails.
async fn swap_exceeding_max_crossings_fails_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, 0);
    let (n0, n1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);
    let (b0, b1) = sim.amounts_for_liquidity(-6000, 6000, L_BACKSTOP, true);
    sim.mint(-6000, 6000, L_BACKSTOP);

    let lp = tb.lp.id();
    let trader = tb.trader.id();
    let mint_narrow = tb.add_mint_note(lp, -120, 120, L_NARROW, n0 as u64, n1 as u64, DEADLINE)?;
    let mint_backstop =
        tb.add_mint_note(lp, -6000, 6000, L_BACKSTOP, b0 as u64, b1 as u64, DEADLINE)?;
    // 1e15 token0 drains everything below tick 0 and then hops empty
    // bitmap words until the iteration bound trips.
    let monster_swap = tb.add_swap_note(trader, 0, 1_000_000_000_000_000, 0, trader, DEADLINE)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    consume_note(&mut mock_chain, pool, mint_narrow.id()).await?;
    consume_note(&mut mock_chain, pool, mint_backstop.id()).await?;

    // The sim confirms this input exceeds the iteration bound.
    let sim_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sim.swap(1_000_000_000_000_000, true)
    }));
    assert!(sim_result.is_err(), "sim must also hit the crossing bound");

    expect_note_failure(&mut mock_chain, pool, monster_swap.id(), "max-crossings swap").await?;
    assert_eq!(vault_balance(&mock_chain, pool, h.token0)?, (n0 + b0) as u64);
    Ok(())
}

#[tokio::test]
async fn swap_with_wrong_faucet_fails() -> anyhow::Result<()> {
    swap_with_wrong_faucet_fails_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn swap_with_wrong_faucet_fails_masm() -> anyhow::Result<()> {
    swap_with_wrong_faucet_fails_scenario(Backend::Masm).await
}

#[tokio::test]
async fn burn_from_non_owner_fails() -> anyhow::Result<()> {
    burn_from_non_owner_fails_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn burn_from_non_owner_fails_masm() -> anyhow::Result<()> {
    burn_from_non_owner_fails_scenario(Backend::Masm).await
}

#[tokio::test]
async fn collect_from_non_owner_fails() -> anyhow::Result<()> {
    collect_from_non_owner_fails_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn collect_from_non_owner_fails_masm() -> anyhow::Result<()> {
    collect_from_non_owner_fails_scenario(Backend::Masm).await
}

#[tokio::test]
async fn slippage_violation_fails_before_deadline() -> anyhow::Result<()> {
    slippage_violation_fails_before_deadline_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn slippage_violation_fails_before_deadline_masm() -> anyhow::Result<()> {
    slippage_violation_fails_before_deadline_scenario(Backend::Masm).await
}

#[tokio::test]
async fn expired_swap_refunds_sender() -> anyhow::Result<()> {
    expired_swap_refunds_sender_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn expired_swap_refunds_sender_masm() -> anyhow::Result<()> {
    expired_swap_refunds_sender_scenario(Backend::Masm).await
}

#[tokio::test]
async fn mint_with_bad_ticks_fails() -> anyhow::Result<()> {
    mint_with_bad_ticks_fails_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn mint_with_bad_ticks_fails_masm() -> anyhow::Result<()> {
    mint_with_bad_ticks_fails_scenario(Backend::Masm).await
}

#[tokio::test]
async fn swap_exceeding_max_crossings_fails() -> anyhow::Result<()> {
    swap_exceeding_max_crossings_fails_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn swap_exceeding_max_crossings_fails_masm() -> anyhow::Result<()> {
    swap_exceeding_max_crossings_fails_scenario(Backend::Masm).await
}
