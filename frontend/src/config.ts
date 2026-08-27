// Public (NoAuth) counter account deployed on Miden testnet.
//
// Resolution rules for `COUNTER_ADDRESS`:
//   - `VITE_MIDEN_COUNTER_ADDRESS` unset (or omitted) → use the live default
//     deployment (the testnet counter the template ships with).
//   - `VITE_MIDEN_COUNTER_ADDRESS=""` (explicit empty string) → unconfigured,
//     `<Counter>` renders the "address not configured" card.
//   - Any other string → that string is used verbatim (e.g. your own deploy).
// v0.15 counter deployed from project-template `migrate-protocol-v015`
// (contracts/counter-account, built with cargo-miden 0.9). Public + NoAuth, so
// anyone can consume increment notes against it. Hex account id (use AccountId.fromHex).
const DEFAULT_COUNTER_ADDRESS = "0x4dcaee76ffebfc511e06582702289d";
const configuredCounterAddress: string | undefined =
  import.meta.env.VITE_MIDEN_COUNTER_ADDRESS;

export const COUNTER_ADDRESS: string | null =
  configuredCounterAddress === ""
    ? null
    : (configuredCounterAddress ?? DEFAULT_COUNTER_ADDRESS);

// StorageMap slot name for the counter (v0.15 counter-account component)
export const COUNTER_SLOT_NAME =
  "counter_account::counter_contract::count_map";

// Block explorer base URL
export const EXPLORER_BASE_URL = "https://testnet.midenscan.com";

// Poll interval (ms) while waiting for a submitted transaction (the increment
// note publish, then the counter's consume) to commit and the count to update.
export const NETWORK_POLL_INTERVAL_MS = 2_500;

// Hard cap (ms) on how long to wait for each step of the increment (publish
// commit, then the post-consume count change) before giving up. Covers several
// testnet block cycles (~3s block time) with margin.
export const NETWORK_POLL_TIMEOUT_MS = 60_000;

// Compiled increment-note package (cargo-miden 0.9, MAST [0,0,3]). Fetched at
// runtime and turned into the note script the counter consumes.
export const INCREMENT_NOTE_PACKAGE_URL = "/packages/increment-note.masp";

// Application display name (used by wallet adapter)
export const APP_NAME = "Miden Template";

// ---------------------------------------------------------------------------
// CLAMM (Uniswap-v3-style pool) frontend configuration
// ---------------------------------------------------------------------------

// Deployment descriptor written by the Rust exporter
// (`cargo run --bin export_web_artifacts --release -- --deploy [--network testnet]`).
// Contains pool/faucet account ids and network URLs.
//
// Which descriptor loads is env-selected:
//   - default (local dev): `/packages/clamm/deployment.json`, written by the
//     local `--deploy` run against the local stack (gitignored).
//   - production build (`.env.production`): VITE_CLAMM_DEPLOYMENT_URL points
//     at `/packages/clamm/deployment.testnet.json`, the committed public
//     testnet deployment written by `--deploy --network testnet`.
export const CLAMM_DEPLOYMENT_URL =
  import.meta.env.VITE_CLAMM_DEPLOYMENT_URL ?? "/packages/clamm/deployment.json";

// Honesty flag for networks whose operator does NOT run an ntx-builder that
// services arbitrary network accounts (measured on the public Miden testnet,
// 2026-08-27: a committed mint note against the deployed pool was never
// consumed). When set (any non-empty value), the app shows a persistent
// notice on the note-submitting flows: submitted notes will sit pending and
// can be reclaimed by their sender after the deadline; pool-state reads are
// unaffected. Leave unset for the local stack, whose ntx-builder services
// the pool within seconds.
export const CLAMM_NTX_PASSIVE: boolean =
  ((import.meta.env.VITE_CLAMM_NTX_PASSIVE as string | undefined) ?? "") !== "";

// Serialized MASM note scripts (NoteScript bytes), written by the exporter.
export const CLAMM_SCRIPT_URLS = {
  swap: "/packages/clamm/swap.notescript",
  mint: "/packages/clamm/mint.notescript",
  burn: "/packages/clamm/burn.notescript",
  collect: "/packages/clamm/collect.notescript",
} as const;

// Pool state poll interval (ms). The pool is updated externally by network
// transactions, so the UI re-imports and re-reads it on a timer.
export const CLAMM_POOL_POLL_MS = 5_000;

// Tracked-note lifecycle poll interval (ms).
export const CLAMM_NOTE_POLL_MS = 5_000;

// Amount minted to the local wallet per faucet request (raw units, 6 decimals).
export const CLAMM_FAUCET_AMOUNT = 1_000_000_000_000n;

// Miden SDK configuration — override via environment variables
export const MIDEN_RPC_URL =
  import.meta.env.VITE_MIDEN_RPC_URL ?? "testnet";
// "devnet" | "testnet" | "local" | custom prover URL (e.g. the local stack's
// remote prover at http://localhost:50051 — it serves gRPC-web + CORS).
export const MIDEN_PROVER =
  (import.meta.env.VITE_MIDEN_PROVER as "devnet" | "testnet" | "local" | (string & {})) ??
  "testnet";
