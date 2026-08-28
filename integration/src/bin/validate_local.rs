//! Phase 4 end-to-end validation of the CLAMM pool against a REAL local
//! Miden network (validator + sequencer + remote prover + ntx-builder),
//! including network-transaction consumption of the production AMM notes
//! by the ntx-builder.
//!
//! Prerequisites: `local-net/start-stack.sh` running (RPC on :57291,
//! ntx-builder `--max-cycles 8388608`).
//!
//! Flow (every step asserts against the host-side `PoolSim` mirror):
//!   1. deploy two faucets, fund two Rust-SDK-wallet users
//!   2. deploy the pool (public account, `AuthNetworkAccount` allowlisting
//!      the four production note-script roots) via its first (empty) tx
//!   3. user A mints narrow [-120,120] (excess -> P2ID refund) and a
//!      backstop [-6000,6000] via NETWORK notes consumed by the ntx-builder
//!   4. user B swaps zero_for_one (crosses -120 downward)
//!   5. user B swaps one_for_zero sized to cross -120 back upward
//!   6. user A burns + collects via network notes
//!   7. adversarial swap (impossible min_amount_out, short deadline):
//!      observe ntx-builder retries until the deadline refund path fires
//!   8. print a measurements table

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use integration::helpers::{setup_local_client, ClientSetup};
use integration::pool::testbed::{build_production_packages, expected_p2id_serial, PoolPackages};
use integration::pool::{
    pool_slot, position_key, tick_felt, u128_limb_felts, u128_to_word, u256_to_words, PoolSim,
    POS_LIQUIDITY, POS_TOKENS_OWED, TICK_OFF,
};
use miden_client::account::component::{
    AuthNetworkAccount, BasicWallet, FungibleFaucet, InitStorageData, MintPolicyConfig,
    PolicyRegistration, StorageValueName, TokenName, TokenPolicyManager,
};
use miden_client::account::{Account, AccountBuilder, AccountComponent, AccountId, AccountType};
use miden_client::asset::{Asset, AssetAmount, AssetCallbackFlag, AssetVaultKey, FungibleAsset, TokenSymbol};
use miden_client::auth::{AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::crypto::RandomCoin;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::note::{Note, NoteTag, NoteType};
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::transaction::TransactionRequestBuilder;
use miden_client::{Client, Felt, Word};
use miden_standards::note::{NetworkAccountTarget, NoteExecutionHint, P2idNote, P2idNoteStorage};
use miden_standards::testing::note::NoteBuilder;

// Pool parameters (identical to the Phase 2/3 MockChain suites).
const FEE_PIPS: u32 = 3000;
const SPACING: i32 = 60;
const INITIAL_TICK: i32 = 0;
const L_NARROW: u128 = 1_000_000_000_000; // 1e12
const L_BACKSTOP: u128 = 10_000_000_000_000; // 1e13
const MINT_EXCESS: u64 = 1000;
const ADVERSARIAL_IN: u64 = 1_000_000_000;

/// Guest serial-derivation salts.
const SALT_SWAP_OUT: u32 = 0;
const SALT_SWAP_REFUND: u32 = 1;
const SALT_MINT_REFUND: u32 = 2;
const SALT_COLLECT: u32 = 3;

const WALLET_FUND: u64 = 10_000_000_000_000_000; // 1e16 of each token per user
const FAUCET_MAX_SUPPLY: u64 = 9_000_000_000_000_000_000;

/// Generous per-operation timeout: a 3.2M-7.0M cycle network tx must be
/// executed AND STARK-proven by the remote prover before landing. Measured
/// on this machine, the 1-cross swap proof alone runs tens of minutes.
const NTX_TIMEOUT: Duration = Duration::from_secs(10_800);
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
        println!("\n================= MEASUREMENTS =================");
        for (l, v) in &self.rows {
            println!("{l:<58} {v}");
        }
        println!("================================================");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Run with cwd = the integration crate dir so every relative path
    // (contracts, store, keystore, local-net logs) matches the MockChain
    // test harness, regardless of where cargo was invoked from. This also
    // keeps `cargo miden build` running inside project-template (the .masp
    // artifacts land crate-relative).
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR"))
        .context("failed to enter integration crate dir")?;

    // Fresh client state on every run (node state is managed by
    // local-net/start-stack.sh; accounts are created anew per run).
    let _ = std::fs::remove_file("../local-store.sqlite3");
    let _ = std::fs::remove_dir_all("../local-keystore");

    let mut m = Measurements { rows: Vec::new() };

    // ------------------------------------------------- phase A: offline setup
    println!("[setup] building production packages (pool + 4 notes + wallet)...");
    let packages = build_production_packages()?;
    let root = |p: &Arc<miden_mast_package::Package>| -> Result<_> {
        Ok(miden_client::note::NoteScript::from_package(p.as_ref())
            .context("note script from package")?
            .root())
    };
    let swap_root = root(&packages.swap_note)?;
    let mint_root = root(&packages.mint_note)?;
    let burn_root = root(&packages.burn_note)?;
    let collect_root = root(&packages.collect_note)?;
    println!("[setup] note-script roots frozen into the pool allowlist:");
    println!("        swap:    {}", Word::from(swap_root).to_hex());
    println!("        mint:    {}", Word::from(mint_root).to_hex());
    println!("        burn:    {}", Word::from(burn_root).to_hex());
    println!("        collect: {}", Word::from(collect_root).to_hex());

    // All accounts are generated offline first: the pool must exist BEFORE
    // the chain starts (genesis seeding), and its InitStorageData needs the
    // faucet IDs, which are fully determined client-side at build time.
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

    let (user_a, user_a_key) = create_user_wallet(&mut seed_rng, &packages)?;
    let (user_b, user_b_key) = create_user_wallet(&mut seed_rng, &packages)?;
    println!("[setup] user A (LP):     {}", user_a.id().to_hex());
    println!("[setup] user B (trader): {}", user_b.id().to_hex());

    // DEVIATION (forced by protocol v0.15): the pool account's Rust-built
    // code serializes to ~600KB, but ACCOUNT_UPDATE_MAX_SIZE caps any
    // transaction's account update at 256KiB -- a first-deployment tx for
    // this account is impossible. The pool is therefore seeded AT GENESIS
    // through the validator's genesis config ([[account]] entry), which is
    // the same mechanism the docker-compose stack exposes via
    // MIDEN_GENESIS_CONFIG_FILE. Everything downstream (ntx discovery,
    // network-account classification, note consumption) is unaffected.
    let allowed: BTreeSet<_> = [swap_root, mint_root, burn_root, collect_root]
        .into_iter()
        .collect();
    let pool_account = build_pool_account(&packages, token0, token1, allowed)?;
    let pool = pool_account.id();
    println!("[setup] pool account (genesis-seeded): {}", pool.to_hex());

    let genesis_dir = Path::new("../local-net/genesis");
    std::fs::create_dir_all(genesis_dir)?;
    miden_client::account::AccountFile::new(pool_account.clone(), vec![])
        .write(genesis_dir.join("pool.mac"))
        .context("writing pool account file")?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as u32;
    std::fs::write(
        genesis_dir.join("genesis.toml"),
        format!(
            "version = 1\ntimestamp = {timestamp}\n\n[fee_parameters]\nverification_base_fee = 0\n\n[[account]]\npath = \"pool.mac\"\n"
        ),
    )?;

    // Restart the stack from a fresh genesis that contains the pool.
    println!("[setup] restarting local stack with pool-seeded genesis...");
    let genesis_toml = std::fs::canonicalize(genesis_dir.join("genesis.toml"))?;
    let status = Command::new("bash")
        .arg("../local-net/start-stack.sh")
        .arg("--fresh")
        .env("MIDEN_GENESIS_CONFIG_FILE", &genesis_toml)
        .status()
        .context("running start-stack.sh --fresh")?;
    ensure!(status.success(), "start-stack.sh --fresh failed");

    // ------------------------------------------------- phase B: online setup
    let ClientSetup {
        mut client,
        keystore,
    } = setup_local_client().await?;

    // Dedicated RPC handle for direct chain polling (pool state reads).
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

    // Fund both users with both tokens: one mint tx per faucet carrying two
    // P2ID notes, then one consume tx per user (the users' first =
    // deployment transactions).
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

    // ------------------------------------------------- pool genesis check
    // Verify the genesis-seeded pool is queryable and matches the init
    // storage.
    let chain_pool = fetch_account(&rpc, pool)
        .await?
        .context("genesis-seeded pool not queryable via RPC")?;
    ensure!(
        read_value(&chain_pool, "sqrt_price")?
            == u128_to_word(amm_math::tick_math::get_sqrt_ratio_at_tick(INITIAL_TICK)),
        "pool initial sqrt_price mismatch"
    );
    let ps = read_value(&chain_pool, "pool_state")?;
    ensure!(
        ps[0].as_canonical_u64() == (INITIAL_TICK + TICK_OFF) as u64,
        "pool initial tick mismatch"
    );
    println!("[PASS] pool on-chain storage matches InitStorageData (tick 0, fee 3000, spacing 60)");

    // The pool emits P2ID notes with tag 0; a tag-0 subscription is
    // required for the client to receive them during sync.
    client.add_note_tag(NoteTag::from(0u32)).await?;

    // Host-side mirror of the pool.
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, INITIAL_TICK);
    let mut note_rng = RandomCoin::new(Word::from([1u32, 2, 3, 4]));

    // ============================================================ step 3: MINT
    println!("\n=== Step 3: user A mints narrow position [-120,120] via network note ===");
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_NARROW, true);
    sim.mint(-120, 120, L_NARROW);
    let deadline_far = client.sync_state().await?.block_num.as_u32() + 2000;

    let mint_note_a = build_amm_note(
        &packages.mint_note,
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
            Felt::from(deadline_far),
        ],
        vec![
            FungibleAsset::new(token0, owed0 as u64 + MINT_EXCESS)?.into(),
            FungibleAsset::new(token1, owed1 as u64 + MINT_EXCESS)?.into(),
        ],
    )?;
    let t_submit = Instant::now();
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
    let (pool_after, ntx_latency) = wait_pool(&rpc, pool, "narrow mint consumed by ntx-builder", |a| {
        Ok(read_map(a, "positions", liq_key)? != Word::default())
    })
    .await?;
    m.add_secs("MINT narrow: note committed -> pool state updated", ntx_latency);
    m.add_secs("MINT narrow: total (submit -> pool state updated)", t_submit.elapsed());

    ensure!(
        read_map(&pool_after, "positions", liq_key)? == u128_to_word(L_NARROW),
        "position liquidity mismatch after mint"
    );
    ensure!(
        read_value(&pool_after, "liquidity")? == u128_to_word(sim.liquidity),
        "active liquidity mismatch after mint"
    );
    ensure!(
        vault_balance(&pool_after, token0)? == owed0 as u64
            && vault_balance(&pool_after, token1)? == owed1 as u64,
        "pool vault should hold exactly the owed amounts after refunding excess"
    );
    println!("[PASS] position recorded (L={L_NARROW}), pool vault holds owed0={owed0} owed1={owed1}");

    // The excess must come back as a pool-emitted P2ID refund note.
    claim_p2id(
        &mut client,
        user_a.id(),
        &mint_note_a,
        SALT_MINT_REFUND,
        &[(token0, MINT_EXCESS), (token1, MINT_EXCESS)],
        "mint excess refund",
    )
    .await?;

    // Backstop position (exact amounts, no refund expected).
    println!("\n=== Step 3b: user A mints backstop [-6000,6000] ===");
    let (b0, b1) = sim.amounts_for_liquidity(-6000, 6000, L_BACKSTOP, true);
    sim.mint(-6000, 6000, L_BACKSTOP);
    let deadline_far = client.sync_state().await?.block_num.as_u32() + 2000;
    let backstop_note = build_amm_note(
        &packages.mint_note,
        user_a.id(),
        pool,
        &mut note_rng,
        vec![
            pool.suffix(),
            Felt::from(pool.prefix()),
            tick_felt(-6000),
            tick_felt(6000),
            u128_limb_felts(L_BACKSTOP)[0],
            u128_limb_felts(L_BACKSTOP)[1],
            u128_limb_felts(L_BACKSTOP)[2],
            u128_limb_felts(L_BACKSTOP)[3],
            Felt::from(deadline_far),
        ],
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
    let t0 = Instant::now();
    let (pool_after, lat) = wait_pool(&rpc, pool, "backstop mint consumed", |a| {
        Ok(read_map(a, "positions", backstop_key)? != Word::default())
    })
    .await?;
    let _ = t0;
    m.add_secs("MINT backstop: note committed -> pool state updated", lat);
    assert_pool_state(&pool_after, &sim)?;
    println!("[PASS] backstop minted; pool state matches PoolSim exactly");

    // ============================================================ step 4: SWAP down (crosses -120)
    // PROVING-MEMORY CONSTRAINT (measured on this 24GB machine): a swap
    // ending INSIDE a tick range runs the binary-search reverse tick
    // mapping, pushing the network tx to ~4.0M cycles = a 2^22-step trace
    // whose STARK proof exhausts physical memory (the prover thrashes in
    // swap and never finishes). Swaps here are therefore sized so the
    // input is consumed EXACTLY at the target tick boundary: the crossing
    // still executes (fee-growth flip + liquidity transition -- the very
    // thing step 4/5 validate) but the reverse mapping is skipped,
    // keeping the trace at 2^21 which this machine can prove.
    let swap_a_in = exact_boundary_input(&sim, true, -120)?;
    println!("\n=== Step 4: user B swaps zero_for_one {swap_a_in} token0 (crosses -120 downward, exact-boundary) ===");
    let out_a = sim.swap(swap_a_in, true);
    ensure!(out_a.crossings == 1, "setup: swap A must cross exactly one tick");
    ensure!(
        out_a.end_sqrt_price == amm_math::tick_math::get_sqrt_ratio_at_tick(-120),
        "setup: swap A must land exactly on the -120 boundary"
    );
    let deadline_far = client.sync_state().await?.block_num.as_u32() + 2000;
    println!(
        "[sim] expected: amount_out={} end_tick={} crossings={} end_liquidity={}",
        out_a.amount_out, out_a.end_tick, out_a.crossings, out_a.end_liquidity
    );
    let swap_note_a = build_amm_note(
        &packages.swap_note,
        user_b.id(),
        pool,
        &mut note_rng,
        vec![
            pool.suffix(),
            Felt::from(pool.prefix()),
            Felt::from(0u32), // zero_for_one
            Felt::from(out_a.amount_out as u64 as u32),
            Felt::from(((out_a.amount_out as u64) >> 32) as u32),
            user_b.id().suffix(),
            Felt::from(user_b.id().prefix()),
            Felt::from(deadline_far),
        ],
        vec![FungibleAsset::new(token0, swap_a_in)?.into()],
    )?;

    // Timed split of the user-side pipeline (execute / prove / submit) for
    // the "local proving time" measurement.
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
    wait_committed(&mut client, user_b.id(), "swap A note publish").await?;

    let expected_price = u128_to_word(sim.sqrt_price);
    let (pool_after, lat) = wait_pool(&rpc, pool, "swap A consumed by ntx-builder", |a| {
        Ok(read_value(a, "sqrt_price")? == expected_price)
    })
    .await?;
    m.add_secs("SWAP zero_for_one (1 cross): note committed -> consumed", lat);
    assert_pool_state(&pool_after, &sim)?;
    println!(
        "[PASS] swap A: price/tick/liquidity/fee-growth match PoolSim (tick {} liquidity {})",
        sim.tick, sim.liquidity
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

    // ============================================================ step 5: SWAP up (crosses -120 back)
    // Exact-boundary again (see step 4 note): from the -120 boundary the
    // swap crosses -120 upward (re-adding the narrow liquidity) and lands
    // exactly on the +120 boundary (crossing it too).
    let swap_b_in = exact_boundary_input(&sim, false, 120)?;
    println!("\n=== Step 5: user B swaps one_for_zero {swap_b_in} token1 (crosses -120 upward, exact-boundary) ===");
    let out_b = sim.swap(swap_b_in, false);
    let deadline_far = client.sync_state().await?.block_num.as_u32() + 2000;
    ensure!(
        out_b.crossings >= 1 && out_b.end_tick > -120,
        "setup: swap B must cross -120 upward (crossings={}, end_tick={})",
        out_b.crossings,
        out_b.end_tick
    );
    println!(
        "[sim] expected: amount_out={} end_tick={} crossings={} end_liquidity={}",
        out_b.amount_out, out_b.end_tick, out_b.crossings, out_b.end_liquidity
    );
    let swap_note_b = build_amm_note(
        &packages.swap_note,
        user_b.id(),
        pool,
        &mut note_rng,
        vec![
            pool.suffix(),
            Felt::from(pool.prefix()),
            Felt::from(1u32), // one_for_zero
            Felt::from(out_b.amount_out as u64 as u32),
            Felt::from(((out_b.amount_out as u64) >> 32) as u32),
            user_b.id().suffix(),
            Felt::from(user_b.id().prefix()),
            Felt::from(deadline_far),
        ],
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
    m.add_secs("SWAP one_for_zero (tick re-cross): note committed -> consumed", lat);
    assert_pool_state(&pool_after, &sim)?;
    println!(
        "[PASS] swap B: crossed -120 upward; liquidity restored to {} (narrow+backstop), fee growth matches",
        sim.liquidity
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
        &packages.burn_note,
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
        &packages.collect_note,
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
        &packages.swap_note,
        user_b.id(),
        pool,
        &mut note_rng,
        vec![
            pool.suffix(),
            Felt::from(pool.prefix()),
            Felt::from(0u32),
            Felt::from(u32::MAX), // min_out = u64::MAX: unsatisfiable
            Felt::from(u32::MAX),
            user_b.id().suffix(),
            Felt::from(user_b.id().prefix()),
            Felt::from(short_deadline),
        ],
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

    // Observe the retry timeline from the ntx-builder's SQLite while
    // waiting for the deadline-refund consumption.
    let bad_id_hex = bad_note.id().to_hex();
    let mut attempts_seen: Vec<(u32, i64, String)> = Vec::new();
    let t_adv = Instant::now();
    let refund_serial = expected_p2id_serial(&bad_note, SALT_SWAP_REFUND);
    let refund_recipient = P2idNoteStorage::new(user_b.id()).into_recipient(refund_serial);
    let refund_note = loop {
        if t_adv.elapsed() > NTX_TIMEOUT {
            bail!("adversarial note was not refund-consumed within timeout; attempts: {attempts_seen:?}");
        }
        // Track (attempt_count, last_attempt_block) transitions.
        if let Some((count, last, err)) = query_ntx_note(&bad_id_hex)? {
            if attempts_seen.last().map(|(c, _, _)| *c) != Some(count) {
                println!(
                    "[adv] t+{:>5.1}s attempt_count={count} last_attempt_block={last} err={}",
                    t_adv.elapsed().as_secs_f64(),
                    err.chars().take(120).collect::<String>()
                );
                attempts_seen.push((count, last, err));
            }
        }
        // Refund appears once the pool consumes-and-refunds at the deadline.
        client.sync_state().await?;
        if let Some(note) = find_note_by_recipient(&client, user_b.id(), refund_recipient.digest()).await? {
            break note;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    let adv_elapsed = t_adv.elapsed();
    let failed_attempts: Vec<_> = attempts_seen.iter().filter(|(c, _, _)| *c > 0).collect();
    println!(
        "[adv] refund consumed after {:.1}s; observed failed attempts: {:?}",
        adv_elapsed.as_secs_f64(),
        failed_attempts
            .iter()
            .map(|(c, b, _)| format!("#{c}@block{b}"))
            .collect::<Vec<_>>()
    );
    ensure!(
        failed_attempts.len() >= 2,
        "expected >=2 observed failed ntx attempts before the deadline refund, saw {}",
        failed_attempts.len()
    );
    m.add(
        "ADVERSARIAL: failed attempts observed before refund",
        format!(
            "{} (blocks {:?}, deadline {})",
            failed_attempts.len(),
            failed_attempts.iter().map(|(_, b, _)| *b).collect::<Vec<_>>(),
            short_deadline
        ),
    );
    m.add_secs("ADVERSARIAL: note committed -> deadline refund consumed", adv_elapsed);

    // Pool state must be untouched by the refund path.
    let pool_now = fetch_account(&rpc, pool)
        .await?
        .context("pool unavailable after adversarial step")?;
    ensure!(
        read_value(&pool_now, "sqrt_price")? == u128_to_word(sim_price_before),
        "adversarial refund must not move the pool price"
    );
    assert_pool_state(&pool_now, &sim)?;

    // Consume the refund and verify the full input came back.
    let bal_before = user_balance(&client, user_b.id(), token0).await?;
    submit_and_confirm(
        &mut client,
        user_b.id(),
        TransactionRequestBuilder::new().build_consume_notes(vec![refund_note])?,
        "user B consumes adversarial refund",
    )
    .await?;
    let bal_after = user_balance(&client, user_b.id(), token0).await?;
    ensure!(
        bal_after - bal_before == ADVERSARIAL_IN,
        "refund must return the full input ({ADVERSARIAL_IN}), got {}",
        bal_after - bal_before
    );
    println!("[PASS] adversarial note: retried with backoff, refunded at deadline, pool untouched");

    // ------------------------------------------------------------- fees
    // Genesis fee finding: default GenesisConfig sets verification_base_fee
    // = 0, so network txs are free and the pool vault needs no fee asset.
    // Cross-check: after the whole run the pool vault holds exactly the
    // sim-implied token balances (no native-asset debits).
    let final0 = vault_balance(&pool_now, token0)?;
    let final1 = vault_balance(&pool_now, token1)?;
    let expect0 = (owed0 as u64 + b0 as u64 + swap_a_in)
        .checked_sub(out_b.amount_out as u64)
        .and_then(|v| v.checked_sub(collect0))
        .context("expected pool token0 balance underflow")?;
    let expect1 = (owed1 as u64 + b1 as u64 + swap_b_in)
        .checked_sub(out_a.amount_out as u64)
        .and_then(|v| v.checked_sub(collect1))
        .context("expected pool token1 balance underflow")?;
    ensure!(
        final0 == expect0 && final1 == expect1,
        "pool vault conservation mismatch: got [{final0}, {final1}], want [{expect0}, {expect1}]"
    );
    println!("[PASS] pool vault conservation exact: token0={final0}, token1={final1}");
    m.add(
        "network-tx fees debited from pool vault",
        "0 (genesis verification_base_fee = 0)".into(),
    );

    // Remote-prover proving times, parsed from the prover log.
    for line in prover_prove_times()? {
        m.add("remote prover: prove span (time.busy)", line);
    }
    m.add(
        "per-op cycle counts (MockChain-measured; ntx-builder does not log cycles)",
        "SWAP_NO_CROSS 3,216,611 / SWAP_1_CROSS 3,971,269 / SWAP_5_CROSS 7,018,622".into(),
    );

    m.print();
    println!("\nALL PHASE 4 END-TO-END ASSERTIONS PASSED");
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

/// User wallet: standard BasicWallet (P2ID consumption + send script) plus
/// the Rust-SDK basic-wallet component (the production notes' reclaim path
/// targets THAT package's `receive_asset` MAST root).
fn create_user_wallet(
    rng: &mut RandomCoin,
    packages: &PoolPackages,
) -> Result<(Account, AuthSecretKey)> {
    let key_pair = AuthSecretKey::new_falcon512_poseidon2_with_rng(rng);
    let wallet_pkg = packages
        .wallet
        .as_ref()
        .context("production package set must include the Rust-SDK wallet")?;
    let rust_wallet =
        AccountComponent::from_package(wallet_pkg.as_ref(), &InitStorageData::default())
            .context("building Rust-SDK basic-wallet component")?;
    let account = AccountBuilder::new(rand::random())
        .account_type(AccountType::Public)
        .with_auth_component(AuthSingleSig::new(
            key_pair.public_key().to_commitment(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .with_component(rust_wallet)
        .build()
        .context("building user wallet account")?;
    Ok((account, key_pair))
}

fn build_pool_account(
    packages: &PoolPackages,
    token0: AccountId,
    token1: AccountId,
    allowed: BTreeSet<miden_client::note::NoteScriptRoot>,
) -> Result<Account> {
    let mut init = InitStorageData::default();
    let mut set = |field: &str, w: Word| -> Result<()> {
        let slot = pool_slot(field)?;
        init.insert_value(StorageValueName::from_slot_name(&slot), w)?;
        Ok(())
    };
    set(
        "pool_config",
        Word::new([
            token0.suffix(),
            Felt::from(token0.prefix()),
            token1.suffix(),
            Felt::from(token1.prefix()),
        ]),
    )?;
    set(
        "pool_params",
        Word::new([
            Felt::from(FEE_PIPS),
            Felt::from(SPACING as u32),
            Felt::from(0u32),
            Felt::from(0u32),
        ]),
    )?;
    set("p2id_root", Word::from(P2idNote::script_root()))?;
    set(
        "sqrt_price",
        u128_to_word(amm_math::tick_math::get_sqrt_ratio_at_tick(INITIAL_TICK)),
    )?;
    set(
        "pool_state",
        Word::new([
            Felt::from((INITIAL_TICK + TICK_OFF) as u32),
            Felt::from(1u32),
            Felt::from(0u32),
            Felt::from(0u32),
        ]),
    )?;
    set("liquidity", Word::default())?;
    set("fee_growth_global0_lo", Word::default())?;
    set("fee_growth_global0_hi", Word::default())?;
    set("fee_growth_global1_lo", Word::default())?;
    set("fee_growth_global1_hi", Word::default())?;

    let pool_component = AccountComponent::from_package(&packages.pool, &init)
        .context("building pool account component")?;
    let auth: AccountComponent = AuthNetworkAccount::with_allowed_notes(allowed)
        .context("allowlist must be non-empty")?
        .into();
    // `build_existing` (nonce 1): the pool enters the chain AT GENESIS, not
    // through a deployment transaction (its ~600KB account code exceeds the
    // 256KiB ACCOUNT_UPDATE_MAX_SIZE any proven tx could carry). Genesis
    // accounts follow the "existing account" convention of nonce >= 1.
    let account = AccountBuilder::new(rand::random())
        .account_type(AccountType::Public)
        .with_component(pool_component)
        .with_auth_component(auth)
        .build_existing()
        .context("building pool account")?;
    Ok(account)
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

/// Builds one production AMM network note: `NoteType::Public` (REQUIRED for
/// ntx-builder discovery) with the scheme-2 `NetworkAccountTarget`
/// attachment targeting the pool. Nothing at build time enforces the
/// public+attachment pairing -- a private note with this attachment would
/// silently never be seen by the ntx-builder.
fn build_amm_note(
    package: &Arc<miden_mast_package::Package>,
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
        .package((**package).clone())
        .note_type(NoteType::Public)
        .tag(NoteTag::with_account_target(pool).into())
        .attachment(attachment)
        .add_assets(assets)
        .note_storage(storage)?
        .build()?;
    Ok(note)
}

/// Finds the exact input amount for which the swap consumes its whole input
/// precisely at `target_tick`'s sqrt price (the guest then skips the
/// reverse tick mapping: `needs_reverse_map` stays false). Binary search
/// over sim clones -- the sim IS the guest algorithm, so the returned
/// amount reproduces the guest's rounding exactly.
fn exact_boundary_input(sim: &PoolSim, zero_for_one: bool, target_tick: i32) -> Result<u64> {
    let target = amm_math::tick_math::get_sqrt_ratio_at_tick(target_tick);
    let (mut lo, mut hi) = (1u64, 400_000_000_000u64);
    // Establish that hi overshoots the boundary.
    {
        let mut probe = sim.clone();
        let out = probe.swap(hi, zero_for_one);
        let overshot = if zero_for_one {
            out.end_sqrt_price < target
        } else {
            out.end_sqrt_price > target
        };
        ensure!(overshot, "exact_boundary_input: search hi bound does not reach the boundary");
    }
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let mut probe = sim.clone();
        let out = probe.swap(mid, zero_for_one);
        if out.end_sqrt_price == target {
            return Ok(mid);
        }
        let before_boundary = if zero_for_one {
            out.end_sqrt_price > target
        } else {
            out.end_sqrt_price < target
        };
        if before_boundary {
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    bail!("exact_boundary_input: no input lands exactly on tick {target_tick}");
}

// ================================================================================================
// Chain interaction helpers
// ================================================================================================

/// Submits a transaction and waits until the client observes it committed
/// (the account's on-chain state update lands in a block).
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
    wait_committed(client, account, label).await?;
    println!(
        "[tx] {label}: committed in {:.1}s (id {})",
        t0.elapsed().as_secs_f64(),
        tx_id.to_hex()
    );
    Ok(())
}

/// Waits until the given account has no uncommitted local transactions.
async fn wait_committed(client: &mut LocalClient, _account: AccountId, label: &str) -> Result<()> {
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
        // "Not found" (pre-deployment) surfaces as an error; treat as None.
        Err(_) => Ok(None),
    }
}

/// Polls the pool's public on-chain state until `pred` holds. Returns the
/// account snapshot and the elapsed wait.
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

/// Asserts on-chain pool state equals the sim exactly (price, tick, active
/// liquidity, both fee-growth accumulators).
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

/// Finds a committed consumable note for `user` whose recipient digest
/// matches; returns it as a full `Note` when present.
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
    // Exact asset check.
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

// ================================================================================================
// ntx-builder observation
// ================================================================================================

/// Queries the ntx-builder's SQLite for the tracked note's retry state:
/// (attempt_count, last_attempt_block, last_error).
fn query_ntx_note(note_id_hex: &str) -> Result<Option<(u32, i64, String)>> {
    let db = Path::new("../local-node-data/ntx-builder/ntx-builder.sqlite3");
    if !db.exists() {
        return Ok(None);
    }
    // Primary match by note_id blob hex; fall back to "the single pending
    // note" (during step 7 the adversarial note is the only pending one),
    // guarding against byte-order differences in the hex encoding.
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

/// Best-effort parse of `prove` span durations from the remote prover log.
fn prover_prove_times() -> Result<Vec<String>> {
    let log = std::fs::read_to_string("../local-net/logs/prover.log").unwrap_or_default();
    let clean = strip_ansi(&log);
    let mut out = Vec::new();
    for line in clean.lines() {
        if line.contains("prove") && line.contains("close") && line.contains("time.busy") {
            if let Some(pos) = line.find("time.busy") {
                let tail: String = line[pos..].chars().take(40).collect();
                out.push(tail);
            }
        }
    }
    if out.is_empty() {
        out.push("(no prove spans found in prover.log)".into());
    }
    Ok(out)
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
