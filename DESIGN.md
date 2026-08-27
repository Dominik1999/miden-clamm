# DESIGN.md — Uniswap-v3-style concentrated-liquidity AMM on Miden

Phase 0 deliverable. Everything below was verified against the pinned sources in
`TOOLCHAIN.md` (protocol v0.15.3, compiler v0.9.0, miden-node v0.15.2,
miden-client v0.15.5, miden-vm v0.23.5). Claims cite `repo/path:line` at those
pins. Anything not verifiable in source is labeled **ASSUMPTION**.

---

## Part 1 — Phase 0 answers, from source

### (a) How does a public account consume notes when no owner is online?

Via **network transactions**, built by the **ntx-builder**, a separate service
in the miden-node repo (`miden-node/bin/ntx-builder/`). The mechanics, all
verified in source:

**What makes an account "network".** There is no storage mode, AccountId flag,
or builder switch. `AccountType` is only `Private | Public`
(`protocol/crates/miden-protocol/src/account/account_id/account_type.rs:13-20`).
A network account is defined *structurally*: it must be public AND its storage
must contain a non-empty map slot
`miden::standards::auth::network_account::allowed_note_scripts`
(`protocol/crates/miden-standards/src/account/auth/network_account/network_account.rs:13-50`).
The standard way to get that slot is the **`AuthNetworkAccount` auth
component** (`.../auth_network_account.rs:62-95`), which holds:
- a **note-script-root allowlist** (must be non-empty) — only notes whose
  script root is listed will be consumed;
- a **tx-script-root allowlist** (default empty = no tx scripts permitted).

Both allowlists are **immutable after creation** — the component intentionally
exports no mutation procedure, and the node "would likely not yet respect
updates" anyway (`auth_network_account.rs:57-61`). The node's store classifies
the account once, at creation, forever
(`miden-node/crates/store/src/db/models/queries/accounts.rs:1260-1272`).

**How notes are addressed.** Not via NoteTag (v0.15's `NoteTag` is a plain
32-bit filter; `NoteExecutionMode` was removed —
`protocol/crates/miden-protocol/src/note/note_tag.rs:17-120`,
`protocol/CHANGELOG.md:565`). Network targeting uses a **`NoteAttachment`,
scheme 2 (`NetworkAccountTarget`)**: one word
`[target_id_suffix, target_id_prefix, exec_hint, 0]`
(`protocol/crates/miden-standards/src/note/network_account_target.rs:13-79`).
Constraints: the target must be `AccountType::Public`
(`network_account_target.rs:49-51`) and the **note itself must be
`NoteType::Public`** (`.../network_note.rs:23-27`; the ntx-builder only scans
public output notes, `miden-node/bin/ntx-builder/src/committed_block.rs:37-45`).
Notes can carry assets normally — attachments are orthogonal to assets.

**ntx-builder pipeline** (all from `miden-node/bin/ntx-builder/src/`):
- Discovers candidate notes from the committed-block stream; stores them in a
  local SQLite table keyed by nullifier (`builder.rs:139-199`).
- One actor per network account; **≤ 20 notes per network tx**
  (`DEFAULT_MAX_NOTES_PER_TX`, `lib.rs:101-103`, hard-capped by
  `miden_tx::MAX_NUM_CHECKER_NOTES = 20`), ≤ 4 network txs in flight globally,
  **one in-flight tx per account** (`actor/mod.rs:189-213`).
- **Ordering is NOT FIFO and not specified**: note selection has no `ORDER BY`
  (`db/models/queries/notes.rs:108-137`), then standard-library notes are
  sorted first, then a checker iteratively **eliminates failing notes**
  (`protocol/crates/miden-tx/src/executor/notes_checker.rs:156-199`).
- **Failure/retry semantics** (critical for our slippage design): a note whose
  script panics is retried with block-based exponential backoff
  (eligible again after `round(e^(0.25·attempts))` blocks, `notes.rs:208-228`),
  up to **`max_note_attempts` = 30** (`lib.rs:117-118`), after which it is
  permanently excluded (`Discarded` status; row kept; note stays unconsumed
  on chain). So: not retried forever, not deleted — abandoned after ~30 tries.
- The ntx-builder **executes** the tx itself (`TransactionExecutor` with
  `UnreachableAuth` — no signature; the auth component checks allowlists
  instead) and **delegates proving** to a remote prover
  (`actor/execute.rs:211-228, 454-463`).
- **Cycle cap**: the ntx-builder CLI defaults to **2^18 = 262,144 cycles per
  network tx** (`commands/mod.rs:34,95-102`; library default 2^19), far below
  the protocol max of 2^29. This is our tightest compute budget.
- **Fees**: `ProvenTransaction` carries a fee; the kernel epilogue removes it
  **from the executing account's vault**
  (`protocol/.../asm/kernels/transaction/lib/epilogue.masm:242-299`). For a
  network tx the executing account is the pool — **the pool pays the fee**.
- **Users cannot push a tx against a network account themselves**: the RPC
  rejects post-deployment user submissions executing against a
  network-classified account ("Network transactions may not be submitted by
  users yet") unless the request carries the node's internal auth header
  (`miden-node/crates/rpc/src/server/api.rs:218-249`). First-deployment txs
  are exempt.

**Local dev reality check** (verified against both the pinned node source and
the locally installed binary): **`miden-node bundled start` does not exist in
node v0.15**. Subcommands are `sequencer | full | bootstrap | migrate |
recover`; the sequencer requires `--validator.url` and `--ntx-builder.url`, and
the ntx-builder additionally requires a `miden-remote-prover`. Local
network-transaction validation needs the compose-style 4–5 service stack
(`miden-node/docker-compose.yml:81-170`). The project CLAUDE.md's quick
reference is stale on this point. Phase 4 plans for this.

### (b) Integer arithmetic beyond the field element

The stdlib is now the **Miden core library**, namespace `miden::core::*`
(`miden-vm/crates/lib/core/`). Wide math (`asm/math/`):

| Module | Repr | Ops | Division |
|---|---|---|---|
| `math::u64` | 2×u32 limbs | full: add/sub/mul (wrapping/overflowing/widening — `widening_mul` gives exact u128, 23 cycles), cmp, bit ops, shifts | `div`/`mod`/`divmod` — **advice-backed** (host event handler computes q,r; circuit verifies `q·b + r == a`, `r < b`) — ~56 cycles |
| `math::u128` | 4×u32 limbs | full: add/sub/mul families, cmp, bit ops, shifts (`widening_mul` returns overflow *flag*, not u256) | `div`/`mod`/`divmod` — advice-backed, same pattern (`u128.masm:1480-1526`) |
| `math::u256` | 8×u32 limbs | **partial only**: add/sub, and/or/xor, eq/eqz, `wrapping_mul` | **none** — no div, no cmp (lt/gt), no shifts, no 128×128→256 widening mul |

No integer sqrt anywhere in the library. No secp math module. The doc claim of
"u256 division" in `CoreLibrary` rustdoc is a doc bug — no proc or handler
exists.

VM natives: full u32 family at ~1–4 cycles/op; **out-of-range u32 operands are
proof-soundness-critical UB** ("poison"), hence the stdlib's strict
`u32assert` discipline (`miden-vm/docs/src/user_docs/assembly/u32_operations.md:13-61`) —
we mirror it.

**Rust SDK pipeline** (compiler v0.9.0): guest builds enable Wasm
`wide-arithmetic`; Rust `u64`/`i64` ops lower to `math::u64` + i64 intrinsics;
Rust `u128` add/sub/mul lower to **single `math::u128` calls**
(`compiler/codegen/masm/src/emit/int128.rs:245-260`). Rust `u128 / u128` has
no dedicated MASM lowering — it compiles through 64-bit software emulation
(correct, cycle-heavy; e2e tests exist). Rust `u64 / u64` lowers to the
56-cycle advice-backed `math::u64::div`. `u256` is not representable in Rust
guest code as a primitive.

**Boundaries**: `u128` works inside `#[component]` method bodies but cannot
cross the component interface (WIT mapping tops out at u64,
`compiler/sdk/base-macros/src/types.rs:272-284`) and cannot be a storage value
directly (`WordValue` = one Word; we implement it ourselves packing 4×u32
limbs). Cross-context calls flatten to **≤ 16 felts** of arguments
(`frontend/wasm/src/component/types/mod.rs:48,62`).

**Advice access from guest code exists**: `adv_push_mapvaln(key)`,
`adv_insert`, and commitment-verifying `adv_load_preimage(num_words,
commitment)` (`compiler/sdk/stdlib-sys/src/intrinsics/advice.rs:20-111`,
`.../stdlib/mem.rs:190`).

**Crucial interaction with (a)/(d)**: the transaction host registers the core
library's event handlers (`protocol/crates/miden-tx/src/host/mod.rs:117-126`),
so **advice-backed u64/u128 division works in every execution context,
including network transactions**. Custom advice-map hints, by contrast, can
only be supplied in interactive txs (`TransactionRequestBuilder::
extend_advice_map`) — there is **no user advice channel in a network tx**.
This constrains the hint-and-verify strategy; see Part 3.

### (c) Note script vs account component — capability matrix

Two kernel guards determine everything (`protocol/.../asm/kernels/transaction/api.masm:63-71`,
`lib/account.masm:811-826`): *native-account-context* and
*account-code-origin*. Verified matrix (NS = note script, AC = native
account component code, TS = tx script):

| Capability | NS | AC | TS | Guard |
|---|---|---|---|---|
| Create output note (`output_note::create`) | ✅ | ✅ | ✅ | native ctx only (api.masm:1135) |
| Add asset to output note | ✅* | ✅ | ✅* | native ctx (*asset must exit the vault via an AC proc) |
| Add/remove asset in account vault | ❌ | ✅ | ❌ | native + account-origin (api.masm:573,604) |
| Read active note storage / assets / serial | ✅ | ✅ | ❌ | active-note-exists only |
| **Read active note sender** | ✅ | **✅** | ❌ | active-note-exists only (api.masm:931) |
| Read active note script root | ✅ | ✅ | ❌ | active-note-exists only (api.masm:1058) |
| Read account storage (`get_item`/`get_map_item`) | ❌ | ✅ | ❌ | account-origin (api.masm:350,410) |
| Write account storage | ❌ | ✅ | ❌ | native + account-origin (api.masm:380,496) |
| Read block number / timestamp | ✅ | ✅ | ✅ | none (api.masm:1453,1470) |
| Read foreign public account (FPI) | ✅ | ✅ | ✅ | read-only |

**Sender verification — the make-or-break question — resolved affirmatively.**
The pool's account code, while consuming a note, can call
`active_note::get_sender()` directly: the backing kernel proc has no origin
guard, the active-note pointer stays set through `call`s into account
procedures (`main.masm:104-127`), and the protocol's own account-side
components do exactly this (`ownable2step.masm:150-163`, `rbac.masm:257`).
The sender in note metadata is kernel-committed state (part of
`INPUT_NOTES_COMMITMENT`, a public input) and was **forced to the creating
account's ID at note creation** (`lib/output_note.masm:456,487`) — it cannot
be spoofed by a note script or consumer.

**Trust model** (adversarial): the pool's exported procedures are callable by
*any* note script; note-script arguments and note args have no provenance.
Therefore pool procedures trust only kernel-read state:
`active_note::get_sender()` (who sent the note),
`active_note::get_script_root()` (which code is driving consumption — part of
the RECIPIENT commitment, unforgeable), `active_note::get_storage()` /
`get_assets()` (note-committed data). Defense in depth: the
`AuthNetworkAccount` component independently rejects any note whose script
root is not allowlisted, at the epilogue.

Rust SDK exposes all of it: `active_note::{get_storage, get_assets,
get_sender, get_script_root, get_serial_number}`, `output_note::{create,
add_asset}`, `note::build_recipient`, `native_account::{add_asset,
remove_asset, get_id}`, `tx::{get_block_number, get_block_timestamp,
update_expiration_block_delta}`
(`compiler/sdk/base-sys/src/bindings/*`).

**ASSUMPTION (high confidence, test early in Phase 2)**: calling
`active_note::get_sender()` from inside a `#[component]` impl compiles and
links — kernel-legal and the binding is an ordinary function, but none of the
12 compiler examples demonstrates that exact combination.

### (d) Advice provider — what exists and who populates it

- **Interactive tx**: the client attaches arbitrary advice-map entries via
  `TransactionRequestBuilder::extend_advice_map`
  (`miden-client/crates/rust-client/src/transaction/request/builder.rs:227-234`);
  they merge into `TransactionArgs` and reach the executor.
- **Always, automatically**: `TransactionAdviceInputs::new` injects every
  input note's storage, assets, and attachments into the advice map, keyed by
  their commitments (`miden-protocol/src/transaction/kernel/advice_inputs.rs:32-80,329-407`).
  This is how `active_note::get_storage()` works under the hood.
- **Network tx**: the ntx-builder builds inputs from on-chain data and submits
  with `TransactionArgs::default()` (`actor/execute.rs:160-172`) — **no custom
  advice, no note args**. Everything our pool logic needs must live in note
  storage/attachments or pool storage.
- **Reactive advice via events**: `emit`-triggered host handlers populate the
  advice stack at runtime — this is how `u64::div`/`u128::div` work, and those
  handlers are registered by every host including the ntx-builder's executor.
  There is no SDK surface for *custom* events.

### (e) Storage maps — key encoding, value size, limits

- Account: ≤ **255 named slots** (`AccountStorage::MAX_NUM_STORAGE_SLOTS`,
  `protocol/.../account/storage/mod.rs:57`); each slot = `Value(Word)` or
  `Map(StorageMap)`. Slots addressed by hashed name → 2-felt `StorageSlotId`.
- `StorageMap` = **sparse Merkle tree, depth 64**; capacity "theoretically
  unlimited"; only the root Word lives in the slot; only *touched* entries
  need Merkle witnesses (advice data) at execution
  (`.../storage/map/mod.rs:27-60`).
- **Key = one user-chosen Word (4 felts), arbitrary** — `[tick, 0, 0, 0]` is
  literally the documented pattern (`StorageMapKey::from_index`,
  `map/key.rs:48-54`). Keys are Poseidon2-hashed before SMT insertion
  (uniform distribution; `account.masm:1851-1862`), so key structure never
  causes imbalance.
- **Value = exactly one Word.** Writing `EMPTY_WORD` deletes; reads of missing
  keys return zero — the protocol does not distinguish absent from all-zero
  (`map/mod.rs:135-189`). Larger records ⇒ field-striped keys, parallel maps,
  or commitment+advice.
- Cost: map read ≈ 85–210 cycles, write ≈ 95–250 cycles (smt.masm cycle docs +
  19-cycle key hash) — negligible against the cycle budget.
- The real write-side ceiling: **`ACCOUNT_UPDATE_MAX_SIZE` = 256 KiB of
  serialized account delta per tx** (`constants.rs:7`, enforced
  `proven_tx.rs:380-386`) ≈ ~4K map writes/tx. No cap on *total* public
  account storage; full public state lives in the node's store and is
  queryable per slot.
- Rust SDK: `StorageMap<K: WordKey, V: WordValue>` / `StorageValue<T>`; both
  traits are public — custom packed types allowed, but V is one Word, ever
  (`compiler/sdk/base/src/types/storage.rs`).

### (f) Notes per transaction — exact constants

From `protocol/crates/miden-protocol/src/constants.rs` (mirrored in the kernel):

| Constant | Value |
|---|---|
| `MAX_INPUT_NOTES_PER_TX` | 1024 |
| `MAX_OUTPUT_NOTES_PER_TX` | 1024 |
| `MAX_NOTE_STORAGE_ITEMS` (v0.15 name for "note inputs") | 1024 felts (~8 KB) |
| `MAX_ASSETS_PER_NOTE` | 64 |
| `MAX_TX_EXECUTION_CYCLES` | 2^29 |
| ntx-builder per-tx note cap / cycle cap | **20 notes / 2^18 cycles (CLI default)** |
| Node block production defaults | 8 txs/batch, 8 batches/block, 3 s blocks |

The binding limits for us are the ntx-builder's 20-note and 2^18-cycle
defaults, not the protocol maxima.

### (g) MockChain / miden-testing

`protocol/crates/miden-testing` executes the **identical production MASM
transaction kernel** (`TransactionKernel::main()` from the compiled
`tx_kernel.masl`) with dummy proofs (real batch/block flow, no ZK proving —
`chain.rs:55-58`). Everything Phase 2–3 needs is present:

- Custom accounts from `.masp`: `AccountComponent::from_package(&package,
  &init_storage_data)` → `AccountBuilder` → `add_account_from_builder(auth,
  builder, AccountState::Exists)` — exactly what
  `project-template/integration/tests/counter_test.rs` does.
- **`Auth::NetworkAccount { allowed_script_roots, allowed_tx_script_roots }`**
  exists in the test harness (`mock_chain/auth.rs:86-93`) — tests can play the
  ntx-builder's role (execute against the pool with no signer) at the
  transaction level. There is **no ntx-builder pipeline simulation** in
  miden-testing; end-to-end network flow is Phase 4's job.
- Custom notes: `NoteBuilder::new(sender, rng).package(masp).add_assets(...)
  .note_storage(felts)?.build()?`.
- Deadline tests: `prove_until_block(n)`, `build_tx_context_at(ref_block, ...)`.
- Failure tests: `assert_transaction_executor_error!(result, ERR_CODE)`
  matching contract MASM error codes; failed txs never touch state.
- Advice: `TransactionContextBuilder::extend_advice_map(...)`.
- **Cycle measurement**: `executed_tx.measurements()` →
  per-note cycles (`note_execution: Vec<(NoteId, usize)>`), `total_cycles()`,
  `trace_length()` — this is how we enforce the 2^18 ntx budget from Phase 2 on.

---

## Part 2 — Revised architecture

The hypothesis survives in outline; the load-bearing revisions forced by
Phase 0 are marked **[REVISED]**.

### Implementation language strategy (decided 2026-08-26)

**Rust first, MASM port after end-to-end works.** Phases 1–4 are implemented
with the Rust SDK (cargo-miden) for development velocity and type safety; once
the full flow passes on the local node, hot paths (math library, pool
component, note scripts) are ported to hand-written MASM for cycle efficiency.
Rationale, verified in source:

- The pure-MASM path exists and is protocol-native: every standard component
  (P2ID, P2IDE, SWAP, pSWAP, BasicWallet, `AuthNetworkAccount`) is
  hand-written MASM assembled via
  `TransactionKernel::assembler().assemble_library_from_dir(...)`
  (`miden-standards/build.rs:45-76`) and wrapped with
  `AccountComponent::new(library, storage_slots, metadata)`
  (`miden-protocol/src/account/component/mod.rs:60`). MockChain and
  miden-client consume `AccountComponent`s regardless of origin, so the port
  changes no deployment or test infrastructure.
- MASM is materially cheaper in cycles: direct `exec.u128::div`
  (advice-backed) where Rust software-emulates 128-bit division, access to
  `u256` add/sub/mul (unreachable from Rust), no Wasm→MASM lowering overhead,
  and no ≤16-felt cross-context argument constraint for internal procedures.
  Precedent: `pswap.masm:168-186` already does widening-mul + u128-div.
- **MEASURED (Phase 2 gates, 2026-08-26): the port is REQUIRED, not
  optional.** In-VM benchmarks of the compiled Rust math (net of a 3,241-cycle
  note baseline; fixed tx overhead 75,699 incl. a 71,318-cycle Falcon512
  check a network account doesn't pay): `mul_div_floor` 110,214 cycles;
  `get_sqrt_ratio_at_tick` 196,434–319,817; `compute_swap_step` (single
  step, no crossing) **702,146 — 2.7× the entire 2^18 ntx budget**;
  binary-search `get_tick_at_sqrt_ratio` 4,748,056. Not one swap fits a
  default network tx in Rust. The arithmetic is verified *correct* in-VM
  (every bench asserts against the expected value); the cost driver is
  Wasm-lowered limb arithmetic (~110–220k per mulDiv-class op) vs MASM's
  direct advice-backed `u128::div` (~10^2 cycles). Consequences: Phases 2–3
  proceed in Rust for correctness under MockChain's 2^29 budget; Phase 4
  runs the local ntx-builder with a raised cycle cap (CLI-configurable) for
  flow validation; a **mandatory MASM port of the math hot paths** lands
  before any default-budget network deployment, re-validated by the same
  implementation-agnostic property suite and re-benchmarked for the real
  throughput table.
- Behavior must not drift during the port: the host-side property-test suite
  (vs the U256 reference) is implementation-agnostic and reruns unchanged
  against the MASM version via `execute_code` in miden-testing.

### Pool account

One **public account per (token0, token1, fee tier)**, created with:

- the pool `#[component]` (Rust SDK), and
- **[REVISED]** the standard **`AuthNetworkAccount`** auth component whose
  note-script allowlist contains exactly our note script roots: `swap-note`,
  `mint-note`, `burn-note`, `collect-note`. The allowlist is **frozen at
  deployment** — any note-script change means a new pool account. Script
  roots must be final (built and hashed) before the pool is created; this
  makes contract build order a hard dependency: notes first, then pool
  deployment. The tx-script allowlist stays empty; **pool state is initialized
  via `InitStorageData` at account creation** (no init tx needed).

Storage layout (all slots named under the pool component; felt packing uses
32-bit limbs so multiplication never overflows the field):

| Slot | Type | Content |
|---|---|---|
| `pool_config` | Value ×2 | **[ADDED post-review]** immutable, set via `InitStorageData`, never written: `[token0_faucet_suffix, token0_faucet_prefix, token1_faucet_suffix, token1_faucet_prefix]` and `[fee_pips, tick_spacing, 0, 0]`. Every swap/mint procedure validates the active note's asset faucet IDs against these — an asset from a random faucet must never enter `swap()` |
| `sqrt_price` | Value | `sqrtPriceX96: u128` (4×u32 limbs in one Word) — see Part 3 naming note |
| `pool_state` | Value | `[current_tick (offset-encoded u32), initialized_flag, 0, 0]` |
| `liquidity` | Value | active liquidity **u128** (4×u32 limbs, one full Word) — see Part 3 |
| `fee_growth_global` | Value×4 | feeGrowthGlobal0/1, **Q128.128 u256 = two Words each** (see Part 3) |
| `ticks` | Map | key `[tick_u32, field_group, 0, 0]`, field-striped: 0 = liquidityGross (u128), 1 = liquidityNet (i128, two's-complement limbs), 2–3 = feeGrowthOutside0 (lo/hi Words), 4–5 = feeGrowthOutside1 (lo/hi Words) |
| `tick_bitmap` | Map | key `[word_index, 0, 0, 0]`, value = 4×u32 limbs = 128 tick-spacing positions per entry |
| `positions` | Map | **[REVISED round 2]** key = `[h0, h1, h2, field_id]` where `(h0,h1,h2)` = Poseidon2 hash of `(owner, tickLower, tickUpper)` truncated to 3 felts (~16–19 cycles, negligible next to the ~100–250-cycle SMT access itself). Field ids: 0 = liquidity (u128), 1–2 = feeGrowthInside0Last (u256), 3–4 = feeGrowthInside1Last (u256), 5 = `[tokensOwed0, tokensOwed1]`. The round-1 "raw un-hashed key, parallel one-Word `position_fees` map" idea is **retracted**: a position record is ≥ 6 Words once fee snapshots are Q128.128, so a single parallel map can't hold it and the raw key leaves no room for a field discriminator |

Tick encoding: the supported tick range **±443,636** (Part 3) is
offset-encoded as `u32` (`tick + 2^19`), so all-zero remains an
"uninitialized" sentinel and felt comparisons are natural-number comparisons.
Mint and initialization enforce the range.

### Note flows

All four notes are **public** (required for network consumption), carry the
**`NetworkAccountTarget` attachment** targeting the pool, and follow the
P2IDE two-path shape verified in source (`p2ide.masm:106-145`): the script
branches on the executing account ID.

**Swap note** — assets: input token. Note storage:
`[pool_id_suffix, pool_id_prefix, direction, min_amount_out_lo, min_amount_out_hi,
recipient_suffix, recipient_prefix, deadline_height]`.
- Path A (executing account == pool): `call` pool `swap` procedure. The pool
  procedure reads *everything from kernel state* — assets, params from
  `active_note::get_storage()`, sender from `active_note::get_sender()` —
  ignoring script-passed arguments entirely. **Check order matters** (review
  round 2): validate asset faucet IDs against `pool_config` first, then the
  deadline, and only then run the tick-crossing swap loop — the expired path
  must consume-and-refund *without* executing any TickMath or state
  mutation (cheap refunds also blunt the cycle cost of spam). **Failure
  semantics [REVISED post-review, hybrid]**:
  - *Before the deadline*, a slippage violation (`amount_out <
    min_amount_out`) **panics** — the tx fails, the note stays unconsumed,
    and the ntx-builder retries with backoff, giving limit-order-like
    fill-if-price-recovers behavior.
  - *At or after the deadline* (`block_number >= deadline_height`), the pool
    does **not** panic: it consumes the note and emits a **refund P2ID note**
    returning the input asset to the sender. Expired notes therefore resolve
    themselves on the next network attempt instead of burning retries.
  - On success: emits a **P2ID note** to the recipient with the output
    tokens.
- Path B (executing account == `active_note::get_sender()` and
  `block_number >= deadline_height`): sender reclaim — assets return to the
  sender's wallet via `receive_asset`. This remains as the **failsafe** for
  the case where the ntx-builder discarded the note (30 failed attempts)
  before its deadline passed, since users cannot execute against the pool
  account themselves.
  **Rust-stage caveat (Phase 3, verified)**: the Rust-SDK note script's
  cross-context `call` targets the MAST root of the *Rust-SDK* basic-wallet
  package's `receive_asset` — which differs from the standard MASM
  `BasicWallet`'s root. Reclaim therefore only works for sender accounts
  carrying the Rust-SDK wallet component (`contracts/basic-wallet`); users
  on standard wallets cannot reclaim until the MASM port, where the note
  script can target the standard wallet procedure directly.

**Mint (add liquidity) note** — assets: token0 + token1 (max amounts). Note
storage: `[pool_id, tickLower, tickUpper, liquidity_desired_lo/hi,
deadline_height]`. Pool computes amounts owed at current price, records/extends
the position keyed by `active_note::get_sender()`, emits a **P2ID refund note**
for the excess. Reclaim path after deadline, same as swap.

**Burn note / collect note** — no assets. Note storage: `[pool_id, tickLower,
tickUpper, liquidity_lo/hi]` (burn) or `[pool_id, tickLower, tickUpper]`
(collect). Authorization: the pool procedure reads
`active_note::get_sender()` and requires it to equal the position owner —
kernel-committed, unforgeable, no signature scheme needed. Burn moves owed
amounts into `tokensOwed`; collect emits a P2ID note with the owed tokens to
the sender. Reclaim path is trivial (no assets) — the sender can consume the
note as a no-op to clean up.

### Implementation reality (Phase 2, measured 2026-08-26): the args adaptation

DESIGN originally required pool procedures to take **no arguments** and read
note storage/assets via `active_note::*` from component context. Empirically
(probed in MockChain, compiler v0.9 / SDK 0.13): the **value-returning**
reads (`get_sender()`, `get_serial_number()`) work from account-component
context — verified by sender-probe — but the **memory-writing** reads
(`get_storage()`, `get_assets()`) return 0 items there, while working
normally in note context. This is an SDK/compiler-level limitation, not a
kernel one (Phase 0 verified the kernel procs carry no origin guard); it
should disappear in the MASM port. Adaptation in `contracts/clamm-pool`:

- The **allowlisted note script** reads its own committed note storage and
  assets in note context and forwards them as ≤16 flat felts of arguments.
- **Trust chain**: the `AuthNetworkAccount` script-root allowlist ensures
  only our scripts drive the pool; those scripts forward their own
  note-committed data faithfully; therefore args inherit the trust of the
  note commitment. Defense in depth on top:
  - **Assets are never trusted from args**: the component reconstructs the
    expected asset via the kernel (`asset::create_fungible_asset` from the
    immutable `pool_config`) and asserts the forwarded key felts match;
    amount lies are fatal because the kernel epilogue's asset-conservation
    check fails any tx whose claimed assets don't match the note's actual
    assets.
  - **Authorization stays kernel-read**: position keys, refund targets, and
    P2ID serials derive from `get_sender()`/`get_serial_number()` inside the
    component, never from arguments.

One storage addition: an immutable `p2id_root` config slot (the P2ID note
script root, seeded at creation) — guest code cannot link miden-standards,
and hardcoded roots are a known pitfall.

### Consequences accepted from the ntx model (documented divergences, Part 4)

- Up to 20 notes execute sequentially *inside one network tx* against
  progressively updated pool state; order within the batch is unspecified.
  Per-note slippage bounds are the only user protection. This is a divergence,
  not a bug.
- The pool pays network-tx fees from its own vault. v1: fund the pool with
  fee asset at deployment; fee economics are an open question (Part 5).
- The whole batch shares the ntx cycle cap (2^18 default), and there is only
  one in-flight network tx per account — so **per-swap cycles translate
  directly into pool throughput**, not just feasibility. A 100k-cycle swap
  means ~2 swaps per network tx; a 10k-cycle swap means ~20. Phase 2's
  deliverable is therefore a **throughput table** (swaps per network tx at
  measured typical/worst-case cycle counts), not merely "one swap fits".
  Tick crossings per swap are bounded by a constant sized from those
  measurements (fail, don't partial-fill, past the bound).

---

## Part 3 — Numbers: representation and math strategy

Constraints from source: field elements < 2^64 with UB-poison u32 semantics;
efficient u64/u128 (advice-backed division works in *every* context incl.
network txs); u256 add/sub/mul only; **no custom advice hints in network
txs**; no runtime sqrt anywhere.

Decisions:

1. **No runtime integer sqrt is needed — but the reverse tick mapping IS.**
   Uniswap v3 never computes a square root at swap time:
   `TickMath.getSqrtRatioAtTick` is multiplicative bit-decomposition over
   precomputed per-bit constants. The task brief's sqrt-via-advice example is
   therefore not required in the v1 hot path (and *couldn't* be used in a
   network tx anyway — no custom advice channel). However, when a swap stops
   *inside* a tick range, the pool must recompute the current tick from the
   final sqrt price (`getTickAtSqrtRatio`), and no hint can supply it — the
   note cannot predict where earlier notes in the same batch leave the price.
   So Phase 1 ships a **deterministic in-circuit reverse mapping**: binary
   search over `get_sqrt_ratio_at_tick` (v1, trivially consistent with the
   forward function). **Measured (Phase 2 gate): 4,748,056 cycles (~21
   forward evaluations) — 475× the switch threshold.** But the threshold
   logic is moot in the Rust stage: even a single *forward* evaluation
   (≈ 200–320k) exceeds the whole 2^18 ntx budget, so no Rust reverse
   mapping can rescue network-tx viability (a Rust log2 port would still be
   ~10 mulDiv-class ops ≈ megacycle territory). Decision: binary search
   stays for Rust-stage correctness (MockChain's 2^29 budget absorbs it);
   the log2 algorithm is implemented **in the MASM port**, where its ~14
   square-and-shift steps cost ~10^2 cycles each.
2. **`sqrtPriceX96: u128`, tick range ±443,636** (vs Uniswap's uint160 over
   ±887,272). Naming per review round 2: "Q64.96 in u128" was imprecise —
   with the restricted range the value is effectively Q32.96 (≤ 32 integer
   bits used) while retaining Uniswap's ×2^96 scaling, so we name it by the
   scaling, as Uniswap does. History: an earlier draft proposed Q64.64 over
   the full range; the arithmetic kills it — at tick −887,272 the sqrt price
   is ≈ 2^-64, which Q64.64 represents with ~zero fractional bits. Halving
   the *tick range* instead of the *fractional precision* keeps Uniswap's
   scaling: sqrtPriceX96 ∈ (2^64, 2^128) fits u128 exactly, TickMath's
   per-bit constants (all < 2^128) port verbatim, and price ratios from
   10^-19 to 10^19 remain expressible. **Clarification (review round 2,
   accepted): only the *final* sqrtPriceX96 fits u128 — TickMath's *working
   state* is Q128.128 with a 2^256-scale inversion for positive ticks and
   needs 256-bit limb intermediates** (our implementation already does
   exactly this; the amm-math crate's `get_sqrt_ratio_at_tick` runs on limb
   arrays internally). Phase 1 property tests against an exact
   high-precision reference gate acceptance; measured max relative error is
   ~2^-96 algorithmic, ≤ 1 ulp raw.
3. **liquidity: u128** (same as Uniswap), stored as 4×u32 limbs in one Word.
   An earlier draft proposed u64 on the grounds that u64 token reserves
   bound liquidity — that reasoning was wrong and is retracted: concentrated
   liquidity is amplified by range width, L ≈ amount·√P·√P_next/(√P_next−√P),
   which for a one-tick-wide position near price 1 is ≈ 20,000× the token
   amount. A single-tick position with only ~9.2×10^14 of token0 (far below
   the u64 asset cap) already produces L ≈ 2^64. u128 liquidity keeps
   narrow-range positions unrestricted; wide-math sizing is unaffected
   ((L≪96)·Δ ≤ 352 bits, within the 6-limb routines).
4. **feeGrowth accumulators: Q128.128 in u256 (two Words), same as Uniswap.**
   Decided now, not deferred (review round 2, accepted). The earlier Q64.64
   u128 proposal was silently invalidated by the switch to u128 liquidity:
   with L ≤ 2^64 (the old assumption) the increment `fee·2^64/L ≥ fee ≥ 1`
   could never truncate to zero, but with L up to 2^128 any pool where
   L > fee·2^64 records **zero** fee growth for the whole swap (e.g.
   L = 2^100, fee = 2^20 ⇒ increment = 2^-16 → 0 — LPs earn nothing).
   Q128.128 restores `increment = fee·2^128/L ≥ fee ≥ 1` for all L ≤ 2^128.
   No u256 division is needed: the operations are u256 add/sub with wrapping
   (differences mod 2^256, Uniswap's trick), `fee << 128 / L` for the
   increment (u64 shifted 128 = ≤192-bit dividend / u128 — inside the
   existing limb routines), and `L × Δ >> 128` for fees owed (≤384-bit
   product — likewise). Single-increment bound: ≤ 2^64·2^128 = 2^192 ≪
   2^256 even at L = 1. Wrap-ambiguity (cumulative growth between touches of
   a position < 2^256) matches Uniswap's own exposure exactly. Storage cost:
   two Words per accumulator.
5. **mulDiv (a·b/d with wide intermediates)**: exact schoolbook
   multiplication over u64 limbs in plain Rust (native u128 products), then
   **long division built on the advice-backed 64/128-bit divisions** (Knuth
   Algorithm D; each quotient-digit step uses `u64`/`u128` division, which
   lowers to the core-lib advice-backed procs whose event handlers every
   host registers). This keeps the hint-and-verify benefit — the expensive
   part of division is host-computed — while remaining fully functional in
   network transactions. The limb routines generalize past 256 bits because
   `liquidity << 96` intermediates in the amount-delta formulas reach ~384
   bits (as in Uniswap, whose mulDiv is 512-bit). Where operands provably
   fit 128 bits we use straight Rust `u128` ops.
6. **Discipline**: all packed limbs are ≤ 32 bits when they participate in
   felt multiplication; advice-loaded values are range-asserted before use
   (mirroring the stdlib's `u32assertw`-after-`adv_pushw` pattern);
   comparisons on quantities always via canonical u64/u128 forms, never raw
   felt comparison (felt ordering is field ordering — project pitfall rule).
7. The math crate is `no_std` pure Rust, compiled both natively (host-side
   property tests vs `primitive-types::U256` Uniswap-reference port) and via
   cargo-miden (guest). No emulated-uint256 library.

---

## Part 4 — Divergences from Uniswap v3 (by design, not TODOs)

| # | Uniswap v3 | This design | Forced by |
|---|---|---|---|
| 1 | Synchronous `swap()` call, atomic revert returns funds in-tx | Asynchronous note; on failure the note sits unconsumed, retried ≤ 30× with backoff, then permanently discarded by the network; funds recovered via sender reclaim path after deadline | ntx-builder retry/discard semantics (`notes.rs:208-228`, `lib.rs:117`) |
| 2 | Caller-controlled ordering within a block (gas priority); mempool privacy varies | **Explicit orderer/MEV model**: swap notes publicly reveal full intent (asset, amount, direction, minAmountOut, recipient, deadline) *before* execution, and the ntx-builder operator chooses batching and order (unspecified, non-FIFO, no priority mechanism). Per-note slippage bounds are the only user guarantee. This belongs in the protocol threat model, not a footnote | ntx-builder note selection (no `ORDER BY`; checker elimination); public network notes |
| 3 | `msg.sender` authorization for positions | Note **sender account ID** read from kernel-committed metadata by pool code | actor model; verified sender-read capability (api.masm:931) |
| 4 | Position owner can be a contract (NFT manager) | Position owner = the account that created the note. Notes created *by a contract* on a user's behalf record the contract as sender — composability caveat documented | `get_sender` = creating account (lib/output_note.masm:456) |
| 5 | Q64.96 sqrtPrice in uint160, ticks ±887,272, u128 liquidity, Q128.128 u256 fee growth, 512-bit mulDiv | `sqrtPriceX96: u128`, **ticks ±443,636** (half range; covers 10^-19–10^19 price ratios), u128 liquidity (unchanged), **Q128.128 u256 fee growth (unchanged)**, limb-based mulDiv (≤384-bit intermediates) + advice-backed division | Goldilocks field, core-lib math surface, ntx advice constraints |
| 6 | Gas paid by swapper | Network-tx fee paid by the **pool's vault**; v1 funds the pool explicitly; sustainable fee economics deferred | kernel epilogue fee debit from executing account |
| 7 | Pool contract upgradeable only by redeploy; router evolves freely | Note-script *roots* are frozen into the pool's allowlist at creation — new note version ⇒ new pool. **v0.15-only**: dissolves at 0.16, where a standardized config note makes allowlists updatable post-deployment (see 0.16 migration watch) | `AuthNetworkAccount` immutability at v0.15 (`auth_network_account.rs:57-61`) |
| 8 | Unbounded tick crossings per swap (gas-limited) | Explicit max-ticks-per-swap bound sized to the 2^18 ntx cycle cap (measured in Phase 2) | ntx-builder cycle cap (CLI default) |
| 9 | Reentrancy guard (lock) | Not needed: one tx at a time mutates the account; notes execute sequentially inside a tx; no callbacks exist | Miden execution model |
| 10 | Pool state readable synchronously by contracts | Other accounts read pool state via FPI (read-only) or the node's public state RPC | FPI is read-only; storage reads are account-code-gated |
| 11 | **Atomic multi-hop routing**: one EVM tx swaps through N pools with all-or-nothing semantics | **No atomic multi-hop.** Each pool is its own account; a tx executing pool A cannot mutate pool B. Chained hops are possible (pool A emits a network note targeting pool B) but each leg is asynchronous with independent price/ordering risk. The alternative — one global DEX account holding all pools — would restore internal atomicity at the cost of a single serialized execution bottleneck. One-account-per-pool is the chosen trade | one native account per tx; ntx serialization per account |

---

## Part 5 — Open questions and labeled assumptions

1. **RETIRED — VERIFIED 2026-08-26**: `active_note::get_sender()` from inside
   a Rust `#[component]` impl compiles, links, and returns the note
   creator's AccountId through the full pipeline
   (`contracts/sender-probe` + `integration/tests/sender_probe_test.rs`,
   green in MockChain: committed storage holds exactly the sender's ID).
   The pool's authorization model stands on tested ground.
1b. **Compiler v0.9.0 findings (workarounds known; all worth upstreaming to
   0xMiden/compiler)**, accumulated across Phase 2:
   - 3+-arm `match`/`if-else` dispatch chains with cross-context account
     calls fail at runtime ("entered unreachable code", code
     13397901377689146813) before any arm executes; flat sequential `if`
     blocks work (see `contracts/bench-note`).
   - `active_note::get_storage()`/`get_assets()` return 0 items from
     account-component context (value-returning reads work) — see the
     Part 2 "args adaptation".
   - Dominance-frontier panic (`midenc-hir frontier.rs:123`) when nested
     loops inline into a loop-bearing caller — fixed with
     `#[inline(never)]` + hoisting the reverse tick mapping after the swap
     loop.
   - Wide-limb math after `hash_elements` in the same call frame
     miscomputes ("operation expected u32 values") — order wide math before
     hashing, pin helpers `#[inline(never)]`.
   - Spill-analysis panic on wide-argument internal calls — narrow args.
   - (Phase 3) Duplicated `active_note::get_assets()` call sites — one per
     branch arm — miscompile the whole note script; hoist to a single call
     above the branch.
   - (Phase 3) The derived `AccountId` `PartialEq` miscompiles when used as
     a branch condition (`if executing == pool_id`); compare canonical
     felts instead (the same eq inside `assert!` is fine).
2. **CONFIRMED LIVE (Phase 4)**: the v0.15.2 ntx-builder supplies default
   `TransactionArgs` (no note args, no custom advice) and does not attach
   the expiration tx script — observed against the real builder in the
   local stack. Testnet operator config remains a separate unknown.
3. **UNVERIFIED**: whether the public **testnet** actually runs an ntx-builder,
   and with what note/cycle/attempt limits (deployment config, not in source).
   Phase 4 tests locally with our own stack first; testnet behavior gets
   measured, not assumed.
4. **RESOLVED (Phase 4, verified)**: default local genesis sets
   `verification_base_fee = 0`
   (`miden-node/crates/store/src/genesis/config/mod.rs:111`) — network txs
   cost nothing locally; the pool vault needs no fee asset. Confirmed
   end-to-end: zero fee debits and exact vault conservation across the
   full Phase 4 run.
5. **UNVERIFIED**: exact cycle cost of Rust-emulated `u128 / u128` division
   and of our Knuth-D mulDiv — measured in Phase 1/2 via
   `measurements().total_cycles()`; representation choices in Part 3 are
   revisited if the worst-case swap cannot fit ~100k cycles.
6. **Open question (product, not protocol)**: pool fee-asset economics — who
   tops up the pool's fee balance long-term. v1 explicitly funds at deploy and
   reports consumption in Phase 4's measurements.
7. **Resolved (design review, 2026-08-26): hybrid failure semantics.**
   Before the deadline: panic on slippage (note stays live, retried —
   limit-order-like). At/after the deadline: consume and refund via P2ID.
   Sender reclaim retained as failsafe for builder-discarded notes. See the
   swap-note flow in Part 2. Switching to always-consume-and-refund remains
   a one-note-script change if production experience favors predictability.
8. **Threat model — retry-spam DoS (from review, accepted)**: anyone can mint
   allowlisted swap notes with impossible `min_amount_out`; each burns up to
   30 ntx-builder executions (bounded by backoff + our deadline-refund path,
   which terminates the retry stream at the deadline). Related: every
   *successful* network tx (including refunds) debits a fee from the pool
   vault at v0.15, so refund-spam drains pool fee balance. Phase 3 tests
   this adversarially; economic mitigation is tied to open question 6 (and
   may evaporate at 0.16 — see the migration watch below).

## 0.16 migration watch (verified against main-branch clones + release tags, 2026-08-26)

0.16 is further along than "dev": node `v0.16.0-rc.1` was tagged 2026-08-14
(verified via `ls-remote`). Verified changes on protocol `main` (`7af87630`)
and node `main` (`083929dc`) that alter this design at migration time:

1. **The kernel-epilogue transaction fee is gone, replaced by a
   network-account script-fee mechanism.** `proven_tx.rs` and the epilogue
   contain no fee logic on protocol `main` (22 references at v0.15.3 → 0).
   Instead, `AuthNetworkAccount` gains **fee-policy storage slots** and
   procedures (`estimate_note_fee`, `set_fee_policy`,
   `add_allowed_fee_policy`, …): a network account prices the note scripts
   it services (including at zero), and **senders prepay via
   `FeeSponsorshipNote`s** — "fee collection asserts every consumed note's
   fee is covered by the sponsorships bound to it"
   (`auth_network_account.rs:200-243` on protocol `main`). Do not assume
   0.16 network execution is free: re-evaluate fee economics against the
   RC API. Upside: this is precisely the mechanism our threat item 8
   (retry-spam / servicing costs) and open question 6 (who funds servicing)
   were missing — spam notes must carry sponsorship, and the pool can
   charge per serviced note.
2. **The ntx-builder attaches a canonical `ExpirationTransactionScript`**,
   and serviced accounts must allowlist its root
   (`bin/ntx-builder/src/actor/execute.rs:486` on node `main`). On 0.16 the
   standard `AuthNetworkAccount::new` allowlists it **by default**, so this
   is only a break for accounts built with the raw `custom` constructor.
3. **Allowlists stop being frozen**: `AuthNetworkAccount::new` on 0.16
   auto-allowlists a standardized `NetworkAccountConfigNote` "so the
   account's allowlists can be updated after deployment by sending that
   note" (`auth_network_account.rs:216-217`). Divergence #7 ("new note
   version ⇒ new pool") holds at v0.15 but dissolves at 0.16 — pools become
   upgradable-by-config-note, which is both an ops win and a new
   governance/threat surface (who may send the config note).

## Phase plan adjustments (from Phase 0 findings)

- **Phase 1** additionally delivers: cycle-count microbenchmarks of mulDiv /
  swap-step math (via `execute_code` + measurements), not just error bounds.
- **Phase 2** gates on: the throughput table — swaps per 2^18-cycle network
  tx measured across four cases (no initialized tick crossed / 1 crossed /
  5 crossed / max permitted), since the go–no-go question is whether a
  *realistic* swap lands nearer 10–30k cycles than 100k; the
  reverse-tick-mapping benchmark with its low switch-to-log2 threshold
  (Part 3 item 1); `get_sender`-from-component test first.
  **MEASURED (Phase 2 core, MockChain per-note cycles, Rust build)**:
  SWAP_NO_CROSS 3,216,611 · SWAP_1_CROSS 3,971,269 · SWAP_5_CROSS
  7,018,622 — i.e. **0 swaps per 2^18 network tx in Rust** (12–27× over;
  ~4.7M of the no-cross figure is the binary-search reverse mapping). The
  Rust-stage throughput table is therefore all-zero by construction; the
  *real* throughput table is a MASM-port deliverable. All 18 MockChain
  tests green (lifecycle incl. tick crossings, fee accounting/vault
  conservation, and all failure paths).
- **Phase 3** note scripts are built and hashed **before** the pool account is
  created (allowlist freeze); tests cover: slippage failure, deadline expiry,
  wrong-pool note, reclaim by sender, reclaim-too-early failure, non-owner
  burn/collect rejection.
- **Phase 4 — COMPLETE (2026-08-26), FULL PASS in real-ntx-builder mode.**
  4-service stack scripted (`local-net/`); every operation (mint ×2, swaps
  crossing the initialized tick in both directions, burn, collect,
  adversarial refund) consumed by the real ntx-builder and proven by the
  real remote prover, every state assertion exact vs the host simulator.
  Measured: network-tx cycles 52k (refund) – 1.64M (2-crossing swap);
  note→state-updated latency 3–30s; remote proving ≈ 9.6s at 2^20 / 26s at
  2^21 traces; user-side local proving 1.1s; observed retry backoff at
  blocks +2/+3/+3 matching `round(e^(0.25·n))`, then deadline
  consume-and-refund; fees 0 (genesis default). **Three new hard facts:**
  1. **The Rust-stage pool cannot be deployed by transaction**: its ~600KB
     account code exceeds `ACCOUNT_UPDATE_MAX_SIZE` (256KiB) — locally the
     pool is seeded at genesis (`MIDEN_GENESIS_CONFIG_FILE`, the compose
     mechanism); testnet deployment requires the MASM port (small code) or
     operator-side genesis help.
  2. **Stock v0.15 ntx-builder cannot prove any network tx slower than
     10s** — hard-coded remote-prover client timeout, no CLI override
     (`bin/ntx-builder/src/lib.rs:462`); patched locally
     (`local-net/ntx-builder-timeout.patch`). Upstream-worthy.
  3. **Proving memory is a second budget**: a 2^22-step trace (in-range
     swap incl. binary-search reverse mapping, 3.98M cycles) exhausts 24GB
     RAM and never proves; 2^20–2^21 prove in 8–26s. The reverse mapping
     (~3.07M cycles in situ) is the cliff — the MASM log2 port eliminates
     it. Phase 4 swaps were sized to land exactly on tick boundaries
     (still exercising crossings) to stay provable.
