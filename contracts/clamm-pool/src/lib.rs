// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

//! Phase 2 core: the Uniswap-v3-style concentrated-liquidity pool account
//! component (DESIGN.md Part 2 storage layout + note flows, Part 3 number
//! formats).
//!
//! ## Trust model (DESIGN Part 1c, adapted -- documented deviation)
//!
//! DESIGN Part 2 wanted every pool procedure to take NO arguments and read
//! note storage/assets via `active_note::get_storage()/get_assets()`.
//! **Verified toolchain limitation (compiler v0.9 / SDK 0.13, tested in
//! MockChain):** the memory-writing active-note reads
//! (`get_storage`, `get_assets`) return `num == 0` when invoked from
//! account-component context, while the value-returning reads
//! (`get_sender`, `get_serial_number`) work correctly there (the
//! sender-probe contract verifies `get_sender`). The adaptation:
//!
//! - the (root-allowlisted) note script reads its own storage and assets in
//!   the NOTE context -- where those bindings demonstrably work -- and
//!   forwards them as flat felt arguments (<= 16 felts per call);
//! - the component NEVER trusts asset arguments directly: it reconstructs
//!   each asset via the kernel's `asset::create_fungible_asset` from the
//!   immutable `pool_config` faucets and asserts the passed key felts
//!   match, so only genuine pool-token assets are accepted;
//! - authorization stays kernel-read: position ownership and refund
//!   targets derive from `active_note::get_sender()`, never from
//!   arguments; P2ID serials derive from `active_note::get_serial_number()`;
//! - defense in depth is unchanged: the `AuthNetworkAccount` allowlist
//!   admits only our note-script roots, and the kernel epilogue's
//!   asset-preservation check rejects any transaction whose claimed assets
//!   do not exactly match the consumed note's assets.
//!
//! ## Number formats (DESIGN Part 3)
//! - sqrtPriceX96: u128, packed as 4 x u32 limbs (little-endian) in one Word.
//! - liquidity: u128, same packing. liquidityNet: i128, two's-complement
//!   bit-pattern stored as the u128 packing.
//! - fee growth accumulators: Q128.128 in u256 = two Words (lo/hi), each
//!   4 x u32 limbs.
//! - ticks: offset-encoded as `tick + 2^19` so all storage keys/values are
//!   natural numbers and all-zero remains the "absent" sentinel.

#[macro_use]
extern crate alloc;

use amm_math::{liquidity_math, sqrt_price_math, swap_math, tick_math, wide};
use miden::*;

/// Tick offset encoding: stored tick = tick + 2^19 (DESIGN Part 2).
const TICK_OFF: i32 = 1 << 19;

/// Hard bound on swap-loop iterations (each iteration covers at most one
/// 128-position bitmap word, crossing at most one initialized tick).
/// DESIGN divergence 8: fail, don't partial-fill, past the bound.
const MAX_TICK_CROSSINGS: u32 = 16;

/// Domain tag mixed into the Poseidon2 position-key hash ("POS1").
const POSITION_DOMAIN: u32 = 0x504F_5331;

/// Field ids of the striped position record (DESIGN Part 2).
const POS_LIQUIDITY: u32 = 0;
const POS_FG0_LO: u32 = 1;
const POS_FG0_HI: u32 = 2;
const POS_FG1_LO: u32 = 3;
const POS_FG1_HI: u32 = 4;
const POS_TOKENS_OWED: u32 = 5;

/// Field groups of the striped tick record (DESIGN Part 2).
const TICK_LIQ_GROSS: u32 = 0;
const TICK_LIQ_NET: u32 = 1;
/// fgOutside u256 groups: `*_LO` is the low word; the high word lives at
/// group `*_LO + 1` (3 and 5).
const TICK_FG0_LO: u32 = 2;
const TICK_FG1_LO: u32 = 4;
const TICK_GROUP_MAX: u32 = 5;

/// Serial-derivation salts for P2ID output notes.
const SALT_SWAP_OUT: u32 = 0;
const SALT_SWAP_REFUND: u32 = 1;
const SALT_MINT_REFUND: u32 = 2;
const SALT_COLLECT: u32 = 3;

/// Goldilocks modulus, for felt-validity assertions on u64 payloads.
const FELT_MODULUS: u64 = 0xFFFF_FFFF_0000_0001;

// ================================================================================================
// Packing helpers (u128 <-> Word as 4 x u32 limbs, u256 <-> two Words)
// ================================================================================================

/// A u256 as 4 little-endian u64 limbs (crate-internal only; never crosses
/// the component interface).
type U256 = [u64; 4];

#[inline(never)]
fn u128_to_word(x: u128) -> Word {
    Word::from([
        Felt::from_u32(x as u32),
        Felt::from_u32((x >> 32) as u32),
        Felt::from_u32((x >> 64) as u32),
        Felt::from_u32((x >> 96) as u32),
    ])
}

#[inline(never)]
fn word_to_u128(w: Word) -> u128 {
    let e: [Felt; 4] = w.into_elements();
    let mut x: u128 = 0;
    let mut i = 0;
    while i < 4 {
        let limb = e[i].as_canonical_u64();
        assert!(limb <= 0xFFFF_FFFF, "clamm: storage word limb exceeds u32");
        x |= (limb as u128) << (32 * i);
        i += 1;
    }
    x
}

#[inline(never)]
fn u256_from_words(lo: Word, hi: Word) -> U256 {
    let l = word_to_u128(lo);
    let h = word_to_u128(hi);
    [l as u64, (l >> 64) as u64, h as u64, (h >> 64) as u64]
}

#[inline(never)]
fn u256_to_words(x: U256) -> (Word, Word) {
    (
        u128_to_word(x[0] as u128 | ((x[1] as u128) << 64)),
        u128_to_word(x[2] as u128 | ((x[3] as u128) << 64)),
    )
}

/// Wrapping u256 addition (Uniswap fee-growth semantics: mod 2^256).
#[inline(never)]
fn u256_add(a: U256, b: U256) -> U256 {
    let mut out = [0u64; 4];
    let mut carry = 0u64;
    let mut i = 0;
    while i < 4 {
        let (s1, c1) = a[i].overflowing_add(b[i]);
        let (s2, c2) = s1.overflowing_add(carry);
        out[i] = s2;
        carry = (c1 as u64) + (c2 as u64);
        i += 1;
    }
    out
}

/// Wrapping u256 subtraction (mod 2^256).
#[inline(never)]
fn u256_sub(a: U256, b: U256) -> U256 {
    let mut out = [0u64; 4];
    let mut borrow = 0u64;
    let mut i = 0;
    while i < 4 {
        let (d1, b1) = a[i].overflowing_sub(b[i]);
        let (d2, b2) = d1.overflowing_sub(borrow);
        out[i] = d2;
        borrow = (b1 as u64) + (b2 as u64);
        i += 1;
    }
    out
}

#[inline(never)]
fn u256_is_zero(x: U256) -> bool {
    x[0] == 0 && x[1] == 0 && x[2] == 0 && x[3] == 0
}

/// Fee-growth increment `floor((fee << 128) / liquidity)` as Q128.128 u256
/// (DESIGN Part 3 item 4; mirrors Uniswap's `FullMath.mulDiv(fee, Q128, L)`).
#[inline(never)]
fn fee_growth_increment(fee: u128, liquidity: u128) -> U256 {
    assert!(liquidity > 0, "clamm: fee growth with zero liquidity");
    let f = wide::limbs_from_u128(fee);
    let dividend = [0u64, 0, f[0], f[1]];
    let (q, _r) = wide::div_rem(&dividend, &wide::limbs_from_u128(liquidity));
    // dividend < 2^256 and divisor >= 1, so the quotient fits 4 limbs.
    [q[0], q[1], q[2], q[3]]
}

/// Fees owed `(liquidity * delta) >> 128`, truncated to u128 exactly like
/// Uniswap's `uint128(FullMath.mulDiv(delta, liquidity, Q128))` cast.
#[inline(never)]
fn fees_owed(delta: U256, liquidity: u128) -> u128 {
    let mut prod = [0u64; 6];
    wide::mul_limbs(&delta, &wide::limbs_from_u128(liquidity), &mut prod);
    (prod[2] as u128) | ((prod[3] as u128) << 64)
}

/// Floor division for signed tick / spacing (Solidity-compatible floor,
/// mirroring Uniswap's compressed-tick computation).
#[inline(never)]
fn floor_div(a: i32, b: i32) -> i32 {
    let q = a / b;
    if a % b != 0 && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

/// Index of the most significant set bit; `x` must be non-zero.
#[inline(never)]
fn msb_u128(mut x: u128) -> u32 {
    assert!(x != 0, "clamm: msb of zero");
    let mut r = 0u32;
    while x > 1 {
        x >>= 1;
        r += 1;
    }
    r
}

/// Index of the least significant set bit; `x` must be non-zero.
#[inline(never)]
fn lsb_u128(mut x: u128) -> u32 {
    assert!(x != 0, "clamm: lsb of zero");
    let mut r = 0u32;
    while x & 1 == 0 {
        x >>= 1;
        r += 1;
    }
    r
}

/// `get_tick_at_sqrt_ratio` behind an `#[inline(never)]` boundary: inlining
/// its nested loops into `swap`'s loop-bearing body trips a midenc v0.9
/// dominance-frontier panic; keeping the function boundary avoids it.
#[inline(never)]
fn reverse_tick_lookup(sqrt_price: u128) -> i32 {
    tick_math::get_tick_at_sqrt_ratio(sqrt_price)
}

/// Decodes an offset-encoded tick felt.
fn decode_tick(f: Felt) -> i32 {
    let off = f.as_canonical_u64();
    assert!(off <= (2 * TICK_OFF) as u64, "clamm: tick offset out of range");
    off as i32 - TICK_OFF
}

/// Reconstructs a u128 from 4 little-endian u32-limb felts.
#[inline(never)]
fn limbs4_to_u128(l0: Felt, l1: Felt, l2: Felt, l3: Felt) -> u128 {
    let parts = [l0, l1, l2, l3];
    let mut x: u128 = 0;
    let mut i = 0;
    while i < 4 {
        let limb = parts[i].as_canonical_u64();
        assert!(limb <= 0xFFFF_FFFF, "clamm: u128 limb exceeds u32");
        x |= (limb as u128) << (32 * i);
        i += 1;
    }
    x
}

/// Token amounts spanned by a position of `liq` over `[lower, upper]` at
/// the current price (Uniswap `_modifyPosition` amount logic).
#[inline(never)]
fn amounts_for_liquidity(
    tick_cur: i32,
    sqrt_price: u128,
    lower: i32,
    upper: i32,
    liq: u128,
    round_up: bool,
) -> (u128, u128) {
    let pl = tick_math::get_sqrt_ratio_at_tick(lower);
    let pu = tick_math::get_sqrt_ratio_at_tick(upper);
    if tick_cur < lower {
        (sqrt_price_math::get_amount0_delta(pl, pu, liq, round_up), 0)
    } else if tick_cur < upper {
        (
            sqrt_price_math::get_amount0_delta(sqrt_price, pu, liq, round_up),
            sqrt_price_math::get_amount1_delta(pl, sqrt_price, liq, round_up),
        )
    } else {
        (0, sqrt_price_math::get_amount1_delta(pl, pu, liq, round_up))
    }
}

/// Poseidon2 position-key base: hash of (owner_suffix, owner_prefix,
/// tick_lower_off, tick_upper_off, POSITION_DOMAIN), truncated to 3 felts.
/// The 5-element (non-multiple-of-4) preimage deliberately routes through
/// `hash_elements` (capacity = len % 8), matching the host-side
/// `Poseidon2::hash_elements` semantics exactly.
#[inline(never)]
fn position_base(owner: AccountId, tick_lower: i32, tick_upper: i32) -> [Felt; 3] {
    let digest = hash_elements(vec![
        owner.suffix,
        owner.prefix,
        Felt::from_u32((tick_lower + TICK_OFF) as u32),
        Felt::from_u32((tick_upper + TICK_OFF) as u32),
        Felt::from_u32(POSITION_DOMAIN),
    ]);
    let w: Word = digest.into();
    let e: [Felt; 4] = w.into_elements();
    [e[0], e[1], e[2]]
}

fn pos_key(base: [Felt; 3], field: u32) -> Word {
    Word::from([base[0], base[1], base[2], Felt::from_u32(field)])
}

fn tick_key(tick: i32, group: u32) -> Word {
    let off = (tick + TICK_OFF) as u32;
    Word::from([
        Felt::from_u32(off),
        Felt::from_u32(group),
        Felt::from_u32(0),
        Felt::from_u32(0),
    ])
}

fn bitmap_key(word_index: u32) -> Word {
    Word::from([
        Felt::from_u32(word_index),
        Felt::from_u32(0),
        Felt::from_u32(0),
        Felt::from_u32(0),
    ])
}

// ================================================================================================
// Storage
// ================================================================================================

/// Storage layout for the pool component (DESIGN Part 2, exact, plus the
/// `p2id_root` config slot needed to build P2ID recipients from guest code
/// -- documented addition).
#[component_storage]
struct ClammPoolStorage {
    /// Immutable: [token0_faucet_suffix, token0_faucet_prefix,
    /// token1_faucet_suffix, token1_faucet_prefix]. Set via InitStorageData,
    /// never written.
    #[storage(description = "immutable token0/token1 faucet ids")]
    pool_config: StorageValue<Word>,

    /// Immutable: [fee_pips, tick_spacing, 0, 0]. Set via InitStorageData,
    /// never written.
    #[storage(description = "immutable fee pips and tick spacing")]
    pool_params: StorageValue<Word>,

    /// Immutable: P2ID note script MAST root used for all output notes.
    /// Seeded from `P2idNote::script_root()` at account creation.
    #[storage(description = "immutable P2ID note script root")]
    p2id_root: StorageValue<Word>,

    /// sqrtPriceX96 as u128 (4 x u32 limbs).
    #[storage(description = "current sqrt price X96 (u128 limbs)")]
    sqrt_price: StorageValue<Word>,

    /// [current_tick_offset_u32, initialized_flag, 0, 0].
    #[storage(description = "current tick (offset-encoded) and init flag")]
    pool_state: StorageValue<Word>,

    /// Active in-range liquidity as u128 (4 x u32 limbs).
    #[storage(description = "active liquidity (u128 limbs)")]
    liquidity: StorageValue<Word>,

    /// feeGrowthGlobal0 Q128.128, low 128 bits.
    #[storage(description = "fee growth global token0, low word")]
    fee_growth_global0_lo: StorageValue<Word>,
    /// feeGrowthGlobal0 Q128.128, high 128 bits.
    #[storage(description = "fee growth global token0, high word")]
    fee_growth_global0_hi: StorageValue<Word>,
    /// feeGrowthGlobal1 Q128.128, low 128 bits.
    #[storage(description = "fee growth global token1, low word")]
    fee_growth_global1_lo: StorageValue<Word>,
    /// feeGrowthGlobal1 Q128.128, high 128 bits.
    #[storage(description = "fee growth global token1, high word")]
    fee_growth_global1_hi: StorageValue<Word>,

    /// Tick records, key [tick_off_u32, field_group, 0, 0]; groups:
    /// 0 liqGross u128, 1 liqNet i128 (two's complement), 2-3 fgOutside0
    /// lo/hi, 4-5 fgOutside1 lo/hi.
    #[storage(description = "tick records (field-striped)")]
    ticks: StorageMap<Word, Word>,

    /// Tick bitmap, key [word_index, 0, 0, 0], 128 bits per Word
    /// (compressed tick offset-encoded by 2^19).
    #[storage(description = "initialized tick bitmap")]
    tick_bitmap: StorageMap<Word, Word>,

    /// Positions, key [h0, h1, h2, field_id] with (h0,h1,h2) = Poseidon2
    /// hash of (owner_suffix, owner_prefix, tick_lower_off, tick_upper_off,
    /// POSITION_DOMAIN) truncated to 3 felts; field ids: 0 liquidity, 1-2
    /// fgInside0Last, 3-4 fgInside1Last, 5 [tokensOwed0, tokensOwed1].
    #[storage(description = "position records (field-striped)")]
    positions: StorageMap<Word, Word>,
}

// ================================================================================================
// Exported API
// ================================================================================================

/// API of the concentrated-liquidity pool component.
///
/// Arguments are forwarded 1:1 from the allowlisted note script's OWN
/// kernel-read storage/assets (see the trust-model note in the crate docs);
/// authorization data (sender, serial) is always re-read from kernel state
/// inside the component. Asset arguments are passed as the
/// (key[2], key[3], amount) felts of the note asset and are revalidated by
/// kernel-side reconstruction before use.
#[component]
trait ClammPool {
    /// Exact-input swap. Check order: config/asset validation -> deadline
    /// -> swap loop. At/after the deadline the input is refunded to the
    /// sender via P2ID and no swap math runs.
    fn swap(
        &mut self,
        asset_key2: Felt,
        asset_key3: Felt,
        amount_in: Felt,
        direction: Felt,
        min_out_lo: Felt,
        min_out_hi: Felt,
        recipient_suffix: Felt,
        recipient_prefix: Felt,
        deadline: Felt,
    ) -> Felt;

    /// Adds liquidity for the note sender. The two asset triples describe
    /// the note's assets (amount 0 = absent). Excess assets beyond the
    /// amounts owed are refunded via P2ID.
    fn mint(
        &mut self,
        a_key2: Felt,
        a_key3: Felt,
        a_amount: Felt,
        b_key2: Felt,
        b_key3: Felt,
        b_amount: Felt,
        tick_lower_off: Felt,
        tick_upper_off: Felt,
        liq_l0: Felt,
        liq_l1: Felt,
        liq_l2: Felt,
        liq_l3: Felt,
        deadline: Felt,
    ) -> Felt;

    /// Burns liquidity from the sender's position: settles fees and
    /// principal into tokensOwed. No note is emitted.
    fn burn(
        &mut self,
        tick_lower_off: Felt,
        tick_upper_off: Felt,
        liq_l0: Felt,
        liq_l1: Felt,
        liq_l2: Felt,
        liq_l3: Felt,
    ) -> Felt;

    /// Pays out the sender's tokensOwed via a single P2ID note and zeroes
    /// them.
    fn collect(&mut self, tick_lower_off: Felt, tick_upper_off: Felt) -> Felt;
}

#[component]
impl ClammPool for ClammPoolStorage {
    fn swap(
        &mut self,
        asset_key2: Felt,
        asset_key3: Felt,
        amount_in: Felt,
        direction: Felt,
        min_out_lo: Felt,
        min_out_hi: Felt,
        recipient_suffix: Felt,
        recipient_prefix: Felt,
        deadline: Felt,
    ) -> Felt {
        self.require_initialized();

        let direction = direction.as_canonical_u64();
        assert!(direction <= 1, "clamm: invalid swap direction");
        let zero_for_one = direction == 0;

        let min_out_lo = min_out_lo.as_canonical_u64();
        let min_out_hi = min_out_hi.as_canonical_u64();
        assert!(
            min_out_lo <= 0xFFFF_FFFF && min_out_hi <= 0xFFFF_FFFF,
            "clamm: min_out limbs exceed u32"
        );
        let min_out = min_out_lo | (min_out_hi << 32);
        let deadline = deadline.as_canonical_u64();

        // Validate the input asset: reconstruct the expected pool-token
        // asset through the kernel and require the note asset to match.
        let amount_in_u64 = amount_in.as_canonical_u64();
        assert!(amount_in_u64 > 0, "clamm: zero swap input");
        let in_token: u32 = if zero_for_one { 0 } else { 1 };
        let input = self.checked_pool_asset(in_token, asset_key2, asset_key3, amount_in);

        // Deadline check AFTER validation, BEFORE any swap math
        // (DESIGN Part 2 check order).
        let block = tx::get_block_number().as_canonical_u64();
        if block >= deadline {
            // Expired: consume-and-refund, no swap math, no state change.
            let sender = active_note::get_sender();
            native_account::add_asset(input);
            self.emit_p2id(&[input], sender.suffix, sender.prefix, SALT_SWAP_REFUND);
            return felt!(1);
        }

        // ---- Uniswap v3 swap loop (exact input) ----
        let params = self.pool_params.get();
        let pe: [Felt; 4] = params.into_elements();
        let fee_pips = pe[0].as_canonical_u64() as u32;
        let spacing = pe[1].as_canonical_u64() as i32;

        let mut sqrt_price = word_to_u128(self.sqrt_price.get());
        let mut tick = self.current_tick();
        let mut liquidity = word_to_u128(self.liquidity.get());
        let mut fg_in: U256 = if zero_for_one { self.read_fg0() } else { self.read_fg1() };
        let fg_other: U256 = if zero_for_one { self.read_fg1() } else { self.read_fg0() };

        let limit: u128 = if zero_for_one {
            tick_math::MIN_SQRT_RATIO
        } else {
            tick_math::MAX_SQRT_RATIO
        };

        let mut remaining: u128 = amount_in_u64 as u128;
        let mut amount_out_total: u128 = 0;
        let mut iterations: u32 = 0;
        // Set when the final step ends strictly inside a tick range; the
        // reverse tick mapping then runs AFTER the loop. (An exact-in step
        // that does not reach its target price always consumes the entire
        // remainder, so the loop is guaranteed to exit -- and hoisting the
        // binary search out of the loop sidesteps a midenc v0.9 dominance
        // bug triggered by loop-in-loop inlining.)
        let mut needs_reverse_map = false;

        loop {
            if remaining == 0 {
                break;
            }
            if sqrt_price == limit {
                break;
            }
            iterations += 1;
            assert!(
                iterations <= MAX_TICK_CROSSINGS,
                "clamm: swap exceeds max tick crossings"
            );

            let (next_tick_raw, initialized) =
                self.next_initialized_tick(tick, spacing, zero_for_one);
            let next_tick = if next_tick_raw < tick_math::MIN_TICK {
                tick_math::MIN_TICK
            } else if next_tick_raw > tick_math::MAX_TICK {
                tick_math::MAX_TICK
            } else {
                next_tick_raw
            };
            let target = tick_math::get_sqrt_ratio_at_tick(next_tick);

            let (next_price, step_in, step_out, step_fee) = swap_math::compute_swap_step(
                sqrt_price,
                target,
                liquidity,
                remaining as i128,
                fee_pips,
            );

            remaining = remaining
                .checked_sub(
                    step_in
                        .checked_add(step_fee)
                        .expect("clamm: swap step amount overflow"),
                )
                .expect("clamm: swap step consumed more than remaining");
            amount_out_total = amount_out_total
                .checked_add(step_out)
                .expect("clamm: swap output overflow");

            if liquidity > 0 && step_fee > 0 {
                fg_in = u256_add(fg_in, fee_growth_increment(step_fee, liquidity));
            }

            // NOTE (compiler v0.9 workaround, see contracts/bench-note):
            // flat sequential `if` blocks instead of an if/else-if chain --
            // the chained form miscompiles midenc's dominance analysis.
            let reached_target = next_price == target;
            if reached_target && initialized {
                let (fg0_now, fg1_now) = if zero_for_one {
                    (fg_in, fg_other)
                } else {
                    (fg_other, fg_in)
                };
                let liq_net = self.cross_tick(next_tick, fg0_now, fg1_now);
                let delta = if zero_for_one { -liq_net } else { liq_net };
                liquidity = liquidity_math::add_delta(liquidity, delta);
            }
            if reached_target {
                tick = if zero_for_one { next_tick - 1 } else { next_tick };
            }
            needs_reverse_map = !reached_target && next_price != sqrt_price;
            sqrt_price = next_price;
        }

        assert!(
            remaining == 0,
            "clamm: insufficient liquidity in range to consume swap input"
        );
        if needs_reverse_map {
            tick = reverse_tick_lookup(sqrt_price);
        }
        assert!(
            amount_out_total >= min_out as u128,
            "clamm: slippage - amount out below minimum"
        );
        assert!(amount_out_total > 0, "clamm: zero swap output");

        // Commit state.
        self.sqrt_price.set(u128_to_word(sqrt_price));
        self.set_current_tick(tick);
        self.liquidity.set(u128_to_word(liquidity));
        if zero_for_one {
            self.write_fg0(fg_in);
        } else {
            self.write_fg1(fg_in);
        }

        // Move assets: input into the vault, output onto a P2ID note.
        native_account::add_asset(input);
        let out_token: u32 = if zero_for_one { 1 } else { 0 };
        assert!(
            amount_out_total <= u64::MAX as u128,
            "clamm: swap output exceeds u64"
        );
        let out_asset = self.make_pool_asset(out_token, amount_out_total as u64);
        self.emit_p2id(&[out_asset], recipient_suffix, recipient_prefix, SALT_SWAP_OUT);
        felt!(1)
    }

    fn mint(
        &mut self,
        a_key2: Felt,
        a_key3: Felt,
        a_amount: Felt,
        b_key2: Felt,
        b_key3: Felt,
        b_amount: Felt,
        tick_lower_off: Felt,
        tick_upper_off: Felt,
        liq_l0: Felt,
        liq_l1: Felt,
        liq_l2: Felt,
        liq_l3: Felt,
        deadline: Felt,
    ) -> Felt {
        self.require_initialized();

        let tick_lower = decode_tick(tick_lower_off);
        let tick_upper = decode_tick(tick_upper_off);
        let liq_desired = limbs4_to_u128(liq_l0, liq_l1, liq_l2, liq_l3);
        let deadline = deadline.as_canonical_u64();

        // Classify + validate the provided assets. Amount 0 = absent slot.
        let mut provided: [u64; 2] = [0, 0];
        let mut note_assets: alloc::vec::Vec<Asset> = vec![];
        if a_amount.as_canonical_u64() > 0 {
            let (idx, asset) = self.classify_asset(a_key2, a_key3, a_amount);
            provided[idx as usize] = provided[idx as usize]
                .checked_add(a_amount.as_canonical_u64())
                .expect("clamm: provided amount overflow");
            note_assets.push(asset);
        }
        if b_amount.as_canonical_u64() > 0 {
            let (idx, asset) = self.classify_asset(b_key2, b_key3, b_amount);
            provided[idx as usize] = provided[idx as usize]
                .checked_add(b_amount.as_canonical_u64())
                .expect("clamm: provided amount overflow");
            note_assets.push(asset);
        }
        assert!(!note_assets.is_empty(), "clamm: mint note carries no assets");

        let sender = active_note::get_sender();
        let block = tx::get_block_number().as_canonical_u64();
        if block >= deadline {
            // Expired: consume-and-refund everything, no state change.
            let mut j = 0;
            while j < note_assets.len() {
                native_account::add_asset(note_assets[j]);
                j += 1;
            }
            self.emit_p2id(
                note_assets.as_slice(),
                sender.suffix,
                sender.prefix,
                SALT_MINT_REFUND,
            );
            return felt!(1);
        }

        // Range checks (DESIGN: +-443,636, spacing multiples).
        let params = self.pool_params.get();
        let pe: [Felt; 4] = params.into_elements();
        let spacing = pe[1].as_canonical_u64() as i32;
        assert!(tick_lower < tick_upper, "clamm: tick_lower must be below tick_upper");
        assert!(
            tick_lower >= tick_math::MIN_TICK && tick_upper <= tick_math::MAX_TICK,
            "clamm: tick out of supported range"
        );
        assert!(
            tick_lower % spacing == 0 && tick_upper % spacing == 0,
            "clamm: tick not aligned to tick spacing"
        );
        assert!(liq_desired > 0, "clamm: zero liquidity mint");
        assert!(
            liq_desired <= i128::MAX as u128,
            "clamm: liquidity delta exceeds i128"
        );

        // Amounts owed for liquidity_desired at the current price, rounded
        // UP (pool-favoring).
        let tick_cur = self.current_tick();
        let sqrt_price = word_to_u128(self.sqrt_price.get());
        let (owed0, owed1) =
            amounts_for_liquidity(tick_cur, sqrt_price, tick_lower, tick_upper, liq_desired, true);
        assert!(
            owed0 <= u64::MAX as u128 && owed1 <= u64::MAX as u128,
            "clamm: owed amount overflow"
        );
        assert!(
            provided[0] as u128 >= owed0 && provided[1] as u128 >= owed1,
            "clamm: mint note assets do not cover amounts owed"
        );

        // Position + tick + bitmap updates (owner = kernel-read sender).
        self.update_position(sender, tick_lower, tick_upper, liq_desired as i128, tick_cur);

        // Active liquidity if the range is in scope.
        if tick_cur >= tick_lower && tick_cur < tick_upper {
            let liq = word_to_u128(self.liquidity.get());
            self.liquidity
                .set(u128_to_word(liquidity_math::add_delta(liq, liq_desired as i128)));
        }

        // Move all provided assets into the vault, then refund the excess.
        let mut j = 0;
        while j < note_assets.len() {
            native_account::add_asset(note_assets[j]);
            j += 1;
        }
        let refund0 = provided[0] - owed0 as u64;
        let refund1 = provided[1] - owed1 as u64;
        let mut refunds: alloc::vec::Vec<Asset> = vec![];
        if refund0 > 0 {
            refunds.push(self.make_pool_asset(0, refund0));
        }
        if refund1 > 0 {
            refunds.push(self.make_pool_asset(1, refund1));
        }
        if !refunds.is_empty() {
            self.emit_p2id(refunds.as_slice(), sender.suffix, sender.prefix, SALT_MINT_REFUND);
        }
        felt!(1)
    }

    fn burn(
        &mut self,
        tick_lower_off: Felt,
        tick_upper_off: Felt,
        liq_l0: Felt,
        liq_l1: Felt,
        liq_l2: Felt,
        liq_l3: Felt,
    ) -> Felt {
        self.require_initialized();

        let tick_lower = decode_tick(tick_lower_off);
        let tick_upper = decode_tick(tick_upper_off);
        let liq = limbs4_to_u128(liq_l0, liq_l1, liq_l2, liq_l3);
        assert!(liq > 0, "clamm: zero liquidity burn");
        assert!(liq <= i128::MAX as u128, "clamm: liquidity delta exceeds i128");

        let params = self.pool_params.get();
        let pe: [Felt; 4] = params.into_elements();
        let spacing = pe[1].as_canonical_u64() as i32;
        assert!(tick_lower < tick_upper, "clamm: tick_lower must be below tick_upper");
        assert!(
            tick_lower >= tick_math::MIN_TICK && tick_upper <= tick_math::MAX_TICK,
            "clamm: tick out of supported range"
        );
        assert!(
            tick_lower % spacing == 0 && tick_upper % spacing == 0,
            "clamm: tick not aligned to tick spacing"
        );

        // Auth: the position key derives from the kernel-read sender -- a
        // non-owner simply addresses an empty position and the liquidity
        // decrease underflows (tx fails).
        let sender = active_note::get_sender();
        let tick_cur = self.current_tick();
        // Principal owed, rounded DOWN (pool-favoring). Computed BEFORE
        // update_position: it depends only on (tick, price, liq), and the
        // wide-limb math must run before any Poseidon2 hash_elements call
        // in this call frame (midenc v0.9: hash-then-wide-math in one
        // frame miscomputes; see the crate docs).
        let sqrt_price = word_to_u128(self.sqrt_price.get());
        let (amount0, amount1) =
            amounts_for_liquidity(tick_cur, sqrt_price, tick_lower, tick_upper, liq, false);
        assert!(
            amount0 <= u64::MAX as u128 && amount1 <= u64::MAX as u128,
            "clamm: burn amount overflow"
        );

        self.update_position(sender, tick_lower, tick_upper, -(liq as i128), tick_cur);

        self.settle_burn(sender, tick_lower, tick_upper, liq, tick_cur, amount0 as u64, amount1 as u64);
        felt!(1)
    }

    fn collect(&mut self, tick_lower_off: Felt, tick_upper_off: Felt) -> Felt {
        self.require_initialized();

        let tick_lower = decode_tick(tick_lower_off);
        let tick_upper = decode_tick(tick_upper_off);

        // Auth: position key derived from the kernel-read sender.
        let sender = active_note::get_sender();
        let base = position_base(sender, tick_lower, tick_upper);
        let owed_key = pos_key(base, POS_TOKENS_OWED);
        let owed = self.positions.get(owed_key);
        let oe: [Felt; 4] = owed.into_elements();
        let owed0 = oe[0].as_canonical_u64();
        let owed1 = oe[1].as_canonical_u64();
        assert!(owed0 > 0 || owed1 > 0, "clamm: nothing to collect");

        // Zero the owed record, then pay out via one P2ID note.
        self.positions.set(owed_key, Word::default());
        let mut payout: alloc::vec::Vec<Asset> = vec![];
        if owed0 > 0 {
            payout.push(self.make_pool_asset(0, owed0));
        }
        if owed1 > 0 {
            payout.push(self.make_pool_asset(1, owed1));
        }
        self.emit_p2id(payout.as_slice(), sender.suffix, sender.prefix, SALT_COLLECT);
        felt!(1)
    }
}

// ================================================================================================
// Inherent (non-exported) pool machinery
// ================================================================================================

impl ClammPoolStorage {
    fn require_initialized(&self) {
        let state = self.pool_state.get();
        let e: [Felt; 4] = state.into_elements();
        assert!(e[1].as_canonical_u64() == 1, "clamm: pool not initialized");
    }

    fn current_tick(&self) -> i32 {
        let state = self.pool_state.get();
        let e: [Felt; 4] = state.into_elements();
        let off = e[0].as_canonical_u64();
        assert!(off <= (2 * TICK_OFF) as u64, "clamm: stored tick out of range");
        off as i32 - TICK_OFF
    }

    fn set_current_tick(&mut self, tick: i32) {
        self.pool_state.set(Word::from([
            Felt::from_u32((tick + TICK_OFF) as u32),
            Felt::from_u32(1),
            Felt::from_u32(0),
            Felt::from_u32(0),
        ]));
    }

    #[inline(never)]
    fn read_fg0(&self) -> U256 {
        u256_from_words(self.fee_growth_global0_lo.get(), self.fee_growth_global0_hi.get())
    }

    #[inline(never)]
    fn read_fg1(&self) -> U256 {
        u256_from_words(self.fee_growth_global1_lo.get(), self.fee_growth_global1_hi.get())
    }

    #[inline(never)]
    fn write_fg0(&mut self, x: U256) {
        let (lo, hi) = u256_to_words(x);
        self.fee_growth_global0_lo.set(lo);
        self.fee_growth_global0_hi.set(hi);
    }

    #[inline(never)]
    fn write_fg1(&mut self, x: U256) {
        let (lo, hi) = u256_to_words(x);
        self.fee_growth_global1_lo.set(lo);
        self.fee_growth_global1_hi.set(hi);
    }

    /// Builds a pool-token fungible asset (token 0 or 1) for `amount`
    /// through the kernel (never from raw felts).
    #[inline(never)]
    fn make_pool_asset(&self, token: u32, amount: u64) -> Asset {
        assert!(amount > 0, "clamm: zero asset amount");
        assert!(amount < FELT_MODULUS, "clamm: asset amount exceeds felt");
        let cfg = self.pool_config.get();
        let e: [Felt; 4] = cfg.into_elements();
        let (suffix, prefix) = if token == 0 { (e[0], e[1]) } else { (e[2], e[3]) };
        asset::create_fungible_asset(
            AccountId::new(prefix, suffix),
            Felt::new_unchecked(amount),
            false,
        )
    }

    /// Rebuilds the expected pool-token asset for `token` and asserts the
    /// note asset's key felts match it exactly (faucet, metadata byte,
    /// fungible composition). Returns the kernel-built asset.
    #[inline(never)]
    fn checked_pool_asset(&self, token: u32, key2: Felt, key3: Felt, amount: Felt) -> Asset {
        let expected = self.make_pool_asset(token, amount.as_canonical_u64());
        let ek: [Felt; 4] = expected.key.into_elements();
        assert!(
            ek[2].as_canonical_u64() == key2.as_canonical_u64()
                && ek[3].as_canonical_u64() == key3.as_canonical_u64(),
            "clamm: asset faucet does not match pool/direction"
        );
        expected
    }

    /// Classifies a note asset triple as token0 (0) or token1 (1) by
    /// kernel-side reconstruction; panics for foreign assets.
    #[inline(never)]
    fn classify_asset(&self, key2: Felt, key3: Felt, amount: Felt) -> (u32, Asset) {
        let expected0 = self.make_pool_asset(0, amount.as_canonical_u64());
        let e0: [Felt; 4] = expected0.key.into_elements();
        if e0[2].as_canonical_u64() == key2.as_canonical_u64()
            && e0[3].as_canonical_u64() == key3.as_canonical_u64()
        {
            return (0, expected0);
        }
        let expected1 = self.make_pool_asset(1, amount.as_canonical_u64());
        let e1: [Felt; 4] = expected1.key.into_elements();
        assert!(
            e1[2].as_canonical_u64() == key2.as_canonical_u64()
                && e1[3].as_canonical_u64() == key3.as_canonical_u64(),
            "clamm: asset faucet does not match pool config"
        );
        (1, expected1)
    }

    #[inline(never)]
    fn bitmap_word(&self, word_index: u32) -> u128 {
        word_to_u128(self.tick_bitmap.get(bitmap_key(word_index)))
    }

    /// Flips the bitmap bit of an (aligned) tick.
    #[inline(never)]
    fn bitmap_flip(&mut self, tick: i32, spacing: i32) {
        assert!(tick % spacing == 0, "clamm: bitmap flip of unaligned tick");
        let compressed = tick / spacing;
        let c_off = (compressed + TICK_OFF) as u32;
        let wi = c_off >> 7;
        let bit = c_off & 127;
        let cur = self.bitmap_word(wi);
        self.tick_bitmap
            .set(bitmap_key(wi), u128_to_word(cur ^ (1u128 << bit)));
    }

    /// Uniswap `nextInitializedTickWithinOneWord` over 128-bit words.
    /// Returns (next_tick, initialized); when nothing is initialized in the
    /// searched word, returns the word-boundary tick with `false`.
    #[inline(never)]
    fn next_initialized_tick(&self, tick: i32, spacing: i32, lte: bool) -> (i32, bool) {
        let compressed = floor_div(tick, spacing);
        if lte {
            let c_off = (compressed + TICK_OFF) as u32;
            let wi = c_off >> 7;
            let bit = c_off & 127;
            let word_val = self.bitmap_word(wi);
            let mask: u128 = if bit == 127 {
                u128::MAX
            } else {
                (1u128 << (bit + 1)) - 1
            };
            let masked = word_val & mask;
            if masked != 0 {
                let msb = msb_u128(masked);
                ((((wi << 7) | msb) as i32 - TICK_OFF) * spacing, true)
            } else {
                (((wi << 7) as i32 - TICK_OFF) * spacing, false)
            }
        } else {
            let start = compressed + 1;
            let c_off = (start + TICK_OFF) as u32;
            let wi = c_off >> 7;
            let bit = c_off & 127;
            let word_val = self.bitmap_word(wi);
            let mask: u128 = !((1u128 << bit) - 1);
            let masked = word_val & mask;
            if masked != 0 {
                let lsb = lsb_u128(masked);
                ((((wi << 7) | lsb) as i32 - TICK_OFF) * spacing, true)
            } else {
                ((((wi << 7) | 127) as i32 - TICK_OFF) * spacing, false)
            }
        }
    }

    #[inline(never)]
    fn tick_u128(&self, tick: i32, group: u32) -> u128 {
        word_to_u128(self.ticks.get(tick_key(tick, group)))
    }

    fn tick_i128(&self, tick: i32, group: u32) -> i128 {
        self.tick_u128(tick, group) as i128
    }

    #[inline(never)]
    fn tick_u256(&self, tick: i32, lo_group: u32) -> U256 {
        u256_from_words(
            self.ticks.get(tick_key(tick, lo_group)),
            self.ticks.get(tick_key(tick, lo_group + 1)),
        )
    }

    #[inline(never)]
    fn set_tick_u256(&mut self, tick: i32, lo_group: u32, x: U256) {
        let (lo, hi) = u256_to_words(x);
        self.ticks.set(tick_key(tick, lo_group), lo);
        self.ticks.set(tick_key(tick, lo_group + 1), hi);
    }

    /// Uniswap `Tick.cross`: flips fgOutside on both accumulators and
    /// returns the tick's liquidityNet.
    #[inline(never)]
    fn cross_tick(&mut self, tick: i32, fg0: U256, fg1: U256) -> i128 {
        let net = self.tick_i128(tick, TICK_LIQ_NET);
        let o0 = self.tick_u256(tick, TICK_FG0_LO);
        let o1 = self.tick_u256(tick, TICK_FG1_LO);
        self.set_tick_u256(tick, TICK_FG0_LO, u256_sub(fg0, o0));
        self.set_tick_u256(tick, TICK_FG1_LO, u256_sub(fg1, o1));
        net
    }

    /// Uniswap `Tick.update`. Returns whether the tick flipped between
    /// initialized and uninitialized.
    #[inline(never)]
    fn tick_update(
        &mut self,
        tick: i32,
        tick_cur: i32,
        delta: i128,
        upper: bool,
        fg_g0: U256,
        fg_g1: U256,
    ) -> bool {
        let gross_before = self.tick_u128(tick, TICK_LIQ_GROSS);
        let gross_after = liquidity_math::add_delta(gross_before, delta);
        let flipped = (gross_after == 0) != (gross_before == 0);

        if gross_before == 0 && tick <= tick_cur {
            // Convention: everything before initialization happened below
            // the tick (Uniswap).
            self.set_tick_u256(tick, TICK_FG0_LO, fg_g0);
            self.set_tick_u256(tick, TICK_FG1_LO, fg_g1);
        }

        self.ticks
            .set(tick_key(tick, TICK_LIQ_GROSS), u128_to_word(gross_after));

        let net_before = self.tick_i128(tick, TICK_LIQ_NET);
        let net_after = if upper {
            net_before
                .checked_sub(delta)
                .expect("clamm: liquidityNet underflow")
        } else {
            net_before
                .checked_add(delta)
                .expect("clamm: liquidityNet overflow")
        };
        self.ticks
            .set(tick_key(tick, TICK_LIQ_NET), u128_to_word(net_after as u128));

        flipped
    }

    /// Deletes all field groups of a tick (Uniswap `Tick.clear`).
    #[inline(never)]
    fn clear_tick(&mut self, tick: i32) {
        let mut g = 0u32;
        while g <= TICK_GROUP_MAX {
            self.ticks.set(tick_key(tick, g), Word::default());
            g += 1;
        }
    }

    /// Uniswap `Tick.getFeeGrowthInside` with wrapping u256 arithmetic.
    #[inline(never)]
    fn fee_growth_inside(
        &self,
        lower: i32,
        upper: i32,
        tick_cur: i32,
        fg_g0: U256,
        fg_g1: U256,
    ) -> (U256, U256) {
        let l0 = self.tick_u256(lower, TICK_FG0_LO);
        let l1 = self.tick_u256(lower, TICK_FG1_LO);
        let u0 = self.tick_u256(upper, TICK_FG0_LO);
        let u1 = self.tick_u256(upper, TICK_FG1_LO);

        let (below0, below1) = if tick_cur >= lower {
            (l0, l1)
        } else {
            (u256_sub(fg_g0, l0), u256_sub(fg_g1, l1))
        };
        let (above0, above1) = if tick_cur < upper {
            (u0, u1)
        } else {
            (u256_sub(fg_g0, u0), u256_sub(fg_g1, u1))
        };
        (
            u256_sub(u256_sub(fg_g0, below0), above0),
            u256_sub(u256_sub(fg_g1, below1), above1),
        )
    }

    /// Uniswap `_updatePosition`: tick updates + bitmap flips + fee
    /// settlement into the position record + snapshot refresh + tick
    /// clearing on emptying burns.
    #[inline(never)]
    fn update_position(
        &mut self,
        owner: AccountId,
        lower: i32,
        upper: i32,
        delta: i128,
        tick_cur: i32,
    ) {
        let fg_g0 = self.read_fg0();
        let fg_g1 = self.read_fg1();
        let params = self.pool_params.get();
        let pe: [Felt; 4] = params.into_elements();
        let spacing = pe[1].as_canonical_u64() as i32;

        let mut flipped_lower = false;
        let mut flipped_upper = false;
        if delta != 0 {
            flipped_lower = self.tick_update(lower, tick_cur, delta, false, fg_g0, fg_g1);
            flipped_upper = self.tick_update(upper, tick_cur, delta, true, fg_g0, fg_g1);
            if flipped_lower {
                self.bitmap_flip(lower, spacing);
            }
            if flipped_upper {
                self.bitmap_flip(upper, spacing);
            }
        }

        let (inside0, inside1) = self.fee_growth_inside(lower, upper, tick_cur, fg_g0, fg_g1);

        // Position record.
        let base = position_base(owner, lower, upper);
        let liq_key = pos_key(base, POS_LIQUIDITY);
        let pos_liq = word_to_u128(self.positions.get(liq_key));
        if delta == 0 {
            assert!(pos_liq > 0, "clamm: poke of empty position");
        }

        let last0 = u256_from_words(
            self.positions.get(pos_key(base, POS_FG0_LO)),
            self.positions.get(pos_key(base, POS_FG0_HI)),
        );
        let last1 = u256_from_words(
            self.positions.get(pos_key(base, POS_FG1_LO)),
            self.positions.get(pos_key(base, POS_FG1_HI)),
        );
        let mut fees0: u128 = 0;
        let mut fees1: u128 = 0;
        if pos_liq > 0 {
            let d0 = u256_sub(inside0, last0);
            let d1 = u256_sub(inside1, last1);
            if !u256_is_zero(d0) {
                fees0 = fees_owed(d0, pos_liq);
            }
            if !u256_is_zero(d1) {
                fees1 = fees_owed(d1, pos_liq);
            }
        }

        let new_liq = liquidity_math::add_delta(pos_liq, delta);
        self.positions.set(liq_key, u128_to_word(new_liq));
        let (i0_lo, i0_hi) = u256_to_words(inside0);
        let (i1_lo, i1_hi) = u256_to_words(inside1);
        self.positions.set(pos_key(base, POS_FG0_LO), i0_lo);
        self.positions.set(pos_key(base, POS_FG0_HI), i0_hi);
        self.positions.set(pos_key(base, POS_FG1_LO), i1_lo);
        self.positions.set(pos_key(base, POS_FG1_HI), i1_hi);

        if fees0 > 0 || fees1 > 0 {
            assert!(
                fees0 <= u64::MAX as u128 && fees1 <= u64::MAX as u128,
                "clamm: fees owed overflow"
            );
            self.add_tokens_owed(base, fees0 as u64, fees1 as u64);
        }

        // Clear emptied ticks on burns (Uniswap `_updatePosition`).
        if delta < 0 {
            if flipped_lower {
                self.clear_tick(lower);
            }
            if flipped_upper {
                self.clear_tick(upper);
            }
        }
    }

    /// Burn tail: principal computation + active-liquidity update +
    /// tokensOwed credit, extracted behind an `#[inline(never)]` boundary
    /// (midenc v0.9 miscompiles the math when inlined into `burn`'s body).
    #[inline(never)]
    fn settle_burn(
        &mut self,
        sender: AccountId,
        tick_lower: i32,
        tick_upper: i32,
        liq: u128,
        tick_cur: i32,
        amount0: u64,
        amount1: u64,
    ) {
        // Active liquidity if the range is in scope.
        if tick_cur >= tick_lower && tick_cur < tick_upper {
            let active = word_to_u128(self.liquidity.get());
            self.liquidity
                .set(u128_to_word(liquidity_math::add_delta(active, -(liq as i128))));
        }

        // Credit principal into tokensOwed (tokens leave via collect).
        if amount0 > 0 || amount1 > 0 {
            let base = position_base(sender, tick_lower, tick_upper);
            self.add_tokens_owed(base, amount0, amount1);
        }
    }

    /// Adds amounts to the position's [tokensOwed0, tokensOwed1] word.
    #[inline(never)]
    fn add_tokens_owed(&mut self, base: [Felt; 3], add0: u64, add1: u64) {
        let key = pos_key(base, POS_TOKENS_OWED);
        let cur = self.positions.get(key);
        let e: [Felt; 4] = cur.into_elements();
        let owed0 = e[0]
            .as_canonical_u64()
            .checked_add(add0)
            .expect("clamm: tokensOwed0 overflow");
        let owed1 = e[1]
            .as_canonical_u64()
            .checked_add(add1)
            .expect("clamm: tokensOwed1 overflow");
        assert!(
            owed0 < FELT_MODULUS && owed1 < FELT_MODULUS,
            "clamm: tokensOwed exceeds felt"
        );
        self.positions.set(
            key,
            Word::from([
                Felt::new_unchecked(owed0),
                Felt::new_unchecked(owed1),
                Felt::from_u32(0),
                Felt::from_u32(0),
            ]),
        );
    }

    /// Emits one public P2ID note to `recipient` carrying `assets` (which
    /// are removed from the vault). The serial number derives
    /// deterministically from the active note's serial + `salt` via
    /// Poseidon2 (5-element preimage, see `position_base` note).
    #[inline(never)]
    fn emit_p2id(
        &mut self,
        assets: &[Asset],
        recipient_suffix: Felt,
        recipient_prefix: Felt,
        salt: u32,
    ) {
        let serial_src = active_note::get_serial_number();
        let se: [Felt; 4] = serial_src.into_elements();
        let digest = hash_elements(vec![se[0], se[1], se[2], se[3], Felt::from_u32(salt)]);
        let serial: Word = digest.into();
        let root = self.p2id_root.get();
        let recipient =
            note::build_recipient(serial, root, vec![recipient_suffix, recipient_prefix]);
        let idx = output_note::create(
            Tag::from(Felt::from_u32(0)),
            NoteType::from(felt!(1)),
            recipient,
        );
        let mut i = 0;
        while i < assets.len() {
            native_account::remove_asset(assets[i]);
            output_note::add_asset(assets[i], idx);
            i += 1;
        }
    }
}
