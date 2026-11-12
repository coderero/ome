#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

user="${1:-admin}"
secret="${2:-${OME_ADMIN_SECRET:-admin}}"
shift $(( $# > 2 ? 2 : $# ))
exec ./target/release/ome-tui --addr "${OME_ADDR:-127.0.0.1:7000}" --user "$user" --secret "$secret" "$@"
