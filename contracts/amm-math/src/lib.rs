//! Fixed-point AMM math for a Uniswap-v3-style pool on Miden.
//!
//! Number formats (spec of record: DESIGN.md Part 3 — do not deviate):
//!
//! - **sqrtPrice**: Q64.96 held in `u128`. Supported tick range is
//!   ±443,636; the min/max sqrt ratios are the values at those ticks and
//!   every entry point asserts its price inputs against that range.
//! - **liquidity**: `u64` (signed deltas: `i64`, via `i128` intermediates).
//! - **fee rate**: `fee_pips: u32`, hundredths of a bip (500, 3000, 10000).
//! - **amounts**: `u64` externally (Miden asset amounts), `u128` internally
//!   where intermediates require.
//!
//! The crate is `no_std`, allocation-free, and has zero non-dev
//! dependencies. All wide intermediates use fixed-size little-endian
//! `u64`-limb arrays (see [`wide`]).
//!
//! **Rounding policy**: every public function documents its rounding
//! direction; whenever Uniswap v3 semantics leave any ambiguity, rounding is
//! resolved so the pool never loses ("rounds toward the pool").
//!
//! **Failure policy**: arithmetic that cannot be represented (division by
//! zero, quotient overflow, liquidity under/overflow, out-of-range inputs)
//! panics. A panic is the desired behavior on Miden: the transaction fails.

#![cfg_attr(not(test), no_std)]
#![deny(unsafe_code)]

pub mod liquidity_math;
pub mod muldiv;
pub mod sqrt_price_math;
pub mod swap_math;
pub mod tick_math;
pub mod wide;
