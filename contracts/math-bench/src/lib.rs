// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

// Phase 2 in-VM cycle microbenchmarks of the amm-math crate (DESIGN.md
// "Phase plan adjustments"). Each procedure runs one amm-math primitive on
// hardcoded inputs (all u128 values stay INTERNAL to the procedure bodies;
// the interface only returns a folded Felt so nothing is dead-code
// eliminated) and asserts the result against the natively computed value,
// so a wrong in-VM u128 lowering fails the transaction instead of silently
// producing a bogus cycle count.
//
// This crate also closes Phase 1's deferred gate: amm-math must compile
// under cargo-miden as a dependency.

use amm_math::{muldiv, swap_math, tick_math};
use miden::{assert_eq, component, component_storage, Felt, StorageValue, Word};

/// XOR-folds a u128 into a u32 so results can cross the component
/// interface as a single Felt.
fn fold_u128(x: u128) -> u32 {
    (x ^ (x >> 32) ^ (x >> 64) ^ (x >> 96)) as u32
}

/// Storage layout for the math-bench component (a component needs at least
/// one storage field; the benches never touch it).
#[component_storage]
struct MathBenchStorage {
    /// Unused scratch slot.
    #[storage(description = "unused scratch slot")]
    scratch: StorageValue<Word>,
}

/// In-VM cycle microbenchmarks over amm-math primitives.
#[component]
trait MathBench {
    /// No-op: establishes the baseline transaction + dispatch overhead.
    fn baseline_noop(&self) -> Felt;
    /// `muldiv::mul_div_floor` with representative operands (256-bit intermediate).
    fn bench_mul_div(&self) -> Felt;
    /// `tick_math::get_sqrt_ratio_at_tick` at a mid-range tick (12345).
    fn bench_sqrt_ratio_mid(&self) -> Felt;
    /// `tick_math::get_sqrt_ratio_at_tick` at the worst-case positive tick (+443,636).
    fn bench_sqrt_ratio_max_pos(&self) -> Felt;
    /// `tick_math::get_sqrt_ratio_at_tick` at the worst-case negative tick (-443,636).
    fn bench_sqrt_ratio_max_neg(&self) -> Felt;
    /// `tick_math::get_tick_at_sqrt_ratio` (binary search, ~20 forward evaluations).
    fn bench_tick_at_sqrt_ratio(&self) -> Felt;
    /// `swap_math::compute_swap_step`, one step, no tick crossing.
    fn bench_swap_step(&self) -> Felt;
}

#[component]
impl MathBench for MathBenchStorage {
    fn baseline_noop(&self) -> Felt {
        Felt::from_u32(0)
    }

    fn bench_mul_div(&self) -> Felt {
        // floor(1e21 * 2^96 / 79244113692157255927378338509) -- the product
        // overflows 128 bits, exercising the 256-bit limb path + Knuth-D.
        let r = muldiv::mul_div_floor(
            1_000_000_000_000_000_000_000u128,
            79_228_162_514_264_337_593_543_950_336u128, // 2^96
            79_244_113_692_157_255_927_378_338_509u128,
        );
        let folded = fold_u128(r);
        // Natively computed: fold(999798708356372253644) = 3641624556.
        assert_eq(Felt::from_u32(folded), Felt::from_u32(3_641_624_556));
        Felt::from_u32(folded)
    }

    fn bench_sqrt_ratio_mid(&self) -> Felt {
        let r = tick_math::get_sqrt_ratio_at_tick(12_345);
        let folded = fold_u128(r);
        // Natively computed: fold(146870458338965608271414022015).
        assert_eq(Felt::from_u32(folded), Felt::from_u32(2_249_825_582));
        Felt::from_u32(folded)
    }

    fn bench_sqrt_ratio_max_pos(&self) -> Felt {
        let r = tick_math::get_sqrt_ratio_at_tick(443_636);
        let folded = fold_u128(r);
        // Natively computed: fold(MAX_SQRT_RATIO).
        assert_eq(Felt::from_u32(folded), Felt::from_u32(2_568_944_252));
        Felt::from_u32(folded)
    }

    fn bench_sqrt_ratio_max_neg(&self) -> Felt {
        let r = tick_math::get_sqrt_ratio_at_tick(-443_636);
        let folded = fold_u128(r);
        // Natively computed: fold(MIN_SQRT_RATIO).
        assert_eq(Felt::from_u32(folded), Felt::from_u32(1_319_134_841));
        Felt::from_u32(folded)
    }

    fn bench_tick_at_sqrt_ratio(&self) -> Felt {
        // Input: get_sqrt_ratio_at_tick(12345) + a small delta, so the
        // binary search does full work and must resolve to tick 12345.
        let t = tick_math::get_tick_at_sqrt_ratio(146_870_458_351_311_287_172_648_589_905u128);
        let folded = t as u32;
        assert_eq(Felt::from_u32(folded), Felt::from_u32(12_345));
        Felt::from_u32(folded)
    }

    fn bench_swap_step(&self) -> Felt {
        // One exact-in step: current price at tick 0 (2^96), target at tick
        // -60, L = 1e18, amount_in = 1e15, fee = 3000 pips. The step ends
        // inside the range (no tick crossing), verified natively.
        let (next, amount_in, amount_out, fee) = swap_math::compute_swap_step(
            79_228_162_514_264_337_593_543_950_336u128, // sqrt ratio at tick 0
            78_990_846_045_029_531_151_608_375_686u128, // sqrt ratio at tick -60
            1_000_000_000_000_000_000u128,
            1_000_000_000_000_000i128,
            3_000,
        );
        let folded =
            fold_u128(next) ^ fold_u128(amount_in) ^ fold_u128(amount_out) ^ fold_u128(fee);
        // Natively computed over (79149250711305166342700278159,
        // 997000000000000, 996006981039903, 3000000000000).
        assert_eq(Felt::from_u32(folded), Felt::from_u32(1_539_353_219));
        Felt::from_u32(folded)
    }
}
