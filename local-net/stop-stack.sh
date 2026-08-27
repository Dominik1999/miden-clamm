#!/usr/bin/env bash
# Stops the local Miden network services started by start-stack.sh.
set -uo pipefail

source "$(dirname "$0")/common.sh"

# Stop in reverse dependency order.
for svc in ntx-builder sequencer prover validator; do
    pf="$(pidfile "$svc")"
    if [ -f "$pf" ]; then
        pid="$(cat "$pf")"
        if kill -0 "$pid" 2>/dev/null; then
            echo "==> Stopping $svc (pid $pid)"
            kill "$pid" 2>/dev/null
            for _ in $(seq 1 20); do
                kill -0 "$pid" 2>/dev/null || break
                sleep 0.25
            done
            kill -9 "$pid" 2>/dev/null || true
        fi
        rm -f "$pf"
    fi
done
echo "==> Stack stopped."
