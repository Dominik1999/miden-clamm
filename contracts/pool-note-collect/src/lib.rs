// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

//! Phase 2 test-harness collect note: minimal driver forwarding to the
//! pool component's `collect()` procedure (single call, no branching).
//! Carries no assets; authorization derives from the kernel-read note
//! sender inside the pool component.

use miden::*;

/// Native account of the note: the clamm-pool `ClammPool` component.
#[account(clamm_pool::ClammPool)]
pub struct Pool;

/// Note storage layout (DESIGN Part 2 collect note; ticks offset-encoded
/// as tick + 2^19).
#[note]
struct PoolCollectNote {
    pool_id_suffix: Felt,
    pool_id_prefix: Felt,
    tick_lower_off: Felt,
    tick_upper_off: Felt,
}

#[note]
impl PoolCollectNote {
    #[note_script]
    fn run(self, _arg: Word, account: &mut Pool) {
        account.collect(self.tick_lower_off, self.tick_upper_off);
    }
}
