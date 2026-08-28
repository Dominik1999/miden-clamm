//! Stage-C testnet smoke test against the PUBLIC Miden testnet deployment
//! written by `export_web_artifacts --deploy --network testnet`
//! (`frontend/public/packages/clamm/deployment.testnet.json`).
//!
//! Answers DESIGN open question 1 for the public testnet: does the testnet
//! operator run an ntx-builder that services arbitrary network accounts?
//!
//! Flow (fully self-contained — mirrors what the browser dApp does):
//!   1. read deployment.testnet.json (pool + faucet ids, demo faucet keys)
//!   2. import both faucets by id, install their demo keys
//!   3. create + fund a fresh user wallet from the faucets (P2ID mints)
//!   4. publish one small MINT network note (Public + NetworkAccountTarget)
//!      and wait generously for the pool's ntx-builder consumption
//!   5. if serviced: publish one small SWAP note, wait for consumption, and
//!      claim the pool-emitted P2ID output note
//!   6. print outcomes + latencies
//!
//! Run: cargo run -p integration --bin testnet_smoke --release

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use clamm_pool_masm::{note_script, PoolNoteKind};
use integration::pool::testbed::expected_p2id_serial;
use integration::pool::{
    pool_slot, position_key, tick_felt, u128_limb_felts, u128_to_word, PoolSim, POS_LIQUIDITY,
};
use miden_client::account::component::BasicWallet;
use miden_client::account::{Account, AccountBuilder, AccountId, AccountType};
use miden_client::asset::{Asset, AssetCallbackFlag, AssetVaultKey, FungibleAsset};
use miden_client::auth::{AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::builder::ClientBuilder;
use miden_client::crypto::RandomCoin;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::note::{Note, NoteTag, NoteType};
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::store::TransactionFilter;
use miden_client::transaction::TransactionRequestBuilder;
use miden_client::utils::Deserializable;
use miden_client::{Client, Felt, Word};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_standards::note::{NetworkAccountTarget, NoteExecutionHint, P2idNote, P2idNoteStorage};
use miden_standards::testing::note::NoteBuilder;

const FEE_PIPS: u32 = 3000;
const SPACING: i32 = 60;
const INITIAL_TICK: i32 = 0;
/// Small smoke liquidity at [-120,120] (same shape as the local E2E narrow
/// position; ~6e9 raw of each token at tick 0).
const L_SMOKE: u128 = 1_000_000_000_000;
const MINT_EXCESS: u64 = 1000;
/// Small in-range swap input (no tick cross expected on a fresh pool).
const SWAP_IN: u64 = 1_000_000_000;
const WALLET_FUND: u64 = 100_000_000_000_000; // 1e14 raw of each token

const SALT_SWAP_OUT: u32 = 0;
const SALT_MINT_REFUND: u32 = 2;

/// Generous ntx-servicing window: the point of the smoke test is to find
/// out whether the testnet services arbitrary network accounts AT ALL, so
/// wait far longer than any reasonable block cadence before concluding.
const NTX_TIMEOUT: Duration = Duration::from_secs(600);
const TX_COMMIT_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(2000);

type SmokeClient = Client<FilesystemKeyStore>;

fn json_str(json: &serde_json::Value, path: &[&str]) -> Result<String> {
    let mut v = json;
    for p in path {
        v = v.get(p).with_context(|| format!("deployment json missing {}", path.join(".")))?;
    }
    Ok(v.as_str().with_context(|| format!("{} not a string", path.join(".")))?.to_string())
}

fn hex_to_bytes(s: &str) -> Result<Vec<u8>> {
    ensure!(s.len() % 2 == 0, "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("bad hex"))
        .collect()
}

async fn wait_committed(client: &mut SmokeClient, label: &str) -> Result<Duration> {
    let t0 = Instant::now();
    loop {
        if t0.elapsed() > TX_COMMIT_TIMEOUT {
            bail!("tx not committed within {TX_COMMIT_TIMEOUT:?}: {label}");
        }
        client.sync_state().await?;
        if client.get_transactions(TransactionFilter::Uncommitted).await?.is_empty() {
            return Ok(t0.elapsed());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn submit_and_confirm(
    client: &mut SmokeClient,
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

async fn fetch_account(rpc: &Arc<GrpcClient>, id: AccountId) -> Result<Option<Account>> {
    match rpc.get_account_details(id).await {
        Ok(acct) => Ok(acct),
        Err(_) => Ok(None),
    }
}

fn read_value(account: &Account, field: &str) -> Result<Word> {
    let slot = pool_slot(field)?;
    account.storage().get_item(&slot).with_context(|| format!("reading pool slot {field}"))
}

fn read_map(account: &Account, field: &str, key: Word) -> Result<Word> {
    let slot = pool_slot(field)?;
    account.storage().get_map_item(&slot, key).with_context(|| format!("reading pool map {field}"))
}

fn vault_balance(account: &Account, faucet: AccountId) -> Result<u64> {
    let key = AssetVaultKey::new_fungible(faucet, AssetCallbackFlag::default());
    Ok(account.vault().get_balance(key).context("vault balance")?.as_u64())
}

/// Polls the pool over raw RPC until `pred` holds. Returns Ok(None) on
/// timeout — a timeout is a smoke OUTCOME (ntx not servicing), not an error.
async fn wait_pool<F>(
    rpc: &Arc<GrpcClient>,
    pool: AccountId,
    what: &str,
    pred: F,
) -> Result<Option<(Account, Duration)>>
where
    F: Fn(&Account) -> Result<bool>,
{
    let t0 = Instant::now();
    loop {
        if t0.elapsed() > NTX_TIMEOUT {
            println!("[ntx] {what}: NOT observed within {NTX_TIMEOUT:?}");
            return Ok(None);
        }
        if let Some(acct) = fetch_account(rpc, pool).await? {
            if pred(&acct)? {
                let dt = t0.elapsed();
                println!("[ntx] {what}: observed after {:.1}s", dt.as_secs_f64());
                return Ok(Some((acct, dt)));
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn build_amm_note(
    kind: PoolNoteKind,
    sender: AccountId,
    pool: AccountId,
    rng: &mut RandomCoin,
    storage: Vec<Felt>,
    assets: Vec<Asset>,
) -> Result<Note> {
    let attachment = NetworkAccountTarget::new(pool, NoteExecutionHint::always())
        .context("building NetworkAccountTarget attachment")?;
    // The testnet ntx-builder discovers notes by TAG routing, not (only) by
    // attachment scanning: without the account-target tag the note is
    // silently orphaned (per the network-transactions tutorial; confirmed by
    // our first deployment sitting unserviced for 537 blocks).
    Ok(NoteBuilder::new(sender, rng)
        .script(note_script(kind).clone())
        .note_type(NoteType::Public)
        .tag(NoteTag::with_account_target(pool).into())
        .attachment(attachment)
        .add_assets(assets)
        .note_storage(storage)?
        .build()?)
}

async fn find_p2id(
    client: &mut SmokeClient,
    user: AccountId,
    input_note: &Note,
    salt: u32,
    label: &str,
) -> Result<Option<(Note, Duration)>> {
    let serial = expected_p2id_serial(input_note, salt);
    let digest = P2idNoteStorage::new(user).into_recipient(serial).digest();
    let t0 = Instant::now();
    loop {
        if t0.elapsed() > NTX_TIMEOUT {
            println!("[p2id] {label}: note (recipient {}) NOT seen within {NTX_TIMEOUT:?}", digest.to_hex());
            return Ok(None);
        }
        client.sync_state().await?;
        for (record, _) in client.get_consumable_notes(Some(user)).await? {
            if record.recipient() == digest {
                let note: Note = record
                    .try_into()
                    .map_err(|e| anyhow::anyhow!("note record conversion: {e:?}"))?;
                println!("[p2id] {label}: received after {:.1}s (note {})", t0.elapsed().as_secs_f64(), note.id().to_hex());
                return Ok(Some((note, t0.elapsed())));
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR"))
        .context("failed to enter integration crate dir")?;

    // ---- deployment descriptor ---------------------------------------------
    let raw = std::fs::read_to_string("../frontend/public/packages/clamm/deployment.testnet.json")
        .context("reading deployment.testnet.json (run export_web_artifacts --deploy --network testnet first)")?;
    let json: serde_json::Value = serde_json::from_str(&raw)?;
    let pool = AccountId::from_hex(&json_str(&json, &["pool", "id"])?)?;
    let faucet0 = AccountId::from_hex(&json_str(&json, &["token0", "id"])?)?;
    let faucet1 = AccountId::from_hex(&json_str(&json, &["token1", "id"])?)?;
    let key0 = AuthSecretKey::read_from_bytes(&hex_to_bytes(&json_str(&json, &["token0", "devSecretKeyHex"])?)?)
        .map_err(|e| anyhow::anyhow!("deserializing faucet0 key: {e}"))?;
    let key1 = AuthSecretKey::read_from_bytes(&hex_to_bytes(&json_str(&json, &["token1", "devSecretKeyHex"])?)?)
        .map_err(|e| anyhow::anyhow!("deserializing faucet1 key: {e}"))?;
    println!("[smoke] pool:    {}", pool.to_hex());
    println!("[smoke] faucet0: {}", faucet0.to_hex());
    println!("[smoke] faucet1: {}", faucet1.to_hex());

    // ---- fresh client against the public testnet ---------------------------
    let _ = std::fs::remove_file("../testnet-store-smoke.sqlite3");
    let _ = std::fs::remove_dir_all("../testnet-keystore-smoke");
    let endpoint = Endpoint::testnet();
    let rpc: Arc<GrpcClient> = Arc::new(GrpcClient::new(&endpoint, 30_000));
    let keystore = Arc::new(
        FilesystemKeyStore::new("../testnet-keystore-smoke".into()).context("keystore")?,
    );
    let mut client = ClientBuilder::new()
        .rpc(Arc::new(GrpcClient::new(&endpoint, 30_000)))
        .sqlite_store("../testnet-store-smoke.sqlite3".into())
        .authenticator(keystore.clone())
        .in_debug_mode(true.into())
        .build()
        .await
        .context("building testnet client")?;
    let sync = client.sync_state().await.context("initial testnet sync")?;
    println!("[smoke] connected to testnet; chain tip: {}", sync.block_num);

    // ---- import faucets (public accounts) + demo keys ----------------------
    for (id, key, label) in [(faucet0, &key0, "faucet0"), (faucet1, &key1, "faucet1")] {
        client
            .import_account_by_id(id)
            .await
            .with_context(|| format!("importing {label} from testnet"))?;
        keystore.add_key(key, id).await?;
        println!("[smoke] {label} imported + demo key installed");
    }

    // ---- fresh user wallet -------------------------------------------------
    let mut rng = RandomCoin::new(Word::from([
        rand::random::<u32>(),
        rand::random::<u32>(),
        rand::random::<u32>(),
        rand::random::<u32>(),
    ]));
    let user_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(&mut rng);
    let user = AccountBuilder::new(rand::random())
        .account_type(AccountType::Public)
        .with_auth_component(AuthSingleSig::new(
            user_key.public_key().to_commitment(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build()
        .context("building user wallet")?;
    client.add_account(&user, false).await?;
    keystore.add_key(&user_key, user.id()).await?;
    println!("[smoke] user wallet: {}", user.id().to_hex());

    // ---- fund the user from both faucets -----------------------------------
    let fund0 = P2idNote::create(
        faucet0,
        user.id(),
        vec![FungibleAsset::new(faucet0, WALLET_FUND)?.into()],
        NoteType::Public,
        Default::default(),
        client.rng(),
    )?;
    submit_and_confirm(
        &mut client,
        faucet0,
        TransactionRequestBuilder::new().own_output_notes(vec![fund0.clone()]).build()?,
        "faucet0 mint to user",
    )
    .await?;
    let fund1 = P2idNote::create(
        faucet1,
        user.id(),
        vec![FungibleAsset::new(faucet1, WALLET_FUND)?.into()],
        NoteType::Public,
        Default::default(),
        client.rng(),
    )?;
    submit_and_confirm(
        &mut client,
        faucet1,
        TransactionRequestBuilder::new().own_output_notes(vec![fund1.clone()]).build()?,
        "faucet1 mint to user",
    )
    .await?;
    submit_and_confirm(
        &mut client,
        user.id(),
        TransactionRequestBuilder::new().build_consume_notes(vec![fund0, fund1])?,
        "user consumes faucet mints (deploys user)",
    )
    .await?;
    println!("[smoke] user funded with {WALLET_FUND} of each token");

    // Pool-emitted P2ID notes carry tag 0.
    client.add_note_tag(NoteTag::from(0u32)).await?;

    // ---- host-side mirror ---------------------------------------------------
    // The smoke test assumes a FRESH pool (deployed by the same Stage-B run,
    // initial tick 0, zero liquidity). Verify before mutating.
    let chain_pool = fetch_account(&rpc, pool).await?.context("pool not found on testnet")?;
    let start_liq = read_value(&chain_pool, "liquidity")?;
    println!(
        "[smoke] pool on-chain: nonce {}, active liquidity {:?}",
        chain_pool.nonce().as_canonical_u64(),
        start_liq
    );
    let mut sim = PoolSim::new(FEE_PIPS, SPACING, INITIAL_TICK);
    ensure!(
        start_liq == u128_to_word(0u128),
        "smoke test expects a fresh pool with zero active liquidity"
    );

    // ============================ MINT ======================================
    let (owed0, owed1) = sim.amounts_for_liquidity(-120, 120, L_SMOKE, true);
    sim.mint(-120, 120, L_SMOKE);
    let deadline = client.sync_state().await?.block_num.as_u32() + 2000;
    let mint_storage: Vec<Felt> = {
        let l = u128_limb_felts(L_SMOKE);
        vec![
            pool.suffix(),
            Felt::from(pool.prefix()),
            tick_felt(-120),
            tick_felt(120),
            l[0],
            l[1],
            l[2],
            l[3],
            Felt::from(deadline),
        ]
    };
    let mint_note = build_amm_note(
        PoolNoteKind::Mint,
        user.id(),
        pool,
        &mut rng,
        mint_storage,
        vec![
            FungibleAsset::new(faucet0, owed0 as u64 + MINT_EXCESS)?.into(),
            FungibleAsset::new(faucet1, owed1 as u64 + MINT_EXCESS)?.into(),
        ],
    )?;
    println!("\n=== SMOKE MINT: [-120,120] L={L_SMOKE} (owed {owed0}/{owed1}) note {} ===", mint_note.id().to_hex());
    submit_and_confirm(
        &mut client,
        user.id(),
        TransactionRequestBuilder::new().own_output_notes(vec![mint_note.clone()]).build()?,
        "publish mint network note",
    )
    .await?;
    let liq_key = position_key(user.id().suffix(), Felt::from(user.id().prefix()), -120, 120, POS_LIQUIDITY);
    let mint_outcome = wait_pool(&rpc, pool, "mint note consumed by testnet ntx-builder", |a| {
        Ok(read_map(a, "positions", liq_key)? != Word::default())
    })
    .await?;

    let Some((pool_after, mint_latency)) = mint_outcome else {
        println!("\n================= SMOKE OUTCOME =================");
        println!("MINT note was committed but NOT consumed within {NTX_TIMEOUT:?}.");
        println!("VERDICT: pool is deployed but PASSIVE on the public testnet");
        println!("         (no ntx-builder servicing arbitrary network accounts).");
        println!("Mint note id: {} (reclaimable after deadline block {deadline})", mint_note.id().to_hex());
        println!("=================================================");
        return Ok(());
    };
    ensure!(
        read_map(&pool_after, "positions", liq_key)? == u128_to_word(L_SMOKE),
        "position liquidity mismatch after mint"
    );
    ensure!(
        vault_balance(&pool_after, faucet0)? == owed0 as u64
            && vault_balance(&pool_after, faucet1)? == owed1 as u64,
        "pool vault mismatch after mint (excess should be refunded)"
    );
    println!("[PASS] mint serviced: position recorded, pool vault holds exactly owed amounts");
    let mint_refund = find_p2id(&mut client, user.id(), &mint_note, SALT_MINT_REFUND, "mint excess refund").await?;

    // ============================ SWAP ======================================
    let outcome = sim.swap(SWAP_IN, true);
    println!(
        "\n=== SMOKE SWAP: zero_for_one {SWAP_IN} TKA (expected out {}, end tick {}) ===",
        outcome.amount_out, outcome.end_tick
    );
    let deadline = client.sync_state().await?.block_num.as_u32() + 2000;
    let min_out = outcome.amount_out as u64;
    let swap_storage: Vec<Felt> = vec![
        pool.suffix(),
        Felt::from(pool.prefix()),
        Felt::from(0u32),
        Felt::from(min_out as u32),
        Felt::from((min_out >> 32) as u32),
        user.id().suffix(),
        Felt::from(user.id().prefix()),
        Felt::from(deadline),
    ];
    let swap_note = build_amm_note(
        PoolNoteKind::Swap,
        user.id(),
        pool,
        &mut rng,
        swap_storage,
        vec![FungibleAsset::new(faucet0, SWAP_IN)?.into()],
    )?;
    println!("[smoke] swap note {}", swap_note.id().to_hex());
    submit_and_confirm(
        &mut client,
        user.id(),
        TransactionRequestBuilder::new().own_output_notes(vec![swap_note.clone()]).build()?,
        "publish swap network note",
    )
    .await?;
    let expected_price = u128_to_word(sim.sqrt_price);
    let swap_outcome = wait_pool(&rpc, pool, "swap note consumed by testnet ntx-builder", |a| {
        Ok(read_value(a, "sqrt_price")? == expected_price)
    })
    .await?;

    let Some((pool_after, swap_latency)) = swap_outcome else {
        println!("\n================= SMOKE OUTCOME =================");
        println!("MINT was serviced ({:.1}s) but the SWAP was not consumed within {NTX_TIMEOUT:?}.", mint_latency.as_secs_f64());
        println!("Swap note id: {} (reclaimable after deadline block {deadline})", swap_note.id().to_hex());
        println!("=================================================");
        return Ok(());
    };
    ensure!(
        read_value(&pool_after, "sqrt_price")? == u128_to_word(sim.sqrt_price),
        "pool sqrt_price mismatch after swap"
    );
    println!("[PASS] swap serviced: pool price matches PoolSim exactly");

    // Claim the swap output P2ID.
    let swap_p2id = find_p2id(&mut client, user.id(), &swap_note, SALT_SWAP_OUT, "swap output").await?;
    let mut claim_note_ids = Vec::new();
    if let Some((note, _)) = &swap_p2id {
        let got: Vec<(AccountId, u64)> = note
            .assets()
            .iter()
            .map(|a| match a {
                Asset::Fungible(f) => (f.faucet_id(), f.amount().as_u64()),
                _ => panic!("unexpected non-fungible asset"),
            })
            .collect();
        ensure!(
            got == vec![(faucet1, outcome.amount_out as u64)],
            "swap P2ID assets mismatch: got {got:?}, want {} TKB",
            outcome.amount_out
        );
        claim_note_ids.push(note.id().to_hex());
        submit_and_confirm(
            &mut client,
            user.id(),
            TransactionRequestBuilder::new().build_consume_notes(vec![note.clone()])?,
            "consume swap-output P2ID",
        )
        .await?;
        let acct = client.get_account(user.id()).await?.context("user missing")?;
        let bal1 = vault_balance(&acct, faucet1)?;
        println!("[PASS] swap output claimed; user TKB balance now {bal1}");
    }

    println!("\n================= SMOKE OUTCOME =================");
    println!("VERDICT: FULLY SERVICED — the public testnet ntx-builder consumes");
    println!("         network notes against our arbitrary (freshly deployed) pool.");
    println!("mint  note {}: serviced in {:.1}s", mint_note.id().to_hex(), mint_latency.as_secs_f64());
    if let Some((_, d)) = &mint_refund {
        println!("mint excess refund P2ID: received {:.1}s after servicing", d.as_secs_f64());
    }
    println!("swap  note {}: serviced in {:.1}s", swap_note.id().to_hex(), swap_latency.as_secs_f64());
    if let Some((n, d)) = &swap_p2id {
        println!("swap output P2ID {}: received {:.1}s after servicing; claimed", n.id().to_hex(), d.as_secs_f64());
    }
    println!("=================================================");
    Ok(())
}
