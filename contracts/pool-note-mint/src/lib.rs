// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

//! Phase 2 test-harness mint note: minimal driver forwarding to the pool
//! component's `mint()` procedure (single call, no branching).
//!
//! Reads its own storage/assets in the NOTE context and forwards them as
//! arguments (see pool-note-swap / clamm-pool docs for the rationale).

use miden::*;

/// Native account of the note: the clamm-pool `ClammPool` component.
#[account(clamm_pool::ClammPool)]
pub struct Pool;

/// Note storage layout (DESIGN Part 2 mint note; ticks offset-encoded as
/// tick + 2^19, liquidity as 4 little-endian u32 limbs).
#[note]
struct PoolMintNote {
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
impl PoolMintNote {
    #[note_script]
    fn run(self, _arg: Word, account: &mut Pool) {
        // One or two pool-token assets; absent slots pass amount 0.
        let assets = active_note::get_assets();
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
}
