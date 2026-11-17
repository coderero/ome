#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "$0")/.."
if [[ -x ./target/release/omectl ]]; then
  exec ./target/release/omectl down
fi
for pidfile in "${OME_DATA:-data}"/*.pid; do
  [[ -f "$pidfile" ]] || continue
  kill "$(cat "$pidfile")" 2>/dev/null && echo "stopped $(basename "$pidfile" .pid)"
  rm -f "$pidfile"
done
