// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

//! Phase 3 PRODUCTION collect note: the P2IDE-style two-path script
//! (DESIGN Part 2 collect-note flow). Carries no assets.
//!
//! - **Path A (executing account == pool_id from note storage):** forwards
//!   the note's kernel-read storage to the pool component's `collect()`
//!   procedure (payout targets the kernel-read note sender inside the
//!   pool).
//! - **Path B (executing account == note sender):** no-op cleanup — no
//!   assets, no deadline gate; the script returns without calling the
//!   pool.
//! - **Anyone else:** panic (wrong pool or unauthorized consumer).
//!
//! Only the `ClammPool` interface is bound: Path B makes no cross-context
//! calls, so no wallet interface is required.

use miden::*;

/// Native account of the note on Path A: the clamm-pool `ClammPool`
/// component.
#[account(clamm_pool::ClammPool)]
pub struct Pool;

/// Note storage layout (DESIGN Part 2 collect note; ticks offset-encoded
/// as tick + 2^19; identical to the Phase 2 harness note).
#[note]
struct AmmCollectNote {
    pool_id_suffix: Felt,
    pool_id_prefix: Felt,
    tick_lower_off: Felt,
    tick_upper_off: Felt,
}

#[note]
impl AmmCollectNote {
    #[note_script]
    fn run(self, _arg: Word, account: &mut Pool) {
        // Branch condition via canonical felt comparisons (the derived
        // `AccountId` PartialEq miscompiles as a branch condition at
        // compiler v0.9; see contracts/amm-note-swap docs).
        let executing = active_account::get_id();
        let is_pool = (executing.prefix.as_canonical_u64()
            == self.pool_id_prefix.as_canonical_u64())
            & (executing.suffix.as_canonical_u64() == self.pool_id_suffix.as_canonical_u64());

        if is_pool {
            account.collect(self.tick_lower_off, self.tick_upper_off);
        }
        if !is_pool {
            // Path B: sender cleanup no-op (no assets to move).
            let sender = active_note::get_sender();
            assert!(
                executing == sender,
                "amm-collect: wrong pool or unauthorized consumer"
            );
        }
    }
}
