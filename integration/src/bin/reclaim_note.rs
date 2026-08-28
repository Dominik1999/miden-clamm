//! Reclaims a past-deadline AMM network note back to its sender wallet
//! (note script Path B: executing account == sender AND block >= deadline).
//!
//! Run: cargo run -p integration --bin reclaim_note --release -- \
//!        <store.sqlite3> <keystore_dir> <sender_account_hex> <note_id_hex>

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use miden_client::account::AccountId;
use miden_client::builder::ClientBuilder;
use miden_client::keystore::FilesystemKeyStore;
use miden_client::note::NoteId;
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};
use miden_client::store::TransactionFilter;
use miden_client::transaction::{TransactionRequestBuilder, TransactionStatus};
use miden_client_sqlite_store::ClientBuilderSqliteExt;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        bail!("usage: reclaim_note <store.sqlite3> <keystore_dir> <sender_hex> <note_id_hex>");
    }
    let store_path = PathBuf::from(&args[1]);
    let keystore_path = PathBuf::from(&args[2]);
    let sender = AccountId::from_hex(&args[3]).context("sender id")?;
    let note_id = NoteId::try_from_hex(&args[4]).map_err(|e| anyhow::anyhow!("note id: {e}"))?;

    let endpoint = Endpoint::testnet();
    let rpc: Arc<GrpcClient> = Arc::new(GrpcClient::new(&endpoint, 30_000));
    let keystore = Arc::new(FilesystemKeyStore::new(keystore_path).context("keystore")?);
    let mut client = ClientBuilder::new()
        .rpc(Arc::new(GrpcClient::new(&endpoint, 30_000)))
        .sqlite_store(store_path)
        .authenticator(keystore)
        .in_debug_mode(true.into())
        .build()
        .await?;
    let sync = client.sync_state().await?;
    println!("[reclaim] chain tip {}", sync.block_num);

    // Fetch the full public note from the node.
    let fetched = rpc.get_notes_by_id(&[note_id]).await.context("get_notes_by_id")?;
    let note = match fetched.into_iter().next() {
        Some(miden_client::rpc::domain::note::FetchedNote::Public(note, _)) => note,
        Some(_) => bail!("note {} is private; cannot reconstruct details", note_id.to_hex()),
        None => bail!("note {} not found on chain", note_id.to_hex()),
    };
    println!("[reclaim] fetched note {} (sender {})", note.id().to_hex(), note.metadata().sender().to_hex());

    let req = TransactionRequestBuilder::new()
        .build_consume_notes(vec![note])
        .context("building consume request")?;
    let tx_id = client
        .submit_new_transaction(sender, req)
        .await
        .context("submitting reclaim tx")?;
    println!("[reclaim] reclaim tx submitted: {}", tx_id.to_hex());

    loop {
        client.sync_state().await?;
        let txs = client.get_transactions(TransactionFilter::Ids(vec![tx_id])).await?;
        if !txs.is_empty() && matches!(txs[0].status, TransactionStatus::Committed { .. }) {
            println!("[reclaim] committed");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    let acct = client.get_account(sender).await?.context("sender not tracked")?;
    println!("[reclaim] sender vault after reclaim:");
    for asset in acct.vault().assets() {
        println!("  {:?}", asset);
    }
    Ok(())
}
