#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

export OME_DATA="${OME_DATA:-data}"
export OME_ADDR="${OME_ADDR:-127.0.0.1:7000}"
export OME_ADMIN_SECRET="${OME_ADMIN_SECRET:-admin}"

if [[ "${1:-}" == "--fresh" ]]; then
  scripts/stop.sh || true
  rm -f "$OME_DATA/ome.journal" "$OME_DATA/bots.conf"
fi

cargo build --release --workspace --quiet
mkdir -p "$OME_DATA"
fresh=0
[[ -s "$OME_DATA/ome.journal" ]] || fresh=1

if [[ -f "$OME_DATA/server.pid" ]] && kill -0 "$(cat "$OME_DATA/server.pid")" 2>/dev/null; then
  echo "server already running (pid $(cat "$OME_DATA/server.pid"))"
else
  ./target/release/ome-server --listen "$OME_ADDR" --journal "$OME_DATA/ome.journal" \
    --admin-secret "$OME_ADMIN_SECRET" >>"$OME_DATA/server.log" 2>&1 &
  echo $! >"$OME_DATA/server.pid"
  echo "server started on $OME_ADDR (pid $!), log: $OME_DATA/server.log"
fi

for _ in $(seq 50); do
  ./target/release/ome --user admin --secret "$OME_ADMIN_SECRET" instruments >/dev/null 2>&1 && break
  sleep 0.1
done

if (( fresh )); then
  scripts/seed.sh
fi
scripts/bots.sh

cat <<MSG

market is live. open a terminal UI:
  scripts/tui.sh admin $OME_ADMIN_SECRET     # admin console: IPOs, halts, balances
  scripts/tui.sh alice alice-pw              # retail trader
  scripts/tui.sh fund fund-pw                # institutional trader
command line:
  ./target/release/ome --user alice --secret alice-pw balance
stop everything:
  scripts/stop.sh
MSG
