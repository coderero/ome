#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --workspace --quiet
args=()
if [[ "${1:-}" == "--fresh" ]]; then args+=(--fresh); fi
if [[ "${OME_RECORD:-0}" == "1" ]]; then args+=(--record); fi
exec ./target/release/omectl up --no-build ${args[@]+"${args[@]}"}
