#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/env.sh

roster="${OME_ROSTER:-config/roster.conf}"
args=(--roster "$roster" --admin-secret "$OME_BROKER_SECRET" --scale "${OME_AGENT_SCALE:-1}")
for p in "${providers[@]}"; do
  read -r name _ _ port <<<"$p"
  args+=(--broker "$name=$OME_HOST:$port")
done
start_bg agents "$BIN/ome-agents" "${args[@]}" "$@"
