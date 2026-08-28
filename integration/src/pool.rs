//! Host-side helpers and an exact mirror ("sim") of the clamm-pool guest
//! logic, used by the MockChain integration tests to compute expected
//! post-state and output amounts natively with amm-math.
//!
//! Every encoding here mirrors `contracts/clamm-pool/src/lib.rs` exactly:
//! u128 as 4 little-endian u32 limbs in one Word, ticks offset-encoded by
//! 2^19, Q128.128 fee growth as [u64; 4] limbs split across lo/hi words,
//! Poseidon2 position keys over a 5-felt preimage.

use std::collections::BTreeMap;

use amm_math::{liquidity_math, sqrt_price_math, swap_math, tick_math, wide};
use anyhow::{Context, Result};
use miden_client::account::StorageSlotName;
use miden_client::crypto::Poseidon2;
use miden_client::{Felt, Word};

/// Tick offset encoding: stored tick = tick + 2^19.
pub const TICK_OFF: i32 = 1 << 19;
/// Domain tag of the Poseidon2 position-key hash ("POS1").
pub const POSITION_DOMAIN: u32 = 0x504F_5331;
/// Guest swap-loop iteration bound (DESIGN divergence 8).
pub const MAX_TICK_CROSSINGS: u32 = 16;

/// Position record field ids.
pub const POS_LIQUIDITY: u32 = 0;
pub const POS_TOKENS_OWED: u32 = 5;

/// Tick record field groups.
pub const TICK_LIQ_GROSS: u32 = 0;
pub const TICK_LIQ_NET: u32 = 1;
pub const TICK_FG0_LO: u32 = 2;
pub const TICK_FG1_LO: u32 = 4;

/// A u256 as 4 little-endian u64 limbs (Q128.128 fee growth).
pub type U256 = [u64; 4];

// ================================================================================================
// Storage slot names
// ================================================================================================

/// Builds a clamm-pool storage slot name (`clamm_pool::clamm_pool::<field>`).
pub fn pool_slot(field: &str) -> Result<StorageSlotName> {
    StorageSlotName::new(format!("clamm_pool::clamm_pool::{field}"))
        .with_context(|| format!("invalid clamm-pool slot name for field {field}"))
}

// ================================================================================================
// Word packing (host mirror)
// ================================================================================================

pub fn u128_to_word(x: u128) -> Word {
    Word::new([
        Felt::from(x as u32),
        Felt::from((x >> 32) as u32),
        Felt::from((x >> 64) as u32),
        Felt::from((x >> 96) as u32),
    ])
}

pub fn word_to_u128(w: Word) -> u128 {
    let mut x: u128 = 0;
    for i in 0..4 {
        let limb = w[i].as_canonical_u64();
        assert!(limb <= 0xFFFF_FFFF, "storage limb exceeds u32: {limb}");
        x |= (limb as u128) << (32 * i);
    }
    x
}

pub fn u256_to_words(x: U256) -> (Word, Word) {
    (
        u128_to_word(x[0] as u128 | ((x[1] as u128) << 64)),
        u128_to_word(x[2] as u128 | ((x[3] as u128) << 64)),
    )
}

pub fn words_to_u256(lo: Word, hi: Word) -> U256 {
    let l = word_to_u128(lo);
    let h = word_to_u128(hi);
    [l as u64, (l >> 64) as u64, h as u64, (h >> 64) as u64]
}

pub fn u256_add(a: U256, b: U256) -> U256 {
    let mut out = [0u64; 4];
    let mut carry = 0u64;
    for i in 0..4 {
        let (s1, c1) = a[i].overflowing_add(b[i]);
        let (s2, c2) = s1.overflowing_add(carry);
        out[i] = s2;
        carry = (c1 as u64) + (c2 as u64);
    }
    out
}

pub fn u256_sub(a: U256, b: U256) -> U256 {
    let mut out = [0u64; 4];
    let mut borrow = 0u64;
    for i in 0..4 {
        let (d1, b1) = a[i].overflowing_sub(b[i]);
        let (d2, b2) = d1.overflowing_sub(borrow);
        out[i] = d2;
        borrow = (b1 as u64) + (b2 as u64);
    }
    out
}

/// `floor((fee << 128) / liquidity)` (guest `fee_growth_increment` mirror).
pub fn fee_growth_increment(fee: u128, liquidity: u128) -> U256 {
    assert!(liquidity > 0);
    let f = wide::limbs_from_u128(fee);
    let dividend = [0u64, 0, f[0], f[1]];
    let (q, _r) = wide::div_rem(&dividend, &wide::limbs_from_u128(liquidity));
    [q[0], q[1], q[2], q[3]]
}

/// `(liquidity * delta) >> 128` truncated to u128 (guest `fees_owed` mirror).
pub fn fees_owed(delta: U256, liquidity: u128) -> u128 {
    let mut prod = [0u64; 6];
    wide::mul_limbs(&delta, &wide::limbs_from_u128(liquidity), &mut prod);
    (prod[2] as u128) | ((prod[3] as u128) << 64)
}

// ================================================================================================
// Keys (host mirror)
// ================================================================================================

/// Poseidon2 position-key base for (owner, tickLower, tickUpper); the
/// 5-element preimage matches the guest exactly.
pub fn position_key(
    owner_suffix: Felt,
    owner_prefix: Felt,
    tick_lower: i32,
    tick_upper: i32,
    field: u32,
) -> Word {
    let digest = Poseidon2::hash_elements(&[
        owner_suffix,
        owner_prefix,
        Felt::from((tick_lower + TICK_OFF) as u32),
        Felt::from((tick_upper + TICK_OFF) as u32),
        Felt::from(POSITION_DOMAIN),
    ]);
    Word::new([digest[0], digest[1], digest[2], Felt::from(field)])
}

pub fn tick_key(tick: i32, group: u32) -> Word {
    Word::new([
        Felt::from((tick + TICK_OFF) as u32),
        Felt::from(group),
        Felt::from(0u32),
        Felt::from(0u32),
    ])
}

pub fn bitmap_key(word_index: u32) -> Word {
    Word::new([
        Felt::from(word_index),
        Felt::from(0u32),
        Felt::from(0u32),
        Felt::from(0u32),
    ])
}

/// (word_index, bit) of a compressed tick in the 128-bit bitmap words.
pub fn bitmap_position(tick: i32, spacing: i32) -> (u32, u32) {
    assert!(tick % spacing == 0);
    let c_off = (tick / spacing + TICK_OFF) as u32;
    (c_off >> 7, c_off & 127)
}

/// Encodes a tick for note storage (offset-encoded felt).
pub fn tick_felt(tick: i32) -> Felt {
    Felt::from((tick + TICK_OFF) as u32)
}

/// Splits a u128 into the 4 little-endian u32-limb felts used in note storage.
pub fn u128_limb_felts(x: u128) -> [Felt; 4] {
    [
        Felt::from(x as u32),
        Felt::from((x >> 32) as u32),
        Felt::from((x >> 64) as u32),
        Felt::from((x >> 96) as u32),
    ]
}

// ================================================================================================
// Pool simulator: exact host-side mirror of the guest logic
// ================================================================================================

#[derive(Default, Clone, Debug)]
pub struct TickData {
    pub gross: u128,
    pub net: i128,
    pub fg_out0: U256,
    pub fg_out1: U256,
}

#[derive(Default, Clone, Debug)]
pub struct PositionData {
    pub liquidity: u128,
    pub fg_inside0_last: U256,
    pub fg_inside1_last: U256,
    pub tokens_owed0: u64,
    pub tokens_owed1: u64,
}

#[derive(Debug)]
pub struct SwapOutcome {
    pub amount_out: u128,
    pub end_sqrt_price: u128,
    pub end_tick: i32,
    pub end_liquidity: u128,
    /// Number of initialized ticks crossed.
    pub crossings: u32,
    /// Loop iterations consumed (guest bound: MAX_TICK_CROSSINGS).
    pub iterations: u32,
    /// Total fee charged on the input token (sum of step fees).
    pub total_fee: u128,
}

/// Host-side pool state mirror. All methods reproduce the guest algorithms
/// exactly (same amm-math calls, same rounding, same iteration structure).
#[derive(Clone)]
pub struct PoolSim {
    pub fee_pips: u32,
    pub spacing: i32,
    pub sqrt_price: u128,
    pub tick: i32,
    pub liquidity: u128,
    pub fg0: U256,
    pub fg1: U256,
    pub ticks: BTreeMap<i32, TickData>,
    pub bitmap: BTreeMap<u32, u128>,
    /// Keyed by (tick_lower, tick_upper); tests use one LP per range.
    pub positions: BTreeMap<(i32, i32), PositionData>,
}

impl PoolSim {
    pub fn new(fee_pips: u32, spacing: i32, initial_tick: i32) -> Self {
        Self {
            fee_pips,
            spacing,
            sqrt_price: tick_math::get_sqrt_ratio_at_tick(initial_tick),
            tick: initial_tick,
            liquidity: 0,
            fg0: [0; 4],
            fg1: [0; 4],
            ticks: BTreeMap::new(),
            bitmap: BTreeMap::new(),
            positions: BTreeMap::new(),
        }
    }

    fn bitmap_word(&self, wi: u32) -> u128 {
        self.bitmap.get(&wi).copied().unwrap_or(0)
    }

    fn bitmap_flip(&mut self, tick: i32) {
        let (wi, bit) = bitmap_position(tick, self.spacing);
        let w = self.bitmap_word(wi) ^ (1u128 << bit);
        if w == 0 {
            self.bitmap.remove(&wi);
        } else {
            self.bitmap.insert(wi, w);
        }
    }

    fn floor_div(a: i32, b: i32) -> i32 {
        let q = a / b;
        if a % b != 0 && ((a < 0) != (b < 0)) {
            q - 1
        } else {
            q
        }
    }

    fn next_initialized_tick(&self, tick: i32, lte: bool) -> (i32, bool) {
        let spacing = self.spacing;
        let compressed = Self::floor_div(tick, spacing);
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
                let msb = 127 - masked.leading_zeros();
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
                let lsb = masked.trailing_zeros();
                ((((wi << 7) | lsb) as i32 - TICK_OFF) * spacing, true)
            } else {
                ((((wi << 7) | 127) as i32 - TICK_OFF) * spacing, false)
            }
        }
    }

    fn tick_update(&mut self, tick: i32, delta: i128, upper: bool) -> bool {
        let tick_cur = self.tick;
        let (fg0, fg1) = (self.fg0, self.fg1);
        let data = self.ticks.entry(tick).or_default();
        let gross_before = data.gross;
        let gross_after = liquidity_math::add_delta(gross_before, delta);
        let flipped = (gross_after == 0) != (gross_before == 0);
        if gross_before == 0 && tick <= tick_cur {
            data.fg_out0 = fg0;
            data.fg_out1 = fg1;
        }
        data.gross = gross_after;
        if upper {
            data.net = data.net.checked_sub(delta).unwrap();
        } else {
            data.net = data.net.checked_add(delta).unwrap();
        }
        flipped
    }

    fn tick_data(&self, tick: i32) -> TickData {
        self.ticks.get(&tick).cloned().unwrap_or_default()
    }

    fn fee_growth_inside(&self, lower: i32, upper: i32) -> (U256, U256) {
        let l = self.tick_data(lower);
        let u = self.tick_data(upper);
        let (below0, below1) = if self.tick >= lower {
            (l.fg_out0, l.fg_out1)
        } else {
            (u256_sub(self.fg0, l.fg_out0), u256_sub(self.fg1, l.fg_out1))
        };
        let (above0, above1) = if self.tick < upper {
            (u.fg_out0, u.fg_out1)
        } else {
            (u256_sub(self.fg0, u.fg_out0), u256_sub(self.fg1, u.fg_out1))
        };
        (
            u256_sub(u256_sub(self.fg0, below0), above0),
            u256_sub(u256_sub(self.fg1, below1), above1),
        )
    }

    fn update_position(&mut self, lower: i32, upper: i32, delta: i128) {
        let mut flipped_lower = false;
        let mut flipped_upper = false;
        if delta != 0 {
            flipped_lower = self.tick_update(lower, delta, false);
            flipped_upper = self.tick_update(upper, delta, true);
            if flipped_lower {
                self.bitmap_flip(lower);
            }
            if flipped_upper {
                self.bitmap_flip(upper);
            }
        }
        let (inside0, inside1) = self.fee_growth_inside(lower, upper);
        let pos = self.positions.entry((lower, upper)).or_default();
        let mut fees0: u128 = 0;
        let mut fees1: u128 = 0;
        if pos.liquidity > 0 {
            let d0 = u256_sub(inside0, pos.fg_inside0_last);
            let d1 = u256_sub(inside1, pos.fg_inside1_last);
            if d0 != [0; 4] {
                fees0 = fees_owed(d0, pos.liquidity);
            }
            if d1 != [0; 4] {
                fees1 = fees_owed(d1, pos.liquidity);
            }
        }
        pos.liquidity = liquidity_math::add_delta(pos.liquidity, delta);
        pos.fg_inside0_last = inside0;
        pos.fg_inside1_last = inside1;
        pos.tokens_owed0 = pos.tokens_owed0.checked_add(fees0 as u64).unwrap();
        pos.tokens_owed1 = pos.tokens_owed1.checked_add(fees1 as u64).unwrap();
        if delta < 0 {
            if flipped_lower {
                self.ticks.remove(&lower);
            }
            if flipped_upper {
                self.ticks.remove(&upper);
            }
        }
    }

    /// Amounts spanned by `liq` over `[lower, upper]` at the current price
    /// (guest `amounts_for_liquidity` mirror).
    pub fn amounts_for_liquidity(
        &self,
        lower: i32,
        upper: i32,
        liq: u128,
        round_up: bool,
    ) -> (u128, u128) {
        let pl = tick_math::get_sqrt_ratio_at_tick(lower);
        let pu = tick_math::get_sqrt_ratio_at_tick(upper);
        if self.tick < lower {
            (sqrt_price_math::get_amount0_delta(pl, pu, liq, round_up), 0)
        } else if self.tick < upper {
            (
                sqrt_price_math::get_amount0_delta(self.sqrt_price, pu, liq, round_up),
                sqrt_price_math::get_amount1_delta(pl, self.sqrt_price, liq, round_up),
            )
        } else {
            (0, sqrt_price_math::get_amount1_delta(pl, pu, liq, round_up))
        }
    }

    /// Mints `liq` over `[lower, upper]`; returns the amounts owed
    /// (rounded up), i.e. what the pool keeps from the note assets.
    pub fn mint(&mut self, lower: i32, upper: i32, liq: u128) -> (u128, u128) {
        let owed = self.amounts_for_liquidity(lower, upper, liq, true);
        self.update_position(lower, upper, liq as i128);
        if self.tick >= lower && self.tick < upper {
            self.liquidity = liquidity_math::add_delta(self.liquidity, liq as i128);
        }
        owed
    }

    /// Burns `liq` from `[lower, upper]`; principal + settled fees land in
    /// the position's tokensOwed. Returns the principal amounts (rounded
    /// down), mirroring the guest.
    pub fn burn(&mut self, lower: i32, upper: i32, liq: u128) -> (u128, u128) {
        self.update_position(lower, upper, -(liq as i128));
        let (a0, a1) = self.amounts_for_liquidity(lower, upper, liq, false);
        if self.tick >= lower && self.tick < upper {
            self.liquidity = liquidity_math::add_delta(self.liquidity, -(liq as i128));
        }
        let pos = self.positions.get_mut(&(lower, upper)).unwrap();
        pos.tokens_owed0 = pos.tokens_owed0.checked_add(a0 as u64).unwrap();
        pos.tokens_owed1 = pos.tokens_owed1.checked_add(a1 as u64).unwrap();
        (a0, a1)
    }

    /// Collects a position's tokensOwed, zeroing them.
    pub fn collect(&mut self, lower: i32, upper: i32) -> (u64, u64) {
        let pos = self.positions.get_mut(&(lower, upper)).unwrap();
        let owed = (pos.tokens_owed0, pos.tokens_owed1);
        pos.tokens_owed0 = 0;
        pos.tokens_owed1 = 0;
        owed
    }

    /// Exact-input swap mirror of the guest loop. Panics exactly where the
    /// guest panics (iteration bound, unconsumed input at the price bound).
    pub fn swap(&mut self, amount_in: u64, zero_for_one: bool) -> SwapOutcome {
        let limit: u128 = if zero_for_one {
            tick_math::MIN_SQRT_RATIO
        } else {
            tick_math::MAX_SQRT_RATIO
        };
        let mut remaining: u128 = amount_in as u128;
        let mut amount_out_total: u128 = 0;
        let mut iterations = 0u32;
        let mut crossings = 0u32;
        let mut total_fee: u128 = 0;
        let mut needs_reverse_map = false;

        let mut sqrt_price = self.sqrt_price;
        let mut tick = self.tick;
        let mut liquidity = self.liquidity;
        let mut fg_in = if zero_for_one { self.fg0 } else { self.fg1 };

        loop {
            if remaining == 0 || sqrt_price == limit {
                break;
            }
            iterations += 1;
            assert!(
                iterations <= MAX_TICK_CROSSINGS,
                "sim: swap exceeds max tick crossings"
            );

            let (next_tick_raw, initialized) = self.next_initialized_tick(tick, zero_for_one);
            let next_tick = next_tick_raw.clamp(tick_math::MIN_TICK, tick_math::MAX_TICK);
            let target = tick_math::get_sqrt_ratio_at_tick(next_tick);

            let (next_price, step_in, step_out, step_fee) = swap_math::compute_swap_step(
                sqrt_price,
                target,
                liquidity,
                remaining as i128,
                self.fee_pips,
            );

            remaining = remaining.checked_sub(step_in.checked_add(step_fee).unwrap()).unwrap();
            amount_out_total = amount_out_total.checked_add(step_out).unwrap();
            total_fee = total_fee.checked_add(step_fee).unwrap();

            if liquidity > 0 && step_fee > 0 {
                fg_in = u256_add(fg_in, fee_growth_increment(step_fee, liquidity));
            }

            let reached_target = next_price == target;
            if reached_target && initialized {
                // cross
                let (fg0_now, fg1_now) = if zero_for_one {
                    (fg_in, self.fg1)
                } else {
                    (self.fg0, fg_in)
                };
                let data = self.ticks.get_mut(&next_tick).unwrap();
                data.fg_out0 = u256_sub(fg0_now, data.fg_out0);
                data.fg_out1 = u256_sub(fg1_now, data.fg_out1);
                let liq_net = data.net;
                let delta = if zero_for_one { -liq_net } else { liq_net };
                liquidity = liquidity_math::add_delta(liquidity, delta);
                crossings += 1;
            }
            if reached_target {
                tick = if zero_for_one { next_tick - 1 } else { next_tick };
            }
            needs_reverse_map = !reached_target && next_price != sqrt_price;
            sqrt_price = next_price;
        }

        assert!(remaining == 0, "sim: insufficient liquidity to consume swap input");
        if needs_reverse_map {
            tick = tick_math::get_tick_at_sqrt_ratio(sqrt_price);
        }
        assert!(amount_out_total > 0, "sim: zero swap output");

        self.sqrt_price = sqrt_price;
        self.tick = tick;
        self.liquidity = liquidity;
        if zero_for_one {
            self.fg0 = fg_in;
        } else {
            self.fg1 = fg_in;
        }

        SwapOutcome {
            amount_out: amount_out_total,
            end_sqrt_price: sqrt_price,
            end_tick: tick,
            end_liquidity: liquidity,
            crossings,
            iterations,
            total_fee,
        }
    }
}

// ================================================================================================
// MockChain testbed shared by the clamm-pool integration tests
// ================================================================================================

pub mod testbed {
    use std::{path::Path, sync::Arc};

    use anyhow::{Context, Result};
    use miden_client::{
        account::{
            component::{InitStorageData, StorageValueName},
            Account, AccountBuilder, AccountComponent, AccountId, AccountType,
        },
        asset::{AssetCallbackFlag, AssetVaultKey, FungibleAsset},
        auth::AuthSchemeId,
        crypto::{Poseidon2, RandomCoin},
        note::{Note, NoteId, NoteScript, NoteTag},
        transaction::RawOutputNote,
        Felt, Word,
    };
    use miden_mast_package::Package;
    use miden_standards::note::{NetworkAccountTarget, NoteExecutionHint, P2idNoteStorage};
    use miden_standards::testing::note::NoteBuilder;
    use miden_testing::{AccountState, Auth, MockChain, MockChainBuilder};
    use miden_client::note::NoteType;
    use miden_client::transaction::ExecutedTransaction;

    use super::{pool_slot, tick_felt, u128_limb_felts, u128_to_word, TICK_OFF};
    use amm_math::tick_math;

    /// Basic-auth shorthand used for all signing accounts.
    fn basic_auth() -> Auth {
        Auth::BasicAuth {
            auth_scheme: AuthSchemeId::Falcon512Poseidon2,
        }
    }

    /// Which contract build backs the testbed.
    ///
    /// - `RustHarness`: Rust-SDK pool + Phase 2 harness notes (`pool-note-*`).
    /// - `RustProduction`: Rust-SDK pool + Phase 3 production notes (`amm-note-*`)
    ///   with Rust-SDK basic-wallet senders (their reclaim path `call`s that
    ///   package's `receive_asset` root).
    /// - `Masm`: hand-written MASM pool + MASM two-path notes, standard-BasicWallet
    ///   senders (the MASM notes reclaim through the STANDARD `receive_asset`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Backend {
        RustHarness,
        RustProduction,
        Masm,
    }

    /// The per-kind note scripts a testbed seeds notes with (backend-independent
    /// surface: the note builders only need scripts).
    #[derive(Clone)]
    pub struct PoolScripts {
        pub swap: NoteScript,
        pub mint: NoteScript,
        pub burn: NoteScript,
        pub collect: NoteScript,
    }

    /// The compiled pool + note packages a testbed runs on. Phase 2 uses
    /// the harness notes (`pool-note-*`, no branching); Phase 3 uses the
    /// production notes (`amm-note-*`, P2IDE-style two-path scripts) plus
    /// the Rust-SDK `basic-wallet` package whose component reclaim-capable
    /// sender accounts must carry (the production notes' reclaim path
    /// `call`s the MAST root of THAT package's `receive_asset`).
    pub struct PoolPackages {
        pub pool: Arc<Package>,
        pub swap_note: Arc<Package>,
        pub mint_note: Arc<Package>,
        pub burn_note: Arc<Package>,
        pub collect_note: Arc<Package>,
        /// `Some` only for the production set.
        pub wallet: Option<Arc<Package>>,
    }

    impl Clone for PoolPackages {
        fn clone(&self) -> Self {
            Self {
                pool: self.pool.clone(),
                swap_note: self.swap_note.clone(),
                mint_note: self.mint_note.clone(),
                burn_note: self.burn_note.clone(),
                collect_note: self.collect_note.clone(),
                wallet: self.wallet.clone(),
            }
        }
    }

    /// Serializes cargo-miden invocations: tests run on parallel threads
    /// and concurrent builds of the same contract crates race on their
    /// shared target directories, corrupting `.masp` reads.
    static BUILD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Per-process caches: each package set is built exactly once per test
    /// binary. Repeated back-to-back `cargo miden build` invocations are
    /// not just slow -- a note build's nested dependency build can still
    /// be flushing the dependency `.masp` when the next invocation's
    /// `#[account]` macro reads it (observed as "failed to deserialize
    /// dependency package ...: unexpected end of file"), so building once
    /// and cloning the `Arc`s avoids the race window entirely.
    static HARNESS_PACKAGES: std::sync::OnceLock<PoolPackages> = std::sync::OnceLock::new();
    static PRODUCTION_PACKAGES: std::sync::OnceLock<PoolPackages> = std::sync::OnceLock::new();

    /// Waits until the on-disk dependency `.masp` artifacts stop changing:
    /// a note build's nested dependency build can still be flushing them
    /// when `cargo_miden::run` returns, and the NEXT build's `#[account]`
    /// macro then reads a truncated file. Polls size stability (two equal
    /// non-zero sizes 300ms apart), bounded at ~15s per file.
    fn settle_dependency_artifacts() {
        const DEP_ARTIFACTS: [&str; 2] = [
            "../contracts/clamm-pool/target/miden/release/clamm-pool.masp",
            "../contracts/basic-wallet/target/miden/release/basic-wallet.masp",
        ];
        for path in DEP_ARTIFACTS {
            let path = Path::new(path);
            let mut last: u64 = 0;
            for _ in 0..50 {
                let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                if size > 0 && size == last {
                    break;
                }
                last = size;
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }
    }

    /// Builds one package, retrying once after a settle delay: the known
    /// transient failure mode is a truncated dependency `.masp` read (see
    /// `settle_dependency_artifacts`), and a genuine compile error simply
    /// fails identically twice.
    fn build_package(dir: &str) -> Result<Arc<Package>> {
        settle_dependency_artifacts();
        let package = match integration_build(dir) {
            Ok(p) => p,
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_secs(3));
                settle_dependency_artifacts();
                integration_build(dir).with_context(|| format!("building {dir} (retry)"))?
            }
        };
        // A build's trailing nested dependency write may still be in
        // flight when `cargo_miden::run` returns; give it a moment before
        // the next invocation reads the same artifacts.
        std::thread::sleep(std::time::Duration::from_secs(1));
        Ok(Arc::new(package))
    }

    pub fn build_pool_packages() -> Result<PoolPackages> {
        if let Some(cached) = HARNESS_PACKAGES.get() {
            return Ok(cached.clone());
        }
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        if let Some(cached) = HARNESS_PACKAGES.get() {
            return Ok(cached.clone());
        }
        let built = PoolPackages {
            pool: build_package("../contracts/clamm-pool")?,
            swap_note: build_package("../contracts/pool-note-swap")?,
            mint_note: build_package("../contracts/pool-note-mint")?,
            burn_note: build_package("../contracts/pool-note-burn")?,
            collect_note: build_package("../contracts/pool-note-collect")?,
            wallet: None,
        };
        let _ = HARNESS_PACKAGES.set(built.clone());
        Ok(built)
    }

    /// Builds the Phase 3 production package set. Build order matters: the
    /// notes' WIT dependencies (`clamm-pool`, `basic-wallet`) must be built
    /// first so their `target/generated-wit/` exists.
    pub fn build_production_packages() -> Result<PoolPackages> {
        if let Some(cached) = PRODUCTION_PACKAGES.get() {
            return Ok(cached.clone());
        }
        let _guard = BUILD_LOCK.lock().expect("build lock poisoned");
        if let Some(cached) = PRODUCTION_PACKAGES.get() {
            return Ok(cached.clone());
        }
        let pool = build_package("../contracts/clamm-pool")?;
        let wallet = build_package("../contracts/basic-wallet")?;
        let built = PoolPackages {
            pool,
            swap_note: build_package("../contracts/amm-note-swap")?,
            mint_note: build_package("../contracts/amm-note-mint")?,
            burn_note: build_package("../contracts/amm-note-burn")?,
            collect_note: build_package("../contracts/amm-note-collect")?,
            wallet: Some(wallet),
        };
        let _ = PRODUCTION_PACKAGES.set(built.clone());
        Ok(built)
    }

    fn integration_build(dir: &str) -> Result<Package> {
        crate::helpers::build_project_in_dir(Path::new(dir), true)
    }

    /// Amount of each token pre-funded into each wallet.
    pub const WALLET_FUND: u64 = 10_000_000_000_000_000; // 1e16

    /// Builder-phase testbed: create accounts + pool, then seed notes, then
    /// `build()` into a MockChain.
    pub struct PoolTestbed {
        pub builder: MockChainBuilder,
        pub backend: Backend,
        pub scripts: PoolScripts,
        pub lp: Account,
        pub trader: Account,
        pub token0: AccountId,
        pub token1: AccountId,
        pub pool: Account,
        pub fee_pips: u32,
        pub spacing: i32,
        pub initial_tick: i32,
        rng_swap: RandomCoin,
        rng_mint: RandomCoin,
        rng_burn: RandomCoin,
        rng_collect: RandomCoin,
    }

    impl PoolTestbed {
        /// Phase 2 testbed: harness notes, standard-BasicWallet senders.
        pub fn new(fee_pips: u32, spacing: i32, initial_tick: i32) -> Result<Self> {
            Self::for_backend(Backend::RustHarness, fee_pips, spacing, initial_tick)
        }

        /// Phase 3 testbed: production notes (allowlisted by their own
        /// roots) and sender wallets carrying the Rust-SDK `basic-wallet`
        /// component so the notes' reclaim path can `call receive_asset`.
        pub fn new_production(fee_pips: u32, spacing: i32, initial_tick: i32) -> Result<Self> {
            Self::for_backend(Backend::RustProduction, fee_pips, spacing, initial_tick)
        }

        /// Stage 2 testbed: hand-written MASM pool + MASM notes, with
        /// standard-BasicWallet senders (`add_existing_wallet_with_assets`) —
        /// the MASM notes' reclaim path targets the STANDARD `receive_asset`.
        pub fn new_masm(fee_pips: u32, spacing: i32, initial_tick: i32) -> Result<Self> {
            Self::for_backend(Backend::Masm, fee_pips, spacing, initial_tick)
        }

        /// Backend-parameterized constructor; the scenario suites run their
        /// bodies against every applicable backend through this entry.
        pub fn for_backend(
            backend: Backend,
            fee_pips: u32,
            spacing: i32,
            initial_tick: i32,
        ) -> Result<Self> {
            let mut builder = MockChain::builder();

            let token0 = builder
                .add_existing_basic_faucet(basic_auth(), "TKA", 9_000_000_000_000_000_000, None)?
                .id();
            let token1 = builder
                .add_existing_basic_faucet(basic_auth(), "TKB", 9_000_000_000_000_000_000, None)?
                .id();

            let fund = |faucet0: AccountId, faucet1: AccountId| -> Result<[
                miden_client::asset::Asset; 2]> {
                Ok([
                    FungibleAsset::new(faucet0, WALLET_FUND)?.into(),
                    FungibleAsset::new(faucet1, WALLET_FUND)?.into(),
                ])
            };

            // Per-backend artifacts: note scripts + the pool component source.
            let packages = match backend {
                Backend::RustHarness => Some(build_pool_packages()?),
                Backend::RustProduction => Some(build_production_packages()?),
                Backend::Masm => None,
            };
            let scripts = match packages.as_ref() {
                Some(packages) => {
                    let script = |p: &Arc<Package>| -> Result<NoteScript> {
                        NoteScript::from_package(p.as_ref()).context("note script from package")
                    };
                    PoolScripts {
                        swap: script(&packages.swap_note)?,
                        mint: script(&packages.mint_note)?,
                        burn: script(&packages.burn_note)?,
                        collect: script(&packages.collect_note)?,
                    }
                },
                None => PoolScripts {
                    swap: clamm_pool_masm::note_script(clamm_pool_masm::PoolNoteKind::Swap).clone(),
                    mint: clamm_pool_masm::note_script(clamm_pool_masm::PoolNoteKind::Mint).clone(),
                    burn: clamm_pool_masm::note_script(clamm_pool_masm::PoolNoteKind::Burn).clone(),
                    collect: clamm_pool_masm::note_script(clamm_pool_masm::PoolNoteKind::Collect)
                        .clone(),
                },
            };

            // Sender wallets. The Rust production notes need the Rust-SDK
            // basic-wallet component (their reclaim `call` targets that
            // package's `receive_asset` root); the harness and MASM backends
            // use standard BasicWallet accounts.
            let (lp, trader) = if let Some(wallet_pkg) =
                packages.as_ref().and_then(|p| p.wallet.as_ref())
            {
                let wallet_component =
                    AccountComponent::from_package(wallet_pkg.as_ref(), &InitStorageData::default())
                        .context("building basic-wallet account component")?;
                let mut mk = |seed: [u8; 32]| -> Result<Account> {
                    builder.add_account_from_builder(
                        basic_auth(),
                        AccountBuilder::new(seed)
                            .account_type(AccountType::Public)
                            .with_component(wallet_component.clone())
                            .with_assets(fund(token0, token1)?),
                        AccountState::Exists,
                    )
                };
                (mk([21_u8; 32])?, mk([22_u8; 32])?)
            } else {
                (
                    builder.add_existing_wallet_with_assets(basic_auth(), fund(token0, token1)?)?,
                    builder.add_existing_wallet_with_assets(basic_auth(), fund(token0, token1)?)?,
                )
            };

            // Note-script roots for the network-account allowlist.
            let allowed = [
                scripts.swap.root(),
                scripts.mint.root(),
                scripts.burn.root(),
                scripts.collect.root(),
            ]
            .into_iter()
            .collect();

            // Pool init storage (DESIGN Part 2 layout), shared by both backends.
            let pool_config = Word::new([
                token0.suffix(),
                Felt::from(token0.prefix()),
                token1.suffix(),
                Felt::from(token1.prefix()),
            ]);
            let pool_params = Word::new([
                Felt::from(fee_pips),
                Felt::from(spacing as u32),
                Felt::from(0u32),
                Felt::from(0u32),
            ]);
            let p2id_root = Word::from(miden_standards::note::P2idNote::script_root());
            let sqrt_price = u128_to_word(tick_math::get_sqrt_ratio_at_tick(initial_tick));
            let pool_state = Word::new([
                Felt::from((initial_tick + TICK_OFF) as u32),
                Felt::from(1u32),
                Felt::from(0u32),
                Felt::from(0u32),
            ]);

            let pool_component = match packages.as_ref() {
                Some(packages) => {
                    let mut init = InitStorageData::default();
                    let mut set = |field: &str, w: Word| -> Result<()> {
                        let slot = pool_slot(field)?;
                        init.insert_value(StorageValueName::from_slot_name(&slot), w)?;
                        Ok(())
                    };
                    set("pool_config", pool_config)?;
                    set("pool_params", pool_params)?;
                    set("p2id_root", p2id_root)?;
                    set("sqrt_price", sqrt_price)?;
                    set("pool_state", pool_state)?;
                    set("liquidity", Word::default())?;
                    set("fee_growth_global0_lo", Word::default())?;
                    set("fee_growth_global0_hi", Word::default())?;
                    set("fee_growth_global1_lo", Word::default())?;
                    set("fee_growth_global1_hi", Word::default())?;
                    AccountComponent::from_package(&packages.pool, &init)
                        .context("building pool account component")?
                },
                None => clamm_pool_masm::component(&clamm_pool_masm::PoolInitStorage {
                    pool_config,
                    pool_params,
                    p2id_root,
                    sqrt_price,
                    pool_state,
                }),
            };
            let pool = builder.add_account_from_builder(
                Auth::NetworkAccount {
                    allowed_script_roots: allowed,
                    allowed_tx_script_roots: Default::default(),
                },
                AccountBuilder::new([11_u8; 32])
                    .account_type(AccountType::Public)
                    .with_component(pool_component),
                AccountState::Exists,
            )?;

            let seed_rng = |s: &NoteScript| RandomCoin::new(Word::from(s.root()));
            let rng_swap = seed_rng(&scripts.swap);
            let rng_mint = seed_rng(&scripts.mint);
            let rng_burn = seed_rng(&scripts.burn);
            let rng_collect = seed_rng(&scripts.collect);

            Ok(Self {
                builder,
                backend,
                scripts,
                lp,
                trader,
                token0,
                token1,
                pool,
                fee_pips,
                spacing,
                initial_tick,
                rng_swap,
                rng_mint,
                rng_burn,
                rng_collect,
            })
        }

        fn pool_id_felts(&self) -> (Felt, Felt) {
            (self.pool.id().suffix(), Felt::from(self.pool.id().prefix()))
        }

        /// Seeds a swap note. `direction`: 0 = zero_for_one (token0 in).
        pub fn add_swap_note(
            &mut self,
            sender: AccountId,
            direction: u32,
            amount_in: u64,
            min_out: u64,
            recipient: AccountId,
            deadline: u32,
        ) -> Result<Note> {
            let faucet = if direction == 0 { self.token0 } else { self.token1 };
            self.add_swap_note_with_asset(
                sender, direction, faucet, amount_in, min_out, recipient, deadline,
            )
        }

        /// Seeds a swap note with an explicit input-asset faucet (used by
        /// the wrong-faucet failure test).
        pub fn add_swap_note_with_asset(
            &mut self,
            sender: AccountId,
            direction: u32,
            faucet: AccountId,
            amount_in: u64,
            min_out: u64,
            recipient: AccountId,
            deadline: u32,
        ) -> Result<Note> {
            let (ps, pp) = self.pool_id_felts();
            let storage = vec![
                ps,
                pp,
                Felt::from(direction),
                Felt::from(min_out as u32),
                Felt::from((min_out >> 32) as u32),
                recipient.suffix(),
                Felt::from(recipient.prefix()),
                Felt::from(deadline),
            ];
            let note = NoteBuilder::new(sender, &mut self.rng_swap)
                .script(self.scripts.swap.clone())
                .add_assets([FungibleAsset::new(faucet, amount_in)?.into()])
                .note_storage(storage)?
                .build()?;
            self.builder.add_output_note(RawOutputNote::Full(note.clone()));
            Ok(note)
        }

        /// Seeds a swap note whose storage points at an EXPLICIT pool id
        /// (used by the wrong-pool adversarial test).
        #[allow(clippy::too_many_arguments)]
        pub fn add_swap_note_with_pool_id(
            &mut self,
            sender: AccountId,
            pool_id: AccountId,
            direction: u32,
            amount_in: u64,
            min_out: u64,
            recipient: AccountId,
            deadline: u32,
        ) -> Result<Note> {
            let faucet = if direction == 0 { self.token0 } else { self.token1 };
            let storage = vec![
                pool_id.suffix(),
                Felt::from(pool_id.prefix()),
                Felt::from(direction),
                Felt::from(min_out as u32),
                Felt::from((min_out >> 32) as u32),
                recipient.suffix(),
                Felt::from(recipient.prefix()),
                Felt::from(deadline),
            ];
            let note = NoteBuilder::new(sender, &mut self.rng_swap)
                .script(self.scripts.swap.clone())
                .add_assets([FungibleAsset::new(faucet, amount_in)?.into()])
                .note_storage(storage)?
                .build()?;
            self.builder.add_output_note(RawOutputNote::Full(note.clone()));
            Ok(note)
        }

        /// Seeds a network-style swap note: explicitly `NoteType::Public`
        /// with the `NetworkAccountTarget` attachment word
        /// `[target_suffix, target_prefix, exec_hint, 0]` (scheme 2)
        /// targeting the pool, exactly what a Phase 4 network deployment
        /// attaches. MockChain consumption ignores the attachment.
        pub fn add_swap_note_network(
            &mut self,
            sender: AccountId,
            direction: u32,
            amount_in: u64,
            min_out: u64,
            recipient: AccountId,
            deadline: u32,
        ) -> Result<Note> {
            let (ps, pp) = self.pool_id_felts();
            let storage = vec![
                ps,
                pp,
                Felt::from(direction),
                Felt::from(min_out as u32),
                Felt::from((min_out >> 32) as u32),
                recipient.suffix(),
                Felt::from(recipient.prefix()),
                Felt::from(deadline),
            ];
            let faucet = if direction == 0 { self.token0 } else { self.token1 };
            let attachment =
                NetworkAccountTarget::new(self.pool.id(), NoteExecutionHint::always())
                    .context("building NetworkAccountTarget attachment")?;
            // The testnet ntx-builder discovers notes by TAG routing, not
            // (only) by attachment scanning: a network note must carry
            // `NoteTag::with_account_target(pool)` or it is silently orphaned.
            // MockChain consumption ignores the tag, but the testbed mirrors
            // the real deployment byte-for-byte.
            let note = NoteBuilder::new(sender, &mut self.rng_swap)
                .script(self.scripts.swap.clone())
                .note_type(NoteType::Public)
                .tag(NoteTag::with_account_target(self.pool.id()).into())
                .attachment(attachment)
                .add_assets([FungibleAsset::new(faucet, amount_in)?.into()])
                .note_storage(storage)?
                .build()?;
            self.builder.add_output_note(RawOutputNote::Full(note.clone()));
            Ok(note)
        }

        /// Seeds a mint note carrying `amount0` token0 and `amount1` token1
        /// (either may be zero, in which case the asset is omitted).
        pub fn add_mint_note(
            &mut self,
            sender: AccountId,
            lower: i32,
            upper: i32,
            liq: u128,
            amount0: u64,
            amount1: u64,
            deadline: u32,
        ) -> Result<Note> {
            let (ps, pp) = self.pool_id_felts();
            let l = u128_limb_felts(liq);
            let storage = vec![
                ps,
                pp,
                tick_felt(lower),
                tick_felt(upper),
                l[0],
                l[1],
                l[2],
                l[3],
                Felt::from(deadline),
            ];
            let mut assets = Vec::new();
            if amount0 > 0 {
                assets.push(FungibleAsset::new(self.token0, amount0)?.into());
            }
            if amount1 > 0 {
                assets.push(FungibleAsset::new(self.token1, amount1)?.into());
            }
            let note = NoteBuilder::new(sender, &mut self.rng_mint)
                .script(self.scripts.mint.clone())
                .add_assets(assets)
                .note_storage(storage)?
                .build()?;
            self.builder.add_output_note(RawOutputNote::Full(note.clone()));
            Ok(note)
        }

        pub fn add_burn_note(
            &mut self,
            sender: AccountId,
            lower: i32,
            upper: i32,
            liq: u128,
        ) -> Result<Note> {
            let (ps, pp) = self.pool_id_felts();
            let l = u128_limb_felts(liq);
            let storage = vec![ps, pp, tick_felt(lower), tick_felt(upper), l[0], l[1], l[2], l[3]];
            let note = NoteBuilder::new(sender, &mut self.rng_burn)
                .script(self.scripts.burn.clone())
                .note_storage(storage)?
                .build()?;
            self.builder.add_output_note(RawOutputNote::Full(note.clone()));
            Ok(note)
        }

        pub fn add_collect_note(&mut self, sender: AccountId, lower: i32, upper: i32) -> Result<Note> {
            let (ps, pp) = self.pool_id_felts();
            let storage = vec![ps, pp, tick_felt(lower), tick_felt(upper)];
            let note = NoteBuilder::new(sender, &mut self.rng_collect)
                .script(self.scripts.collect.clone())
                .note_storage(storage)?
                .build()?;
            self.builder.add_output_note(RawOutputNote::Full(note.clone()));
            Ok(note)
        }

        pub fn build(self) -> Result<(MockChain, PoolHandles)> {
            let chain = self.builder.build()?;
            Ok((
                chain,
                PoolHandles {
                    lp: self.lp,
                    trader: self.trader,
                    token0: self.token0,
                    token1: self.token1,
                    pool: self.pool,
                    fee_pips: self.fee_pips,
                    spacing: self.spacing,
                    initial_tick: self.initial_tick,
                },
            ))
        }
    }

    /// Post-build handles.
    pub struct PoolHandles {
        pub lp: Account,
        pub trader: Account,
        pub token0: AccountId,
        pub token1: AccountId,
        pub pool: Account,
        pub fee_pips: u32,
        pub spacing: i32,
        pub initial_tick: i32,
    }

    impl PoolHandles {
        /// A fresh sim mirroring the pool's initial state.
        pub fn sim(&self) -> super::PoolSim {
            super::PoolSim::new(self.fee_pips, self.spacing, self.initial_tick)
        }
    }

    /// Attempts to consume one note against `account` WITHOUT committing.
    /// Any failure (context building, execution) surfaces as `Err`; a
    /// success leaves the chain untouched (nothing is committed).
    pub async fn try_consume(
        mock_chain: &MockChain,
        account: AccountId,
        note: NoteId,
    ) -> Result<ExecutedTransaction> {
        Ok(mock_chain
            .build_tx_context(account, &[note], &[])?
            .build()?
            .execute()
            .await?)
    }

    /// Consumes one note against the pool, commits the block, and returns
    /// the executed transaction.
    pub async fn consume_note(
        mock_chain: &mut MockChain,
        pool: AccountId,
        note: NoteId,
    ) -> Result<ExecutedTransaction> {
        let executed = mock_chain
            .build_tx_context(pool, &[note], &[])?
            .build()?
            .execute()
            .await?;
        mock_chain.add_pending_executed_transaction(&executed)?;
        mock_chain.prove_next_block()?;
        Ok(executed)
    }

    /// Reads a pool Value slot from the committed account state.
    pub fn read_value(mock_chain: &MockChain, pool: AccountId, field: &str) -> Result<Word> {
        let slot = pool_slot(field)?;
        Ok(mock_chain
            .committed_account(pool)?
            .storage()
            .get_item(&slot)
            .context("reading pool value slot")?)
    }

    /// Reads a pool Map entry from the committed account state.
    pub fn read_map(
        mock_chain: &MockChain,
        pool: AccountId,
        field: &str,
        key: Word,
    ) -> Result<Word> {
        let slot = pool_slot(field)?;
        Ok(mock_chain
            .committed_account(pool)?
            .storage()
            .get_map_item(&slot, key)
            .context("reading pool map entry")?)
    }

    /// Committed pool vault balance for a faucet.
    pub fn vault_balance(mock_chain: &MockChain, account: AccountId, faucet: AccountId) -> Result<u64> {
        let key = AssetVaultKey::new_fungible(faucet, AssetCallbackFlag::default());
        let amount = mock_chain
            .committed_account(account)?
            .vault()
            .get_balance(key)
            .context("reading vault balance")?;
        Ok(amount.as_u64())
    }

    /// Expected serial of a pool-emitted P2ID note: Poseidon2 over the
    /// consumed note's serial + salt (guest `emit_p2id` mirror).
    pub fn expected_p2id_serial(input_note: &Note, salt: u32) -> Word {
        let s = input_note.serial_num();
        Poseidon2::hash_elements(&[s[0], s[1], s[2], s[3], Felt::from(salt)])
    }

    /// Asserts the executed transaction emitted exactly one P2ID note with
    /// the given derivation (input serial + salt), target, and asset list;
    /// returns nothing on success.
    pub fn assert_p2id_output(
        executed: &ExecutedTransaction,
        input_note: &Note,
        salt: u32,
        target: AccountId,
        expected_assets: &[(AccountId, u64)],
    ) -> Result<()> {
        let serial = expected_p2id_serial(input_note, salt);
        let expected_recipient = P2idNoteStorage::new(target).into_recipient(serial);
        let notes: Vec<_> = executed.output_notes().iter().collect();
        anyhow::ensure!(
            notes.len() == 1,
            "expected exactly one output note, got {}",
            notes.len()
        );
        let note = &notes[0];
        anyhow::ensure!(
            note.recipient_digest() == expected_recipient.digest(),
            "output note recipient mismatch (target/serial)"
        );
        let assets = note.assets();
        let mut got: Vec<(AccountId, u64)> = assets
            .iter()
            .map(|a| match a {
                miden_client::asset::Asset::Fungible(f) => (f.faucet_id(), f.amount().as_u64()),
                _ => panic!("unexpected non-fungible asset on output note"),
            })
            .collect();
        got.sort_by_key(|(id, _)| id.prefix().as_u64());
        let mut want = expected_assets.to_vec();
        want.sort_by_key(|(id, _)| id.prefix().as_u64());
        anyhow::ensure!(
            got == want,
            "output note assets mismatch: got {got:?}, want {want:?}"
        );
        Ok(())
    }
}
