#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/env.sh

if [[ "${1:-}" == "--fresh" ]]; then
  scripts/stop.sh || true
  rm -f "$OME_DATA"/*.journal
fi

cargo build --release --workspace --quiet
mkdir -p "$OME_DATA"
fresh=0
[[ -s "$OME_DATA/exchange.journal" ]] || fresh=1

start_bg exchange "$BIN/ome-exchange" --listen "$OME_EXCHANGE" --journal "$OME_DATA/exchange.journal" --admin-secret "$OME_EXCHANGE_SECRET" \
  --require-prospectus
wait_for exchange_cmd instruments
(( fresh )) && scripts/seed.sh members

start_bg clearing "$BIN/ome-clearing" --listen "$OME_CLEARING" --exchange "$OME_EXCHANGE" --member ccp \
  --member-secret "$OME_MEMBER_SECRET" --journal "$OME_DATA/clearing.journal" --ops-secret "$OME_CLEARING_SECRET" \
  --cycle-ms "$OME_CYCLE_MS"
wait_for clearing_cmd status
(( fresh )) && scripts/seed.sh clearing

start_bg simctl "$BIN/ome-simctl" --listen "$OME_SIMCTL" --journal "$OME_DATA/simctl.journal" \
  --secret "$OME_SIM_SECRET" --runner-secret "$OME_SIM_RUNNER_SECRET" --exchange "$OME_EXCHANGE" \
  --exchange-secret "$OME_EXCHANGE_SECRET" --newsroom "$OME_NEWS" --news-secret "$OME_NEWS_SECRET"
wait_for sim_cmd status

if [[ "$OME_WRITER" != "templates" ]] && ! curl -sf "$OME_LLM/api/tags" >/dev/null 2>&1 && command -v docker >/dev/null 2>&1; then
  echo "starting Ollama for the newsroom (OME_WRITER=templates skips this)"
  scripts/llm.sh up || echo "Ollama could not start; the newsroom falls back to its event catalogue"
fi
writer=()
if [[ "$OME_WRITER" == "llm" ]] || { [[ "$OME_WRITER" == "auto" ]] && curl -sf "$OME_LLM/api/tags" >/dev/null 2>&1; }; then
  writer=(--llm "$OME_LLM" --llm-model "$OME_LLM_MODEL")
fi
start_bg newsroom "$BIN/ome-newsroom" --listen "$OME_NEWS" --journal "$OME_DATA/newsroom.journal" \
  --companies config/companies.conf --ops-secret "$OME_NEWS_SECRET" --exchange "$OME_EXCHANGE" \
  --exchange-secret "$OME_EXCHANGE_SECRET" \
  --quarter-days "$OME_QUARTER_DAYS" --news-per-day "$OME_NEWS_PER_DAY" "${writer[@]}"
wait_for news_cmd status
(( fresh )) && scripts/seed.sh news

for v in "${venues[@]}"; do
  read -r name kind _ port <<<"$v"
  start_bg "$name" "$BIN/ome-venue" --kind "$kind" --listen "$OME_HOST:$port" --exchange "$OME_EXCHANGE" \
    --member "$name" --member-secret "$OME_MEMBER_SECRET" --journal "$OME_DATA/$name.journal" --ops-secret "$OME_VENUE_SECRET"
done
for v in "${venues[@]}"; do
  read -r name _ _ _ <<<"$v"
  wait_for venue_cmd "$name" status
done
(( fresh )) && scripts/seed.sh venues

for p in "${providers[@]}"; do
  read -r name kind member port <<<"$p"
  routes=()
  if [[ "$OME_RECORD" == "1" ]]; then routes+=(--postgres "$OME_POSTGRES"); fi
  if [[ "$OME_REGIME" == "us" && "$name" == "retail-co" ]]; then routes+=(--wholesaler "$(venue_addr wholesale-co)"); fi
  if [[ "$OME_REGIME" == "us" && "$name" == "prime-co" ]]; then routes+=(--ats "$(venue_addr ats-co)"); fi
  start_bg "$name" "$BIN/ome-broker" --profile "$kind" --listen "$OME_HOST:$port" --exchange "$OME_EXCHANGE" \
    --member "$name" --member-secret "$OME_MEMBER_SECRET" --journal "$OME_DATA/$name.journal" --admin-secret "$OME_BROKER_SECRET" \
    --clearing "$OME_CLEARING" --regime "$OME_REGIME" --news "$OME_NEWS" "${routes[@]}"
done
for p in "${providers[@]}"; do
  read -r name _ _ _ <<<"$p"
  wait_for broker_admin "$name" accounts
done

(( fresh )) && scripts/seed.sh market
if (( fresh )) && [[ "$OME_TRADING_DAY" != "off" ]]; then
  # shellcheck disable=SC2086
  exchange_cmd schedule $OME_TRADING_DAY
fi
scripts/agents.sh

if [[ "$OME_RECORD" == "1" ]]; then
  recorder_args=(--postgres "$OME_POSTGRES" --redis "$OME_REDIS" --exchange "$OME_EXCHANGE"
    --exchange-secret "$OME_EXCHANGE_SECRET" --clearing "$OME_CLEARING" --clearing-secret "$OME_CLEARING_SECRET"
    --broker-secret "$OME_BROKER_SECRET" --venue-secret "$OME_VENUE_SECRET" --news "$OME_NEWS" --news-secret "$OME_NEWS_SECRET")
  for p in "${providers[@]}"; do
    read -r name _ _ port <<<"$p"
    recorder_args+=(--broker "$name=$OME_HOST:$port")
  done
  for v in "${venues[@]}"; do
    read -r name _ _ port <<<"$v"
    recorder_args+=(--venue "$name=$OME_HOST:$port")
  done
  start_bg recorder "$BIN/ome-recorder" "${recorder_args[@]}"
fi

cat <<MSG

market is live:
  exchange    $OME_EXCHANGE           ./target/release/ome exchange members | instruments | halt ACME news
  clearing    $OME_CLEARING           ./target/release/ome clearing status   (one cycle per trading day, T+1)
  sessions    trading day (pre-open open closing closed, seconds): $OME_TRADING_DAY   ome exchange status | session | schedule
  retail      $(provider_addr retail-co)   alice / alice-pw, bob / bob-pw
  prime       $(provider_addr prime-co)   fund / fund-pw (quant), issuers
  dma         $(provider_addr dma-co)   HFT desks
  lp          $(provider_addr lp-co)   market-making desks
  wholesaler  $(venue_addr wholesale-co)   buys retail flow from retail-co, pays PFOF   ./target/release/ome venue --addr $(venue_addr wholesale-co) status
  dark pool   $(venue_addr ats-co)   midpoint matching for prime-co (regime: $OME_REGIME)
examples:
  ./target/release/ome broker --addr $(provider_addr retail-co) --user alice --secret alice-pw balance
  ./target/release/ome broker --addr $(provider_addr retail-co) --user alice --secret alice-pw buy ACME mkt 5
recording:   OME_RECORD=1 scripts/demo.sh records into PostgreSQL and Redis (scripts/db.sh up first)
news:        ./target/release/ome news status | list | read <seq> | watch | inject lawsuit ACME | inject cpi
simulation:  ./target/release/ome sim status | pause noise | crash ACME | news NOVA -8 | fair ZEN 85.00 2 | speed 3 30 3 3
stop everything:  scripts/stop.sh
MSG
