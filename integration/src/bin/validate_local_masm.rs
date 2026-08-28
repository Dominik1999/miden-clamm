//! MASM-port Stage 3: Phase 4 end-to-end validation re-run against a REAL
//! local Miden network (validator + sequencer + remote prover + ntx-builder)
//! with the hand-written MASM pool, proving the two things the Rust build
//! could not:
//!
//!   1. **Transaction deployment** of the pool account: the MASM component
//!      serializes to ~160KB (< the 256KiB ACCOUNT_UPDATE_MAX_SIZE), so the
//!      pool is deployed through a normal first-deployment transaction
//!      (exempt from the network-account RPC gate) instead of genesis
//!      seeding.
//!   2. **Default-budget network execution**: the ntx-builder runs at its
//!      STOCK cycle cap (2^18 = 262,144; no --max-cycles raise), and swaps
//!      ending IN-RANGE (reverse tick mapping active -- the shape that was
//!      unprovable in Rust at ~4.0M cycles) execute, prove, and land.
//!
//! Flow (every step asserts against the host-side `PoolSim` mirror):
//!   1. deploy two faucets + two STANDARD-BasicWallet users, fund both
//!   2. deploy the MASM pool BY TRANSACTION (AuthNetworkAccount allowlisting
//!      the four MASM note-script roots)
//!   3. user A mints narrow [-120,120] (excess -> P2ID refund) and a
//!      backstop [-6000,6000] via NETWORK notes consumed by the ntx-builder
//!   4. user B swaps zero_for_one crossing -120 and ending IN-RANGE
//!   5. batch: user B publishes 2 in-range swap notes in ONE tx; observe
//!      whether the ntx-builder consumes both in ONE network transaction
//!   6. user B swaps one_for_zero crossing -120 back up, ending in-range
//!   7. user A burns + collects via network notes
//!   8. adversarial swap (impossible min_amount_out, short deadline):
//!      ntx-builder retries until the deadline refund path fires
//!   9. 5-cross swap note (~280k cycles > 2^18): the ntx-builder must FAIL
//!      it on the cycle limit; at the deadline the (cheap) refund path
//!      consumes the note and returns the input
//!  10. print a measurements table (incl. per-network-tx trace lengths from
//!      the prover log; assert none exceeded 2^18 cycles)

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use clamm_pool_masm::{component, note_script, pool_library_size, PoolInitStorage, PoolNoteKind};
use integration::helpers::{setup_local_client, ClientSetup};
use integration::pool::testbed::expected_p2id_serial;
use integration::pool::{
    pool_slot, position_key, tick_felt, u128_limb_felts, u128_to_word, u256_to_words, PoolSim,
    POS_LIQUIDITY, POS_TOKENS_OWED, TICK_OFF,
};
use miden_client::account::component::{
    AuthNetworkAccount, BasicWallet, FungibleFaucet, MintPolicyConfig, PolicyRegistration,
    TokenName, TokenPolicyManager,
};
use miden_client::account::{Account, AccountBuilder, AccountComponent, AccountId, AccountType};
use miden_client::asset::{
    Asset, AssetAmount, AssetCallbackFlag, AssetVaultKey, FungibleAsset, TokenSymbol,
};
use miden_client::auth::{AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::builder::ClientBuilder;
use miden_client::crypto::RandomCoin;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::note::{Note, NoteScript, NoteTag, NoteType};
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::transaction::TransactionRequestBuilder;
use miden_client::{Client, Felt, Word};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_standards::note::{NetworkAccountTarget, NoteExecutionHint, P2idNote, P2idNoteStorage};
use miden_standards::testing::note::NoteBuilder;

// Pool parameters (identical to the Phase 2/3/4 suites).
const FEE_PIPS: u32 = 3000;
const SPACING: i32 = 60;
const INITIAL_TICK: i32 = 0;
const L_NARROW: u128 = 1_000_000_000_000; // 1e12
const L_BACKSTOP: u128 = 10_000_000_000_000; // 1e13
const MINT_EXCESS: u64 = 1000;
const ADVERSARIAL_IN: u64 = 1_000_000_000;

/// Batch-test swap inputs (distinct so the two consumption orders produce
/// distinguishable end states).
const BATCH_IN_1: u64 = 2_000_000_000;
const BATCH_IN_2: u64 = 3_000_000_000;

/// Ladder positions minted for the 5-cross check (tick -120 is re-created
/// by the first range after the narrow position was burned).
const FIVE_CROSS_LADDER: [(i32, i32); 4] = [(-240, -120), (-360, -240), (-480, -360), (-600, -480)];

/// Guest serial-derivation salts.
const SALT_SWAP_OUT: u32 = 0;
const SALT_SWAP_REFUND: u32 = 1;
const SALT_MINT_REFUND: u32 = 2;
const SALT_COLLECT: u32 = 3;

const WALLET_FUND: u64 = 10_000_000_000_000_000; // 1e16 of each token per user
const FAUCET_MAX_SUPPLY: u64 = 9_000_000_000_000_000_000;

/// The STOCK ntx-builder cycle cap (`DEFAULT_MAX_CYCLES = 1 << 18` in
/// miden-node v0.15.2 bin/ntx-builder/src/commands/mod.rs). The stack is
/// started with exactly this value -- proving default-budget viability is
/// the point of this run.
const NTX_DEFAULT_MAX_CYCLES: u64 = 1 << 18;

/// Per-operation timeout. MASM network txs are ~66k-190k cycles -> 2^17-2^18
/// traces that prove in seconds, so this is pure slack (block interval 3s,
/// ntx discovery poll, retry backoff for the failure steps).
const NTX_TIMEOUT: Duration = Duration::from_secs(900);
const POLL_INTERVAL: Duration = Duration::from_millis(1000);

type LocalClient = Client<FilesystemKeyStore>;

struct Measurements {
    rows: Vec<(String, String)>,
}

impl Measurements {
    fn add(&mut self, label: &str, value: String) {
        self.rows.push((label.to_string(), value));
    }
    fn add_secs(&mut self, label: &str, d: Duration) {
        self.add(label, format!("{:.1} s", d.as_secs_f64()));
    }
    fn print(&self) {
        println!("\n================= MEASUREMENTS (MASM pool, ntx-builder at DEFAULT 2^18 cycle cap) =================");
        for (l, v) in &self.rows {
            println!("{l:<64} {v}");
        }
        println!("====================================================================================================");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Run with cwd = the integration crate dir so every relative path
    // matches the MockChain test harness regardless of invocation dir.
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR"))
        .context("failed to enter integration crate dir")?;

    // Fresh client state on every run.
    let _ = std::fs::remove_file("../local-store.sqlite3");
    let _ = std::fs::remove_dir_all("../local-keystore");
    let _ = std::fs::remove_file("../local-store-pool-deploy.sqlite3");
    let _ = std::fs::remove_dir_all("../local-keystore-pool-deploy");

    let mut m = Measurements { rows: Vec::new() };

    // ------------------------------------------------- phase A: offline setup
    // No cargo-miden builds: the MASM pool component and its four note
    // scripts assemble in-process from contracts/clamm-pool-masm/asm/.
    println!("[setup] assembling MASM pool component + 4 note scripts...");
    let swap_script = note_script(PoolNoteKind::Swap);
    let mint_script = note_script(PoolNoteKind::Mint);
    let burn_script = note_script(PoolNoteKind::Burn);
    let collect_script = note_script(PoolNoteKind::Collect);
    println!("[setup] MASM note-script roots frozen into the pool allowlist:");
    println!("        swap:    {}", Word::from(swap_script.root()).to_hex());
    println!("        mint:    {}", Word::from(mint_script.root()).to_hex());
    println!("        burn:    {}", Word::from(burn_script.root()).to_hex());
    println!("        collect: {}", Word::from(collect_script.root()).to_hex());

    let pool_code_size = pool_library_size();
    println!(
        "[setup] pool component library size: {} B ({} KiB) vs ACCOUNT_UPDATE_MAX_SIZE 262,144 B \
         (Rust build was ~600KB -> genesis-only)",
        pool_code_size,
        pool_code_size / 1024
    );
    ensure!(
        pool_code_size < 256 * 1024,
        "pool component too large for tx deployment: {pool_code_size} B"
    );
    m.add("pool component serialized size (tx-deployability)", format!("{pool_code_size} B < 262,144 B"));

    let mut seed_rng = RandomCoin::new(Word::from([
        rand::random::<u32>(),
        rand::random::<u32>(),
        rand::random::<u32>(),
        rand::random::<u32>(),
    ]));
    let (faucet0, faucet0_key) = create_faucet(&mut seed_rng, "TKA")?;
    let (faucet1, faucet1_key) = create_faucet(&mut seed_rng, "TKB")?;
    let token0 = faucet0.id();
    let token1 = faucet1.id();
    println!("[setup] token0 faucet: {}", token0.to_hex());
    println!("[setup] token1 faucet: {}", token1.to_hex());

    // STANDARD BasicWallet users only -- one of the MASM port's wins: the
    // MASM notes' reclaim path targets the STANDARD `receive_asset` root, so
    // no Rust-SDK wallet component is needed on senders anymore.
    let (user_a, user_a_key) = create_user_wallet(&mut seed_rng)?;
    let (user_b, user_b_key) = create_user_wallet(&mut seed_rng)?;
    println!("[setup] user A (LP, standard BasicWallet):     {}", user_a.id().to_hex());
    println!("[setup] user B (trader, standard BasicWallet): {}", user_b.id().to_hex());

    // The pool is a NEW account (nonce 0, with seed): it will be deployed
    // through its first transaction, not seeded at genesis.
    let allowed: BTreeSet<_> = [
        swap_script.root(),
        mint_script.root(),
        burn_script.root(),
        collect_script.root(),
    ]
    .into_iter()
    .collect();
    let pool_account = build_pool_account(token0, token1, allowed)?;
    let pool = pool_account.id();
    println!("[setup] pool account (TO BE DEPLOYED BY TX): {}", pool.to_hex());

    // Restart the stack fresh, at the ntx-builder's DEFAULT cycle cap and
    // with the DEFAULT genesis (no pool seeding, fees 0 by default).
    println!("[setup] restarting local stack: fresh default genesis, ntx-builder --max-cycles {NTX_DEFAULT_MAX_CYCLES} (stock default)...");
    let status = Command::new("bash")
        .arg("../local-net/start-stack.sh")
        .arg("--fresh")
        .env("NTX_MAX_CYCLES", NTX_DEFAULT_MAX_CYCLES.to_string())
        .env_remove("MIDEN_GENESIS_CONFIG_FILE")
        .status()
        .context("running start-stack.sh --fresh")?;
    ensure!(status.success(), "start-stack.sh --fresh failed");

    // Log offsets: parse only this run's lines later.
    let prover_log_offset = file_len("../local-net/logs/prover.log");
    let ntx_log_offset = file_len("../local-net/logs/ntx-builder.log");

    // ------------------------------------------------- phase B: online setup
    let ClientSetup {
        mut client,
        keystore,
    } = setup_local_client().await?;

    let rpc: Arc<GrpcClient> = Arc::new(GrpcClient::new(
        &Endpoint::new("http".into(), "localhost".into(), Some(57291)),
        30_000,
    ));

    let sync = client.sync_state().await.context("initial sync failed")?;
    println!("[setup] connected to local node; chain tip: {}", sync.block_num);

    for (acct, key) in [
        (&faucet0, &faucet0_key),
        (&faucet1, &faucet1_key),
        (&user_a, &user_a_key),
        (&user_b, &user_b_key),
    ] {
        client.add_account(acct, false).await?;
        keystore.add_key(key, acct.id()).await?;
    }

    // ==================================================================
    // HEADLINE 1: deploy the pool BY TRANSACTION.
    //
    // A separate client (own store/keystore) executes the deployment so the
    // main client never tracks the pool natively (network txs update the
    // pool externally afterwards). The deployment is an EMPTY first tx: the
    // AuthNetworkAccount auth procedure passes (no notes, no tx script) and
    // increments the nonce because the account is new. First-deployment txs
    // are exempt from the RPC's network-account gate (vendor/miden-node
    // rpc/src/server/api/submit_proven_tx.rs: initial_state_commitment
    // empty => candidate exempt).
    // ==================================================================
    println!("\n=== HEADLINE: deploying MASM pool by transaction ({pool_code_size} B component) ===");
    {
        let ClientSetup {
            client: mut deploy_client,
            keystore: _deploy_keystore,
        } = setup_pool_deploy_client().await?;
        deploy_client.sync_state().await.context("deploy client sync")?;
        deploy_client
            .add_account(&pool_account, false)
            .await
            .context("adding new pool account (with seed) to deploy client")?;
        let req = TransactionRequestBuilder::new()
            .build()
            .context("building empty deployment tx request")?;
        let t_exec = Instant::now();
        let tx_result = deploy_client
            .execute_transaction(pool, req)
            .await
            .context("CRITICAL: pool first-deployment tx EXECUTION failed")?;
        let d_exec = t_exec.elapsed();
        let t_prove = Instant::now();
        let proven = deploy_client
            .prove_transaction(&tx_result)
            .await
            .context("CRITICAL: pool first-deployment tx local PROVING failed")?;
        let d_prove = t_prove.elapsed();
        let t_sub = Instant::now();
        let height = deploy_client
            .submit_proven_transaction(proven, &tx_result)
            .await
            .context("CRITICAL: pool first-deployment tx SUBMISSION rejected by the RPC")?;
        let update = deploy_client
            .get_transaction_store_update(&tx_result, height)
            .await?;
        deploy_client.apply_transaction_update(update).await?;
        let d_sub = t_sub.elapsed();
        // Wait until the deployment lands on chain.
        let (chain_pool, d_commit) = wait_pool(&rpc, pool, "pool deployment tx committed", |a| {
            Ok(a.nonce().as_canonical_u64() >= 1)
        })
        .await?;
        m.add_secs("POOL DEPLOY BY TX: execute", d_exec);
        m.add_secs("POOL DEPLOY BY TX: local prove", d_prove);
        m.add_secs("POOL DEPLOY BY TX: submit", d_sub);
        m.add_secs("POOL DEPLOY BY TX: submit -> on-chain", d_commit);

        // The deployed pool must be queryable and match the init storage.
        ensure!(
            read_value(&chain_pool, "sqrt_price")?
                == u128_to_word(amm_math::tick_math::get_sqrt_ratio_at_tick(INITIAL_TICK)),
            "deployed pool initial sqrt_price mismatch"
        );
        let ps = read_value(&chain_pool, "pool_state")?;
        ensure!(
            ps[0].as_canonical_u64() == (INITIAL_TICK + TICK_OFF) as u64,
            "deployed pool initial tick mismatch"
        );
        ensure!(
            chain_pool.nonce().as_canonical_u64() == 1,
            "deployed pool nonce must be 1, got {}",
            chain_pool.nonce().as_canonical_u64()
        );
        println!(
            "[PASS] HEADLINE: pool DEPLOYED BY TRANSACTION (nonce 1, storage matches init: tick 0, fee 3000, spacing 60) -- \
             impossible for the ~600KB Rust pool"
        );
    }

    // ------------------------------------------------- fund users
    let mint_a0 = mint_note(&mut client, token0, user_a.id())?;
    let mint_b0 = mint_note(&mut client, token0, user_b.id())?;
    submit_and_confirm(
        &mut client,
        token0,
        TransactionRequestBuilder::new()
            .own_output_notes(vec![mint_a0.clone(), mint_b0.clone()])
            .build()?,
        "faucet0 mint (deploys faucet0)",
    )
    .await?;
    let mint_a1 = mint_note(&mut client, token1, user_a.id())?;
    let mint_b1 = mint_note(&mut client, token1, user_b.id())?;
    submit_and_confirm(
        &mut client,
        token1,
        TransactionRequestBuilder::new()
            .own_output_notes(vec![mint_a1.clone(), mint_b1.clone()])
            .build()?,
        "faucet1 mint (deploys faucet1)",
    )
    .await?;
    submit_and_confirm(
        &mut client,
        user_a.id(),
        TransactionRequestBuilder::new().build_consume_notes(vec![mint_a0, mint_a1])?,
        "user A consumes faucet mints (deploys user A)",
    )
    .await?;
    submit_and_confirm(
        &mut client,
        user_b.id(),
        TransactionRequestBuilder::new().build_consume_notes(vec![mint_b0, mint_b1])?,
        "user B consumes faucet mints (deploys user B)",
    )
    .await?;
    for (user, label) in [(user_a.id(), "user A"), (user_b.id(), "user B")] {
        let acct = client
            .get_account(user)
            .await?
            .context("funded user not in store")?;
        for tok in [token0, token1] {
            ensure!(
                vault_balance(&acct, tok)? == WALLET_FUND,
                "{label} funding mismatch for {}",
                tok.to_hex()
            );
        }
    }
    println!("[PASS] both users funded with {WALLET_FUND} of each token");

    // The pool emits P2ID notes with tag 0.
    client.add_note_tag(NoteTag::from(0u32)).await?;

    // Host-side mirror + expected pool vault balances.
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, INITIAL_TICK);
    let mut note_rng = RandomCoin::new(Word::from([1u32, 2, 3, 4]));
    let mut expect_vault0: u128 = 0;
    let mut expect_vault1: u128 = 0;

    // ============================================================ step 3: MINT
    println!("\n=== Step 3: user A mints narrow position [-120,120] via network note ===");
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);
    expect_vault0 += owed0;
    expect_vault1 += owed1;
    let deadline_far = client.sync_state().await?.block_num.as_u32() + 2000;
    let mint_note_a = build_amm_note(
        mint_script,
        user_a.id(),
        pool,
        &mut note_rng,
        mint_storage(pool, -120, 120, L_NARROW, deadline_far),
        vec![
            FungibleAsset::new(token0, owed0 as u64 + MINT_EXCESS)?.into(),
            FungibleAsset::new(token1, owed1 as u64 + MINT_EXCESS)?.into(),
        ],
    )?;
    submit_and_confirm(
        &mut client,
        user_a.id(),
        TransactionRequestBuilder::new()
            .own_output_notes(vec![mint_note_a.clone()])
            .build()?,
        "publish narrow-mint network note",
    )
    .await?;
    let liq_key = position_key(
        user_a.id().suffix(),
        Felt::from(user_a.id().prefix()),
        -120,
        120,
        POS_LIQUIDITY,
    );
    let (pool_after, lat) = wait_pool(&rpc, pool, "narrow mint consumed by ntx-builder", |a| {
        Ok(read_map(a, "positions", liq_key)? != Word::default())
    })
    .await?;
    m.add_secs("MINT narrow: note committed -> pool state updated", lat);
    ensure!(
        read_map(&pool_after, "positions", liq_key)? == u128_to_word(L_NARROW),
        "position liquidity mismatch after mint"
    );
    ensure!(
        vault_balance(&pool_after, token0)? == owed0 as u64
            && vault_balance(&pool_after, token1)? == owed1 as u64,
        "pool vault should hold exactly the owed amounts after refunding excess"
    );
    println!("[PASS] position recorded (L={L_NARROW}), pool vault holds owed0={owed0} owed1={owed1}");
    claim_p2id(
        &mut client,
        user_a.id(),
        &mint_note_a,
        SALT_MINT_REFUND,
        &[(token0, MINT_EXCESS), (token1, MINT_EXCESS)],
        "mint excess refund",
    )
    .await?;

    println!("\n=== Step 3b: user A mints backstop [-6000,6000] ===");
    let (b0, b1) = sim.amounts_for_liquidity(-6000, 6000, L_BACKSTOP, true);
    sim.mint(-6000, 6000, L_BACKSTOP);
    expect_vault0 += b0;
    expect_vault1 += b1;
    let deadline_far = client.sync_state().await?.block_num.as_u32() + 2000;
    let backstop_note = build_amm_note(
        mint_script,
        user_a.id(),
        pool,
        &mut note_rng,
        mint_storage(pool, -6000, 6000, L_BACKSTOP, deadline_far),
        vec![
            FungibleAsset::new(token0, b0 as u64)?.into(),
            FungibleAsset::new(token1, b1 as u64)?.into(),
        ],
    )?;
    submit_and_confirm(
        &mut client,
        user_a.id(),
        TransactionRequestBuilder::new()
            .own_output_notes(vec![backstop_note.clone()])
            .build()?,
        "publish backstop-mint network note",
    )
    .await?;
    let backstop_key = position_key(
        user_a.id().suffix(),
        Felt::from(user_a.id().prefix()),
        -6000,
        6000,
        POS_LIQUIDITY,
    );
    let (pool_after, lat) = wait_pool(&rpc, pool, "backstop mint consumed", |a| {
        Ok(read_map(a, "positions", backstop_key)? != Word::default())
    })
    .await?;
    m.add_secs("MINT backstop: note committed -> pool state updated", lat);
    assert_pool_state(&pool_after, &sim)?;
    println!("[PASS] backstop minted; pool state matches PoolSim exactly");

    // ============================================================ step 4: SWAP A
    // zero_for_one, sized to CROSS -120 downward AND end IN-RANGE below it
    // (~ tick -180): the full 1-cross + reverse-tick-mapping shape that the
    // Rust pool could not prove on this machine (2^22 trace) and could
    // never fit in a default network tx (3.97M cycles). MASM: ~125k cycles.
    let target_a = amm_math::tick_math::get_sqrt_ratio_at_tick(-180);
    let swap_a_in = input_for_price(&sim, true, target_a, 400_000_000_000)?;
    println!("\n=== Step 4: user B swaps zero_for_one {swap_a_in} token0 (crosses -120 down, ends IN-RANGE ~tick -180) ===");
    let out_a = sim.swap(swap_a_in, true);
    ensure!(out_a.crossings == 1, "setup: swap A must cross exactly one tick, got {}", out_a.crossings);
    ensure!(
        out_a.end_tick < -120 && out_a.end_tick > -6000,
        "setup: swap A must end in-range below -120, got tick {}",
        out_a.end_tick
    );
    ensure!(
        out_a.end_sqrt_price != amm_math::tick_math::get_sqrt_ratio_at_tick(-120),
        "setup: swap A must NOT land exactly on the -120 boundary (reverse mapping must run)"
    );
    expect_vault0 += swap_a_in as u128;
    expect_vault1 -= out_a.amount_out;
    println!(
        "[sim] expected: amount_out={} end_tick={} crossings={} end_liquidity={}",
        out_a.amount_out, out_a.end_tick, out_a.crossings, out_a.end_liquidity
    );
    let deadline_far = client.sync_state().await?.block_num.as_u32() + 2000;
    let swap_note_a = build_amm_note(
        swap_script,
        user_b.id(),
        pool,
        &mut note_rng,
        swap_storage(pool, 0, out_a.amount_out as u64, user_b.id(), deadline_far),
        vec![FungibleAsset::new(token0, swap_a_in)?.into()],
    )?;

    // Timed split of the user-side pipeline (execute / prove / submit).
    let publish_req = TransactionRequestBuilder::new()
        .own_output_notes(vec![swap_note_a.clone()])
        .build()?;
    let t_exec = Instant::now();
    let tx_result = client.execute_transaction(user_b.id(), publish_req).await?;
    let d_exec = t_exec.elapsed();
    let t_prove = Instant::now();
    let proven = client.prove_transaction(&tx_result).await?;
    let d_prove = t_prove.elapsed();
    let t_sub = Instant::now();
    let height = client.submit_proven_transaction(proven, &tx_result).await?;
    let update = client.get_transaction_store_update(&tx_result, height).await?;
    client.apply_transaction_update(update).await?;
    let d_sub = t_sub.elapsed();
    m.add_secs("user-side tx: execute (swap-note publish)", d_exec);
    m.add_secs("user-side tx: LOCAL PROVE (swap-note publish)", d_prove);
    m.add_secs("user-side tx: submit + store update", d_sub);
    wait_committed(&mut client, "swap A note publish").await?;

    let expected_price = u128_to_word(sim.sqrt_price);
    let (pool_after, lat) = wait_pool(&rpc, pool, "swap A (in-range end) consumed by ntx-builder", |a| {
        Ok(read_value(a, "sqrt_price")? == expected_price)
    })
    .await?;
    m.add_secs("SWAP zero_for_one (1 cross + IN-RANGE end): committed -> consumed", lat);
    assert_pool_state(&pool_after, &sim)?;
    println!(
        "[PASS] swap A: crossed -120 down, ended IN-RANGE (tick {}, reverse mapping ran on-chain at default budget); \
         price/tick/liquidity/fee-growth match PoolSim",
        sim.tick
    );
    claim_p2id(
        &mut client,
        user_b.id(),
        &swap_note_a,
        SALT_SWAP_OUT,
        &[(token1, out_a.amount_out as u64)],
        "swap A output",
    )
    .await?;

    // ============================================================ step 4b: BATCH
    // Two small in-range (no-cross) swap notes published in ONE user tx so
    // they commit in the same block and become ntx-candidates together.
    // Observation target: does the ntx-builder consume both in ONE network
    // transaction? (2 x ~85k + ~14k kernel overhead fits the 2^18 budget.)
    println!("\n=== Step 4b: BATCH -- user B publishes 2 in-range swap notes in one tx ===");
    {
        let mut probe = sim.clone();
        let o1 = probe.swap(BATCH_IN_1, true);
        let o2 = probe.swap(BATCH_IN_2, true);
        ensure!(
            o1.crossings == 0 && o2.crossings == 0,
            "setup: batch swaps must be no-cross in-range swaps"
        );
    }
    // Both consumption orders, computed up front; the chain decides.
    let mut fork12 = sim.clone();
    let f12_o1 = fork12.swap(BATCH_IN_1, true);
    let f12_o2 = fork12.swap(BATCH_IN_2, true);
    let mut fork21 = sim.clone();
    let f21_o2 = fork21.swap(BATCH_IN_2, true);
    let f21_o1 = fork21.swap(BATCH_IN_1, true);

    let deadline_far = client.sync_state().await?.block_num.as_u32() + 2000;
    let batch_note_1 = build_amm_note(
        swap_script,
        user_b.id(),
        pool,
        &mut note_rng,
        swap_storage(pool, 0, 1, user_b.id(), deadline_far),
        vec![FungibleAsset::new(token0, BATCH_IN_1)?.into()],
    )?;
    let batch_note_2 = build_amm_note(
        swap_script,
        user_b.id(),
        pool,
        &mut note_rng,
        swap_storage(pool, 0, 1, user_b.id(), deadline_far),
        vec![FungibleAsset::new(token0, BATCH_IN_2)?.into()],
    )?;
    let nonce_before_batch = fetch_account(&rpc, pool)
        .await?
        .context("pool missing before batch")?
        .nonce()
        .as_canonical_u64();
    let batch_log_offset = file_len("../local-net/logs/ntx-builder.log");
    submit_and_confirm(
        &mut client,
        user_b.id(),
        TransactionRequestBuilder::new()
            .own_output_notes(vec![batch_note_1.clone(), batch_note_2.clone()])
            .build()?,
        "publish BOTH batch swap notes (one tx, same block)",
    )
    .await?;
    let p12 = u128_to_word(fork12.sqrt_price);
    let p21 = u128_to_word(fork21.sqrt_price);
    let (pool_after, lat) = wait_pool(&rpc, pool, "both batch swaps consumed", |a| {
        let p = read_value(a, "sqrt_price")?;
        Ok(p == p12 || p == p21)
    })
    .await?;
    m.add_secs("BATCH (2 in-range swaps): committed -> both consumed", lat);
    // The two orders often share the SAME final pool state (fees are
    // order-independent for in-range swaps), while the per-note P2ID output
    // amounts differ by rounding -- so the order is adopted from the
    // actually-emitted P2ID amount for note 1, not from the price.
    let n1_p2id = wait_for_p2id(&mut client, user_b.id(), &batch_note_1, SALT_SWAP_OUT).await?;
    let n1_amount = match n1_p2id.assets().iter().next() {
        Some(Asset::Fungible(f)) => f.amount().as_u64(),
        _ => bail!("batch note 1 P2ID has no fungible asset"),
    };
    let (out_1, out_2) = if n1_amount == f12_o1.amount_out as u64 {
        sim = fork12;
        println!("[batch] emitted P2ID amounts match order (note1, note2)");
        (f12_o1.amount_out, f12_o2.amount_out)
    } else if n1_amount == f21_o1.amount_out as u64 {
        sim = fork21;
        println!("[batch] emitted P2ID amounts match order (note2, note1)");
        (f21_o1.amount_out, f21_o2.amount_out)
    } else {
        bail!(
            "batch note 1 P2ID amount {n1_amount} matches neither order ({} / {})",
            f12_o1.amount_out,
            f21_o1.amount_out
        );
    };
    assert_pool_state(&pool_after, &sim)?;
    expect_vault0 += (BATCH_IN_1 + BATCH_IN_2) as u128;
    expect_vault1 -= out_1 + out_2;
    let nonce_after_batch = pool_after.nonce().as_canonical_u64();
    let batch_txs = nonce_after_batch - nonce_before_batch;
    // Log evidence: how many notes per network tx in the batch window.
    let batch_log = read_log_from("../local-net/logs/ntx-builder.log", batch_log_offset);
    let mut batch_note_counts: Vec<String> = Vec::new();
    for line in batch_log.lines() {
        if line.contains("executing network transaction") {
            if let Some(pos) = line.find("num_notes=") {
                let n: String = line[pos + 10..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                batch_note_counts.push(n);
            }
        }
    }
    println!(
        "[batch] pool nonce delta over batch window: {batch_txs} network tx(s); \
         ntx-builder 'executing network transaction' num_notes seen: {batch_note_counts:?}"
    );
    if batch_txs == 1 {
        println!("[PASS] BATCH: ntx-builder consumed BOTH swap notes in ONE network transaction");
    } else {
        println!(
            "[OBSERVED] BATCH: ntx-builder consumed the 2 notes in {batch_txs} separate network transactions"
        );
    }
    m.add(
        "BATCH: network txs used for the 2 notes (pool nonce delta)",
        format!("{batch_txs} (num_notes per attempt: {batch_note_counts:?})"),
    );
    claim_p2id(
        &mut client,
        user_b.id(),
        &batch_note_1,
        SALT_SWAP_OUT,
        &[(token1, out_1 as u64)],
        "batch swap 1 output",
    )
    .await?;
    claim_p2id(
        &mut client,
        user_b.id(),
        &batch_note_2,
        SALT_SWAP_OUT,
        &[(token1, out_2 as u64)],
        "batch swap 2 output",
    )
    .await?;

    // ============================================================ step 5: SWAP B
    // one_for_zero from below -120: crosses -120 upward (re-adding narrow
    // liquidity) and ends IN-RANGE near tick 0 (again: crossing + reverse
    // mapping, both previously unprovable in one default network tx).
    let target_b = amm_math::tick_math::get_sqrt_ratio_at_tick(0);
    let swap_b_in = input_for_price(&sim, false, target_b, 400_000_000_000)?;
    println!("\n=== Step 5: user B swaps one_for_zero {swap_b_in} token1 (crosses -120 up, ends IN-RANGE ~tick 0) ===");
    let out_b = sim.swap(swap_b_in, false);
    ensure!(
        out_b.crossings == 1 && out_b.end_tick > -120 && out_b.end_tick < 120,
        "setup: swap B must cross -120 upward and end inside (-120,120); got crossings={} end_tick={}",
        out_b.crossings,
        out_b.end_tick
    );
    expect_vault1 += swap_b_in as u128;
    expect_vault0 -= out_b.amount_out;
    println!(
        "[sim] expected: amount_out={} end_tick={} crossings={} end_liquidity={}",
        out_b.amount_out, out_b.end_tick, out_b.crossings, out_b.end_liquidity
    );
    let deadline_far = client.sync_state().await?.block_num.as_u32() + 2000;
    let swap_note_b = build_amm_note(
        swap_script,
        user_b.id(),
        pool,
        &mut note_rng,
        swap_storage(pool, 1, out_b.amount_out as u64, user_b.id(), deadline_far),
        vec![FungibleAsset::new(token1, swap_b_in)?.into()],
    )?;
    submit_and_confirm(
        &mut client,
        user_b.id(),
        TransactionRequestBuilder::new()
            .own_output_notes(vec![swap_note_b.clone()])
            .build()?,
        "publish swap B network note",
    )
    .await?;
    let expected_price = u128_to_word(sim.sqrt_price);
    let (pool_after, lat) = wait_pool(&rpc, pool, "swap B consumed by ntx-builder", |a| {
        Ok(read_value(a, "sqrt_price")? == expected_price)
    })
    .await?;
    m.add_secs("SWAP one_for_zero (re-cross + IN-RANGE end): committed -> consumed", lat);
    assert_pool_state(&pool_after, &sim)?;
    println!(
        "[PASS] swap B: crossed -120 upward, ended in-range at tick {}; liquidity restored to {} (narrow+backstop)",
        sim.tick, sim.liquidity
    );
    claim_p2id(
        &mut client,
        user_b.id(),
        &swap_note_b,
        SALT_SWAP_OUT,
        &[(token0, out_b.amount_out as u64)],
        "swap B output",
    )
    .await?;

    // ============================================================ step 6: BURN + COLLECT
    println!("\n=== Step 6: user A burns the narrow position, then collects ===");
    let (principal0, principal1) = sim.burn(-120, 120, L_NARROW);
    let (collect0, collect1) = sim.collect(-120, 120);
    ensure!(
        collect0 > principal0 as u64 && collect1 > principal1 as u64,
        "fees must have accrued on the narrow position"
    );
    let burn_note = build_amm_note(
        burn_script,
        user_a.id(),
        pool,
        &mut note_rng,
        vec![
            pool.suffix(),
            Felt::from(pool.prefix()),
            tick_felt(-120),
            tick_felt(120),
            u128_limb_felts(L_NARROW)[0],
            u128_limb_felts(L_NARROW)[1],
            u128_limb_felts(L_NARROW)[2],
            u128_limb_felts(L_NARROW)[3],
        ],
        vec![],
    )?;
    submit_and_confirm(
        &mut client,
        user_a.id(),
        TransactionRequestBuilder::new()
            .own_output_notes(vec![burn_note])
            .build()?,
        "publish burn network note",
    )
    .await?;
    let owed_key = position_key(
        user_a.id().suffix(),
        Felt::from(user_a.id().prefix()),
        -120,
        120,
        POS_TOKENS_OWED,
    );
    let (pool_after, lat) = wait_pool(&rpc, pool, "burn consumed by ntx-builder", |a| {
        Ok(read_map(a, "positions", liq_key)? == Word::default()
            && read_map(a, "positions", owed_key)? != Word::default())
    })
    .await?;
    m.add_secs("BURN: note committed -> consumed", lat);
    let owed_word = read_map(&pool_after, "positions", owed_key)?;
    ensure!(
        owed_word[0].as_canonical_u64() == collect0 && owed_word[1].as_canonical_u64() == collect1,
        "tokensOwed mismatch after burn: got [{}, {}], want [{collect0}, {collect1}]",
        owed_word[0].as_canonical_u64(),
        owed_word[1].as_canonical_u64()
    );
    assert_pool_state(&pool_after, &sim)?;
    println!("[PASS] burn: position liquidity zeroed, tokensOwed = principal + fees = [{collect0}, {collect1}]");

    let collect_note = build_amm_note(
        collect_script,
        user_a.id(),
        pool,
        &mut note_rng,
        vec![
            pool.suffix(),
            Felt::from(pool.prefix()),
            tick_felt(-120),
            tick_felt(120),
        ],
        vec![],
    )?;
    submit_and_confirm(
        &mut client,
        user_a.id(),
        TransactionRequestBuilder::new()
            .own_output_notes(vec![collect_note.clone()])
            .build()?,
        "publish collect network note",
    )
    .await?;
    let (pool_after, lat) = wait_pool(&rpc, pool, "collect consumed by ntx-builder", |a| {
        Ok(read_map(a, "positions", owed_key)? == Word::default())
    })
    .await?;
    m.add_secs("COLLECT: note committed -> consumed", lat);
    assert_pool_state(&pool_after, &sim)?;
    expect_vault0 -= collect0 as u128;
    expect_vault1 -= collect1 as u128;
    claim_p2id(
        &mut client,
        user_a.id(),
        &collect_note,
        SALT_COLLECT,
        &[(token0, collect0), (token1, collect1)],
        "collect payout",
    )
    .await?;
    println!("[PASS] collect: owed amounts received by user A");

    // ============================================================ step 7: adversarial
    println!("\n=== Step 7: adversarial swap (impossible min_out, short deadline) ===");
    let tip = client.sync_state().await?.block_num.as_u32();
    let short_deadline = tip + 10;
    println!("[adv] chain tip {tip}, deadline height {short_deadline} (~30s away)");
    let bad_note = build_amm_note(
        swap_script,
        user_b.id(),
        pool,
        &mut note_rng,
        swap_storage(pool, 0, u64::MAX, user_b.id(), short_deadline),
        vec![FungibleAsset::new(token0, ADVERSARIAL_IN)?.into()],
    )?;
    let sim_price_before = sim.sqrt_price;
    submit_and_confirm(
        &mut client,
        user_b.id(),
        TransactionRequestBuilder::new()
            .own_output_notes(vec![bad_note.clone()])
            .build()?,
        "publish adversarial swap note",
    )
    .await?;
    let (adv_attempts, adv_elapsed) = observe_refund(
        &mut client,
        user_b.id(),
        &bad_note,
        "adversarial (min_out=u64::MAX)",
    )
    .await?;
    ensure!(
        adv_attempts.len() >= 2,
        "expected >=2 observed failed ntx attempts before the deadline refund, saw {}",
        adv_attempts.len()
    );
    m.add(
        "ADVERSARIAL: failed attempts observed before refund",
        format!(
            "{} (blocks {:?}, deadline {})",
            adv_attempts.len(),
            adv_attempts.iter().map(|(_, b, _)| *b).collect::<Vec<_>>(),
            short_deadline
        ),
    );
    m.add_secs("ADVERSARIAL: note committed -> deadline refund consumed", adv_elapsed);

    let pool_now = fetch_account(&rpc, pool)
        .await?
        .context("pool unavailable after adversarial step")?;
    ensure!(
        read_value(&pool_now, "sqrt_price")? == u128_to_word(sim_price_before),
        "adversarial refund must not move the pool price"
    );
    assert_pool_state(&pool_now, &sim)?;
    let bal_before = user_balance(&client, user_b.id(), token0).await?;
    claim_refund(&mut client, user_b.id(), &bad_note, token0, ADVERSARIAL_IN, "adversarial refund").await?;
    let bal_after = user_balance(&client, user_b.id(), token0).await?;
    ensure!(
        bal_after - bal_before == ADVERSARIAL_IN,
        "refund must return the full input ({ADVERSARIAL_IN}), got {}",
        bal_after - bal_before
    );
    println!("[PASS] adversarial note: retried with backoff, refunded at deadline, pool untouched");

    // ============================================================ step 8: 5-CROSS
    // A swap crossing 5 initialized ticks runs ~280k cycles -- ABOVE the
    // default 2^18 cap. Expectation: every pre-deadline ntx attempt FAILS on
    // the cycle limit; at the deadline the (cheap, ~6k-cycle) refund path
    // consumes the note and returns the input. This is the designed outcome
    // that motivates the crossing-bound guidance for default-budget
    // deployments.
    println!("\n=== Step 8: 5-cross swap note (~280k cycles > 2^18 cap) must FAIL on the cycle limit ===");
    for (lower, upper) in FIVE_CROSS_LADDER {
        let (a0, a1) = sim.amounts_for_liquidity(lower, upper, L_NARROW, true);
        sim.mint(lower, upper, L_NARROW);
        expect_vault0 += a0;
        expect_vault1 += a1;
        let deadline_far = client.sync_state().await?.block_num.as_u32() + 2000;
        let mut assets: Vec<Asset> = Vec::new();
        if a0 > 0 {
            assets.push(FungibleAsset::new(token0, a0 as u64)?.into());
        }
        if a1 > 0 {
            assets.push(FungibleAsset::new(token1, a1 as u64)?.into());
        }
        let ladder_note = build_amm_note(
            mint_script,
            user_a.id(),
            pool,
            &mut note_rng,
            mint_storage(pool, lower, upper, L_NARROW, deadline_far),
            assets,
        )?;
        submit_and_confirm(
            &mut client,
            user_a.id(),
            TransactionRequestBuilder::new()
                .own_output_notes(vec![ladder_note])
                .build()?,
            &format!("publish ladder mint [{lower},{upper}]"),
        )
        .await?;
        let key = position_key(
            user_a.id().suffix(),
            Felt::from(user_a.id().prefix()),
            lower,
            upper,
            POS_LIQUIDITY,
        );
        let (_p, _lat) = wait_pool(&rpc, pool, &format!("ladder mint [{lower},{upper}] consumed"), |a| {
            Ok(read_map(a, "positions", key)? != Word::default())
        })
        .await?;
    }
    let pool_after = fetch_account(&rpc, pool).await?.context("pool missing after ladder")?;
    assert_pool_state(&pool_after, &sim)?;
    println!("[PASS] ladder minted: initialized ticks at -120,-240,-360,-480,-600 below current tick {}", sim.tick);

    // Size the swap for exactly 5 crossings (ends in the backstop-only
    // region between -600 and -6000). NOT applied to `sim`: it must never
    // execute on-chain.
    let target_5x = amm_math::tick_math::get_sqrt_ratio_at_tick(-900);
    let five_cross_in = input_for_price(&sim, true, target_5x, 2_000_000_000_000)?;
    let five_cross_expect = {
        let mut probe = sim.clone();
        let o = probe.swap(five_cross_in, true);
        ensure!(
            o.crossings == 5,
            "setup: 5-cross input must cross exactly 5 ticks, got {}",
            o.crossings
        );
        o
    };
    let tip = client.sync_state().await?.block_num.as_u32();
    let five_cross_deadline = tip + 20;
    println!(
        "[5x] input {five_cross_in} token0 (sim: 5 crossings, would end tick {}), min_out satisfiable, deadline {five_cross_deadline}",
        five_cross_expect.end_tick
    );
    let five_cross_note = build_amm_note(
        swap_script,
        user_b.id(),
        pool,
        &mut note_rng,
        swap_storage(
            pool,
            0,
            five_cross_expect.amount_out as u64,
            user_b.id(),
            five_cross_deadline,
        ),
        vec![FungibleAsset::new(token0, five_cross_in)?.into()],
    )?;
    submit_and_confirm(
        &mut client,
        user_b.id(),
        TransactionRequestBuilder::new()
            .own_output_notes(vec![five_cross_note.clone()])
            .build()?,
        "publish 5-cross swap note",
    )
    .await?;
    let (x_attempts, x_elapsed) =
        observe_refund(&mut client, user_b.id(), &five_cross_note, "5-cross (cycle-limit)").await?;
    ensure!(
        !x_attempts.is_empty(),
        "expected >=1 observed failed ntx attempt for the 5-cross note before the deadline refund"
    );
    let cycle_error = x_attempts
        .iter()
        .map(|(_, _, e)| e.as_str())
        .find(|e| !e.is_empty())
        .unwrap_or("")
        .replace('\n', " | ");
    println!("[5x] ntx-builder failure error (verbatim, truncated): {}", cycle_error.chars().take(300).collect::<String>());
    // Pool state must be untouched by the failed swap + refund.
    let pool_now = fetch_account(&rpc, pool).await?.context("pool missing after 5-cross")?;
    assert_pool_state(&pool_now, &sim)?;
    claim_refund(&mut client, user_b.id(), &five_cross_note, token0, five_cross_in, "5-cross refund").await?;
    println!(
        "[PASS] 5-cross note: {} failed attempts on the default cycle cap, deadline refund returned the full {five_cross_in} input, pool untouched",
        x_attempts.len()
    );
    m.add(
        "5-CROSS (~280k cycles > 2^18): ntx attempts before refund",
        format!(
            "{} failed (blocks {:?}); err: {}",
            x_attempts.len(),
            x_attempts.iter().map(|(_, b, _)| *b).collect::<Vec<_>>(),
            cycle_error.chars().take(120).collect::<String>()
        ),
    );
    m.add_secs("5-CROSS: note committed -> deadline refund consumed", x_elapsed);

    // ------------------------------------------------------------- vault conservation
    let pool_final = fetch_account(&rpc, pool).await?.context("pool missing at end")?;
    let final0 = vault_balance(&pool_final, token0)? as u128;
    let final1 = vault_balance(&pool_final, token1)? as u128;
    ensure!(
        final0 == expect_vault0 && final1 == expect_vault1,
        "pool vault conservation mismatch: got [{final0}, {final1}], want [{expect_vault0}, {expect_vault1}]"
    );
    println!("[PASS] pool vault conservation exact: token0={final0}, token1={final1}");
    m.add(
        "network-tx fees debited from pool vault",
        "0 (default genesis verification_base_fee = 0)".into(),
    );

    // ------------------------------------------------------------- prover-log analysis
    // Every remote-prover request in this run is a network tx (user txs
    // prove locally). Pair each "Generated execution trace" line with its
    // enclosing prove-span close to get (cycles, padded trace, prove time).
    let entries = parse_prover_log(prover_log_offset)?;
    let mut max_cycles: u64 = 0;
    println!("\n--- network-tx traces (remote prover, this run) ---");
    for (i, e) in entries.iter().enumerate() {
        max_cycles = max_cycles.max(e.cycles);
        println!(
            "  ntx #{:>2}: {:>7} cycles -> trace {:>7} steps (2^{}) -> prove {}",
            i + 1,
            e.cycles,
            e.steps,
            (63 - e.steps.leading_zeros()),
            e.busy
        );
        m.add(
            &format!("remote prover ntx #{:>2}: cycles / trace / prove time", i + 1),
            format!("{} / {} (2^{}) / {}", e.cycles, e.steps, 63 - e.steps.leading_zeros(), e.busy),
        );
    }
    ensure!(
        !entries.is_empty(),
        "no prove requests found in prover.log for this run"
    );
    ensure!(
        max_cycles <= NTX_DEFAULT_MAX_CYCLES,
        "a network tx exceeded the default cycle cap: {max_cycles} > {NTX_DEFAULT_MAX_CYCLES}"
    );
    println!("[PASS] NO network tx exceeded 2^18 = {NTX_DEFAULT_MAX_CYCLES} cycles (max observed: {max_cycles})");
    m.add(
        "max network-tx cycles observed (cap 2^18 = 262,144)",
        format!("{max_cycles}"),
    );

    // ntx-builder execute+prove+submit windows per successful network tx
    // (Phase 4 comparable: Rust mints ~9.6-9.8s, swap B ~26.0s @2^21).
    let windows = parse_ntx_windows(ntx_log_offset);
    println!("\n--- ntx-builder execute+prove+submit windows (successful network txs, this run) ---");
    for (i, (secs, notes)) in windows.iter().enumerate() {
        println!("  ntx #{:>2}: {secs:.2} s (num_notes={notes})", i + 1);
        m.add(
            &format!("ntx #{:>2} execute+prove+submit window (num_notes={notes})", i + 1),
            format!("{secs:.2} s"),
        );
    }
    if let Some(max_window) = windows.iter().map(|(s, _)| *s).fold(None::<f64>, |a, b| {
        Some(a.map_or(b, |a| a.max(b)))
    }) {
        m.add(
            "max ntx execute+prove+submit window",
            format!("{max_window:.2} s (Phase 4 Rust: 9.6-26.0 s)"),
        );
    }

    m.print();

    println!("\n--- side-by-side vs Phase 4 (Rust pool, --max-cycles 2^23, exact-boundary swaps) ---");
    println!("  op                         Rust cycles      MASM run   |  Rust latency   (see MASM rows above)");
    println!("  mint narrow                947,246          (above)    |  11.4 s");
    println!("  mint backstop              1,041,749        (above)    |  11.8 s");
    println!("  swap 1-cross               911,727 (boundary-only)     |  11.7 s   -- MASM adds the IN-RANGE end (Rust in-range: 3,983,508 cyc, UNPROVABLE on 24GB)");
    println!("  swap re-cross              1,641,179 (boundary-only)   |  29.9 s");
    println!("  burn                       715,660          (above)    |  14.9 s");
    println!("  collect                    65,770           (above)    |   3.1 s");
    println!("  deadline refund            52,407           (above)    |  39.1 s (incl. 4 retries)");
    println!("  remote prove per ntx       9.6 s @2^20 / 26.0 s @2^21  |  MASM: see per-ntx rows (expect ~1-3 s @2^17-2^18)");
    println!("  pool deployment            IMPOSSIBLE (600KB > 256KiB) |  MASM: BY TRANSACTION (this run)");
    println!("  ntx-builder cycle cap      2^23 (raised 32x)           |  MASM: 2^18 STOCK DEFAULT (this run)");

    println!("\nALL MASM STAGE-3 END-TO-END ASSERTIONS PASSED");
    Ok(())
}

// ================================================================================================
// Account creation
// ================================================================================================

fn create_faucet(rng: &mut RandomCoin, symbol: &str) -> Result<(Account, AuthSecretKey)> {
    let key_pair = AuthSecretKey::new_falcon512_poseidon2_with_rng(rng);
    let faucet_component = FungibleFaucet::builder()
        .name(TokenName::new(symbol)?)
        .symbol(TokenSymbol::new(symbol)?)
        .decimals(6)
        .max_supply(AssetAmount::new(FAUCET_MAX_SUPPLY)?)
        .build()?;
    let account = AccountBuilder::new(rand::random())
        .account_type(AccountType::Public)
        .with_auth_component(AuthSingleSig::new(
            key_pair.public_key().to_commitment(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(faucet_component)
        .with_components(
            TokenPolicyManager::new()
                .with_mint_policy(MintPolicyConfig::AllowAll, PolicyRegistration::Active)?,
        )
        .build()
        .context("building faucet account")?;
    Ok((account, key_pair))
}

/// User wallet: STANDARD BasicWallet only. The MASM notes' reclaim path
/// `call`s the standard `receive_asset` MAST root, so no Rust-SDK wallet
/// component is required (Phase 4 needed one).
fn create_user_wallet(rng: &mut RandomCoin) -> Result<(Account, AuthSecretKey)> {
    let key_pair = AuthSecretKey::new_falcon512_poseidon2_with_rng(rng);
    let account = AccountBuilder::new(rand::random())
        .account_type(AccountType::Public)
        .with_auth_component(AuthSingleSig::new(
            key_pair.public_key().to_commitment(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build()
        .context("building user wallet account")?;
    Ok((account, key_pair))
}

/// Builds the MASM pool account as a NEW account (with seed, nonce 0): it
/// is deployed through its first transaction, unlike the Phase 4 Rust pool
/// which had to be seeded at genesis (`build_existing`).
fn build_pool_account(
    token0: AccountId,
    token1: AccountId,
    allowed: BTreeSet<miden_client::note::NoteScriptRoot>,
) -> Result<Account> {
    let init = PoolInitStorage {
        pool_config: Word::new([
            token0.suffix(),
            Felt::from(token0.prefix()),
            token1.suffix(),
            Felt::from(token1.prefix()),
        ]),
        pool_params: Word::new([
            Felt::from(FEE_PIPS),
            Felt::from(SPACING as u32),
            Felt::from(0u32),
            Felt::from(0u32),
        ]),
        p2id_root: Word::from(P2idNote::script_root()),
        sqrt_price: u128_to_word(amm_math::tick_math::get_sqrt_ratio_at_tick(INITIAL_TICK)),
        pool_state: Word::new([
            Felt::from((INITIAL_TICK + TICK_OFF) as u32),
            Felt::from(1u32),
            Felt::from(0u32),
            Felt::from(0u32),
        ]),
    };
    let pool_component = component(&init);
    let auth: AccountComponent = AuthNetworkAccount::with_allowed_notes(allowed)
        .context("allowlist must be non-empty")?
        .into();
    let account = AccountBuilder::new(rand::random())
        .account_type(AccountType::Public)
        .with_component(pool_component)
        .with_auth_component(auth)
        .build()
        .context("building pool account (new, tx-deployable)")?;
    Ok(account)
}

/// Client dedicated to the pool deployment tx (own store/keystore) so the
/// main client never tracks the pool natively: network transactions update
/// the pool on-chain afterwards, which would diverge from a natively
/// tracked local state.
async fn setup_pool_deploy_client() -> Result<ClientSetup> {
    let endpoint = Endpoint::new("http".into(), "localhost".into(), Some(57291));
    let rpc_client = Arc::new(GrpcClient::new(&endpoint, 30_000));
    let keystore = Arc::new(
        FilesystemKeyStore::new(std::path::PathBuf::from("../local-keystore-pool-deploy"))
            .context("initializing pool-deploy keystore")?,
    );
    let client = ClientBuilder::new()
        .rpc(rpc_client)
        .sqlite_store(std::path::PathBuf::from("../local-store-pool-deploy.sqlite3"))
        .authenticator(keystore.clone())
        .in_debug_mode(true.into())
        .build()
        .await
        .context("building pool-deploy client")?;
    Ok(ClientSetup { client, keystore })
}

// ================================================================================================
// Notes
// ================================================================================================

fn mint_note(client: &mut LocalClient, faucet: AccountId, target: AccountId) -> Result<Note> {
    let asset = FungibleAsset::new(faucet, WALLET_FUND)?;
    Ok(P2idNote::create(
        faucet,
        target,
        vec![asset.into()],
        NoteType::Public,
        Default::default(),
        client.rng(),
    )?)
}

/// One production MASM network note: `NoteType::Public` (REQUIRED for
/// ntx-builder discovery) with the scheme-2 `NetworkAccountTarget`
/// attachment targeting the pool.
fn build_amm_note(
    script: &NoteScript,
    sender: AccountId,
    pool: AccountId,
    rng: &mut RandomCoin,
    storage: Vec<Felt>,
    assets: Vec<Asset>,
) -> Result<Note> {
    let attachment = NetworkAccountTarget::new(pool, NoteExecutionHint::always())
        .context("building NetworkAccountTarget attachment")?;
    // Tag routing: the ntx-builder discovers network notes by
    // `NoteTag::with_account_target(pool)`; without it the note is silently
    // orphaned (see testnet_smoke.rs).
    let note = NoteBuilder::new(sender, rng)
        .script(script.clone())
        .note_type(NoteType::Public)
        .tag(NoteTag::with_account_target(pool).into())
        .attachment(attachment)
        .add_assets(assets)
        .note_storage(storage)?
        .build()?;
    Ok(note)
}

fn swap_storage(
    pool: AccountId,
    direction: u32,
    min_out: u64,
    recipient: AccountId,
    deadline: u32,
) -> Vec<Felt> {
    vec![
        pool.suffix(),
        Felt::from(pool.prefix()),
        Felt::from(direction),
        Felt::from(min_out as u32),
        Felt::from((min_out >> 32) as u32),
        recipient.suffix(),
        Felt::from(recipient.prefix()),
        Felt::from(deadline),
    ]
}

fn mint_storage(pool: AccountId, lower: i32, upper: i32, liq: u128, deadline: u32) -> Vec<Felt> {
    let l = u128_limb_felts(liq);
    vec![
        pool.suffix(),
        Felt::from(pool.prefix()),
        tick_felt(lower),
        tick_felt(upper),
        l[0],
        l[1],
        l[2],
        l[3],
        Felt::from(deadline),
    ]
}

/// Smallest input whose swap end price passes `target` (binary search over
/// sim clones; the sim IS the guest algorithm, so rounding matches
/// exactly). Targets are chosen strictly inside tick ranges, so the result
/// ends in-range just past the target.
fn input_for_price(sim: &PoolSim, zero_for_one: bool, target: u128, hi_bound: u64) -> Result<u64> {
    let end_price = |input: u64| -> u128 {
        let mut probe = sim.clone();
        probe.swap(input, zero_for_one).end_sqrt_price
    };
    let passed = |p: u128| {
        if zero_for_one {
            p <= target
        } else {
            p >= target
        }
    };
    ensure!(
        passed(end_price(hi_bound)),
        "input_for_price: hi bound {hi_bound} does not reach the target price"
    );
    let (mut lo, mut hi) = (1u64, hi_bound);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if passed(end_price(mid)) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(lo)
}

// ================================================================================================
// Chain interaction helpers
// ================================================================================================

async fn submit_and_confirm(
    client: &mut LocalClient,
    account: AccountId,
    request: miden_client::transaction::TransactionRequest,
    label: &str,
) -> Result<()> {
    let t0 = Instant::now();
    let tx_id = client
        .submit_new_transaction(account, request)
        .await
        .with_context(|| format!("submitting tx: {label}"))?;
    wait_committed(client, label).await?;
    println!(
        "[tx] {label}: committed in {:.1}s (id {})",
        t0.elapsed().as_secs_f64(),
        tx_id.to_hex()
    );
    Ok(())
}

async fn wait_committed(client: &mut LocalClient, label: &str) -> Result<()> {
    use miden_client::store::TransactionFilter;
    let t0 = Instant::now();
    loop {
        if t0.elapsed() > Duration::from_secs(120) {
            bail!("tx not committed within 120s: {label}");
        }
        client.sync_state().await?;
        let pending = client
            .get_transactions(TransactionFilter::Uncommitted)
            .await?;
        if pending.is_empty() {
            return Ok(());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn fetch_account(rpc: &Arc<GrpcClient>, id: AccountId) -> Result<Option<Account>> {
    match rpc.get_account_details(id).await {
        Ok(acct) => Ok(acct),
        Err(_) => Ok(None),
    }
}

async fn wait_pool<F>(
    rpc: &Arc<GrpcClient>,
    pool: AccountId,
    what: &str,
    pred: F,
) -> Result<(Account, Duration)>
where
    F: Fn(&Account) -> Result<bool>,
{
    let t0 = Instant::now();
    loop {
        if t0.elapsed() > NTX_TIMEOUT {
            bail!("timed out waiting for: {what}");
        }
        if let Some(acct) = fetch_account(rpc, pool).await? {
            if pred(&acct)? {
                let dt = t0.elapsed();
                println!("[ntx] {what}: observed after {:.1}s", dt.as_secs_f64());
                return Ok((acct, dt));
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn read_value(account: &Account, field: &str) -> Result<Word> {
    let slot = pool_slot(field)?;
    account
        .storage()
        .get_item(&slot)
        .with_context(|| format!("reading pool value slot {field}"))
}

fn read_map(account: &Account, field: &str, key: Word) -> Result<Word> {
    let slot = pool_slot(field)?;
    account
        .storage()
        .get_map_item(&slot, key)
        .with_context(|| format!("reading pool map slot {field}"))
}

fn vault_balance(account: &Account, faucet: AccountId) -> Result<u64> {
    let key = AssetVaultKey::new_fungible(faucet, AssetCallbackFlag::default());
    Ok(account
        .vault()
        .get_balance(key)
        .context("reading vault balance")?
        .as_u64())
}

async fn user_balance(client: &LocalClient, user: AccountId, faucet: AccountId) -> Result<u64> {
    let acct = client
        .get_account(user)
        .await?
        .context("user account missing from store")?;
    vault_balance(&acct, faucet)
}

fn assert_pool_state(account: &Account, sim: &PoolSim) -> Result<()> {
    ensure!(
        read_value(account, "sqrt_price")? == u128_to_word(sim.sqrt_price),
        "sqrt_price mismatch"
    );
    let state = read_value(account, "pool_state")?;
    ensure!(
        state[0].as_canonical_u64() == (sim.tick + TICK_OFF) as u64,
        "current tick mismatch: got {} want {}",
        state[0].as_canonical_u64() as i64 - TICK_OFF as i64,
        sim.tick
    );
    ensure!(
        read_value(account, "liquidity")? == u128_to_word(sim.liquidity),
        "active liquidity mismatch"
    );
    let (fg0_lo, fg0_hi) = u256_to_words(sim.fg0);
    let (fg1_lo, fg1_hi) = u256_to_words(sim.fg1);
    ensure!(read_value(account, "fee_growth_global0_lo")? == fg0_lo, "fg0 lo mismatch");
    ensure!(read_value(account, "fee_growth_global0_hi")? == fg0_hi, "fg0 hi mismatch");
    ensure!(read_value(account, "fee_growth_global1_lo")? == fg1_lo, "fg1 lo mismatch");
    ensure!(read_value(account, "fee_growth_global1_hi")? == fg1_hi, "fg1 hi mismatch");
    Ok(())
}

async fn find_note_by_recipient(
    client: &LocalClient,
    user: AccountId,
    recipient_digest: Word,
) -> Result<Option<Note>> {
    for (record, _) in client.get_consumable_notes(Some(user)).await? {
        if record.recipient() == recipient_digest {
            let note: Note = record
                .try_into()
                .map_err(|e| anyhow::anyhow!("note record conversion: {e:?}"))?;
            return Ok(Some(note));
        }
    }
    Ok(None)
}

/// Waits for a pool-emitted P2ID note (derived from `input_note` + `salt`)
/// to appear for `user` and returns it WITHOUT consuming it.
async fn wait_for_p2id(
    client: &mut LocalClient,
    user: AccountId,
    input_note: &Note,
    salt: u32,
) -> Result<Note> {
    let serial = expected_p2id_serial(input_note, salt);
    let recipient = P2idNoteStorage::new(user).into_recipient(serial);
    let digest = recipient.digest();
    let t0 = Instant::now();
    loop {
        if t0.elapsed() > Duration::from_secs(180) {
            bail!("expected P2ID note (recipient {digest:?}) never appeared for {user:?}");
        }
        client.sync_state().await?;
        if let Some(note) = find_note_by_recipient(client, user, digest).await? {
            return Ok(note);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Waits for a pool-emitted P2ID note (derived from `input_note` + `salt`),
/// verifies its assets exactly, consumes it with `user`, and verifies the
/// vault deltas.
async fn claim_p2id(
    client: &mut LocalClient,
    user: AccountId,
    input_note: &Note,
    salt: u32,
    expected_assets: &[(AccountId, u64)],
    label: &str,
) -> Result<()> {
    let serial = expected_p2id_serial(input_note, salt);
    let recipient = P2idNoteStorage::new(user).into_recipient(serial);
    let digest = recipient.digest();
    let t0 = Instant::now();
    let note = loop {
        if t0.elapsed() > Duration::from_secs(180) {
            bail!("{label}: expected P2ID note (recipient {digest:?}) never appeared for {user:?}");
        }
        client.sync_state().await?;
        if let Some(note) = find_note_by_recipient(client, user, digest).await? {
            break note;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    let mut got: Vec<(AccountId, u64)> = note
        .assets()
        .iter()
        .map(|a| match a {
            Asset::Fungible(f) => (f.faucet_id(), f.amount().as_u64()),
            _ => panic!("unexpected non-fungible asset on P2ID note"),
        })
        .collect();
    got.sort_by_key(|(id, _)| id.prefix().as_u64());
    let mut want = expected_assets.to_vec();
    want.sort_by_key(|(id, _)| id.prefix().as_u64());
    ensure!(got == want, "{label}: P2ID assets mismatch: got {got:?}, want {want:?}");

    let mut before = Vec::new();
    for (faucet, _) in &want {
        before.push(user_balance(client, user, *faucet).await?);
    }
    submit_and_confirm(
        client,
        user,
        TransactionRequestBuilder::new().build_consume_notes(vec![note])?,
        &format!("consume {label} P2ID note"),
    )
    .await?;
    for (i, (faucet, amount)) in want.iter().enumerate() {
        let after = user_balance(client, user, *faucet).await?;
        ensure!(
            after - before[i] == *amount,
            "{label}: vault delta mismatch for {}: got {}, want {amount}",
            faucet.to_hex(),
            after - before[i]
        );
    }
    println!("[PASS] {label}: P2ID note received and consumed; assets exact {want:?}");
    Ok(())
}

/// Consumes the SALT_SWAP_REFUND P2ID emitted when the pool deadline-refunds
/// a swap note, verifying the exact refunded asset.
async fn claim_refund(
    client: &mut LocalClient,
    user: AccountId,
    input_note: &Note,
    faucet: AccountId,
    amount: u64,
    label: &str,
) -> Result<()> {
    claim_p2id(client, user, input_note, SALT_SWAP_REFUND, &[(faucet, amount)], label).await
}

/// Polls the ntx-builder's SQLite retry state for `input_note` while waiting
/// for the pool's deadline-refund P2ID to appear for `user`. Returns the
/// observed failed attempts `(attempt_count, last_attempt_block, error)` and
/// the elapsed time. The refund note itself is NOT consumed here.
async fn observe_refund(
    client: &mut LocalClient,
    user: AccountId,
    input_note: &Note,
    label: &str,
) -> Result<(Vec<(u32, i64, String)>, Duration)> {
    let note_id_hex = input_note.id().to_hex();
    let mut attempts_seen: Vec<(u32, i64, String)> = Vec::new();
    let t0 = Instant::now();
    let refund_serial = expected_p2id_serial(input_note, SALT_SWAP_REFUND);
    let refund_recipient = P2idNoteStorage::new(user).into_recipient(refund_serial);
    loop {
        if t0.elapsed() > NTX_TIMEOUT {
            bail!("{label}: note was not refund-consumed within timeout; attempts: {attempts_seen:?}");
        }
        if let Some((count, last, err)) = query_ntx_note(&note_id_hex)? {
            if count > 0 && attempts_seen.last().map(|(c, _, _)| *c) != Some(count) {
                println!(
                    "[{label}] t+{:>5.1}s attempt_count={count} last_attempt_block={last} err={}",
                    t0.elapsed().as_secs_f64(),
                    err.chars().take(160).collect::<String>()
                );
                attempts_seen.push((count, last, err));
            }
        }
        client.sync_state().await?;
        if find_note_by_recipient(client, user, refund_recipient.digest())
            .await?
            .is_some()
        {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let elapsed = t0.elapsed();
    println!(
        "[{label}] refund note appeared after {:.1}s; failed attempts: {:?}",
        elapsed.as_secs_f64(),
        attempts_seen
            .iter()
            .map(|(c, b, _)| format!("#{c}@block{b}"))
            .collect::<Vec<_>>()
    );
    Ok((attempts_seen, elapsed))
}

// ================================================================================================
// ntx-builder / prover observation
// ================================================================================================

/// Queries the ntx-builder's SQLite for the tracked note's retry state:
/// (attempt_count, last_attempt_block, last_error).
fn query_ntx_note(note_id_hex: &str) -> Result<Option<(u32, i64, String)>> {
    let db = Path::new("../local-node-data/ntx-builder/ntx-builder.sqlite3");
    if !db.exists() {
        return Ok(None);
    }
    let query = format!(
        "select attempt_count, coalesce(last_attempt, -1), coalesce(last_error,'') \
         from notes where lower(hex(note_id)) = '{id}' \
         union all \
         select attempt_count, coalesce(last_attempt, -1), coalesce(last_error,'') \
         from notes where committed_at is null \
           and not exists (select 1 from notes where lower(hex(note_id)) = '{id}') \
         limit 1;",
        id = note_id_hex.trim_start_matches("0x").to_lowercase()
    );
    let out = Command::new("sqlite3")
        .arg(db)
        .arg("-separator")
        .arg("|")
        .arg(&query)
        .output()
        .context("running sqlite3 against ntx-builder db")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let mut parts = line.splitn(3, '|');
    let count: u32 = parts.next().unwrap_or("0").trim().parse().unwrap_or(0);
    let last: i64 = parts.next().unwrap_or("-1").trim().parse().unwrap_or(-1);
    let err = parts.next().unwrap_or("").trim().to_string();
    Ok(Some((count, last, err)))
}

fn file_len(path: &str) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Reads a log file from a byte offset, ANSI-stripped.
fn read_log_from(path: &str, offset: u64) -> String {
    let data = std::fs::read(path).unwrap_or_default();
    let slice = if (offset as usize) < data.len() {
        &data[offset as usize..]
    } else {
        &[]
    };
    strip_ansi(&String::from_utf8_lossy(slice))
}

struct ProveEntry {
    /// Actual trace rows before padding ("padded from N") ~= VM cycles.
    cycles: u64,
    /// Padded trace steps (power of two).
    steps: u64,
    /// The prove request span's time.busy when the log format carries span
    /// closes; the trace-line -> next-trace-line lower bound otherwise.
    busy: String,
}

/// Parses this run's remote-prover log region. Every prove request logs
/// "Generated execution trace of C columns and S steps (padded from N)";
/// depending on the tracing configuration the log may or may not also carry
/// span-close lines with time.busy, so proving time is taken from the close
/// span when present and reported as "n/a" otherwise (the ntx-builder
/// windows below carry the timing in that case).
fn parse_prover_log(offset: u64) -> Result<Vec<ProveEntry>> {
    let log = read_log_from("../local-net/logs/prover.log", offset);
    let mut entries: Vec<ProveEntry> = Vec::new();
    let mut busy_queue: std::collections::VecDeque<String> = Default::default();
    for line in log.lines() {
        if line.contains("Generated execution trace of") {
            // "... and 262144 steps (padded from 190123)"
            let steps = line
                .split(" and ")
                .nth(1)
                .and_then(|s| s.split(' ').next())
                .and_then(|s| s.parse::<u64>().ok());
            let cycles = line
                .split("padded from ")
                .nth(1)
                .and_then(|s| s.split(')').next())
                .and_then(|s| s.trim().parse::<u64>().ok());
            if let (Some(steps), Some(cycles)) = (steps, cycles) {
                entries.push(ProveEntry {
                    cycles,
                    steps,
                    busy: "n/a".into(),
                });
            }
        } else if line.contains("server::prover")
            && line.contains("close")
            && line.contains("time.busy:")
        {
            let busy = line
                .split("time.busy:")
                .nth(1)
                .map(|s| s.split(',').next().unwrap_or("").trim().to_string())
                .unwrap_or_default();
            busy_queue.push_back(busy);
        }
    }
    // Pair close spans (if any) with trace lines in order.
    for (entry, busy) in entries.iter_mut().zip(busy_queue) {
        entry.busy = busy;
    }
    Ok(entries)
}

/// Seconds-since-midnight from a log timestamp "2026-08-27T11:20:48.439655Z".
fn log_ts_secs(line: &str) -> Option<f64> {
    let t = line.split('T').nth(1)?;
    let hh: f64 = t.get(0..2)?.parse().ok()?;
    let mm: f64 = t.get(3..5)?.parse().ok()?;
    let ss: f64 = t.get(6..15)?.trim_end_matches('Z').parse().ok()?;
    Some(hh * 3600.0 + mm * 60.0 + ss)
}

/// Per-successful-network-tx execute+prove+submit windows from the
/// ntx-builder log region: the gap between each "executing network
/// transaction" line and the following "network transaction executed" line
/// (failed attempts log "executing" without a completion and are skipped by
/// keeping only the last "executing" before each completion).
fn parse_ntx_windows(offset: u64) -> Vec<(f64, usize)> {
    let log = read_log_from("../local-net/logs/ntx-builder.log", offset);
    let mut windows = Vec::new();
    let mut last_exec: Option<(f64, usize)> = None;
    for line in log.lines() {
        if line.contains("executing network transaction") {
            let num_notes = line
                .find("num_notes=")
                .map(|pos| {
                    line[pos + 10..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>()
                        .parse::<usize>()
                        .unwrap_or(1)
                })
                .unwrap_or(1);
            if let Some(ts) = log_ts_secs(line) {
                last_exec = Some((ts, num_notes));
            }
        } else if line.contains("network transaction executed") {
            if let (Some((t0, n)), Some(t1)) = (last_exec.take(), log_ts_secs(line)) {
                windows.push((t1 - t0, n));
            }
        }
    }
    windows
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            while let Some(&n) = chars.peek() {
                chars.next();
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
