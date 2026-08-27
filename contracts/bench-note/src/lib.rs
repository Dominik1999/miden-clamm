// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

use miden::*;

/// Native account of the note: exposes the `math-bench` component methods
/// gathered from the `math-bench` package.
#[account(math_bench::MathBench)]
pub struct Wallet;

/// Selects which amm-math microbenchmark procedure to run. Selector 0 is
/// the no-op baseline; the dispatch shape is identical on every path, so
/// subtracting the baseline cancels it out of the per-primitive numbers.
///
/// NOTE (compiler v0.9.0 workaround): the dispatch is a flat sequence of
/// independent `if` blocks, NOT a `match` / `if-else` chain. Chained
/// conditionals with 3+ cross-context account calls miscompile: the
/// transaction fails with "assertion failed with error code
/// 13397901377689146813" (an 'entered unreachable code' guard) before any
/// arm executes. Flat single-arm `if`s compile and run correctly.
#[note]
struct BenchNote {
    selector: u32,
}

#[note]
impl BenchNote {
    #[note_script]
    fn run(self, _arg: Word, account: &mut Wallet) {
        let s = self.selector;
        if s == 0 {
            account.baseline_noop();
        }
        if s == 1 {
            account.bench_mul_div();
        }
        if s == 2 {
            account.bench_sqrt_ratio_mid();
        }
        if s == 3 {
            account.bench_sqrt_ratio_max_pos();
        }
        if s == 4 {
            account.bench_sqrt_ratio_max_neg();
        }
        if s == 5 {
            account.bench_tick_at_sqrt_ratio();
        }
        if s == 6 {
            account.bench_swap_step();
        }
    }
}
