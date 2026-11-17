#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OME_LLM="${OME_LLM:-http://127.0.0.1:11434}"
OME_LLM_MODEL="${OME_LLM_MODEL:-qwen2.5:7b}"

files=(-f docker-compose.yml)
if docker info 2>/dev/null | grep -q "Runtimes:.*nvidia"; then
  files+=(-f docker-compose.gpu.yml)
fi
compose() { docker compose "${files[@]}" --profile llm "$@"; }

case "${1:-}" in
  up)
    compose up -d --wait ollama
    compose exec ollama ollama pull "$OME_LLM_MODEL"
    echo "ollama ready at $OME_LLM with $OME_LLM_MODEL ($([[ ${#files[@]} -gt 2 ]] && echo GPU || echo CPU))"
    ;;
  down) compose stop ollama ;;
  status) compose exec ollama ollama ps ;;
  *) echo "usage: scripts/llm.sh up|down|status" >&2; exit 2 ;;
esac
