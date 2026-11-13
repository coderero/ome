#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "$0")/.."
source scripts/env.sh

for name in recorder bots retail-co prime-co dma-co lp-co wholesale-co ats-co clearing exchange; do
  pidfile="$OME_DATA/$name.pid"
  if [[ -f "$pidfile" ]]; then
    pid="$(cat "$pidfile")"
    kill "$pid" 2>/dev/null && echo "stopped $name (pid $pid)"
    rm -f "$pidfile"
  fi
done
