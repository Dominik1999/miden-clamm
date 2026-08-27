// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

//! Phase 3 PRODUCTION burn note: the P2IDE-style two-path script
//! (DESIGN Part 2 burn-note flow). Carries no assets.
//!
//! - **Path A (executing account == pool_id from note storage):** forwards
//!   the note's kernel-read storage to the pool component's `burn()`
//!   procedure (position authorization derives from the kernel-read note
//!   sender inside the pool).
//! - **Path B (executing account == note sender):** no-op cleanup — the
//!   note carries no assets, so the sender may consume it at any time to
//!   remove it from the chain; the script returns without calling the
//!   pool. No deadline gate is needed.
//! - **Anyone else:** panic (wrong pool or unauthorized consumer).
//!
//! Only the `ClammPool` interface is bound: Path B makes no cross-context
//! calls, so no wallet interface is required.

use miden::*;

/// Native account of the note on Path A: the clamm-pool `ClammPool`
/// component.
#[account(clamm_pool::ClammPool)]
pub struct Pool;

/// Note storage layout (DESIGN Part 2 burn note; ticks offset-encoded as
/// tick + 2^19, liquidity as 4 little-endian u32 limbs; identical to the
/// Phase 2 harness note).
#[note]
struct AmmBurnNote {
    pool_id_suffix: Felt,
    pool_id_prefix: Felt,
    tick_lower_off: Felt,
    tick_upper_off: Felt,
    liq_l0: Felt,
    liq_l1: Felt,
    liq_l2: Felt,
    liq_l3: Felt,
}

#[note]
impl AmmBurnNote {
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
            account.burn(
                self.tick_lower_off,
                self.tick_upper_off,
                self.liq_l0,
                self.liq_l1,
                self.liq_l2,
                self.liq_l3,
            );
        }
        if !is_pool {
            // Path B: sender cleanup no-op (no assets to move).
            let sender = active_note::get_sender();
            assert!(
                executing == sender,
                "amm-burn: wrong pool or unauthorized consumer"
            );
        }
    }
}
