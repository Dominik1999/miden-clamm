//! Shared test-only reference arithmetic: U512/U1024 big integers used to
//! verify the crate's limb arithmetic and fixed-point algorithms.

#![allow(dead_code)]

use primitive_types::U512;

// The allow covers upstream code expanded from the uint crate's macro.
#[allow(clippy::manual_div_ceil)]
mod u1024 {
    uint::construct_uint! {
        pub struct U1024(16);
    }
}
pub use u1024::U1024;

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

pub fn u512_from_limbs(l: &[u64]) -> U512 {
    assert!(l.len() <= 8);
    let mut a = [0u64; 8];
    a[..l.len()].copy_from_slice(l);
    U512(a)
}

pub fn u1024_from_limbs(l: &[u64]) -> U1024 {
    assert!(l.len() <= 16);
    let mut a = [0u64; 16];
    a[..l.len()].copy_from_slice(l);
    U1024(a)
}

pub fn u512(x: u128) -> U512 {
    U512::from(x)
}

pub fn u1024(x: u128) -> U1024 {
    U1024::from(x)
}

pub fn u512_to_u128(x: U512) -> u128 {
    assert!(x.bits() <= 128, "u512_to_u128: overflow");
    x.low_u128()
}

pub fn fits_u128(x: U512) -> bool {
    x.bits() <= 128
}

// ---------------------------------------------------------------------------
// U1024 fixed-point reference for tick math
// ---------------------------------------------------------------------------

/// Fractional bits of the high-precision fixed-point reference.
pub const F: usize = 384;

/// Floor integer square root via Newton iteration.
pub fn isqrt_u1024(n: U1024) -> U1024 {
    if n.is_zero() {
        return U1024::zero();
    }
    let bits = n.bits();
    // Initial guess >= sqrt(n).
    let mut x = U1024::one() << (bits / 2 + 1);
    loop {
        let y = (x + n / x) >> 1;
        if y >= x {
            break;
        }
        x = y;
    }
    assert!(x * x <= n, "isqrt: not a lower bound");
    assert!((x + 1) * (x + 1) > n, "isqrt: not tight");
    x
}

/// `p[i] = sqrt(1.0001)^-(2^i)` in Q.F fixed point (floor at each step).
///
/// sqrt(1.0001) is computed by exact integer sqrt at Q.F precision, then
/// the inverse powers by repeated squaring. Accumulated relative error is
/// ~2^-(F-19), i.e. far below anything the Q128 rounding can observe.
pub fn inv_sqrt_powers() -> [U1024; 19] {
    let x = (U1024::from(10001u64) << F) / U1024::from(10000u64); // 1.0001 in Q.F
    let s = isqrt_u1024(x << F); // sqrt(1.0001) in Q.F
    let inv = (U1024::one() << (2 * F)) / s; // 1/sqrt(1.0001) in Q.F
    let mut out = [U1024::zero(); 19];
    let mut p = inv;
    for (i, o) in out.iter_mut().enumerate() {
        if i > 0 {
            p = (p * p) >> F;
        }
        *o = p;
    }
    out
}

/// `round(2^128 * sqrt(1.0001)^-(2^i))` for i in 0..19 — the exact values
/// the library's `TICK_BIT_CONSTANTS` table must contain.
pub fn derive_tick_constants() -> [u128; 19] {
    let powers = inv_sqrt_powers();
    let mut out = [0u128; 19];
    for (o, p) in out.iter_mut().zip(powers.iter()) {
        let c = ((*p << 128) + (U1024::one() << (F - 1))) >> F;
        assert!(c.bits() <= 128);
        *o = c.low_u128();
    }
    out
}

/// High-precision reference: `sqrt(1.0001)^tick` in Q.F fixed point.
/// Relative error ~2^-(F-25): effectively exact against a Q64.96 result.
pub fn ref_sqrt_ratio_qf(powers: &[U1024; 19], tick: i32) -> U1024 {
    let mut r = U1024::one() << F;
    let a = tick.unsigned_abs();
    for (i, p) in powers.iter().enumerate() {
        if (a >> i) & 1 == 1 {
            r = (r * *p) >> F;
        }
    }
    if tick > 0 {
        r = (U1024::one() << (2 * F)) / r;
    }
    r
}

// ---------------------------------------------------------------------------
// U512 reference ports of SqrtPriceMath (full-precision formulas, Uniswap
// rounding directions)
// ---------------------------------------------------------------------------

pub fn div_round(n: U512, d: U512, round_up: bool) -> U512 {
    let (q, r) = n.div_mod(d);
    if round_up && !r.is_zero() {
        q + U512::one()
    } else {
        q
    }
}

/// Two-step `amount0` exactly as Uniswap computes it (round at both steps).
pub fn ref_amount0(sqrt_a: u128, sqrt_b: u128, liquidity: u128, round_up: bool) -> U512 {
    let (lo, hi) = if sqrt_a <= sqrt_b {
        (sqrt_a, sqrt_b)
    } else {
        (sqrt_b, sqrt_a)
    };
    if liquidity == 0 || lo == hi {
        return U512::zero();
    }
    let n1 = u512(liquidity) << 96;
    let num = n1 * u512(hi - lo);
    let step1 = div_round(num, u512(hi), round_up);
    div_round(step1, u512(lo), round_up)
}

pub fn ref_amount1(sqrt_a: u128, sqrt_b: u128, liquidity: u128, round_up: bool) -> U512 {
    let (lo, hi) = if sqrt_a <= sqrt_b {
        (sqrt_a, sqrt_b)
    } else {
        (sqrt_b, sqrt_a)
    };
    div_round(u512(liquidity) * u512(hi - lo), U512::one() << 96, round_up)
}

/// Reference next price from a token0 amount. `None` where the library
/// panics (removal exceeding reserves).
pub fn ref_next_from_amount0(p: u128, liquidity: u128, amount: u128, add: bool) -> Option<U512> {
    if amount == 0 {
        return Some(u512(p));
    }
    let n1 = u512(liquidity) << 96;
    let product = u512(amount) * u512(p);
    let denominator = if add {
        n1 + product
    } else {
        if product >= n1 {
            return None;
        }
        n1 - product
    };
    Some(div_round(n1 * u512(p), denominator, true))
}

/// Reference next price from a token1 amount. `None` where the library
/// panics (price underflow on removal).
pub fn ref_next_from_amount1(p: u128, liquidity: u128, amount: u128, add: bool) -> Option<U512> {
    assert!(liquidity > 0);
    let shifted = u512(amount) << 96;
    if add {
        let quotient = shifted / u512(liquidity);
        Some(u512(p) + quotient)
    } else {
        let quotient = div_round(shifted, u512(liquidity), true);
        if quotient >= u512(p) {
            return None;
        }
        Some(u512(p) - quotient)
    }
}

pub fn ref_next_from_input(p: u128, l: u128, amount_in: u128, zero_for_one: bool) -> Option<U512> {
    if zero_for_one {
        ref_next_from_amount0(p, l, amount_in, true)
    } else {
        ref_next_from_amount1(p, l, amount_in, true)
    }
}

pub fn ref_next_from_output(p: u128, l: u128, amount_out: u128, zero_for_one: bool) -> Option<U512> {
    if zero_for_one {
        ref_next_from_amount1(p, l, amount_out, false)
    } else {
        ref_next_from_amount0(p, l, amount_out, false)
    }
}

// ---------------------------------------------------------------------------
// U512 reference port of SwapMath.computeSwapStep
// ---------------------------------------------------------------------------

/// Mirrors `swap_math::compute_swap_step` at U512 precision. Returns `None`
/// wherever the library would panic (a quantity exceeding `u128`, or a
/// next-price formula outside its domain).
pub fn ref_compute_swap_step(
    current: u128,
    target: u128,
    liquidity: u128,
    amount_remaining: i128,
    fee_pips: u32,
) -> Option<(u128, u128, u128, u128)> {
    const D: u128 = 1_000_000;
    let zero_for_one = current >= target;
    let exact_in = amount_remaining >= 0;

    let mut amount_in = U512::zero();
    let mut amount_out = U512::zero();
    let next: u128;

    if exact_in {
        let less_fee = u512(amount_remaining as u128) * u512(D - fee_pips as u128) / u512(D);
        amount_in = if zero_for_one {
            ref_amount0(target, current, liquidity, true)
        } else {
            ref_amount1(current, target, liquidity, true)
        };
        if !fits_u128(amount_in) {
            return None;
        }
        next = if less_fee >= amount_in {
            target
        } else {
            let n = ref_next_from_input(current, liquidity, u512_to_u128(less_fee), zero_for_one)?;
            if !fits_u128(n) {
                return None;
            }
            u512_to_u128(n)
        };
    } else {
        let requested = u512(amount_remaining.unsigned_abs());
        amount_out = if zero_for_one {
            ref_amount1(target, current, liquidity, false)
        } else {
            ref_amount0(current, target, liquidity, false)
        };
        if !fits_u128(amount_out) {
            return None;
        }
        next = if requested >= amount_out {
            target
        } else {
            let n = ref_next_from_output(
                current,
                liquidity,
                amount_remaining.unsigned_abs(),
                zero_for_one,
            )?;
            if !fits_u128(n) {
                return None;
            }
            u512_to_u128(n)
        };
    }

    let max = next == target;

    if zero_for_one {
        if !(max && exact_in) {
            amount_in = ref_amount0(next, current, liquidity, true);
        }
        if !(max && !exact_in) {
            amount_out = ref_amount1(next, current, liquidity, false);
        }
    } else {
        if !(max && exact_in) {
            amount_in = ref_amount1(current, next, liquidity, true);
        }
        if !(max && !exact_in) {
            amount_out = ref_amount0(current, next, liquidity, false);
        }
    }
    if !fits_u128(amount_in) || !fits_u128(amount_out) {
        return None;
    }

    if !exact_in {
        let requested = u512(amount_remaining.unsigned_abs());
        if amount_out > requested {
            amount_out = requested;
        }
    }

    let fee = if exact_in && next != target {
        u512(amount_remaining as u128) - amount_in
    } else {
        div_round(
            amount_in * u512(fee_pips as u128),
            u512(D - fee_pips as u128),
            true,
        )
    };
    if !fits_u128(fee) {
        return None;
    }

    Some((
        next,
        u512_to_u128(amount_in),
        u512_to_u128(amount_out),
        u512_to_u128(fee),
    ))
}
