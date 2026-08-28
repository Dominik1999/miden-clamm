//! Queries the public testnet for the ntx-builder's view of network notes:
//! `GetNetworkNoteStatus` (status, last error, attempt count) plus on-chain
//! inclusion (`GetNotesById`) and nullifier spent-status.
//!
//! Run: cargo run -p integration --bin note_status --release -- <note_id_hex>...
//!      [--nullifier <nullifier_hex>]...

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use miden_client::block::BlockNumber;
use miden_client::note::{NoteId, Nullifier};
use miden_client::rpc::{Endpoint, GrpcClient, NodeRpcClient};

#[tokio::main]
async fn main() -> Result<()> {
    let endpoint = Endpoint::testnet();
    let rpc: Arc<GrpcClient> = Arc::new(GrpcClient::new(&endpoint, 30_000));

    match rpc.get_status_unversioned().await {
        Ok(s) => {
            println!("node version: {} (chain tip {})", s.version, s.chain_tip);
            if let Some(bp) = &s.block_producer {
                println!("block-producer version: {} status: {}", bp.version, bp.status);
            }
        }
        Err(e) => println!("status query error: {e}"),
    }

    let mut note_ids: Vec<NoteId> = Vec::new();
    let mut nullifiers: BTreeSet<Nullifier> = BTreeSet::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--nullifier" {
            let n = args.next().context("--nullifier needs a value")?;
            nullifiers.insert(
                Nullifier::from_hex(&n).map_err(|e| anyhow::anyhow!("bad nullifier hex: {e}"))?,
            );
        } else {
            note_ids.push(NoteId::try_from_hex(&a).map_err(|e| anyhow::anyhow!("bad note id {a}: {e}"))?);
        }
    }

    for id in &note_ids {
        println!("=== note {} ===", id.to_hex());
        match rpc.get_notes_by_id(&[*id]).await {
            Ok(found) if !found.is_empty() => {
                for f in &found {
                    println!(
                        "  on-chain: committed (inclusion block {})",
                        f.inclusion_proof().location().block_num()
                    );
                    if let miden_client::rpc::domain::note::FetchedNote::Public(note, _) = f {
                        let felts: Vec<u64> = note
                            .storage()
                            .items()
                            .iter()
                            .map(|x| x.as_canonical_u64())
                            .collect();
                        println!("  storage felts:     {felts:?}");
                        println!("  sender:            {}", note.metadata().sender().to_hex());
                    }
                }
            }
            Ok(_) => println!("  on-chain: NOT found by GetNotesById"),
            Err(e) => println!("  on-chain: GetNotesById error: {e}"),
        }
        match rpc.get_network_note_status(*id).await {
            Ok(info) => {
                println!("  ntx status:        {}", info.status);
                println!("  attempt_count:     {}", info.attempt_count);
                println!("  last_attempt_blk:  {:?}", info.last_attempt_block_num);
                println!("  last_error:        {:?}", info.last_error);
            }
            Err(e) => println!("  ntx status: RPC error: {e}"),
        }
    }

    if !nullifiers.is_empty() {
        match rpc
            .get_nullifier_commit_heights(nullifiers, BlockNumber::from(0u32))
            .await
        {
            Ok(heights) => {
                for (n, h) in heights {
                    match h {
                        Some(h) => println!(
                            "nullifier {}: SPENT, committed at block {}",
                            n.as_word().to_hex(),
                            h
                        ),
                        None => println!("nullifier {}: UNSPENT", n.as_word().to_hex()),
                    }
                }
            }
            Err(e) => println!("nullifier query error: {e}"),
        }
    }
    Ok(())
}
