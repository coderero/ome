#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "$0")/.."

DATA="${OME_DATA:-data}"
for name in bots server; do
  pidfile="$DATA/$name.pid"
  if [[ -f "$pidfile" ]]; then
    pid="$(cat "$pidfile")"
    if kill "$pid" 2>/dev/null; then
      echo "stopped $name (pid $pid)"
    fi
    rm -f "$pidfile"
  fi
done
