// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

//! Rust-SDK basic wallet component (mirrors the compiler's
//! `examples/basic-wallet`). The Phase 3 production AMM notes bind this
//! interface for the sender-reclaim path: a cross-context `call` generated
//! from a Rust-SDK WIT binding targets the MAST root of THIS package's
//! exported procedure, so reclaim-capable sender accounts must carry this
//! component (the standard miden-standards MASM `BasicWallet` exports a
//! different MAST root and cannot service these calls).

use miden::{component, component_storage, output_note, Asset, NoteIdx};

#[component_storage]
struct BasicWalletStorage;

/// API of the basic wallet account component.
#[component]
trait BasicWallet {
    /// Adds an asset to the account.
    fn receive_asset(&mut self, asset: Asset);

    /// Moves an asset from the account to the note identified by `note_idx`.
    fn move_asset_to_note(&mut self, asset: Asset, note_idx: NoteIdx);
}

#[component]
impl BasicWallet for BasicWalletStorage {
    fn receive_asset(&mut self, asset: Asset) {
        self.add_asset(asset);
    }

    fn move_asset_to_note(&mut self, asset: Asset, note_idx: NoteIdx) {
        self.remove_asset(asset);
        output_note::add_asset(asset, note_idx);
    }
}
