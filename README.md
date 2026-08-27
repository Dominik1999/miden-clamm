# miden-clamm

A Uniswap-v3-style concentrated-liquidity AMM built natively on
[Miden](https://miden.xyz)'s asynchronous actor model: one public network
account per pool, user intents as notes, tick-crossing swap math proven in
zero knowledge, and P2ID payouts back to users — with a hand-written MASM
implementation that runs a full swap in a stock 2^18-cycle network
transaction and deploys by ordinary transaction.

Not a Solidity port: where Miden's model forces divergence from Uniswap v3
(asynchronous execution, orderer-based MEV surface, no atomic multi-hop,
fee-payer inversion), the divergence is designed and documented, not
papered over. Read `DESIGN.md` (architecture and verified ground truth,
every claim source-cited against pinned Miden v0.15 releases) and
`WRITEUP.md` (retrospective: what mapped, what didn't, measurements, and
the MEV analysis). `TOOLCHAIN.md` pins every source commit.

## Layout

| Path | What |
|---|---|
| `contracts/amm-math` | Rust no_std math library (TickMath, SqrtPriceMath, SwapMath, limb arithmetic) — bit-exact vs exact U512/U1024 references; serves as the oracle for the MASM build |
| `contracts/amm-math-masm` | The same math hand-written in Miden Assembly — bit-equal to the Rust oracle, 19–199× fewer cycles, includes the log2 reverse tick mapping |
| `contracts/clamm-pool` | Rust pool account component (development reference) |
| `contracts/clamm-pool-masm` | **The production pool**: MASM component (160KB — tx-deployable) + the four production note scripts (swap/mint/burn/collect with sender-reclaim paths) |
| `contracts/amm-note-*`, `basic-wallet`, `pool-note-*` | Rust production notes, wallet component, and test-harness notes for the Rust backend |
| `contracts/sender-probe`, `probe-note`, `math-bench`, `bench-note` | Capability probes and in-VM cycle benchmarks |
| `integration/` | MockChain scenario suites (all pool scenarios run against BOTH backends), the host-side `PoolSim` reference, and the E2E binaries |
| `local-net/` | Scripts for the 4-service local network (validator, sequencer, remote prover, ntx-builder) + the ntx-builder prover-timeout patch |
| `frontend/` | React + `@miden-sdk/react` dApp: pool view, swap, positions, note-lifecycle tracker with reclaim, P2ID activity feed |

## Prerequisites

- Rust nightly (pinned by `rust-toolchain.toml`) with `wasm32-wasip2`
- `cargo-miden` 0.9.x (`cargo +nightly-2026-04-30 install cargo-miden --version 0.9.0 --locked`)
- `miden-node` v0.15.x on PATH; the ntx-builder and remote prover are built
  from the miden-node v0.15.2 source (apply
  `local-net/ntx-builder-timeout.patch` to the ntx-builder — the stock
  binary's 10s prover timeout is too short for real proofs) into
  `local-net/bin/`
- Node + yarn for the frontend

## Test everything

```sh
cargo test -p integration --release      # MockChain suites, both backends
cd contracts/amm-math && cargo test --release        # Rust math vs exact refs
cd contracts/amm-math-masm && cargo test --release   # MASM math vs Rust oracle
cd frontend && yarn && npx vitest --run && npx tsc -b --noEmit && yarn build
```

## Run the full stack locally

```sh
local-net/start-stack.sh --fresh                     # 4 services + genesis
cargo run -p integration --bin validate_local_masm --release   # full E2E, deploys pool by tx
cargo run -p integration --bin export_web_artifacts --release -- --deploy
cd frontend && cp .env.local.example .env.local && yarn dev    # browser dApp on :5173
local-net/stop-stack.sh
```

The v0.15 node RPC serves gRPC-web with CORS natively — the browser talks
to `localhost:57291` directly, no proxy.

## Status

All phases complete and verified: source-grounded design, math (Rust +
MASM, bit-equal), pool + notes (54/54 MockChain scenarios across both
backends), two full end-to-end runs on a real local network with real
ntx-builder consumption and real STARK proving, and the browser dApp
exercised live against the stack. Out of scope for v1: flash swaps, TWAP
oracle, multi-hop router, position NFTs, protocol fee switch.
