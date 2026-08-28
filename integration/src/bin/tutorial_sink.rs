//! ASSET variant of the tutorial repro: the network note CARRIES fungible
//! assets which the note script moves into the network account's vault
//! (BasicWallet) before incrementing the counter. Isolates the "asset-bearing
//! network note + vault update" trace feature of the CLAMM pool txs.
//!
//! Run: cargo run -p integration --bin tutorial_asset --release

use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use miden_client::account::component::{
    AccountComponentMetadata, AuthNetworkAccount, BasicWallet, FungibleFaucet, MintPolicyConfig,
    PolicyRegistration, TokenName, TokenPolicyManager,
};
use miden_client::account::{
    AccountBuilder, AccountComponent, AccountType, StorageSlot, StorageSlotName,
};
use miden_client::asset::{AssetAmount, FungibleAsset, TokenSymbol};
use miden_client::auth::{AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::builder::ClientBuilder;
use miden_client::crypto::FeltRng;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::note::{
    NetworkAccountTarget, Note, NoteAssets, NoteAttachments, NoteError, NoteExecutionHint,
    NoteRecipient, NoteStorage, NoteTag, NoteType, PartialNoteMetadata,
};
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::store::TransactionFilter;
use miden_client::transaction::{TransactionId, TransactionRequestBuilder, TransactionStatus};
use miden_client::{Client, ClientError, Felt, Word};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_standards::note::P2idNote;
use rand::RngCore;
use tokio::time::{sleep, Duration};

fn now() -> String {
    let out = std::process::Command::new("date")
        .arg("-u")
        .arg("+%Y-%m-%dT%H:%M:%SZ")
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => "unknown-time".to_string(),
    }
}

async fn wait_for_tx(
    client: &mut Client<FilesystemKeyStore>,
    tx_id: TransactionId,
) -> Result<(), ClientError> {
    loop {
        client.sync_state().await?;
        let txs = client.get_transactions(TransactionFilter::Ids(vec![tx_id])).await?;
        let tx_committed = if !txs.is_empty() {
            matches!(txs[0].status, TransactionStatus::Committed { .. })
        } else {
            false
        };
        if tx_committed {
            println!("committed {} [{}]", tx_id.to_hex(), now());
            break;
        }
        sleep(Duration::from_secs(2)).await;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = Endpoint::testnet();
    let rpc: Arc<GrpcClient> = Arc::new(GrpcClient::new(&endpoint, 30_000));
    let scratch = std::env::temp_dir().join(format!("clamm-tutorial-asset-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let keystore = Arc::new(FilesystemKeyStore::new(scratch.join("keystore")).unwrap());
    let mut client = ClientBuilder::new()
        .rpc(Arc::new(GrpcClient::new(&endpoint, 30_000)))
        .sqlite_store(PathBuf::from(scratch.join("store.sqlite3")))
        .authenticator(keystore.clone())
        .in_debug_mode(true.into())
        .build()
        .await?;
    let sync = client.sync_state().await.unwrap();
    println!("Latest block: {} [{}]", sync.block_num, now());

    // ---- Alice ----
    let mut seed = [0u8; 32];
    client.rng().fill_bytes(&mut seed);
    let alice_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let alice = AccountBuilder::new(seed)
        .account_type(AccountType::Public)
        .with_auth_component(AuthSingleSig::new(
            alice_key.public_key().to_commitment(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build()
        .unwrap();
    client.add_account(&alice, false).await?;
    keystore.add_key(&alice_key, alice.id()).await?;
    println!("alice: {}", alice.id().to_hex());

    // ---- Faucet ----
    let mut fseed = [0u8; 32];
    client.rng().fill_bytes(&mut fseed);
    let faucet_key = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let faucet = AccountBuilder::new(fseed)
        .account_type(AccountType::Public)
        .with_auth_component(AuthSingleSig::new(
            faucet_key.public_key().to_commitment(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(
            FungibleFaucet::builder()
                .name(TokenName::new("TST")?)
                .symbol(TokenSymbol::new("TST")?)
                .decimals(6)
                .max_supply(AssetAmount::new(1_000_000_000_000)?)
                .build()?,
        )
        .with_components(
            TokenPolicyManager::new()
                .with_mint_policy(MintPolicyConfig::AllowAll, PolicyRegistration::Active)?,
        )
        .build()
        .unwrap();
    client.add_account(&faucet, false).await?;
    keystore.add_key(&faucet_key, faucet.id()).await?;
    println!("faucet: {}", faucet.id().to_hex());

    // ---- Counter network account (counter component + BasicWallet) ----
    let counter_code = include_str!("../../masm/tutorial/accounts/counter_sink.masm");
    let note_code = include_str!("../../masm/tutorial/notes/network_increment_note_asset.masm");

    let note_script = client
        .code_builder()
        .with_linked_module("external_contract::counter_contract", counter_code)?
        .compile_note_script(note_code)?;
    let note_script_root = note_script.root();
    println!("asset note script root: {}", note_script_root.to_hex());

    let counter_slot_name = StorageSlotName::new("miden::tutorials::counter").expect("valid");
    let component_code = client
        .code_builder()
        .compile_component_code("external_contract::counter_contract", counter_code)?;
    let counter_component = AccountComponent::new(
        component_code,
        vec![
            StorageSlot::with_value(
                counter_slot_name.clone(),
                [Felt::new_unchecked(0); 4].into(),
            ),
            StorageSlot::with_value(
                StorageSlotName::new("miden::tutorials::p2id_root").expect("valid"),
                P2idNote::script_root().into(),
            ),
            StorageSlot::with_empty_map(
                StorageSlotName::new("miden::tutorials::map").expect("valid"),
            ),
        ],
        AccountComponentMetadata::new("external_contract::counter_contract"),
    )?;
    let mut cseed = [0u8; 32];
    client.rng().fill_bytes(&mut cseed);
    let network_auth = AuthNetworkAccount::with_allowed_notes(BTreeSet::from([note_script_root]))?;
    let counter_contract = AccountBuilder::new(cseed)
        .account_type(AccountType::Public)
        .with_auth_component(network_auth)
        .with_component(counter_component)
        .with_component(BasicWallet)
        .build()
        .unwrap();
    client.add_account(&counter_contract, false).await.unwrap();
    println!("counter+wallet contract: {}", counter_contract.id().to_hex());

    // ---- deploy contract (empty first tx, like the pool) ----
    println!("[deploy contract] [{}]", now());
    let tx = client
        .submit_new_transaction(counter_contract.id(), TransactionRequestBuilder::new().build()?)
        .await?;
    wait_for_tx(&mut client, tx).await?;

    // ---- fund alice ----
    println!("[fund alice] [{}]", now());
    let fund = P2idNote::create(
        faucet.id(),
        alice.id(),
        vec![FungibleAsset::new(faucet.id(), 1_000_000)?.into()],
        NoteType::Public,
        Default::default(),
        client.rng(),
    )?;
    let tx = client
        .submit_new_transaction(
            faucet.id(),
            TransactionRequestBuilder::new().own_output_notes(vec![fund.clone()]).build()?,
        )
        .await?;
    wait_for_tx(&mut client, tx).await?;
    let tx = client
        .submit_new_transaction(
            alice.id(),
            TransactionRequestBuilder::new().build_consume_notes(vec![fund])?,
        )
        .await?;
    wait_for_tx(&mut client, tx).await?;
    println!("alice funded");

    // ---- asset-bearing network note ----
    let serial_num = client.rng().draw_word();
    let note_storage = NoteStorage::new([].to_vec())?;
    let recipient = NoteRecipient::new(serial_num, note_script, note_storage);
    let tag = NoteTag::with_account_target(counter_contract.id());
    let attachment = NetworkAccountTarget::new(counter_contract.id(), NoteExecutionHint::Always)
        .map_err(|e| NoteError::other(e.to_string()))?
        .into();
    let metadata = PartialNoteMetadata::new(alice.id(), NoteType::Public).with_tag(tag);
    let attachments = NoteAttachments::new(vec![attachment]).unwrap();
    let assets = NoteAssets::new(vec![FungibleAsset::new(faucet.id(), 250_000)?.into()])?;
    let note = Note::with_attachments(assets, metadata, recipient, attachments);
    println!("asset network note id: {}", note.id().to_hex());
    let tx = client
        .submit_new_transaction(
            alice.id(),
            TransactionRequestBuilder::new().own_output_notes(vec![note.clone()]).build()?,
        )
        .await?;
    wait_for_tx(&mut client, tx).await?;
    println!("asset network note committed, observing [{}]", now());

    sleep(Duration::from_secs(6)).await;
    let mut last_val = None;
    for i in 0..60 {
        client.sync_state().await?;
        if let Ok(Some(acct)) = rpc.get_account_details(counter_contract.id()).await {
            let count: Word = acct.storage().get_item(&counter_slot_name).unwrap().into();
            let val = count[0].as_canonical_u64();
            if val >= 1 {
                println!("SERVICED: counter = {} (poll {}, [{}])", val, i, now());
                return Ok(());
            }
            last_val = Some(val);
        }
        sleep(Duration::from_secs(6)).await;
    }
    Err(format!(
        "NOT serviced within window (last counter {:?}); note {} [{}]",
        last_val,
        note.id().to_hex(),
        now()
    )
    .into())
}
