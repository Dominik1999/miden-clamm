//! Exports everything the web frontend needs to build and track CLAMM pool
//! notes in the browser, into `frontend-template/public/packages/clamm/`.
//!
//! Two modes:
//!
//! * **Default (offline, no node needed)** — writes:
//!   - `swap.notescript` / `mint.notescript` / `burn.notescript` /
//!     `collect.notescript`: the serialized MASM note scripts
//!     (`NoteScript::to_bytes`), loadable in the browser via
//!     `NoteScript.deserialize`.
//!   - `golden.json`: cross-check vectors for the frontend's pure-TS
//!     encoders — deterministic accounts, note-storage felt layouts, the
//!     `NetworkAccountTarget` attachment word, full serialized golden notes
//!     (byte-exact, with note ids), P2ID serial/recipient derivations,
//!     Poseidon2 position keys, and tick→sqrtPriceX96 vectors.
//!
//! * **`--deploy` (requires the local stack at :57291)** — additionally
//!   deploys two fresh faucets and a fresh MASM pool (empty first-deployment
//!   transactions) and writes `deployment.json` with the account ids, pool
//!   parameters, script roots, and the DEV-ONLY faucet secret keys (so the
//!   browser can mint test tokens to its local wallet).
//!
//! Run from anywhere; paths resolve relative to the integration crate dir:
//!   cargo run --bin export_web_artifacts --release
//!   cargo run --bin export_web_artifacts --release -- --deploy

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use clamm_pool_masm::{component, note_script, PoolInitStorage, PoolNoteKind};
use integration::pool::testbed::expected_p2id_serial;
use integration::pool::{position_key, tick_felt, u128_limb_felts, u128_to_word, POS_LIQUIDITY, POS_TOKENS_OWED, TICK_OFF};
use miden_client::account::component::{
    AuthNetworkAccount, BasicWallet, FungibleFaucet, MintPolicyConfig, PolicyRegistration,
    TokenName, TokenPolicyManager,
};
use miden_client::account::{Account, AccountBuilder, AccountComponent, AccountId, AccountType};
use miden_client::asset::{Asset, AssetAmount, FungibleAsset, TokenSymbol};
use miden_client::auth::{AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::builder::ClientBuilder;
use miden_client::crypto::RandomCoin;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::note::{Note, NoteType};
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::transaction::TransactionRequestBuilder;
use miden_client::utils::Serializable;
use miden_client::{Felt, Word};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_standards::note::{
    NetworkAccountTarget, NoteExecutionHint, P2idNote, P2idNoteStorage,
};
use miden_standards::testing::note::NoteBuilder;

// Pool parameters — identical to validate_local_masm.
const FEE_PIPS: u32 = 3000;
const SPACING: i32 = 60;
const INITIAL_TICK: i32 = 0;
const FAUCET_MAX_SUPPLY: u64 = 9_000_000_000_000_000_000;

/// Deterministic serial used by every golden note.
const GOLDEN_SERIAL: [u64; 4] = [11, 22, 33, 44];

fn out_dir() -> PathBuf {
    PathBuf::from("../../frontend-template/public/packages/clamm")
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}

fn felt_dec(f: Felt) -> String {
    format!("\"{}\"", f.as_canonical_u64())
}

fn word_felts_json(w: Word) -> String {
    format!(
        "[{}, {}, {}, {}]",
        felt_dec(w[0]),
        felt_dec(w[1]),
        felt_dec(w[2]),
        felt_dec(w[3])
    )
}

fn felts_json(felts: &[Felt]) -> String {
    let items: Vec<String> = felts.iter().map(|f| felt_dec(*f)).collect();
    format!("[{}]", items.join(", "))
}

fn account_json(label: &str, id: AccountId) -> String {
    format!(
        "\"{label}\": {{\"hex\": \"{}\", \"prefixFelt\": {}, \"suffixFelt\": {}}}",
        id.to_hex(),
        felt_dec(Felt::from(id.prefix())),
        felt_dec(id.suffix()),
    )
}

fn create_faucet(rng: &mut RandomCoin, seed: [u8; 32], symbol: &str) -> Result<(Account, AuthSecretKey)> {
    let key_pair = AuthSecretKey::new_falcon512_poseidon2_with_rng(rng);
    let faucet_component = FungibleFaucet::builder()
        .name(TokenName::new(symbol)?)
        .symbol(TokenSymbol::new(symbol)?)
        .decimals(6)
        .max_supply(AssetAmount::new(FAUCET_MAX_SUPPLY)?)
        .build()?;
    let account = AccountBuilder::new(seed)
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

fn create_user_wallet(rng: &mut RandomCoin, seed: [u8; 32]) -> Result<(Account, AuthSecretKey)> {
    let key_pair = AuthSecretKey::new_falcon512_poseidon2_with_rng(rng);
    let account = AccountBuilder::new(seed)
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

fn build_pool_account(seed: [u8; 32], token0: AccountId, token1: AccountId) -> Result<Account> {
    let allowed: BTreeSet<_> = [
        note_script(PoolNoteKind::Swap).root(),
        note_script(PoolNoteKind::Mint).root(),
        note_script(PoolNoteKind::Burn).root(),
        note_script(PoolNoteKind::Collect).root(),
    ]
    .into_iter()
    .collect();
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
    AccountBuilder::new(seed)
        .account_type(AccountType::Public)
        .with_component(pool_component)
        .with_auth_component(auth)
        .build()
        .context("building pool account")
}

/// Builds one golden note exactly like validate_local_masm's `build_amm_note`,
/// but with a fixed serial number for reproducibility.
fn build_golden_note(
    kind: PoolNoteKind,
    sender: AccountId,
    pool: AccountId,
    storage: Vec<Felt>,
    assets: Vec<Asset>,
) -> Result<Note> {
    let attachment = NetworkAccountTarget::new(pool, NoteExecutionHint::always())
        .context("building NetworkAccountTarget attachment")?;
    let serial = Word::new([
        Felt::from(GOLDEN_SERIAL[0] as u32),
        Felt::from(GOLDEN_SERIAL[1] as u32),
        Felt::from(GOLDEN_SERIAL[2] as u32),
        Felt::from(GOLDEN_SERIAL[3] as u32),
    ]);
    // NoteBuilder::new derives a random serial from the rng; override it.
    let mut rng = RandomCoin::new(Word::from([9u32, 9, 9, 9]));
    let note = NoteBuilder::new(sender, &mut rng)
        .script(note_script(kind).clone())
        .note_type(NoteType::Public)
        .attachment(attachment)
        .add_assets(assets)
        .note_storage(storage)?
        .serial_number(serial)
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

fn note_json(kind: &str, note: &Note, sender: AccountId, storage: &[Felt], assets: &[(AccountId, u64)], attachment_word: Word) -> String {
    let asset_items: Vec<String> = assets
        .iter()
        .map(|(f, a)| format!("{{\"faucet\": \"{}\", \"amount\": \"{a}\"}}", f.to_hex()))
        .collect();
    format!(
        concat!(
            "{{\"kind\": \"{kind}\", \"senderHex\": \"{sender}\", \"tag\": {tag}, ",
            "\"serial\": {serial}, \"storage\": {storage}, \"assets\": [{assets}], ",
            "\"attachmentWord\": {attachment}, \"noteId\": \"{id}\", ",
            "\"recipientDigest\": \"{digest}\", \"bytesHex\": \"{bytes}\"}}"
        ),
        kind = kind,
        sender = sender.to_hex(),
        tag = note.metadata().tag().as_u32(),
        serial = word_felts_json(note.serial_num()),
        storage = felts_json(storage),
        assets = asset_items.join(", "),
        attachment = word_felts_json(attachment_word),
        id = note.id().to_hex(),
        digest = note.recipient().digest().to_hex(),
        bytes = hex_bytes(&note.to_bytes()),
    )
}

fn export_offline() -> Result<(AccountId, AccountId, AccountId, AccountId)> {
    let dir = out_dir();
    std::fs::create_dir_all(&dir).context("creating output dir")?;

    // ---- note scripts -------------------------------------------------------
    for (kind, name) in [
        (PoolNoteKind::Swap, "swap"),
        (PoolNoteKind::Mint, "mint"),
        (PoolNoteKind::Burn, "burn"),
        (PoolNoteKind::Collect, "collect"),
    ] {
        let bytes = note_script(kind).to_bytes();
        std::fs::write(dir.join(format!("{name}.notescript")), &bytes)
            .with_context(|| format!("writing {name}.notescript"))?;
        println!("[export] {name}.notescript: {} B (root {})", bytes.len(), Word::from(note_script(kind).root()).to_hex());
    }

    // ---- deterministic golden fixtures --------------------------------------
    let mut rng = RandomCoin::new(Word::from([7u32, 7, 7, 7]));
    let (faucet0, _k0) = create_faucet(&mut rng, [1u8; 32], "TKA")?;
    let (faucet1, _k1) = create_faucet(&mut rng, [2u8; 32], "TKB")?;
    let (user, _uk) = create_user_wallet(&mut rng, [3u8; 32])?;
    let pool = build_pool_account([4u8; 32], faucet0.id(), faucet1.id())?;

    let pool_id = pool.id();
    let user_id = user.id();
    let token0 = faucet0.id();
    let token1 = faucet1.id();

    // Attachment word: [pool_suffix, pool_prefix, exec_hint(always)=1, 0].
    let mut att_word = Word::default();
    att_word[0] = pool_id.suffix();
    att_word[1] = Felt::from(pool_id.prefix());
    att_word[2] = Felt::from(1u32);

    // Golden notes (fixed parameters).
    let s_storage = swap_storage(pool_id, 0, 12_345_678_901, user_id, 4242);
    let swap_note = build_golden_note(
        PoolNoteKind::Swap,
        user_id,
        pool_id,
        s_storage.clone(),
        vec![FungibleAsset::new(token0, 1_000_000)?.into()],
    )?;
    let m_storage = mint_storage(pool_id, -120, 120, 1_000_000_000_000u128, 4242);
    let mint_note = build_golden_note(
        PoolNoteKind::Mint,
        user_id,
        pool_id,
        m_storage.clone(),
        vec![
            FungibleAsset::new(token0, 600_000)?.into(),
            FungibleAsset::new(token1, 700_000)?.into(),
        ],
    )?;
    let liq_limbs = u128_limb_felts(1_000_000_000_000u128);
    let b_storage = vec![
        pool_id.suffix(),
        Felt::from(pool_id.prefix()),
        tick_felt(-120),
        tick_felt(120),
        liq_limbs[0],
        liq_limbs[1],
        liq_limbs[2],
        liq_limbs[3],
    ];
    let burn_note = build_golden_note(PoolNoteKind::Burn, user_id, pool_id, b_storage.clone(), vec![])?;
    let c_storage = vec![
        pool_id.suffix(),
        Felt::from(pool_id.prefix()),
        tick_felt(-120),
        tick_felt(120),
    ];
    let collect_note =
        build_golden_note(PoolNoteKind::Collect, user_id, pool_id, c_storage.clone(), vec![])?;

    // P2ID derivation vectors from the golden swap note.
    let mut p2id_salts = Vec::new();
    for salt in 0u32..4 {
        let serial = expected_p2id_serial(&swap_note, salt);
        p2id_salts.push(format!("\"{salt}\": \"{}\"", serial.to_hex()));
    }
    let p2id_serial_salt0 = expected_p2id_serial(&swap_note, 0);
    let p2id_recipient = P2idNoteStorage::new(user_id).into_recipient(p2id_serial_salt0);
    let p2id_storage_felts = vec![user_id.suffix(), Felt::from(user_id.prefix())];

    // Position keys.
    let pk_liq = position_key(user_id.suffix(), Felt::from(user_id.prefix()), -120, 120, POS_LIQUIDITY);
    let pk_owed = position_key(user_id.suffix(), Felt::from(user_id.prefix()), -120, 120, POS_TOKENS_OWED);

    // Tick → sqrtPriceX96 vectors.
    let ratio_ticks: [i32; 8] = [-443_636, -6000, -180, -120, 0, 120, 6000, 443_636];
    let ratios: Vec<String> = ratio_ticks
        .iter()
        .map(|t| {
            format!(
                "\"{t}\": \"{}\"",
                amm_math::tick_math::get_sqrt_ratio_at_tick(*t)
            )
        })
        .collect();

    let golden = format!(
        concat!(
            "{{\n",
            "  \"roots\": {{\"swap\": \"{r_swap}\", \"mint\": \"{r_mint}\", \"burn\": \"{r_burn}\", ",
            "\"collect\": \"{r_collect}\", \"p2id\": \"{r_p2id}\"}},\n",
            "  \"accounts\": {{{a_user}, {a_pool}, {a_f0}, {a_f1}}},\n",
            "  \"tickOff\": {tick_off},\n",
            "  \"sqrtRatios\": {{{ratios}}},\n",
            "  \"positionKeys\": [\n",
            "    {{\"owner\": \"user\", \"lower\": -120, \"upper\": 120, \"field\": {f_liq}, \"key\": \"{pk_liq}\"}},\n",
            "    {{\"owner\": \"user\", \"lower\": -120, \"upper\": 120, \"field\": {f_owed}, \"key\": \"{pk_owed}\"}}\n",
            "  ],\n",
            "  \"p2id\": {{\"salts\": {{{p2id_salts}}}, \"recipientDigestSalt0\": \"{p2id_digest}\", ",
            "\"storageFelts\": {p2id_storage}}},\n",
            "  \"notes\": [\n    {n_swap},\n    {n_mint},\n    {n_burn},\n    {n_collect}\n  ]\n",
            "}}\n"
        ),
        r_swap = Word::from(note_script(PoolNoteKind::Swap).root()).to_hex(),
        r_mint = Word::from(note_script(PoolNoteKind::Mint).root()).to_hex(),
        r_burn = Word::from(note_script(PoolNoteKind::Burn).root()).to_hex(),
        r_collect = Word::from(note_script(PoolNoteKind::Collect).root()).to_hex(),
        r_p2id = Word::from(P2idNote::script_root()).to_hex(),
        a_user = account_json("user", user_id),
        a_pool = account_json("pool", pool_id),
        a_f0 = account_json("faucet0", token0),
        a_f1 = account_json("faucet1", token1),
        tick_off = TICK_OFF,
        ratios = ratios.join(", "),
        f_liq = POS_LIQUIDITY,
        f_owed = POS_TOKENS_OWED,
        pk_liq = pk_liq.to_hex(),
        pk_owed = pk_owed.to_hex(),
        p2id_salts = p2id_salts.join(", "),
        p2id_digest = p2id_recipient.digest().to_hex(),
        p2id_storage = felts_json(&p2id_storage_felts),
        n_swap = note_json("swap", &swap_note, user_id, &s_storage, &[(token0, 1_000_000)], att_word),
        n_mint = note_json("mint", &mint_note, user_id, &m_storage, &[(token0, 600_000), (token1, 700_000)], att_word),
        n_burn = note_json("burn", &burn_note, user_id, &b_storage, &[], att_word),
        n_collect = note_json("collect", &collect_note, user_id, &c_storage, &[], att_word),
    );
    std::fs::write(dir.join("golden.json"), golden).context("writing golden.json")?;
    println!("[export] golden.json written ({} note vectors)", 4);

    Ok((token0, token1, user_id, pool_id))
}

async fn deploy() -> Result<()> {
    let dir = out_dir();

    // Fresh dedicated client state for every deploy.
    let _ = std::fs::remove_file("../local-store-web-deploy.sqlite3");
    let _ = std::fs::remove_dir_all("../local-keystore-web-deploy");

    let endpoint = Endpoint::new("http".into(), "localhost".into(), Some(57291));
    let rpc: Arc<GrpcClient> = Arc::new(GrpcClient::new(&endpoint, 30_000));
    let keystore = Arc::new(
        FilesystemKeyStore::new(PathBuf::from("../local-keystore-web-deploy"))
            .context("initializing web-deploy keystore")?,
    );
    let mut client = ClientBuilder::new()
        .rpc(Arc::new(GrpcClient::new(&endpoint, 30_000)))
        .sqlite_store(PathBuf::from("../local-store-web-deploy.sqlite3"))
        .authenticator(keystore.clone())
        .in_debug_mode(true.into())
        .build()
        .await
        .context("building web-deploy client (is the local stack running on :57291?)")?;
    let sync = client
        .sync_state()
        .await
        .context("initial sync failed (is the local stack running on :57291?)")?;
    println!("[deploy] connected to local node; chain tip: {}", sync.block_num);

    let mut rng = RandomCoin::new(Word::from([
        rand::random::<u32>(),
        rand::random::<u32>(),
        rand::random::<u32>(),
        rand::random::<u32>(),
    ]));
    let (faucet0, key0) = create_faucet(&mut rng, rand::random(), "TKA")?;
    let (faucet1, key1) = create_faucet(&mut rng, rand::random(), "TKB")?;
    let pool = build_pool_account(rand::random(), faucet0.id(), faucet1.id())?;

    for (acct, key) in [(&faucet0, Some(&key0)), (&faucet1, Some(&key1)), (&pool, None)] {
        client.add_account(acct, false).await?;
        if let Some(key) = key {
            keystore.add_key(key, acct.id()).await?;
        }
    }

    // Deploy each account through an empty first transaction.
    for (id, label) in [
        (faucet0.id(), "faucet0 (TKA)"),
        (faucet1.id(), "faucet1 (TKB)"),
        (pool.id(), "pool"),
    ] {
        let req = TransactionRequestBuilder::new().build()?;
        client
            .submit_new_transaction(id, req)
            .await
            .with_context(|| format!("deploying {label}"))?;
        println!("[deploy] {label} deployment tx submitted: {}", id.to_hex());
    }

    // Wait until all three are queryable on-chain with nonce >= 1.
    for (id, label) in [
        (faucet0.id(), "faucet0"),
        (faucet1.id(), "faucet1"),
        (pool.id(), "pool"),
    ] {
        let t0 = std::time::Instant::now();
        loop {
            ensure!(
                t0.elapsed() < std::time::Duration::from_secs(120),
                "{label} deployment did not land on-chain within 120s"
            );
            client.sync_state().await?;
            if let Ok(Some(acct)) = rpc.get_account_details(id).await {
                if acct.nonce().as_canonical_u64() >= 1 {
                    println!("[deploy] {label} on-chain (nonce {})", acct.nonce().as_canonical_u64());
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        }
    }

    let deployment = format!(
        concat!(
            "{{\n",
            "  \"network\": {{\"rpcUrl\": \"http://localhost:57291\", \"proverUrl\": \"http://localhost:50051\"}},\n",
            "  \"pool\": {{\"id\": \"{pool}\", \"feePips\": {fee}, \"tickSpacing\": {spacing}, \"initialTick\": {tick}}},\n",
            "  \"token0\": {{\"id\": \"{t0}\", \"symbol\": \"TKA\", \"decimals\": 6, \"devSecretKeyHex\": \"{k0}\"}},\n",
            "  \"token1\": {{\"id\": \"{t1}\", \"symbol\": \"TKB\", \"decimals\": 6, \"devSecretKeyHex\": \"{k1}\"}},\n",
            "  \"roots\": {{\"swap\": \"{r_swap}\", \"mint\": \"{r_mint}\", \"burn\": \"{r_burn}\", \"collect\": \"{r_collect}\", \"p2id\": \"{r_p2id}\"}}\n",
            "}}\n"
        ),
        pool = pool.id().to_hex(),
        fee = FEE_PIPS,
        spacing = SPACING,
        tick = INITIAL_TICK,
        t0 = faucet0.id().to_hex(),
        k0 = hex_bytes(&key0.to_bytes()),
        t1 = faucet1.id().to_hex(),
        k1 = hex_bytes(&key1.to_bytes()),
        r_swap = Word::from(note_script(PoolNoteKind::Swap).root()).to_hex(),
        r_mint = Word::from(note_script(PoolNoteKind::Mint).root()).to_hex(),
        r_burn = Word::from(note_script(PoolNoteKind::Burn).root()).to_hex(),
        r_collect = Word::from(note_script(PoolNoteKind::Collect).root()).to_hex(),
        r_p2id = Word::from(P2idNote::script_root()).to_hex(),
    );
    std::fs::write(dir.join("deployment.json"), deployment).context("writing deployment.json")?;
    println!("[deploy] deployment.json written to {}", dir.join("deployment.json").display());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR"))
        .context("failed to enter integration crate dir")?;

    let (token0, token1, user, pool) = export_offline()?;
    println!("[export] offline artifacts complete");
    println!("         golden fixture accounts: user={} pool={} token0={} token1={}",
        user.to_hex(), pool.to_hex(), token0.to_hex(), token1.to_hex());

    if std::env::args().any(|a| a == "--deploy") {
        deploy().await?;
    } else {
        println!("[export] skipping deployment (pass --deploy with the local stack running to write deployment.json)");
    }
    Ok(())
}
