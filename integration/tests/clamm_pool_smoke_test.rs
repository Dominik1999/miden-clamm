//! Smoke test for the note plumbing, run against BOTH backends: an expired swap
//! against a pool with NO liquidity exercises note deserialization, asset
//! validation (argument-forwarded in the Rust build, kernel-read in the MASM
//! build), the deadline check, and refund P2ID emission in isolation from any
//! swap math.

use integration::pool::testbed::{assert_p2id_output, consume_note, Backend, PoolTestbed};

const FEE_PIPS: u32 = 3000;
const SPACING: i32 = 60;

async fn expired_swap_refund_scenario(backend: Backend) -> anyhow::Result<()> {
    let mut tb = PoolTestbed::for_backend(backend, FEE_PIPS, SPACING, 0)?;
    let trader = tb.trader.id();
    let amount_in: u64 = 1_000_000_000;
    let expired_swap = tb.add_swap_note(trader, 0, amount_in, 0, trader, 0)?;
    let (mut mock_chain, h) = tb.build()?;
    let pool = h.pool.id();

    let executed = consume_note(&mut mock_chain, pool, expired_swap.id()).await?;
    assert_p2id_output(&executed, &expired_swap, 1, trader, &[(h.token0, amount_in)])?;
    Ok(())
}

#[tokio::test]
async fn expired_swap_refund_works_without_liquidity() -> anyhow::Result<()> {
    expired_swap_refund_scenario(Backend::RustHarness).await
}

#[tokio::test]
async fn expired_swap_refund_works_without_liquidity_masm() -> anyhow::Result<()> {
    expired_swap_refund_scenario(Backend::Masm).await
}
