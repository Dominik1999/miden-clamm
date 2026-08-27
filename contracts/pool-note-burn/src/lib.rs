// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

//! Phase 2 test-harness burn note: minimal driver forwarding to the pool
//! component's `burn()` procedure (single call, no branching). Carries no
//! assets; authorization derives from the kernel-read note sender inside
//! the pool component.

use miden::*;

/// Native account of the note: the clamm-pool `ClammPool` component.
#[account(clamm_pool::ClammPool)]
pub struct Pool;

/// Note storage layout (DESIGN Part 2 burn note; ticks offset-encoded as
/// tick + 2^19, liquidity as 4 little-endian u32 limbs).
#[note]
struct PoolBurnNote {
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
impl PoolBurnNote {
    #[note_script]
    fn run(self, _arg: Word, account: &mut Pool) {
        account.burn(
            self.tick_lower_off,
            self.tick_upper_off,
            self.liq_l0,
            self.liq_l1,
            self.liq_l2,
            self.liq_l3,
        );
    }
}
