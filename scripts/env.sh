OME_DATA="${OME_DATA:-data}"
OME_HOST="${OME_HOST:-127.0.0.1}"
OME_EXCHANGE="${OME_EXCHANGE:-$OME_HOST:7000}"
OME_EXCHANGE_SECRET="${OME_EXCHANGE_SECRET:-ops}"
OME_BROKER_SECRET="${OME_BROKER_SECRET:-admin}"
OME_MEMBER_SECRET="${OME_MEMBER_SECRET:-member-pw}"
OME_CLEARING="${OME_CLEARING:-$OME_HOST:7200}"
OME_CLEARING_SECRET="${OME_CLEARING_SECRET:-ops}"
OME_TRADING_DAY="${OME_TRADING_DAY:-5 45 5 5}"
OME_REGIME="${OME_REGIME:-us}"
if [[ "$OME_TRADING_DAY" == "off" ]]; then
  OME_CYCLE_MS="${OME_CYCLE_MS:-30000}"
else
  OME_CYCLE_MS="${OME_CYCLE_MS:-0}"
fi
OME_COLLATERAL="${OME_COLLATERAL:-2000000}"
BIN=./target/release

providers=(
  "retail-co retail broker 7101"
  "prime-co prime broker 7102"
  "dma-co dma broker 7103"
  "lp-co lp lp 7104"
)

provider_addr() {
  local name kind member port
  for p in "${providers[@]}"; do
    read -r name kind member port <<<"$p"
    if [[ "$name" == "$1" ]]; then echo "$OME_HOST:$port"; return; fi
  done
  return 1
}

clearing_cmd() { "$BIN/ome" clearing --addr "$OME_CLEARING" --user ops --secret "$OME_CLEARING_SECRET" "$@"; }
exchange_cmd() { "$BIN/ome" exchange --addr "$OME_EXCHANGE" --member ops --secret "$OME_EXCHANGE_SECRET" "$@"; }
broker_admin() { local addr; addr="$(provider_addr "$1")"; shift; "$BIN/ome" broker --addr "$addr" --user admin --secret "$OME_BROKER_SECRET" "$@"; }
broker_as() { local addr; addr="$(provider_addr "$1")"; shift; local user="$1" secret="$2"; shift 2; "$BIN/ome" broker --addr "$addr" --user "$user" --secret "$secret" "$@"; }

wait_for() {
  for _ in $(seq 100); do "$@" >/dev/null 2>&1 && return 0; sleep 0.1; done
  echo "timed out waiting for: $*" >&2
  return 1
}

start_bg() {
  local name="$1"; shift
  local pidfile="$OME_DATA/$name.pid"
  if [[ -f "$pidfile" ]] && kill -0 "$(cat "$pidfile")" 2>/dev/null; then
    echo "$name already running (pid $(cat "$pidfile"))"
    return
  fi
  "$@" >>"$OME_DATA/$name.log" 2>&1 &
  echo $! >"$pidfile"
  echo "started $name (pid $!), log: $OME_DATA/$name.log"
}
