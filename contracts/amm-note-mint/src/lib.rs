// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

//! Phase 3 PRODUCTION mint (add-liquidity) note: the P2IDE-style two-path
//! script (DESIGN Part 2 mint-note flow).
//!
//! - **Path A (executing account == pool_id from note storage):** forwards
//!   the note's own kernel-read storage and assets to the pool component's
//!   `mint()` procedure (the pool refunds excess via P2ID, and handles the
//!   post-deadline consume-and-refund itself).
//! - **Path B (executing account == note sender):** failsafe reclaim of
//!   the max-amount assets, only at/after the deadline
//!   (`tx::get_block_number() >= deadline_height`, otherwise panic).
//! - **Anyone else:** panic (wrong pool or unauthorized consumer).
//!
//! See `contracts/amm-note-swap` for the dual-interface binding mechanism
//! and the 2-arm branching constraint (compiler v0.9, DESIGN Part 5 1b).

use miden::*;

/// Accounts this note can execute against: the clamm-pool (Path A) or a
/// Rust-SDK basic wallet (Path B).
#[account(clamm_pool::ClammPool, basic_wallet::BasicWallet)]
pub struct Consumer;

/// Note storage layout (DESIGN Part 2 mint note; ticks offset-encoded as
/// tick + 2^19, liquidity as 4 little-endian u32 limbs; identical to the
/// Phase 2 harness note).
#[note]
struct AmmMintNote {
    pool_id_suffix: Felt,
    pool_id_prefix: Felt,
    tick_lower_off: Felt,
    tick_upper_off: Felt,
    liq_l0: Felt,
    liq_l1: Felt,
    liq_l2: Felt,
    liq_l3: Felt,
    deadline_height: Felt,
}

#[note]
impl AmmMintNote {
    #[note_script]
    fn run(self, _arg: Word, account: &mut Consumer) {
        // Branch condition via canonical felt comparisons: using the
        // derived `AccountId` PartialEq (`executing == pool_id`) as the
        // branch condition miscompiles at compiler v0.9 (probe-verified:
        // the pool path is then never taken; the same derived eq inside
        // the Path B assert works fine).
        let executing = active_account::get_id();
        let is_pool = (executing.prefix.as_canonical_u64()
            == self.pool_id_prefix.as_canonical_u64())
            & (executing.suffix.as_canonical_u64() == self.pool_id_suffix.as_canonical_u64());

        // Single get_assets call site, hoisted above the branch: duplicated
        // `active_note::get_assets()` calls (one per arm) miscompile at
        // compiler v0.9 (probe-verified; see the crate docs).
        let assets = active_note::get_assets();

        if is_pool {
            // Path A: one or two pool-token assets; absent slots pass 0.
            let n = assets.len();
            assert(Felt::from_u32((n >= 1 && n <= 2) as u32));

            let zero = felt!(0);
            let (ak2, ak3, aamt) = (assets[0].key[2], assets[0].key[3], assets[0].value[0]);
            let (bk2, bk3, bamt) = if n == 2 {
                (assets[1].key[2], assets[1].key[3], assets[1].value[0])
            } else {
                (zero, zero, zero)
            };

            account.mint(
                ak2,
                ak3,
                aamt,
                bk2,
                bk3,
                bamt,
                self.tick_lower_off,
                self.tick_upper_off,
                self.liq_l0,
                self.liq_l1,
                self.liq_l2,
                self.liq_l3,
                self.deadline_height,
            );
        }
        if !is_pool {
            // Path B: sender reclaim, only at/after the deadline.
            let sender = active_note::get_sender();
            assert!(
                executing == sender,
                "amm-mint: wrong pool or unauthorized consumer"
            );
            let block = tx::get_block_number();
            assert!(
                block.as_canonical_u64() >= self.deadline_height.as_canonical_u64(),
                "amm-mint: reclaim before deadline"
            );
            for asset in assets {
                account.receive_asset(asset);
            }
        }
    }
}
