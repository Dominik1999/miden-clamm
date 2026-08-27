#![allow(dead_code)]
#![allow(unused_imports)]
//! Shared helpers for the amm-math-masm verification suite.

use amm_math_masm::{assemble_library, assemble_program};
use miden_assembly::Library;
use miden_processor::Program;
use std::sync::OnceLock;

pub use amm_math_masm::{execute, execute_with_cycles, limbs_to_u128, u128_to_limbs};

/// The assembled `amm::math` library, shared across a test binary.
pub fn library() -> &'static Library {
    static LIB: OnceLock<Library> = OnceLock::new();
    LIB.get_or_init(assemble_library)
}

/// Assembles a driver program against the shared library.
pub fn program(source: &str) -> Program {
    assemble_program(library(), source)
}

/// Converts little-endian u32 limbs to little-endian u64 limbs (len must be even).
pub fn u32_to_u64_limbs(limbs: &[u64]) -> Vec<u64> {
    assert!(limbs.len() % 2 == 0);
    limbs
        .chunks(2)
        .map(|c| c[0] | (c[1] << 32))
        .collect()
}

/// Converts little-endian u64 limbs to little-endian u32 limbs.
pub fn u64_to_u32_limbs(limbs: &[u64]) -> Vec<u64> {
    let mut out = Vec::with_capacity(limbs.len() * 2);
    for &l in limbs {
        out.push(l & 0xFFFF_FFFF);
        out.push(l >> 32);
    }
    out
}

/// Deterministic RNG for reproducible property tests.
pub fn rng(seed: u64) -> rand_chacha::ChaCha8Rng {
    use rand::SeedableRng;
    rand_chacha::ChaCha8Rng::seed_from_u64(seed)
}

/// Random u128 with a random bit-width in `1..=max_bits` (dense coverage of all sizes).
pub fn random_u128(r: &mut impl rand::Rng, max_bits: u32) -> u128 {
    let bits = r.random_range(1..=max_bits);
    let raw: u128 = r.random();
    if bits >= 128 {
        raw
    } else {
        raw & ((1u128 << bits) - 1)
    }
}

/// Reverses a top-first stack listing into the advice order that reproduces it on the
/// operand stack after `repeat.N adv_push end` (first-consumed value lands deepest).
pub fn advice_for_stack(stack_top_first: &[u64]) -> Vec<u64> {
    let mut v = stack_top_first.to_vec();
    v.reverse();
    v
}

/// Runs the oracle, returning `None` when it panics (used for panic-parity checks:
/// whenever the oracle panics, the MASM execution must fail too).
pub fn catch<T>(f: impl FnOnce() -> T + std::panic::UnwindSafe) -> Option<T> {
    std::panic::catch_unwind(f).ok()
}
