//! Stage-A testnet recon probe: verifies that this repo's miden-client 0.15
//! can talk to the public Miden testnet at all.
//!
//! Steps:
//!   1. connect to `Endpoint::testnet()` (https://rpc.testnet.miden.io)
//!   2. `sync_state` — proves proto/version compatibility end-to-end
//!   3. `get_account_details` on a known public account (pass an account id
//!      hex as the first arg, defaults to the template's public counter)
//!
//! Run: cargo run -p integration --bin testnet_probe --release [account_hex]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use miden_client::account::AccountId;
use miden_client::builder::ClientBuilder;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client_sqlite_store::ClientBuilderSqliteExt;

#[tokio::main]
async fn main() -> Result<()> {
    let scratch = std::env::temp_dir().join(format!("clamm-testnet-probe-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;

    let endpoint = Endpoint::testnet();
    println!("[probe] endpoint: {endpoint}");

    let rpc: Arc<GrpcClient> = Arc::new(GrpcClient::new(&endpoint, 30_000));
    let keystore = Arc::new(
        FilesystemKeyStore::new(scratch.join("keystore")).context("initializing keystore")?,
    );
    let mut client = ClientBuilder::new()
        .rpc(Arc::new(GrpcClient::new(&endpoint, 30_000)))
        .sqlite_store(PathBuf::from(scratch.join("store.sqlite3")))
        .authenticator(keystore)
        .build()
        .await
        .context("building testnet client")?;

    let sync = client.sync_state().await.context("testnet sync_state failed")?;
    println!("[probe] sync OK; chain tip: {}", sync.block_num);

    let account_hex = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0x4dcaee76ffebfc511e06582702289d".to_string());
    let id = AccountId::from_hex(&account_hex).context("parsing account id")?;
    match rpc.get_account_details(id).await {
        Ok(Some(acct)) => {
            println!(
                "[probe] account {} found: nonce {}, commitment {}",
                id.to_hex(),
                acct.nonce().as_canonical_u64(),
                acct.to_commitment().to_hex(),
            );
        }
        Ok(None) => println!("[probe] account {} not found (private or unknown)", id.to_hex()),
        Err(e) => println!("[probe] get_account_details error: {e:#}"),
    }
    Ok(())
}
