//! `u64`-limb big-integer helpers over little-endian fixed-size arrays.
//!
//! This module underpins every other module in the crate: schoolbook
//! multiplication produces exact wide products (up to 512 bits) and a
//! general Knuth Algorithm D division handles the wide dividends that
//! Uniswap-style formulas produce (`liquidity << 96` intermediates reach
//! ~384 bits; exact-denominator next-price formulas need divisors wider
//! than `u128`, up to ~257 bits).
//!
//! Conventions:
//! - Limb order is **little-endian**: `x[0]` is the least significant limb.
//! - All functions are allocation-free and `no_std`.
//! - Division by zero panics (desired Miden behavior: the tx fails).

/// Maximum number of limbs supported by [`div_rem`] for both the dividend
/// and the divisor (8 limbs = 512 bits).
pub const MAX_LIMBS: usize = 8;

#[cfg(test)]
pub(crate) static ADD_BACK_HITS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Splits a `u128` into two little-endian `u64` limbs.
#[inline]
pub fn limbs_from_u128(x: u128) -> [u64; 2] {
    [x as u64, (x >> 64) as u64]
}

/// Reassembles little-endian limbs into a `u128`.
///
/// Panics if any limb at index >= 2 is non-zero (value does not fit).
pub fn limbs_to_u128(limbs: &[u64]) -> u128 {
    for &l in limbs.iter().skip(2) {
        assert!(l == 0, "wide: value does not fit in u128");
    }
    let lo = limbs.first().copied().unwrap_or(0) as u128;
    let hi = limbs.get(1).copied().unwrap_or(0) as u128;
    lo | (hi << 64)
}

/// Number of significant limbs (index of highest non-zero limb + 1; 0 for zero).
#[inline]
pub fn sig_limbs(x: &[u64]) -> usize {
    x.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1)
}

/// Numeric comparison of two little-endian limb slices (lengths may differ).
pub fn cmp_limbs(a: &[u64], b: &[u64]) -> core::cmp::Ordering {
    let (na, nb) = (sig_limbs(a), sig_limbs(b));
    if na != nb {
        return na.cmp(&nb);
    }
    for i in (0..na).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    core::cmp::Ordering::Equal
}

/// `acc += b` in place. Panics if the sum does not fit in `acc`.
pub fn add_assign(acc: &mut [u64], b: &[u64]) {
    let mut carry = 0u64;
    for (i, a) in acc.iter_mut().enumerate() {
        let bi = b.get(i).copied().unwrap_or(0);
        let (s1, c1) = a.overflowing_add(bi);
        let (s2, c2) = s1.overflowing_add(carry);
        *a = s2;
        carry = (c1 as u64) + (c2 as u64);
    }
    assert!(
        carry == 0 && sig_limbs(b) <= acc.len(),
        "wide: addition overflow"
    );
}

/// `acc -= b` in place. Panics on underflow (`acc < b`).
pub fn sub_assign(acc: &mut [u64], b: &[u64]) {
    let mut borrow = 0u64;
    for (i, a) in acc.iter_mut().enumerate() {
        let bi = b.get(i).copied().unwrap_or(0);
        let (d1, b1) = a.overflowing_sub(bi);
        let (d2, b2) = d1.overflowing_sub(borrow);
        *a = d2;
        borrow = (b1 as u64) + (b2 as u64);
    }
    assert!(
        borrow == 0 && sig_limbs(b) <= acc.len(),
        "wide: subtraction underflow"
    );
}

/// `x += 1` in place. Panics on overflow past the buffer.
pub fn add_one(x: &mut [u64]) {
    for l in x.iter_mut() {
        let (s, c) = l.overflowing_add(1);
        *l = s;
        if !c {
            return;
        }
    }
    panic!("wide: increment overflow");
}

/// Schoolbook multiply: `out = a * b`.
///
/// Requires `out.len() >= a.len() + b.len()`; `out` is fully overwritten.
pub fn mul_limbs(a: &[u64], b: &[u64], out: &mut [u64]) {
    assert!(
        out.len() >= a.len() + b.len(),
        "wide: mul output buffer too small"
    );
    for o in out.iter_mut() {
        *o = 0;
    }
    for (i, &ai) in a.iter().enumerate() {
        if ai == 0 {
            continue;
        }
        let mut carry = 0u64;
        for (j, &bj) in b.iter().enumerate() {
            let t = (ai as u128) * (bj as u128) + (out[i + j] as u128) + (carry as u128);
            out[i + j] = t as u64;
            carry = (t >> 64) as u64;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let (s, c) = out[k].overflowing_add(carry);
            out[k] = s;
            carry = c as u64;
            k += 1;
        }
    }
}

/// Exact 256-bit product of two `u128` values, as 4 little-endian limbs.
pub fn mul_u128(a: u128, b: u128) -> [u64; 4] {
    let mut out = [0u64; 4];
    mul_limbs(&limbs_from_u128(a), &limbs_from_u128(b), &mut out);
    out
}

/// General long division: returns `(quotient, remainder)` of `u / v`.
///
/// - `u` and `v` are little-endian limb slices of at most [`MAX_LIMBS`] limbs.
/// - Divisors of one significant limb use short division; wider divisors use
///   Knuth's Algorithm D (u64 digits, normalization, two-limb `qhat`
///   estimation with correction and the add-back branch).
/// - Panics if `v == 0` (desired Miden behavior: the tx fails).
pub fn div_rem(u: &[u64], v: &[u64]) -> ([u64; MAX_LIMBS], [u64; MAX_LIMBS]) {
    assert!(
        u.len() <= MAX_LIMBS && v.len() <= MAX_LIMBS,
        "wide: div_rem operand too wide"
    );
    let n = sig_limbs(v);
    assert!(n != 0, "wide: division by zero");
    let ul = sig_limbs(u);

    let mut q = [0u64; MAX_LIMBS];
    let mut r = [0u64; MAX_LIMBS];

    // Dividend smaller (fewer significant limbs) than divisor: q = 0, r = u.
    if ul < n {
        r[..u.len()].copy_from_slice(u);
        return (q, r);
    }

    if n == 1 {
        // Short division by a single limb.
        let d = v[sig_limbs(v) - 1] as u128;
        debug_assert!(d == v[0] as u128);
        let mut rem = 0u128;
        for i in (0..ul).rev() {
            let cur = (rem << 64) | u[i] as u128;
            q[i] = (cur / d) as u64;
            rem = cur % d;
        }
        r[0] = rem as u64;
        return (q, r);
    }

    // Knuth Algorithm D. Normalize so the divisor's top limb has its high
    // bit set, giving accurate qhat estimates.
    let s = v[n - 1].leading_zeros();
    let mut vn = [0u64; MAX_LIMBS];
    shl_into(&v[..n], s, &mut vn);
    let mut un = [0u64; MAX_LIMBS + 1];
    shl_into(&u[..ul], s, &mut un);

    let m = ul - n; // quotient has m + 1 digits
    let b: u128 = 1 << 64;
    for j in (0..=m).rev() {
        let numhi = ((un[j + n] as u128) << 64) | un[j + n - 1] as u128;
        let mut qhat = numhi / vn[n - 1] as u128;
        let mut rhat = numhi % vn[n - 1] as u128;
        // Correct qhat down (at most twice per Knuth's Theorem B).
        while qhat >= b || qhat * (vn[n - 2] as u128) > (rhat << 64) + un[j + n - 2] as u128 {
            qhat -= 1;
            rhat += vn[n - 1] as u128;
            if rhat >= b {
                break;
            }
        }

        // Multiply and subtract: un[j..=j+n] -= qhat * vn[0..n].
        let qh = qhat as u64;
        let mut mul_carry = 0u64;
        let mut borrow = 0u64;
        for i in 0..n {
            let p = (qh as u128) * (vn[i] as u128) + mul_carry as u128;
            mul_carry = (p >> 64) as u64;
            let (d1, b1) = un[j + i].overflowing_sub(p as u64);
            let (d2, b2) = d1.overflowing_sub(borrow);
            un[j + i] = d2;
            borrow = (b1 as u64) + (b2 as u64);
        }
        let (d1, b1) = un[j + n].overflowing_sub(mul_carry);
        let (d2, b2) = d1.overflowing_sub(borrow);
        un[j + n] = d2;

        q[j] = qh;
        if b1 || b2 {
            // qhat was one too large despite the estimate: add the divisor
            // back and decrement the quotient digit ("add back" branch).
            #[cfg(test)]
            ADD_BACK_HITS.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
            q[j] -= 1;
            let mut carry = 0u64;
            for i in 0..n {
                let (s1, c1) = un[j + i].overflowing_add(vn[i]);
                let (s2, c2) = s1.overflowing_add(carry);
                un[j + i] = s2;
                carry = (c1 as u64) + (c2 as u64);
            }
            un[j + n] = un[j + n].wrapping_add(carry);
        }
    }

    // Denormalize the remainder: r = un[0..n] >> s.
    for i in 0..n {
        r[i] = if s == 0 {
            un[i]
        } else {
            (un[i] >> s) | ((un[i + 1] as u128) << (64 - s)) as u64
        };
    }
    (q, r)
}

/// `dst[..src.len()+1] = src << s` for `s < 64`. `dst` must be one limb
/// longer than `src` (remaining limbs are zeroed).
fn shl_into(src: &[u64], s: u32, dst: &mut [u64]) {
    for d in dst.iter_mut() {
        *d = 0;
    }
    if s == 0 {
        dst[..src.len()].copy_from_slice(src);
        return;
    }
    let mut carry = 0u64;
    for (i, &l) in src.iter().enumerate() {
        dst[i] = (l << s) | carry;
        carry = l >> (64 - s);
    }
    dst[src.len()] = carry;
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::Ordering;

    /// Verify `q * v + r == u` and `r < v` using only in-module primitives.
    fn check_identity(u: &[u64], v: &[u64]) {
        let (q, r) = div_rem(u, v);
        assert!(cmp_limbs(&r, v) == core::cmp::Ordering::Less);
        let mut back = [0u64; 2 * MAX_LIMBS];
        mul_limbs(&q, v, &mut back);
        add_assign(&mut back, &r);
        assert_eq!(sig_limbs(&back[MAX_LIMBS..]), 0, "product overflowed");
        assert_eq!(cmp_limbs(&back[..MAX_LIMBS], u), core::cmp::Ordering::Equal);
    }

    #[test]
    fn add_back_branch_is_exercised() {
        // For 2-limb divisors the qhat correction test is exact (it sees the
        // entire divisor), so the add-back branch requires a divisor of at
        // least 3 limbs whose low limb is invisible to the test:
        //   u = 2^255, v = 2^191 + 1.
        // At the first quotient digit, qhat = 1 passes the two-limb test
        // (vn[n-2] = 0) but the subtraction of v0 = 1 underflows, forcing
        // the add-back.
        let before = ADD_BACK_HITS.load(Ordering::SeqCst);
        let u = [0u64, 0, 0, 1u64 << 63];
        let v = [1u64, 0, 1u64 << 63];
        check_identity(&u, &v);
        let (q, r) = div_rem(&u, &v);
        // u = (2^191 + 1) * (2^64 - 1) + (2^191 - 2^64 + 1),
        // so q = 2^64 - 1 and r = [1, u64::MAX, 2^63 - 1].
        assert_eq!(q[0], u64::MAX);
        assert_eq!(sig_limbs(&q[1..]), 0);
        assert_eq!(r[0], 1);
        assert_eq!(r[1], u64::MAX);
        assert_eq!(r[2], (1u64 << 63) - 1);
        let after = ADD_BACK_HITS.load(Ordering::SeqCst);
        assert!(after > before, "add-back branch was not taken");
    }

    #[test]
    fn qhat_correction_boundaries() {
        // Divisor high-limb at the normalization boundaries and dividends
        // shaped to force qhat over-estimation.
        let cases: &[(&[u64], &[u64])] = &[
            (&[u64::MAX, u64::MAX, u64::MAX, u64::MAX], &[1, 1u64 << 63]),
            (&[u64::MAX, u64::MAX, u64::MAX, u64::MAX], &[u64::MAX, u64::MAX]),
            (&[0, u64::MAX, u64::MAX - 1, 0], &[u64::MAX, u64::MAX]),
            (&[3, 0, 1u64 << 63], &[1, 1u64 << 63]),
            (&[0, 0, 1u64 << 63, 1u64 << 63], &[1, 1u64 << 63]),
            (&[u64::MAX, 0, 0, 1], &[u64::MAX, 1]),
            (&[0, 0, 0, 0, 0, u64::MAX], &[u64::MAX, u64::MAX >> 1]),
            (&[5, 4, 3, 2, 1, 6], &[7, 0, 1]),
            (&[u64::MAX; 8], &[u64::MAX, u64::MAX, u64::MAX, 1u64 << 63]),
        ];
        for (u, v) in cases {
            check_identity(u, v);
        }
    }

    #[test]
    fn short_and_trivial_paths() {
        check_identity(&[0], &[1]);
        check_identity(&[42], &[42]);
        check_identity(&[41], &[42]);
        check_identity(&[u64::MAX; 6], &[3]);
        // u < v with equal limb counts.
        check_identity(&[1, 1], &[2, 1]);
    }

    #[test]
    #[should_panic(expected = "division by zero")]
    fn div_by_zero_panics() {
        let _ = div_rem(&[1, 2, 3], &[0, 0]);
    }
}
