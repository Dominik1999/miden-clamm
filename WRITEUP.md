# Uniswap v3 on Miden — Phase 5 write-up

2026-08-26. All four build phases complete and independently verified:
math crate (60 tests, bit-exact vs exact references), pool component +
production notes (27 MockChain tests, state bit-exact vs a host simulator),
and a full end-to-end run on a real local Miden network with real
ntx-builder consumption and real STARK proving. This document is the
retrospective the project brief asked for: what mapped cleanly, what did
not, the ordering/MEV analysis, and open questions. Source of record for
the design itself: `DESIGN.md`; pins in `TOOLCHAIN.md`.

## 1. What mapped cleanly

- **The core mathematics, essentially verbatim.** Keeping Uniswap's Q64.96
  sqrt-price scaling in a u128 (ticks ±443,636 — half of Uniswap's range,
  still 10^-19–10^19 price ratios) let TickMath, SqrtPriceMath, and
  SwapMath port with their exact algorithms and rounding directions.
  Property tests against exact U512/U1024 references show ~2^-96
  algorithmic error, and every on-chain state transition in Phases 2–4
  matched the host-side reference implementation to the bit.
- **Pool = one public account per (token0, token1, fee tier).** Storage
  maps (depth-64 SMTs, one-Word values, arbitrary user keys) carried
  ticks, the tick bitmap, and positions naturally; only touched entries
  need witnesses, so map size never bloats proofs.
- **Authorization without signatures.** The kernel commits the note
  sender's account ID; pool code reads it mid-consumption
  (`active_note::get_sender()`) and derives position keys from it. No
  arguments are trusted for identity; a non-owner burn simply addresses an
  empty position and the transaction fails. This is *stronger* than
  `msg.sender` in one way: it cannot be delegated accidentally.
- **The network-account machinery.** `AuthNetworkAccount` (allowlisted
  note-script roots, no signer) + public notes with the
  `NetworkAccountTarget` attachment gave exactly the "pool executes while
  nobody is online" semantics. Observed live: discovery, batching,
  execution, remote proving, and block inclusion, 3–30s note-to-state.
- **Hybrid failure semantics.** Panic-before-deadline (note stays live,
  retried with backoff — limit-order-like; observed retrying at
  `round(e^(0.25·n))` block spacing exactly as the source predicted) and
  consume-and-refund at the deadline. The refund path runs no swap math.
- **Testing rigor transferred.** MockChain executes the production kernel,
  so Phase 2/3 assertions (including failure paths and cycle counts) were
  meaningful predictions of Phase 4 behavior — nothing that passed
  MockChain behaved differently on the real network.

## 2. What did not map (and became design, not TODOs)

- **Synchronous atomicity.** A failed swap is not an EVM revert-with-refund:
  the note sits on chain, gets retried, and after ~30 attempts is
  permanently ignored by the network — with user RPC hard-blocked from
  executing against a network account, sender reclaim is the only exit.
  The design treats the note lifecycle as a small state machine
  (pending → filled | refunded | reclaimed) rather than pretending at
  atomicity.
- **Atomic multi-hop routing.** One native account per transaction means no
  router that atomically touches two pools. Chained hops via pool-emitted
  network notes are possible but asynchronous, with per-leg price risk.
- **The fee payer inverts (v0.15).** The kernel epilogue debits the
  *executing* account, i.e. the pool pays to service its users. Local
  genesis defaults fees to zero, so Phase 4 ran free; on 0.16 this becomes
  a per-note-script fee the account charges, prepaid by senders via
  sponsorship notes — the mechanism our economics were missing.
- **Upgradeability inverts too (v0.15).** Note-script roots freeze into the
  pool's allowlist at creation: new note version ⇒ new pool. 0.16's
  standardized config note dissolves this (and creates a governance
  surface in its place).
- **Number formats narrowed deliberately**: u128 sqrtPriceX96 (half tick
  range), u128 liquidity (unchanged from Uniswap after review), Q128.128
  u256 fee growth (unchanged — the Q64.64 attempt died by review: with
  u128 liquidity, increments truncate to zero), limb-based mulDiv with
  advice-backed division instead of native uint256.

## 3. Ordering and MEV: the ntx-builder is an orderer

The brief asked specifically about the implications of the ntx-builder
choosing note order. From source (verified) and observation (Phase 4):

- **No FIFO, no priority, no guarantee.** Note selection has no `ORDER BY`
  (SQLite row order over a nullifier-keyed table), standard-library notes
  sort first, then a checker iteratively drops failing notes. Within a
  batch of up to 20, a later note can execute before an earlier one, and
  each note sees the price impact of those executed before it.
- **Full intent is public before execution.** A swap note reveals asset,
  amount, direction, min-out, recipient, and deadline the moment its block
  commits. The ntx-builder operator (and anyone watching) sees the order
  flow before it lands.
- **The slippage bound is the only user protection**, and it is real: the
  pool enforces it in-circuit, so no ordering can extract more than the
  user's stated tolerance. But everything inside that tolerance is the
  orderer's to take (sandwich-shaped reordering within a batch is
  undetectable and unattributable).
- **Grinding**: since selection order plausibly follows nullifier order
  (unverified — no `ORDER BY` is the only verified fact), an adversary
  could grind note serials to bias intra-batch position. Cheap to attempt,
  bounded by others' slippage tolerances.
- **Throughput coupling**: one in-flight network tx per account serializes
  the pool. Whoever influences which ≤20 notes enter a batch influences
  both ordering and latency for everyone else.
- **Mitigations available today**: tight per-note slippage, short
  deadlines, splitting large swaps. **Not available**: priority fees,
  private order flow, batch auctions — all future work at the protocol or
  operator level. Honest framing: this v1 is a *trusted-orderer* DEX with
  slippage-bounded damage, which should be stated to users rather than
  implied away.

## 4. Measured results (Rust stage, this machine: 12-core M-series, 24GB)

| Metric | Value |
|---|---|
| Math max relative error (algorithmic / raw) | ~2^-96.2 / ≤1 ulp |
| Network-tx cycles: refund / collect / burn / mint / 1-cross swap / 2-cross swap | 52k / 66k / 716k / 947k–1.04M / 912k / 1.64M |
| In-range swap incl. binary-search reverse tick mapping | 3.98M (2^22 trace — unprovable on 24GB) |
| Remote proving | ~9.6s @ 2^20, ~26s @ 2^21 |
| User-side local proving (note publish) | 1.1s |
| Note committed → pool state updated | 3–30s |
| Adversarial note: publish → deadline refund | 39.1s (4 retries observed, e^(0.25n) backoff) |
| Fees debited | 0 (local genesis default) |

## 5. The MASM port is the v1 finish line (three independent proofs)

1. **Cycle budget**: a Rust swap step alone (702k cycles) is 2.7× the
   default 2^18 network-tx budget; the cost is Wasm-lowered limb
   arithmetic vs MASM's ~10^2-cycle advice-backed `u128::div`.
2. **Deployability**: the Rust pool's ~600KB account code exceeds the
   256KiB per-tx account-update cap — it cannot be deployed to any network
   it isn't genesis-seeded into. MASM code is orders of magnitude smaller.
3. **Proving memory**: 2^22-step traces (any in-range Rust swap) exhaust
   24GB during proving. The reverse tick mapping is ~77% of a crossing
   swap; the MASM log2 algorithm reduces it from ~3.1M cycles to ~10^3.

The port is de-risked by construction: the property suite and MockChain
tests are implementation-agnostic (they assert state, not code), the
pure-MASM authoring path is protocol-native (`assemble_library_from_dir` +
`AccountComponent::new`), and porting also retires the SDK's Rust-stage
limitations (component-context note reads, the wallet MAST-root coupling
that currently restricts reclaim to Rust-SDK wallets).

## 6. Open questions

1. Testnet operator configuration (does it run an ntx-builder; note/cycle/
   attempt limits; prover timeout) — measured, not assumed, when we get
   there.
2. 0.16 migration: script-fee economics (who prices swap servicing, what
   sponsorship UX looks like), config-note governance (who may update a
   pool's allowlists), and re-verification of the fee/expiration behavior
   on the RC line.
3. Position composability: `get_sender` records the *creating account* —
   a contract minting on a user's behalf owns the position. Fine for v1
   (positions keyed to wallets), a real constraint for protocols building
   on top.
4. `maxLiquidityPerTick` (Uniswap's per-tick cap) is not implemented;
   derive whether u128 arithmetic makes it unnecessary or add it in the
   MASM port.
5. Orderer trust (Section 3): whether Miden grows protocol-level ordering
   fairness or private order flow is outside this project's control but
   determines how far past "trusted-orderer DEX" v1 can go.

## 7. Upstream findings worth filing (0xMiden)

- compiler v0.9 (7 miscompilation/ICE bugs, all with minimal reproductions
  documented in DESIGN.md Part 5 1b): 3+-arm dispatch chains;
  component-context `get_storage`/`get_assets` returning empty; nested-loop
  inlining dominance panic; wide-math-after-Poseidon2 miscompute;
  spill-analysis panic; duplicated `get_assets` call sites; derived
  `AccountId` eq as branch condition.
- node v0.15: ntx-builder's hard-coded 10s remote-prover timeout (no CLI
  override) makes any >10s proof impossible through the stock binary
  (one-line patch in `local-net/ntx-builder-timeout.patch`).
- template docs: `miden-node bundled start` does not exist at v0.15; the
  parent CLAUDE.md quick reference needs the 4-service topology.

## Postscript (2026-08-27): the MASM port landed

All three stages complete, every gate green. The math library is bit-equal
to the Rust oracle (22 test binaries incl. an exhaustive 2.66M-probe
bracket verification of the log2 reverse mapping); the pool component and
four note scripts run every MockChain scenario on both backends (54/54)
with the original no-args kernel-read trust model restored and reclaim
working on standard wallets; and the Phase 4 end-to-end scenario re-ran on
the real network stack with the two proofs the Rust build could not give:

- **Deployed by transaction**: 160,503-byte component, on-chain in 2.1s
  (vs the ~600KB genesis-only Rust build).
- **Stock 2^18 network budget**: max observed network tx 192,374 cycles —
  including a real two-swap batch consumed in ONE network transaction.
  In-range swaps (previously unprovable on 24GB) prove in 0.31–2.62s;
  note→state latency dropped to 3–6.3s. An over-budget 5-cross swap was
  correctly rejected on the cycle limit six times and then gracefully
  consumed by the deadline-refund path — the designed degradation.

Cycle deltas, per operation (Rust → MASM): swap in-range 3.22M → 85k
(38×), 1-cross 3.97M → 144k *including* the in-range ending (28×), mint
947k → 86k, burn 716k → 94k, collect 66k → 32k, refund 52k → 26k; the
reverse tick mapping alone went from 4.75M (unprovable in situ) to 24–46k
(103–199×). Deployment guidance stands: bound tick crossings ≤ ~4 per swap
at the default budget, or split larger swaps.

The v1 described in this write-up is now the MASM build. The Rust build
remains in-tree as the bit-equal oracle and the development reference.
