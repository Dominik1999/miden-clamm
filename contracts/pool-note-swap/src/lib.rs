// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

//! Phase 2 test-harness swap note: a minimal driver that forwards to the
//! pool component's `swap()` procedure (single call, no branching -- the
//! P2IDE-style reclaim/refund branching is Phase 3).
//!
//! The note reads its OWN storage (via the `#[note]` macro) and assets
//! (via `active_note::get_assets()`) in the NOTE context and forwards them
//! as arguments: the memory-writing active-note reads return empty from
//! the account context at compiler v0.9 / SDK 0.13 (see the clamm-pool
//! crate docs). The pool revalidates the asset by kernel-side
//! reconstruction and reads sender/serial from kernel state itself.

use miden::*;

/// Native account of the note: the clamm-pool `ClammPool` component.
#[account(clamm_pool::ClammPool)]
pub struct Pool;

/// Note storage layout (DESIGN Part 2 swap note).
#[note]
struct PoolSwapNote {
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
impl PoolSwapNote {
    #[note_script]
    fn run(self, _arg: Word, account: &mut Pool) {
        // The swap note must carry exactly one (fungible) input asset.
        let assets = active_note::get_assets();
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
}
