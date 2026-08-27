// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

//! Phase 3 PRODUCTION swap note: the P2IDE-style two-path script
//! (DESIGN Part 2 swap-note flow).
//!
//! - **Path A (executing account == pool_id from note storage):** forwards
//!   the note's own kernel-read storage and assets to the pool component's
//!   `swap()` procedure, exactly like the Phase 2 harness note. The pool
//!   handles the hybrid failure semantics internally (pre-deadline slippage
//!   panics; at/after the deadline it consumes-and-refunds via P2ID).
//! - **Path B (executing account == note sender):** failsafe reclaim for
//!   notes the ntx-builder discarded. Only valid at/after the deadline
//!   (`tx::get_block_number() >= deadline_height`, otherwise panic); moves
//!   the note's assets into the sender's wallet via `receive_asset`.
//! - **Anyone else:** panic (wrong pool or unauthorized consumer).
//!
//! Dual-interface binding: `#[account(...)]` lists BOTH the `ClammPool`
//! and `BasicWallet` interfaces on one wrapper. Each generated method is a
//! lazy cross-context `call` against the MAST root of the corresponding
//! dependency package's procedure, so a call into an interface the
//! executing account does not expose only panics IF that call site is
//! reached — and each path only calls the interface its executing account
//! actually has (the pool never runs Path B, the sender wallet never runs
//! Path A). Reclaim-capable senders must carry the Rust-SDK
//! `basic-wallet` component (see `contracts/basic-wallet`).
//!
//! Compiler v0.9 constraints (DESIGN Part 5 1b, plus two new findings
//! probe-verified in Phase 3):
//! - at most 2 branch arms with cross-context calls; this script uses two
//!   flat sequential `if` blocks (the bench-note-verified shape);
//! - `active_note::get_assets()` must have exactly ONE call site: a
//!   duplicated call (one per arm) miscompiles the whole script, so the
//!   call is hoisted above the branch;
//! - the derived `AccountId` PartialEq must not be the branch condition
//!   (`if executing == pool_id` miscompiles; canonical felt comparisons
//!   work, and the same derived eq inside an `assert!` works).

use miden::*;

/// Accounts this note can execute against: the clamm-pool (Path A) or a
/// Rust-SDK basic wallet (Path B).
#[account(clamm_pool::ClammPool, basic_wallet::BasicWallet)]
pub struct Consumer;

/// Note storage layout (DESIGN Part 2 swap note; identical to the Phase 2
/// harness note).
#[note]
struct AmmSwapNote {
    pool_id_suffix: Felt,
    pool_id_prefix: Felt,
    /// 0 = zero_for_one (token0 in), 1 = one_for_zero (token1 in).
    direction: Felt,
    /// Minimum acceptable output amount, little-endian u32 limbs of a u64.
    min_out_lo: Felt,
    min_out_hi: Felt,
    recipient_suffix: Felt,
    recipient_prefix: Felt,
    deadline_height: Felt,
}

#[note]
impl AmmSwapNote {
    #[note_script]
    fn run(self, _arg: Word, account: &mut Consumer) {
        // Branch condition via canonical felt comparisons (the derived
        // `AccountId` PartialEq miscompiles as a branch condition at
        // compiler v0.9; see the crate docs).
        let executing = active_account::get_id();
        let is_pool = (executing.prefix.as_canonical_u64()
            == self.pool_id_prefix.as_canonical_u64())
            & (executing.suffix.as_canonical_u64() == self.pool_id_suffix.as_canonical_u64());

        // Single get_assets call site, hoisted above the branch
        // (duplicated calls miscompile; see the crate docs).
        let assets = active_note::get_assets();

        if is_pool {
            // Path A: the pool executes. The swap note must carry exactly
            // one (fungible) input asset; forward storage + asset felts.
            assert_eq(Felt::from_u32(assets.len() as u32), felt!(1));
            let input = assets[0];
            account.swap(
                input.key[2],
                input.key[3],
                input.value[0],
                self.direction,
                self.min_out_lo,
                self.min_out_hi,
                self.recipient_suffix,
                self.recipient_prefix,
                self.deadline_height,
            );
        }
        if !is_pool {
            // Path B: sender reclaim, only at/after the deadline.
            let sender = active_note::get_sender();
            assert!(
                executing == sender,
                "amm-swap: wrong pool or unauthorized consumer"
            );
            let block = tx::get_block_number();
            assert!(
                block.as_canonical_u64() >= self.deadline_height.as_canonical_u64(),
                "amm-swap: reclaim before deadline"
            );
            for asset in assets {
                account.receive_asset(asset);
            }
        }
    }
}
