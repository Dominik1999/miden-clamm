# Shared configuration for the Phase 4 local Miden network stack.
# Sourced by start-stack.sh / stop-stack.sh / status.sh.
#
# Topology (mirrors vendor/miden-node docker-compose.yml at v0.15.2):
#   miden-validator start          :50101  (gRPC, block signing)
#   miden-remote-prover            :50051  (gRPC, --kind transaction; serves the ntx-builder)
#   miden-node sequencer           :57291  (public RPC; drives batches/blocks)
#   miden-ntx-builder start        :50301  (gRPC; network-transaction builder)
#
# The sequencer and ntx-builder share NTX_AUTH_SECRET so the ntx-builder's
# SubmitProvenTx calls pass the RPC's network-account guard.

LOCAL_NET_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$LOCAL_NET_DIR")"
DATA_DIR="$PROJECT_DIR/local-node-data"
LOG_DIR="$LOCAL_NET_DIR/logs"
RUN_DIR="$LOCAL_NET_DIR/run"

VALIDATOR_PORT=50101
PROVER_PORT=50051
RPC_PORT=57291
NTX_PORT=50301

NTX_AUTH_SECRET="phase4-ntx-secret"

# Our Rust-built swaps measure 3.2M-7.0M cycles (Phase 2); the ntx-builder
# default cap is 2^18 = 262,144. Raise to 2^23 = 8,388,608 for the Rust
# backend. Overridable from the environment: the MASM-port validation
# (validate_local_masm) runs the stack at the STOCK default (2^18) to prove
# default-budget viability -- it exports NTX_MAX_CYCLES=262144.
NTX_MAX_CYCLES="${NTX_MAX_CYCLES:-8388608}"

BLOCK_INTERVAL="3s"
BATCH_INTERVAL="1s"

SERVICES="validator prover sequencer ntx-builder"

# The stock v0.15 miden-ntx-builder creates its remote-prover client with a
# hard-coded 10s request timeout (RemoteTransactionProver::new default; no
# with_timeout call in bin/ntx-builder/src/lib.rs). Multi-megacycle network
# txs (our Rust-built swaps) need far longer to prove, so Phase 4 runs a
# patched binary (timeout raised to 30m; see ntx-builder-timeout.patch,
# applied to vendor/miden-node v0.15.2 and built with
# `cargo build --release -p miden-ntx-builder`). Falls back to the PATH
# binary when the patched one is absent.
if [ -x "$LOCAL_NET_DIR/bin/miden-ntx-builder" ]; then
    NTX_BUILDER_BIN="$LOCAL_NET_DIR/bin/miden-ntx-builder"
else
    NTX_BUILDER_BIN="miden-ntx-builder"
fi

# Prefer a remote prover built from vendor/miden-node (guarantees the
# miden-tx "concurrent" feature so STARK proving parallelizes across cores).
if [ -x "$LOCAL_NET_DIR/bin/miden-remote-prover" ]; then
    PROVER_BIN="$LOCAL_NET_DIR/bin/miden-remote-prover"
else
    PROVER_BIN="miden-remote-prover"
fi

pidfile() { echo "$RUN_DIR/$1.pid"; }
logfile() { echo "$LOG_DIR/$1.log"; }

is_running() {
    local pf
    pf="$(pidfile "$1")"
    [ -f "$pf" ] && kill -0 "$(cat "$pf")" 2>/dev/null
}

port_open() {
    nc -z 127.0.0.1 "$1" >/dev/null 2>&1
}
