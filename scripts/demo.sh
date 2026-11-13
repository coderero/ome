#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/env.sh

if [[ "${1:-}" == "--fresh" ]]; then
  scripts/stop.sh || true
  rm -f "$OME_DATA"/*.journal "$OME_DATA/bots.conf"
fi

cargo build --release --workspace --quiet
mkdir -p "$OME_DATA"
fresh=0
[[ -s "$OME_DATA/exchange.journal" ]] || fresh=1

start_bg exchange "$BIN/ome-exchange" --listen "$OME_EXCHANGE" --journal "$OME_DATA/exchange.journal" --admin-secret "$OME_EXCHANGE_SECRET"
wait_for exchange_cmd instruments
(( fresh )) && scripts/seed.sh members

start_bg clearing "$BIN/ome-clearing" --listen "$OME_CLEARING" --exchange "$OME_EXCHANGE" --member ccp \
  --member-secret "$OME_MEMBER_SECRET" --journal "$OME_DATA/clearing.journal" --ops-secret "$OME_CLEARING_SECRET" \
  --cycle-ms "$OME_CYCLE_MS"
wait_for clearing_cmd status
(( fresh )) && scripts/seed.sh clearing

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
  if [[ "$OME_REGIME" == "us" && "$name" == "retail-co" ]]; then routes=(--wholesaler "$(venue_addr wholesale-co)"); fi
  if [[ "$OME_REGIME" == "us" && "$name" == "prime-co" ]]; then routes=(--ats "$(venue_addr ats-co)"); fi
  start_bg "$name" "$BIN/ome-broker" --profile "$kind" --listen "$OME_HOST:$port" --exchange "$OME_EXCHANGE" \
    --member "$name" --member-secret "$OME_MEMBER_SECRET" --journal "$OME_DATA/$name.journal" --admin-secret "$OME_BROKER_SECRET" \
    --clearing "$OME_CLEARING" --regime "$OME_REGIME" "${routes[@]}"
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
scripts/bots.sh

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
stop everything:  scripts/stop.sh
MSG
