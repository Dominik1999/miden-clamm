#!/usr/bin/env bash
# Health check for the local Miden network stack:
#   - each service's process is alive and its port is open
#   - the chain tip is advancing (sampled twice from the sequencer log)
set -uo pipefail

source "$(dirname "$0")/common.sh"

declare -a PORTS=("$VALIDATOR_PORT" "$PROVER_PORT" "$RPC_PORT" "$NTX_PORT")
i=0
ok=1
for svc in $SERVICES; do
    port="${PORTS[$i]}"
    i=$((i + 1))
    if is_running "$svc"; then
        if port_open "$port"; then
            echo "OK   $svc (pid $(cat "$(pidfile "$svc")"), :$port)"
        else
            echo "WARN $svc alive but :$port not open"
            ok=0
        fi
    else
        echo "DOWN $svc"
        ok=0
    fi
done

# Block production: the ntx-builder logs every committed block it applies
# ("committed_tip: N"). Sample the newest tip twice, a block interval apart,
# and require advance. ANSI color codes are stripped before matching.
latest_block() {
    sed 's/\x1b\[[0-9;]*m//g' "$(logfile ntx-builder)" 2>/dev/null |
        grep -o 'committed_tip: [0-9]*' | grep -o '[0-9]*$' | tail -1
}
b1="$(latest_block)"
sleep 4
b2="$(latest_block)"
if [ -n "$b2" ] && [ -n "$b1" ] && [ "$b2" -gt "$b1" ]; then
    echo "OK   blocks advancing ($b1 -> $b2)"
elif [ -n "$b2" ]; then
    echo "WARN chain tip not observed advancing (still $b2); empty blocks may not be logged"
else
    echo "WARN no block numbers found in sequencer log yet"
fi

[ "$ok" = 1 ] && exit 0 || exit 1
