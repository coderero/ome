#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

case "${1:-}" in
  up) docker compose up -d --wait ;;
  down) docker compose down ;;
  reset) docker compose down -v ;;
  psql) shift; docker compose exec postgres psql -U ome -d ome "$@" ;;
  redis) shift; docker compose exec redis redis-cli "$@" ;;
  *) echo "usage: scripts/db.sh up|down|reset|psql [args]|redis [args]" >&2; exit 2 ;;
esac
