#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/env.sh

seed_members() {
  exchange_cmd add-member ccp "$OME_MEMBER_SECRET" clearing
  for p in "${providers[@]}" "${venues[@]}"; do
    read -r name _ member _ <<<"$p"
    exchange_cmd add-member "$name" "$OME_MEMBER_SECRET" "$member"
  done
}

seed_news() {
  for p in "${providers[@]}"; do
    read -r name _ _ _ <<<"$p"
    news_cmd register "$name" "$OME_MEMBER_SECRET" || true
  done
}

seed_venues() {
  wait_for venue_cmd wholesale-co register retail-co "$OME_MEMBER_SECRET" || true
  wait_for venue_cmd ats-co register prime-co "$OME_MEMBER_SECRET" || true
}

registered() { clearing_cmd status | grep -q " $1 "; }

seed_clearing() {
  for p in "${providers[@]}" "wholesale-co"; do
    read -r name _ _ _ <<<"$p"
    wait_for clearing_cmd register "$name" "$OME_MEMBER_SECRET" || registered "$name"
    clearing_cmd deposit "$name" "$OME_COLLATERAL"
  done
}

account_id() { awk '{print $2}'; }

listed_everywhere() {
  local symbol="$1" name
  for p in "${providers[@]}"; do
    read -r name _ _ _ <<<"$p"
    broker_admin "$name" instruments | grep -q "^$symbol " || return 1
  done
}

seed_market() {
  broker_admin retail-co create-account alice alice-pw retail 25000
  broker_admin retail-co create-account bob bob-pw retail 10000
  broker_admin prime-co create-account fund fund-pw quant 5000000

  local listings=(
    "ACME 25.00 52000 Acme Robotics"
    "NOVA 12.50 104000 Nova Energy"
    "ZEN 80.00 16000 Zen Biotech"
  )
  for listing in "${listings[@]}"; do
    read -r symbol price shares name <<<"$listing"
    local lower="${symbol,,}" issuer
    issuer="$(broker_admin prime-co create-account "${lower}_ir" "${lower}-ir-pw" issuer 0 | account_id)"
    exchange_cmd ipo "$symbol" "$price" "$shares" prime-co "$issuer" "$name"
    wait_for listed_everywhere "$symbol"
    exchange_cmd dmm "$symbol" lp-co
    news_cmd status >/dev/null 2>&1 || sim_cmd fair "$symbol" "$price"
  done
}

case "${1:-}" in
  members) seed_members ;;
  clearing) seed_clearing ;;
  venues) seed_venues ;;
  news) seed_news ;;
  market) seed_market ;;
  *) echo "usage: scripts/seed.sh members|clearing|venues|news|market" >&2; exit 2 ;;
esac
