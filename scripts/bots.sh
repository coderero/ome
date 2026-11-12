#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

DATA="${OME_DATA:-data}"
BOTS_FILE="${1:-$DATA/bots.conf}"

if [[ ! -f "$BOTS_FILE" ]]; then
  echo "no bots file at $BOTS_FILE (run scripts/seed.sh first)" >&2
  exit 1
fi
if [[ -f "$DATA/bots.pid" ]] && kill -0 "$(cat "$DATA/bots.pid")" 2>/dev/null; then
  echo "bots already running (pid $(cat "$DATA/bots.pid"))"
  exit 0
fi

./target/release/ome-bots --addr "${OME_ADDR:-127.0.0.1:7000}" --file "$BOTS_FILE" >>"$DATA/bots.log" 2>&1 &
echo $! >"$DATA/bots.pid"
echo "bots started (pid $!), log: $DATA/bots.log"
