#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/env.sh

bots_file="${1:-$OME_DATA/bots.conf}"
if [[ ! -f "$bots_file" ]]; then
  echo "no bots file at $bots_file (run scripts/seed.sh market first)" >&2
  exit 1
fi
start_bg bots "$BIN/ome-bots" --file "$bots_file"
