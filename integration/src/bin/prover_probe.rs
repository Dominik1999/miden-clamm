//! Differential probe of the PUBLIC testnet remote prover
//! (https://tx-prover.testnet.miden.io): proves two user transactions with the
//! REMOTE prover and submits them.
//!
//!   tx1 (control): fresh wallet first-deployment tx with a tiny tx script.
//!   tx2 (probe):   same wallet, tx script with a large dummy loop (~150k+
//!                  cycles -> trace comparable to our AMM network txs).
//!
//! If tx2 is rejected by the node with "constraint mismatch: quotient *
//! vanishing != folded constraints" while tx1 lands, the testnet remote
//! prover produces invalid proofs for large traces — same failure the
//! ntx-builder hits when servicing our pool.
//!
//! Run: cargo run -p integration --bin prover_probe --release

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use miden_client::account::component::BasicWallet;
use miden_client::account::{AccountBuilder, AccountType};
use miden_client::auth::{AuthSchemeId, AuthSecretKey, AuthSingleSig};
use miden_client::builder::ClientBuilder;
use miden_client::keystore::{FilesystemKeyStore, Keystore};
use miden_client::rpc::{Endpoint, GrpcClient};
use miden_client::transaction::TransactionRequestBuilder;
use miden_client::RemoteTransactionProver;
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use rand::RngCore;

const SMALL_SCRIPT: &str = "begin\n    push.1 drop\nend\n";
const HEAVY_SCRIPT: &str = "begin\n    repeat.50000\n        push.1 drop\n    end\nend\n";

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

#[tokio::main]
async fn main() -> Result<()> {
    let scratch = std::env::temp_dir().join(format!("clamm-prover-probe-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;

    let endpoint = Endpoint::testnet();
    let keystore = Arc::new(FilesystemKeyStore::new(scratch.join("keystore")).context("keystore")?);
    let mut client = ClientBuilder::new()
        .rpc(Arc::new(GrpcClient::new(&endpoint, 30_000)))
        .sqlite_store(PathBuf::from(scratch.join("store.sqlite3")))
        .authenticator(keystore.clone())
        .prover(Arc::new(RemoteTransactionProver::new(
            "https://tx-prover.testnet.miden.io".to_string(),
        )))
        .in_debug_mode(true.into())
        .build()
        .await?;
    let sync = client.sync_state().await.context("sync")?;
    println!("[probe] chain tip {} [{}]", sync.block_num, now());
    println!("[probe] prover: REMOTE https://tx-prover.testnet.miden.io");

    // Fresh wallet.
    let mut init_seed = [0_u8; 32];
    client.rng().fill_bytes(&mut init_seed);
    let key_pair = AuthSecretKey::new_falcon512_poseidon2_with_rng(client.rng());
    let wallet = AccountBuilder::new(init_seed)
        .account_type(AccountType::Public)
        .with_auth_component(AuthSingleSig::new(
            key_pair.public_key().to_commitment(),
            AuthSchemeId::Falcon512Poseidon2,
        ))
        .with_component(BasicWallet)
        .build()
        .unwrap();
    client.add_account(&wallet, false).await?;
    keystore.add_key(&key_pair, wallet.id()).await?;
    println!("[probe] wallet: {}", wallet.id().to_hex());

    for (label, code) in [("SMALL control", SMALL_SCRIPT), ("HEAVY probe", HEAVY_SCRIPT)] {
        let script = client.code_builder().compile_tx_script(code)?;
        println!("\n[probe] {label}: tx script root {}", script.root().to_hex());
        let req = TransactionRequestBuilder::new().custom_script(script).build()?;
        println!("[probe] {label}: executing + remote-proving + submitting [{}]", now());
        match client.submit_new_transaction(wallet.id(), req).await {
            Ok(tx_id) => {
                println!("[probe] {label}: ACCEPTED by node, tx {} [{}]", tx_id.to_hex(), now());
            }
            Err(e) => {
                println!("[probe] {label}: REJECTED/FAILED [{}]:\n{e:#?}", now());
            }
        }
    }
    Ok(())
}
