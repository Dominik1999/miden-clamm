#!/usr/bin/env bash
# Starts the 4-service local Miden network (validator + remote prover +
# sequencer + ntx-builder), bootstrapping genesis on first run.
#
# Usage:
#   ./start-stack.sh          # bootstrap if needed, start everything
#   ./start-stack.sh --fresh  # wipe all node data first (clean genesis)
#
# Data:  project-template/local-node-data/   (gitignored)
# Logs:  project-template/local-net/logs/    (gitignored)
#
# Genesis note: the validator's built-in default GenesisConfig sets
# fee_parameters.verification_base_fee = 0, so network transactions cost
# zero fees on this stack (verified in
# vendor/miden-node/crates/store/src/genesis/config/mod.rs:111).
set -euo pipefail

source "$(dirname "$0")/common.sh"

if [ "${1:-}" = "--fresh" ]; then
    echo "==> Wiping node data for a fresh genesis"
    "$LOCAL_NET_DIR/stop-stack.sh" >/dev/null 2>&1 || true
    rm -rf "$DATA_DIR"
fi

mkdir -p "$DATA_DIR" "$LOG_DIR" "$RUN_DIR"

for svc in $SERVICES; do
    if is_running "$svc"; then
        echo "ERROR: $svc already running (pid $(cat "$(pidfile "$svc")")). Run stop-stack.sh first." >&2
        exit 1
    fi
done

# ---------------------------------------------------------------- bootstrap
if [ ! -f "$DATA_DIR/validator/.bootstrapped" ]; then
    echo "==> Bootstrapping validator (genesis block)"
    rm -rf "$DATA_DIR/genesis" "$DATA_DIR/validator" "$DATA_DIR/accounts"
    mkdir -p "$DATA_DIR/genesis" "$DATA_DIR/validator" "$DATA_DIR/accounts"
    # Optional custom genesis config (MIDEN_GENESIS_CONFIG_FILE), mirroring
    # the docker-compose contract. Phase 4 uses this to seed the pool
    # account AT GENESIS: its Rust-built account code serializes to ~600KB,
    # far above the protocol's 256KiB ACCOUNT_UPDATE_MAX_SIZE, so it cannot
    # be deployed through a transaction at v0.15.
    GENESIS_CONFIG_ARGS=""
    if [ -n "${MIDEN_GENESIS_CONFIG_FILE:-}" ]; then
        echo "    using genesis config: $MIDEN_GENESIS_CONFIG_FILE"
        GENESIS_CONFIG_ARGS="--genesis-config-file $MIDEN_GENESIS_CONFIG_FILE"
    fi
    # shellcheck disable=SC2086
    miden-validator bootstrap \
        --data-directory "$DATA_DIR/validator" \
        --genesis-block-directory "$DATA_DIR/genesis" \
        --accounts-directory "$DATA_DIR/accounts" \
        $GENESIS_CONFIG_ARGS \
        >>"$(logfile bootstrap)" 2>&1
    touch "$DATA_DIR/validator/.bootstrapped"
fi

if [ ! -f "$DATA_DIR/node/.bootstrapped" ]; then
    echo "==> Bootstrapping node store"
    rm -rf "$DATA_DIR/node"
    mkdir -p "$DATA_DIR/node"
    miden-node bootstrap \
        --data-directory "$DATA_DIR/node" \
        --file "$DATA_DIR/genesis/genesis.dat" \
        >>"$(logfile bootstrap)" 2>&1
    touch "$DATA_DIR/node/.bootstrapped"
fi

if [ ! -f "$DATA_DIR/ntx-builder/.bootstrapped" ]; then
    echo "==> Bootstrapping ntx-builder database"
    rm -rf "$DATA_DIR/ntx-builder"
    mkdir -p "$DATA_DIR/ntx-builder"
    "$NTX_BUILDER_BIN" bootstrap \
        --data-directory "$DATA_DIR/ntx-builder" \
        --file "$DATA_DIR/genesis/genesis.dat" \
        >>"$(logfile bootstrap)" 2>&1
    touch "$DATA_DIR/ntx-builder/.bootstrapped"
fi

# ---------------------------------------------------------------- services
start_service() {
    local name="$1"
    shift
    echo "==> Starting $name: $*"
    RUST_LOG="${RUST_LOG:-info}" nohup "$@" >>"$(logfile "$name")" 2>&1 &
    echo $! >"$(pidfile "$name")"
}

wait_port() {
    local name="$1" port="$2" tries="${3:-60}"
    for _ in $(seq 1 "$tries"); do
        if port_open "$port"; then
            echo "    $name is listening on :$port"
            return 0
        fi
        if ! is_running "$name"; then
            echo "ERROR: $name exited during startup; see $(logfile "$name")" >&2
            exit 1
        fi
        sleep 0.5
    done
    echo "ERROR: $name did not open :$port in time; see $(logfile "$name")" >&2
    exit 1
}

start_service validator \
    miden-validator start \
    --listen "127.0.0.1:$VALIDATOR_PORT" \
    --data-directory "$DATA_DIR/validator"
wait_port validator "$VALIDATOR_PORT"

# Default request timeout is 60s; our Rust-built network txs run 3.2M-7.0M
# VM cycles and their STARK proofs can take substantially longer.
echo "==> prover binary: $PROVER_BIN"
start_service prover \
    "$PROVER_BIN" \
    --kind transaction \
    --port "$PROVER_PORT" \
    --timeout 2h
wait_port prover "$PROVER_PORT"

start_service sequencer \
    miden-node sequencer \
    --data-directory "$DATA_DIR/node" \
    --rpc.listen "127.0.0.1:$RPC_PORT" \
    --validator.url "http://127.0.0.1:$VALIDATOR_PORT" \
    --ntx-builder.url "http://127.0.0.1:$NTX_PORT" \
    --rpc.network-tx-auth-header-value "$NTX_AUTH_SECRET" \
    --block.interval "$BLOCK_INTERVAL" \
    --batch.interval "$BATCH_INTERVAL"
wait_port sequencer "$RPC_PORT"

echo "==> ntx-builder binary: $NTX_BUILDER_BIN"
start_service ntx-builder \
    "$NTX_BUILDER_BIN" start \
    --listen "127.0.0.1:$NTX_PORT" \
    --rpc.url "http://127.0.0.1:$RPC_PORT" \
    --rpc.auth-header-value "$NTX_AUTH_SECRET" \
    --tx-prover.url "http://127.0.0.1:$PROVER_PORT" \
    --max-cycles "$NTX_MAX_CYCLES" \
    --data-directory "$DATA_DIR/ntx-builder"
wait_port ntx-builder "$NTX_PORT"

echo "==> Stack is up. RPC: http://127.0.0.1:$RPC_PORT (ntx max-cycles: $NTX_MAX_CYCLES)"
echo "    Verify block production with: ./status.sh"
