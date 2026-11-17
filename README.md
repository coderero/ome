# OME: Order Matching Exchange

A simulated equities market built like the real thing: an **exchange** that only deals with member firms, **brokers and liquidity providers** that hold client accounts and route orders, and **trading agents** that can only reach the market through a provider. Services talk over a binary **SBE over TCP** protocol, every service recovers from crashes through its own journal, and prebuilt bots generate real traffic.

Every price, balance and trade comes from real orders matching in the engine. Nothing is mocked.

## Quick Start

```bash
scripts/demo.sh --fresh       # build, then `omectl up --fresh`: preflight checks, start every service of config/topology.conf
                              # under supervision, approve the declared members, wait until the market is ready
target/release/omectl status  # live cluster view: health, dependencies and metrics reported by every service
scripts/stop.sh               # `omectl down`

# With Docker, omectl starts PostgreSQL and Redis (recording, chart history) and Ollama (the newsroom's writer and
# the world's invented events, GPU when the NVIDIA runtime is installed) by itself; it asks before downloading
# the model (qwen2.5:7b). OME_RECORD=0 or OME_WRITER=templates skip them.
```

On a Mac, with or without Docker, follow [docs/macos.md](docs/macos.md).

```bash
ome exchange members                                                          # who is connected
ome broker --addr 127.0.0.1:7101 --user alice --secret alice-pw buy ACME mkt 5
ome broker --addr 127.0.0.1:7101 --user alice --secret alice-pw balance
```

## Market Structure

```
 agents: HFT desks · market makers · quant / prop funds · retail traders
        │  client protocol (SBE)          ── agents never connect to the exchange ──
        ▼
 ┌─────────────────────── providers (exchange members) ────────────────────────┐
 │ retail-co :7101  retail broker       retail accounts                        │
 │ prime-co  :7102  prime broker        quant, prop and issuer accounts        │
 │ dma-co    :7103  DMA provider        HFT accounts (direct market access)    │
 │ lp-co     :7104  liquidity provider  market-making desks                    │
 │ client accounts, balances, pre-trade risk, routing, market-data fan-out     │
 └─────────────────────────────┬───────────────────────────────────────────────┘
                               │  member protocol (SBE, sequenced)
                               ▼
 ┌──────────────────────────── exchange :7000 ─────────────────────────────────┐
 │ members only (no client balances): matching, listings and IPOs, halts,      │
 │ 30% circuit breaker, price bands, per-member sequenced execution feeds,     │
 │ fees, member credit limits                                                  │
 └──────────────┬──────────────────────────────────────────────▲───────────────┘
                │ every trade + IPO allocation (sequenced)      │ cycles, settled, credit limits
                ▼                                               │
 ┌──────────────────────────── clearing house :7200 ───────────┴───────────────┐
 │ central counterparty + depository: trade-day cycles, T+1 netting and        │
 │ delivery-versus-payment between members, collateral, margin, fails          │
 └─────────────────────────────────────────────────────────────────────────────┘
       settlement reports (sequenced) ──► providers mark client trades settled
```

## Workers and Masters

Every service is either a **worker** or a **master**.

- **Workers** behave like the real institutions and only use real interfaces: exchange, clearing house, brokers, venues, the newsroom (a news vendor), the recorder (a market data vendor) and the agents (participants). None of them knows it is part of a simulation.
- **ome-simctl** is the simulation master. It owns the simulated world: the economy, the companies and what happens to them, the agent roster, scenarios, recordings and the trading-day speed. It acts on the workers only the way a real company, statistics office, operator or customer would: it files prospectuses and lists IPOs through the exchange, opens accounts through the brokers' account desks, and sends what happens to the newsroom as wire events.
- **omectl** is the infrastructure master. It runs the topology, starts services in order, supervises them (restart on crash with backoff), keeps the service registry, runs preflight checks and approves the membership applications of the participants declared in the topology. It manages services, not markets.

```
          omectl (infrastructure)                        ome-simctl (simulation)
  topology · start order · restart · registry      world · companies · events · roster · scenarios
   preflight · admits declared members                    │           │              │
          │ health, dependencies, metrics                  │ wire       │ prospectus,  │ accounts,
          ▼ (every service reports each second)            ▼ events     ▼ IPO, DMM     ▼ agent assignments
  ┌───────────────────────── workers (real interfaces only) ─────────────────────────────────────┐
  │ newsroom (vendor) ─ news ─► brokers ◄── agents        exchange ◄── brokers, venues, clearing  │
  └───────────────────────────────────────────────────────────────────────────────────────────────┘
```

## Running the Market: omectl

`omectl` reads `config/topology.conf`: one line per service (`<kind> <name> port=<port> [options]`) plus `set` lines for shared settings (host, data directory, registry port, regime, writer, trading day, default collateral). `OME_HOST`, `OME_DATA`, `OME_REGIME`, `OME_TRADING_DAY`, `OME_WRITER` and `OME_RECORD` override them.

| Command | What it does |
|---|---|
| `omectl up [--fresh] [--record\|--no-record] [--no-build] [--writer auto\|llm\|templates]` | starts PostgreSQL, Redis and Ollama in Docker when they are needed and not running (asking before a model download), builds, runs the preflight checks (refuses on a failure), starts the supervisor in the background and waits until every service reports ready |
| `omectl status` | the live cluster view from the registry: health, process, address, dependencies that are down, metrics |
| `omectl logs <service> [-f]` · `omectl restart <service>` | a service's log; stop and start one service |
| `omectl doctor` | preflight only: binaries, free ports, writable data directory, free disk, world and roster files, Ollama and the model, PostgreSQL and Redis when recording, Docker, default secrets on a non-loopback host |
| `omectl admit` | approve pending applications of declared participants now (the supervisor does this every two seconds) |
| `omectl down` | stops every service in reverse start order, then the supervisor |

- **Start order**: exchange, clearing house, newsroom, venues, brokers, simctl, agent runners, recorder. Each waits until the previous one has registered (at most 20 seconds); services retry their own links anyway.
- **Supervision**: a service that exits is restarted after 1, 2, 4 ... up to 30 seconds; the delay resets after a minute of uptime. Every service recovers from its journal, so a killed broker comes back with the same balances.
- **Registry and health**: every service reports once a second to the registry that omectl runs (`OME_REGISTRY`, port 7600): its address, health (starting, ready, degraded, stopping), the state of each dependency (exchange link, clearing link, news feed, venues, LLM writer, databases), lag and metrics, and what it offers. A service that falls silent for five seconds shows as down. Services also find each other by name in the registry: the recorder and simctl discover brokers and venues as they join, and brokers resolve venue names.
- **Backing services**: PostgreSQL, Redis and Ollama are topology entries too. `record=auto` (the default) records whenever the databases run or Docker can start them; `writer=auto` uses the local model when Ollama runs or Docker can start it and the model is downloaded (omectl asks before pulling it; without a terminal it uses templates). They show in `omectl status`, and `omectl down` stops the containers omectl started, nothing else.
- **Backends**: processes on this machine today, behind a `Backend` trait; Docker, SSH hosts and Kubernetes are planned (phase 10).

## Membership Applications

Brokers, venues and the clearing house are not wired together by scripts. Like real firms they apply:

1. A firm that is refused at logon sends an `Apply` message (member kind, name, secret, a note) on the same connection. Its link keeps retrying, so it logs on by itself once approved.
2. Exchange operations, clearing operations, each venue's operators and the newsroom's desk see pending applications (`ome exchange applications`, `ome clearing applications`, ...) and approve or refuse them (`approve <name>`, `refuse <name> [reason]`). A clearing approval can record the first collateral deposit (`ome clearing approve retail-co 2000000`).
3. Approval journals the registration the service already had (exchange member, clearing member, venue routing member, news subscriber). Pending applications are kept in memory; an applicant whose application was lost in a restart simply applies again on its next logon.

`omectl` approves the participants declared in the topology with the declared member kind, collateral, venue routes and news subscription. Anyone else, for example a broker started by hand, waits for an operator: `ome exchange approve my-broker`. The older direct commands (`ome exchange add-member`, `ome clearing register`, `ome venue register`, `ome news register`) still work.

Responsibilities are split the way real markets split them:

| Concern | Lives in |
|---|---|
| Order book, matching, price-time priority | exchange (`ome-core` engine) |
| Listings, IPOs, halts, circuit breaker | exchange (admin = exchange operations member) |
| Self-trade prevention | exchange, per beneficial account (member + account) |
| Settlement between members, collateral, margin, member credit limits | clearing house |
| Client accounts, cash and positions, buying power | provider |
| Client roles and order limits | provider profile |
| Who may open which account type | provider profile (retail broker can't open HFT accounts, etc.) |
| Trading strategies | agents, connected to a provider |

## Workspace

```
crates/
├── ome-core       matching engine, risk checks, candles, trade tape, symbols (pure, no I/O)
├── ome-protocol   SBE codec + SOFH framing; client, member, server, simulation and registry message families
├── ome-store      generic crash-safe journal (write-ahead log of SBE frames) behind the FactLog trait
├── ome-net        TCP session server, outbound back-pressure, auth hashing, logging, membership applications,
│                  registry reporter and name directory
├── ome-llm        Ollama client behind the Model trait (the newsroom writes with it, simctl invents events with it)
├── ome-ctl        omectl: topology, preflight, supervisor, service registry, admissions, backing services
├── ome-bench      ome-load: capacity load test of the exchange and a broker (orders/s, latency, fan-out)
├── ome-exchange   exchange service (lib + `ome-exchange` binary)
├── ome-clearing   clearing house (lib + `ome-clearing` binary)
├── ome-broker     provider service with retail / prime / dma / lp profiles (lib + `ome-broker`)
├── ome-venue      off-exchange venues: wholesaler and dark pool (lib + `ome-venue`)
├── ome-simctl     simulation master: world model, wire events, seeding, agent roster and scenarios (lib + `ome-simctl`)
├── ome-newsroom   news vendor: takes wire events from contributors, writes and publishes stories (lib + `ome-newsroom`)
├── ome-recorder   market recorder into PostgreSQL and Redis; finds brokers and venues in the registry (lib + `ome-recorder`)
├── ome-client     client library (sessions, background links) + `ome` CLI
├── ui/ome-ui      egui kit: bundled Open Sans, dark/light theme, tables, section strips, widgets, toasts, candle chart
├── ui/ome-ui-data data layer for the apps: broker link thread and the market/account/news store
├── ui/apps/ome-retail   OME Trader, the retail client's desktop platform
└── ome-agents     agent runner: runs the strategies simctl assigns (HFT, DMM, quant, pension, prop, retail personas)
config/   topology.conf (services for omectl) · world.conf (companies, demo accounts) · roster.conf (agent groups)
          profiles.example.conf (copy to profiles.conf, yours, not in git: liquidity profiles per symbol)
scripts/  demo.sh (build + omectl up) · stop.sh (omectl down) · db.sh · llm.sh
docker-compose.yml   PostgreSQL 16 + Redis 7 for the recorder, Ollama (profile llm) for the newsroom
docker-compose.gpu.yml   GPU reservation for Ollama
```

Each service is a library with a thin binary. That lets the end-to-end tests start a real exchange and real brokers in-process on random ports.

## Account Types (at providers)

| Role | Provider | Per-order limit | Notes |
|---|---|---|---|
| `retail` | retail broker | 1,000 shares / 100k notional | |
| `quant` | prime broker | 1,000,000 / 100M | institutional quant funds |
| `prop` | prime broker | 100,000 / 20M | prop trading desks |
| `issuer` | prime broker | n/a | company accounts that receive IPO shares and proceeds |
| `hft` | DMA provider | 50,000 / 10M | institutional HFT, sponsored market access |
| `lp` | liquidity provider | 100,000 / 20M | market-making desks |
| `admin` | any provider | cannot trade | creates accounts, sets balances |

Exchange members are `broker`, `liquidity-provider` or `admin` (exchange operations).

## Order Lifecycle

1. An agent sends `NewOrder` to its provider.
2. The provider checks the role's limits, the instrument status and the price band, and confirms buying power. It then records an `OrderAccepted` fact, reserves the cash or shares, and forwards the order to the exchange. During continuous trading, market orders are sent as **IOC limits a collar past the touch** (2% beyond the best ask for a buy, below the best bid for a sell; `--collar-bps`), so one order cannot sweep a thin book to the price-band edge, and a fill can never exceed what was reserved. In auctions they join at the band edge, as before. The broker paces what it sends to the exchange below the exchange's per-session message rate (`--exchange-rate`, default 4,500/s against the exchange's 5,000/s, `--session-rate`): a burst waits in the broker's queue instead of being refused and lost.
3. The exchange matches and sends **sequenced** execution reports to every member involved.
4. Each provider settles its own client's side (refunding price improvement, releasing reservations on cancel or expiry) and forwards a client-facing report.

## Protocol

SBE (schema 7, version 2) with SOFH framing on every connection. All integers are little-endian; prices are `u64` ticks (0.01).

| Family | Direction | Messages |
|---|---|---|
| client | agent → provider | Logon, NewOrder, Cancel, CancelAll, Quote, SetMmp, Conditional, Oco, Bracket, CancelConditional, AlgoOrder, CancelAlgo, Subscribe, QueryAccount, QueryInstruments; admin: CreateAccount, SetBalance, SetAccountStatus, SetRestricted, SetLossLimit, QueryAccounts |
| member | provider → exchange, clearing, venues, newsroom | Logon (with `last_seq`), Apply (before logon), NewOrder (account ref + time in force), Cancel, CancelAll, Quote, SetMmp, TradeReport, Subscribe, Wire (contributor → newsroom); ops: CreateMember, QueryApplications, Decide, ListInstrument, FileProspectus, Halt, Resume, QueryMembers |
| simulation | operator, runner → simctl | Logon, Command, Accelerate, QueryStatus, QueryWorld, Inject, Record, Replay, Groups (runner reports) |
| registry | service, omectl CLI → registry | Announce (health, dependencies, metrics), Query, Control |
| server | both directions back | LogonAck, MemberLogonAck, ReplayComplete, Execution, Allocation, Account, InstrumentList, Instrument, Book, Trade, Candles, AccountList, MemberList, Applications, WorldStatus, SimAssign, SimRetire, Services, ConditionalUpdate, AlgoUpdate, ContractNote, Ack, Reject, Heartbeat |

Client and member templates never overlap; sending one family's message to the other service fails to decode (tested).

## Fees and Taxes

Every fill produces a **contract note** with an itemised breakdown, sent to the account that traded.

**Exchange fees** (per share, charged to members; a negative fee is a rebate):

| Member | Maker (rests in the book) | Taker (removes liquidity) |
|---|---|---|
| broker | −0.15¢ rebate | 0.30¢ |
| liquidity provider | −0.25¢ rebate | 0.30¢ |

**Broker charges** (on the fill's notional; providers pass exchange fees through to the client):

| Charge | Applies to | Rate |
|---|---|---|
| brokerage | retail | 0.03%, capped at 20.00 per order (across partial fills) |
| | quant, prop | 0.005% |
| | HFT | 0.0005% |
| | issuer (IPO underwriting) | 2% |
| | LP desks | none |
| transaction tax | sells | 0.1% |
| stamp duty | buys | 0.015% |
| regulatory fee | all fills | 0.0001% |
| GST | brokerage + regulatory fee + exchange fee (rebates are not taxed) | 18% |

Buy orders reserve the notional **plus an upper bound of their charges**, so a client can never end up unable to pay for a fill; the unused part is released when the order completes. The charges are computed when the exchange report arrives and stored in the broker's fact journal, so a later change to the fee schedule never rewrites past balances on replay.

```bash
ome broker --addr 127.0.0.1:7101 --user alice --secret alice-pw buy ACME mkt 10
# contract note: buy 10 ACME @ 24.88 | brokerage 0.07 exchange 0.03 tax 0.00 stamp 0.04 reg 0.00 GST 0.02 | total 0.16 | net -248.96
ome broker --addr 127.0.0.1:7101 fees        # broker admin: revenue and taxes collected
ome exchange members                         # net exchange fees per member
```

## Order Types

All order types below are matched natively by the exchange; brokers decide which account types may use them.

| Option | Meaning | Who may use it |
|---|---|---|
| `gtc` | good till cancelled (default for limits) | everyone |
| `day` | expires at the close | everyone except issuers |
| `ioc` | fill what you can now, cancel the rest (default for market orders) | everyone except issuers |
| `fok` | fill completely now or not at all; the book is untouched if it can't | everyone except issuers |
| `gtd:<seconds>` | good till a date; expired by a journaled exchange command | everyone except issuers |
| `auction` | only for the current auction (pre-open or closing), then expires | everyone except issuers |
| `post` | post-only: rejected if it would take liquidity | quant, prop, HFT, LP desks |
| `show:<qty>` | iceberg: shows `<qty>`; each refill goes to the back of the queue; hidden quantity is not in published depth but does join auctions | quant, prop, HFT, LP desks |
| `stp:keep\|new\|both` | self-trade prevention: cancel the resting order (default), the incoming order, or both | quant, prop, HFT, LP desks |

```bash
ome broker --addr 127.0.0.1:7102 --user fund --secret fund-pw buy ACME 24.00 2000 show:50
ome broker --addr 127.0.0.1:7102 --user fund --secret fund-pw buy ACME 26.50 5 post
# rejected: post-only order would take liquidity
```

## Broker-Held Orders

Stops, trailing stops, GTT, after-market, OCO and bracket orders live at the broker and are never visible to the exchange until they fire. They trigger on the exchange's last trade price, only during continuous trading and never while a symbol is halted. When one fires, the broker runs its usual checks (funds, shares, band, session) and sends a normal order; the trigger and the released order are journaled as one fact, so a crash cannot lose one without the other. Armed orders lock no funds or shares, so a stop and a target can cover the same shares; a released order that no longer has the shares is rejected and reported.

| Command | What it is | Who |
|---|---|---|
| `stop <SYM> <side> <qty> <trigger> [limit]` | stop-loss (market) or stop-limit; sells fire at or below the trigger, buys at or above | everyone except issuers |
| `trail <SYM> <side> <qty> <distance>` | trailing stop that follows the best price seen since arming | retail, prop |
| `gtt <SYM> <side> <qty> <above\|below> <trigger> <limit>` | good-till-triggered limit order; survives across trading days | retail, prop |
| `amo <SYM> <side> <qty> <price\|mkt>` | after-market order, accepted while the market is closed and released when it opens | retail, prop |
| `oco <SYM> <side> <qty> <target> <stop> [stop-limit]` | one-cancels-other exit: whichever leg fires cancels the other | retail, prop |
| `bracket <SYM> <side> <qty> <price\|mkt> <target> <stop>` | entry order whose fills arm an OCO exit for the filled quantity; later fills grow the exit | retail, prop |
| `conditionals` / `cancel-cond <id>` | list armed orders; cancelling one leg of an OCO cancels both | |

```bash
ome broker --addr 127.0.0.1:7101 --user alice --secret alice-pw bracket ACME buy 5 mkt 27.00 23.50
# entry #695 filled buy filled 5/5
# cond #5 armed sell 5 ACME when last >= 27.00 (bracket exit armed)
# cond #6 armed sell 5 ACME when last <= 23.50 (bracket exit armed)
```

## Off-Exchange Execution

Brokers run under a market regime: `--regime us` (the default) lets retail and prime brokers **internalize**, and `--regime india` sends every order to the exchange, as SEBI requires. Set it for the demo with `OME_REGIME=india`.

The internalizer only crosses **marketable** orders, so resting liquidity always reaches the lit book. A client order that would trade immediately at the exchange (a market order, or a limit at or through the best price) is held until the broker's next 50 ms crossing batch. Held buys and sells from different accounts are matched first-come first-served at the **exchange midpoint**: both clients get price improvement over the quote, and neither pays an exchange fee. Brokerage and taxes still apply. Whatever does not cross is sent on to the exchange at once. Held orders are journaled, so a crash resumes the batch instead of losing them.

Every internal cross is reported to the exchange with a `TradeReport` and printed on the public tape flagged with its venue (`◆ ... (internal print)` in `watch`). Prints update last price, volume and candles everywhere, and the exchange rejects any print outside the circuit limits or while the symbol is halted. `ome broker fees` shows the broker's routing statistics: orders routed to the exchange, internal crosses and internalized shares.

### Wholesaler and dark pool

Two off-exchange venues run as their own service, `ome-venue`, each an exchange member with its own journal. Brokers connect to them with the member protocol, so routing has the same sequencing, replay and gap detection as the exchange link.

- **Wholesaler** (`--kind wholesaler`, port 7301, member `wholesale-co`). The retail broker sends marketable retail orders that did not cross internally to the wholesaler before the exchange. The wholesaler takes about 80% of that flow (`--accept-pct`) and sends the rest on to the lit market. It fills each order it takes completely from its own inventory at the exchange quote improved by 20% of the spread, and pays the broker **payment for order flow** (0.2 tick per share by default, kept by the broker, not passed to the client). It declines orders that are too large, would breach its inventory limit, or have a limit it cannot meet; the broker then routes the order to the exchange at once. It hedges inventory past 500 shares on the exchange and flattens everything in the last seconds before the close.
- **Dark pool** (`--kind ats`, port 7302, member `ats-co`). Quant and prop accounts at the prime broker send non-displayed midpoint orders with `ome broker dark <SYM> <side> <qty> [limit] [min:<qty>]`. Orders match at the exchange midpoint, first-come first-served, only between different accounts and only for at least each order's minimum quantity. They expire at the close.

Every venue fill is printed on the tape with its venue and is cleared T+1 between the two members, like an exchange trade. `ome venue --addr <addr> status` shows fills, routing members, inventory and PFOF; `ome broker fees` gives the broker's 606-style routing report:

```
order routing (606):
  exchange       198 orders
  internal       39 crosses, 359 shares
  wholesaler     205 orders, 2489 shares, payment for order flow 4.14
  dark pool      0 shares
```

Under `--regime india` brokers start without venue links and refuse dark orders.

### Market statistics and designated market makers

The exchange publishes `MarketStats` for every symbol a member subscribes to, refreshed whenever it trades: open, high, low, last, VWAP, trade count, turnover and **volume by venue** (exchange, internal crosses, wholesaler, dark pool). Off-exchange prints count, so consolidated volume and VWAP describe the whole market. Brokers forward the stats to their clients. Stats reset with the trading day.

```bash
ome exchange stats ACME            # or: ome broker ... stats ACME
# ACME open 25.00 high 25.00 low 24.98 last 25.00 vwap 24.99
# volume 6350 in 70 trades, turnover 158746.36
#   exchange          5600   88.2%
#   internal            63    1.0%
#   wholesaler         687   10.8%
```

**Limit up-limit down.** On top of the 10% price band and the 30% circuit breaker, a trade more than `--luld-bps` (default 5%) away from the average price of the last `--luld-window-ms` of trading pauses the symbol for `--luld-pause-ms` (default 15 s). The window defaults to 5 minutes, needs at least 5 trades and resets each trading day.
- During the pause the symbol is `Paused`, not halted: orders are collected without matching, as in pre-open, and brokers accept them. Stops, algos and internal crosses wait.
- When the pause ends, a reopening auction uncrosses the collected orders at one price. That price becomes the new reference, and the day's reference stays unchanged.
- `--luld-bps 0` turns it off.

**IPO close.** An IPO offering stays on the book through its first full trading day. At the next pre-open the exchange cancels whatever is unsold and the shares stay with the issuer, so no wall of unsold stock pins the price at the listing price.

`ome exchange vol [SYMBOL]` reports what the market actually did: the typical and largest 1-minute move and the range over the exchange's recent candles. It only observes and never feeds back into the market.

Exchange ops can name a **designated market maker** per symbol (`ome exchange dmm ACME lp-co`, `none` to clear). The DMM earns a better maker rebate in that symbol (0.35 tick per share instead of 0.25). In return it carries a quoting obligation: once a second during continuous trading, the exchange checks that the member shows a bid and an ask, at least 100 shares each summed over its accounts, no more than 2% apart. `ome exchange obligations` shows each DMM's current state and compliance rate. The demo designates `lp-co` for every listing.

## Algorithmic Orders

Quant and prop accounts at a prime broker can hand the broker a large **parent order** and let its algo engine work it through child orders. Children are ordinary orders with every broker check (funds, shares, band, risk controls) and only one is live at a time. The parent itself reserves nothing and is never visible to the exchange. Each child is journaled together with its link to the parent, so after a crash the parent resumes where it stopped.

| Command | Strategy | Children |
|---|---|---|
| `twap <SYM> <side> <qty> <seconds> <slices> [limit]` | time-weighted: equal slices, one per interval | IOC at the limit, or protected market |
| `vwap <SYM> <side> <qty> <seconds> [limit]` | volume-weighted: 20 slices along a U-shaped intraday volume curve (heavier at the open and close) | IOC |
| `pov <SYM> <side> <qty> <percent> <seconds> [limit]` | percentage of volume: keeps its fills at `percent` of other participants' traded volume, checked every second | IOC, minimum clip 1% of the parent |
| `slice <SYM> <side> <qty> <clip> <limit>` | broker-side iceberg: rests one clip at the limit, the next after it completes | resting GTC limit |
| `algos` / `cancel-algo <id>` | progress (filled, average price, children); cancelling pulls a resting child | |

Add `follow` to stream progress until the algo finishes. A parent ends **done** when filled, **expired** when its time is up, **rejected** if a child fails a broker check, or **cancelled** by the client or an account suspension. Algos pause while the market is not in continuous trading or the symbol is halted.

```bash
ome broker --addr 127.0.0.1:7102 --user fund --secret fund-pw twap ACME buy 200 10 5 follow
# algo #1 working buy ACME 40/200 filled avg 25.00 in 1 child order(s) [TWAP 5 slices over 10s]
# ...
# algo #1 done buy ACME 200/200 filled avg 25.00 in 5 child order(s) [TWAP 5 slices over 10s] (fully filled)
```

## Broker Risk Controls

Each provider guards its own accounts before anything reaches the exchange. Settings are journaled facts, so they survive a crash; throttling and duplicate checks are live gates that never reach the journal.

| Control | Set by | What it does |
|---|---|---|
| **Account kill switch** | broker admin: `suspend-account <account>` / `reinstate-account` | cancels the account's exchange orders and armed conditionals; every new order, quote or conditional is refused |
| **Restricted list** | broker admin: `restrict <SYMBOL> on\|off` | buys, buy-side quotes and buy conditionals are refused; closing sells stay allowed |
| **Daily loss limit** | broker admin: `loss-limit <account> <amount>` | day P&L (realized at average cost, net of fees, plus open positions marked to the last trade) at or below `-amount` blocks new buys; exits stay allowed |
| **Per-account throttle** | built in | order messages per second (burst): retail 10 (20), issuer 5 (10), prop 200 (400), quant 500 (1,000), HFT and LP 5,000 (10,000) |
| **Duplicate check** | built in | retail, prop and issuer accounts cannot send the same symbol, side, price and quantity twice within one second |

Day P&L resets when the exchange starts a new trading day: carried positions are re-based to the previous close, which the broker journals with the day change. `ome broker accounts` shows each account's day P&L, loss limit and suspension; `balance` shows the trader's own day P&L.

## Member Controls and Market Making

| Control | Who | What it does |
|---|---|---|
| **Kill switch** | exchange ops | `ome exchange suspend <member>` cancels every order of the member and rejects new ones until `reinstate` |
| **Cancel-on-disconnect** | exchange ops | `ome exchange cod <member> on`: when the member's last session drops, the exchange cancels all its orders (a journaled command, so replay agrees) |
| **Message throttle** | exchange | each member session gets 5,000 messages/s with a 10,000 burst; excess messages are rejected before they reach the journal |
| **Two-sided quotes** | HFT and LP desks | `quote <SYMBOL> <bid> <qty> <ask> <qty>` replaces the account's previous quote in that symbol atomically; both sides are post-only |
| **Market maker protection** | HFT and LP desks | `mmp <window-ms> <max-qty>`: if quote fills exceed `max-qty` within the window, the exchange pulls every quote of that account and refuses new ones until protection is reset |

The market-making bots quote this way: they arm protection on start, replace their quote as the price moves, and after a trip wait out a cooldown before resetting.

## Market Sessions and Auctions

The exchange runs a trading day in four phases, either manually or on a repeating schedule set by exchange operations:

| Phase | What happens |
|---|---|
| **pre-open** | orders are collected but **not matched**; the book may cross; subscribers get an indicative auction price, volume and imbalance |
| **open** | the opening auction **uncrosses** at one price, then continuous matching |
| **closing auction** | orders are collected again; the closing uncross sets the closing price |
| **closed** | new orders are rejected (cancels still work); **DAY orders expire**; GTC orders stay |

- **Auction price**: the price that executes the most volume; ties go to the smallest imbalance, then the price closest to the reference. Everything that crosses fills at that single price in time priority; self-matches cancel the newer order.
- During auctions IOC limit orders are rejected, and market orders join as limits at the price-band edge.
- Each new trading day resets the circuit-breaker reference to the previous close.
- Orders carry a time in force: `gtc` (default for limits), `day`, or `ioc` (default for market orders).
- The clearing house can follow the exchange calendar (`--cycle-ms 0`): each pre-open starts a new clearing cycle, so T+1 means the next trading day.
- No schedule means the market is always open.
- The exchange runs its own calendar from `--schedule PRE,OPEN,CLOSING,CLOSED` (seconds, or a preset name) when its journal has none yet; omectl passes `trading-day` from the topology (default `compressed`).
- **Presets**: `real` is a full exchange day (pre-open 15 min, open 6 h 15 min, closing auction 10 min, closed overnight, 24 h in all), `compressed` is a trading day in 10 minutes, `fast` a day a minute. `ome exchange preset <name>` switches at the next pre-open, so the current day keeps its length; `ome sim speed <name>` switches at once.
- **Simulated clock**: the session maps onto exchange hours (pre-open 09:00, open 09:15-15:30, closing auction to 15:40, then the night), with Monday to Friday by trading day. `ome exchange status` and the trading app's status strip show it, e.g. `Wed 12:01 · day 3`. Changing it later is an operator request (`ome exchange schedule`, or `ome sim speed` from the simulation master), journaled like any command.

```bash
ome exchange status                         # market open · trading day 2 · next change in 8s
ome exchange schedule 5 45 5 5              # pre-open 5s, open 45s, closing auction 5s, closed 5s, from now
ome exchange preset compressed              # switch the day length at the next pre-open; `preset` lists them, `... now` applies at once
ome exchange session close                  # manual control; `schedule off` to stop the schedule
ome broker --addr 127.0.0.1:7101 --user alice --secret alice-pw buy ACME 23.00 3 day
```

## Clearing and Settlement

The clearing house (`ome-clearing`) is an exchange member of type `clearing`. It runs the market calendar and settles trades between members, like a central counterparty (CCP) plus a securities depository (CSD).

- **Trade-day cycles.** Clearing opens a new cycle every `--cycle-ms` (60 s by default, 30 s in the demo) and tells the exchange, which stamps every fill with the current cycle.
- **Drop copy.** Every trade (buyer and seller member, accounts, fees) and every IPO allocation is sent to clearing on its own sequenced feed, with the same replay-on-reconnect as members.
- **T+1 delivery versus payment.** Trades of cycle *T* settle when cycle *T+2* opens. Obligations are netted per member: net shares move between depository positions, and net cash (including exchange fees) moves between collateral accounts. Delivering shares a member doesn't hold is recorded as a **fail** and reported.
- **Collateral and margin.** Members deposit collateral; the margin requirement is 10% of unsettled gross notional. A withdrawal that would leave collateral below the requirement is refused.
- **Credit limits.** Clearing pushes each member's credit limit (collateral ÷ margin rate) to the exchange. The exchange tracks exposure (open order notional plus unsettled traded notional) and rejects orders that would exceed it. Exposure drops when clearing reports a cycle settled.
- **Settlement reports.** Each member receives a sequenced report per settled cycle. Providers keep each client's fills as **unsettled** cash and shares until the report for their cycle arrives (trade-date accounting: buying power is not reduced). Balances show both.
- **Conservation.** Members' collateral plus the clearing house's fee pool always equals total deposits (verified live to the cent).

```bash
ome clearing status                                   # cycles, collateral, margin, unsettled, credit, fails
ome clearing register retail-co <secret>              # make an exchange member a clearing member
ome clearing deposit retail-co 2000000
ome broker --addr 127.0.0.1:7101 --user alice --secret alice-pw balance
# unsettled cash -250.17
#   ACME   10 (reserved 0, unsettled 10)
```

## Trading Agents and the Simulation Control Plane

`config/roster.conf` lists the firms, a group per line: broker, role, strategy, symbols, cash, starting inventory and strategy parameters. `ome-simctl` owns the roster. Once the IPOs are listed it opens every agent's account through the broker's account desk (journaled, so a restart never opens it twice) and assigns the agents to the connected `ome-agents` runners, spreading them evenly; when a runner leaves, its agents go to the others. A runner only runs what it is given: it buys starting inventory with real orders and trades each firm through its broker with the order types of its tier:

| Tier | Strategies |
|---|---|
| Institutions (prime) | index funds holding free-float market-cap weights and rebalancing with limit orders in the closing auction; domestic mutual funds that receive savers' money every trading day, buy below their own valuation and trim far above it; foreign funds whose equity exposure follows global risk appetite read from macro news; a sovereign fund that buys only deep discounts. They execute with percentage-of-volume parents (5-8% of what others trade), so their flow follows the market's activity |
| HFT (DMA) | Avellaneda-Stoikov market making: spread widens with volatility, quotes skew away from inventory, two-sided quotes with market maker protection, plus resting levels behind the quote (`levels=3`) with growing size |
| Liquidity providers | designated market maker quoting round lots within the 2% obligation, four levels deep; quote size grows with capital and leans against inventory |
| Quant funds (prime) | pairs stat-arb executed with POV algos or in the dark pool; factor model (momentum, reversal, volatility) rebalanced with VWAP parents; value investing from their own valuation of the published fundamentals; news-driven funds trading fresh stories by tone and fair value impact, unwound after a minute |
| Slow money (prime) | pension funds rebalancing to equal weights with participation-capped parents |
| Prop desks (prime) | momentum, breakout and mean reversion entries, each position protected by an OCO take-profit/stop |
| Retail | day traders (protective stops, flat before the close), swing traders (brackets on dips), investors (GTT dip buys, after-market orders, trailing stops), ML personas, value-minded retail, noise traders |

**Value investors** (`strategy=value`) value each company themselves. They take the prospectus (or the first price they saw) as an anchor, then rescale it by forward earnings and add one-off cash effects, using the fundamentals that every company story carries (`fund=SYMBOL:eps:growth:cash`). Each investor draws its own valuation bias (about ±4%) and reads news 2 to 30 seconds late. It rests a bid below and an offer above its own value, sized by capital, and refreshes them when its view moves or every 20 seconds. Nobody is told a "true" price: prices form from these disagreeing valuations and the order flow. `ome sim fair` remains an explicit operator override for scenarios.

The demo starts close to balance: each IPO covers everyone's starting inventory plus 10%, and value funds keep half their capital in cash so that someone can buy a dip.

**ML retail personas** (`strategy=ml-retail`, groups `ml-cautious` and `ml-bold`) each train their own online logistic regression on the live tape: features are the 5, 20 and 100-tick returns, volatility, z-score, news sentiment and the spread, and the label is whether the price is higher a horizon later. They stay out of the market until the model has seen enough samples, then trade the edge of the predicted probability over the model's base rate: a strong edge enters with a bracket, a moderate one with a GTT dip buy, holdings get a trailing stop, and a negative edge exits at market (often filled by the wholesaler). Every agent draws its own horizon, learning rate and sample subset, so agents watching the same tape learn different models. `style=cautious|balanced|bold` sets thresholds and size; `enter`, `strong`, `exit`, `horizon`, `lr`, `diversity` and others can be changed live, and the model is kept across changes. Each agent's status line shows samples seen, hit rate, base rate and its strongest feature. In a live run the models learned short-term mean reversion (a negative weight on the 5-tick return), as expected in a market anchored by value investors.

`ome-simctl` (port 7400) is the simulation master; workers never see it as anything but a customer, issuer or operator. Operators use `ome sim ...`, runners follow it with `--simctl`, and every command is journaled, so a restarted simctl or a reconnecting runner gets the current settings back. Settings sent before a group has agents on a runner still apply when its first agent arrives. Funding a group is done by simctl through the brokers' account desks.

```bash
ome sim status                        # groups, running/target, paused, fills, equity, parameters, fair values, scenarios
ome sim pause noise                   # cancel the group's open orders and stop it (resume noise | pause *)
ome sim count swing 25                # scale a group; new accounts are opened automatically
ome sim param hft-vega gamma=0.6      # live parameter change (rebuilt, except ML personas, which keep their model)
ome sim fund daytrader 1000           # add cash to every account in a group
ome sim fair ZEN 85.00 2              # operator override: value investors use this value (and drift in bps per minute) instead of their own
ome sim crash ACME 15                 # flash crash: panic selling, liquidity withdraws, value buyers wait, then recover
ome sim news NOVA -8                  # news shock, published by the newsroom as a story (-8% to earnings and fair value)
ome sim drought 30 | volatility 30    # market-wide liquidity drought or activity burst
ome sim speed 3 30 3 3                # time acceleration: trading day phase lengths in seconds
ome sim record start crash-drill      # record operator commands from now on, with a tape mark per symbol
ome sim record stop crash-drill       # close the recording (tape marked again)
ome sim recordings                    # saved recordings with their tape marks
ome sim recording crash-drill         # timeline: baseline settings, then each step with its offset
ome sim replay crash-drill 2          # re-issue the recording at twice the speed (replay stop cancels)
```

A recording starts with the settings in effect at that moment (group settings, fair values, active scenarios) as its baseline, then holds each operator command and speed change with its offset. The tape marks (last price and day volume per symbol at both ends, plus wall-clock times) tie it to the trades in PostgreSQL. Recordings live in the simctl journal, so they survive restarts. A replay sends the baseline first and then each step with its original spacing, scaled by the speed; replayed commands are journaled like any other command. A replay that is running when simctl restarts is not resumed.

In a 6-minute fresh demo in phase 5b, prices stayed on the published fundamentals (ACME 24.48 vs 24.44, NOVA 11.69 vs 12.34, ZEN 80.00 vs 76.68). The typical 1-minute move (about one simulated trading day) was 2-4%, the range over 5 minutes was 6-12%, and there were 4 limit up-limit down pauses. The first version of these changes, before the starting balance was fixed, still moved ACME about 10% a minute with a 65% range. In the first 6-minute run under omectl (phase 5c, with an injected lawsuit, a -8% NOVA shock and a broker killed and restarted) the typical 1-minute move was 7.5% for ACME, 6.3% for NOVA and 1.6% for ZEN; this is measured only, and the deep calibrated market of phase 5d works on it.

## The World, the Wire and the News

The world lives in `ome-simctl`. The newsroom only reports on it.

**The world** (`ome-simctl`, companies in `config/world.conf`) holds the economy (policy rate, inflation, growth, risk appetite) and every company's fundamentals (sector, listing price, shares, revenue, net margin, growth, sensitivity to the economy). It follows the exchange's trading calendar and decides what happens:

| Event | When | Effect |
|---|---|---|
| prospectus | when the world starts | financial baseline: revenue, margin, EPS, growth, shares, price; simctl files it with the exchange |
| earnings | pre-open, every `--quarter-days` trading days, staggered by company | new revenue, margin and EPS against the analyst consensus; the surprise sets the tone |
| inflation (CPI), GDP | pre-open on a calendar | actual vs expected moves risk appetite |
| central bank decision | pre-open on a calendar | hikes against inflation, cuts for weak growth, sometimes a surprise |
| company events | during the session, about `--news-per-day` per company per trading day (default 1; the open session is measured from the exchange calendar) | contract wins, launches, lawsuits, recalls, executive departures, buybacks, guidance changes, analyst ratings, takeover rumours |
| sector events | during the session | regulation, demand booms, supply shocks, subsidies for every company in the sector |

Effects are sized against the company's own numbers:
- **Contract wins** add a one-off profit of 0.2-0.8% of annual revenue.
- **Lawsuits and recalls** cost 0.25-2.5% of annual revenue once.
- **Guidance changes** move growth by 0.4-1.5 points.
- **Sector news** moves margins by 0.5-2%.
- **Analyst ratings, rumours and buybacks** carry sentiment only.

Growth and margins revert toward the company's normal level at each earnings report. The world's value estimate is the listing price scaled by forward earnings (growth counts once) and a re-rating from rates and risk appetite, plus one-off cash per share. A unit test runs 2,000 events with quarterly earnings: the median event moves value under 0.6% and nine in ten under 2%, and over 33 quarters value follows the company's growth instead of random-walking.

**Events invented by the local model.** With `--llm`, most company and sector events are invented by Ollama rather than drawn from the catalogue. The prompt gives the company's numbers, its last eight events (so it does not repeat itself), the economy, a good, bad, mixed or minor lean and one of sixteen angles (customers, suppliers, workforce, a plant, cyber security, pricing, a recall, financing ...). The model replies with a headline, a body, a sentiment and three economic effects (growth, relative margin, one-off profit or cost). simctl checks every reply before it can move any value: the scope is valid and a company event names the company; the text has no figures; the tone does not contradict the effect or the wording; effects lie within realistic bounds (growth ±1 point, margin ±2%, one-off -2% to +0.8% of revenue; far outside is refused, slightly large is trimmed); the value effect is at most 3 bps per point of sentiment. When Ollama is down or every reply is refused, the catalogue takes over, so the world never goes quiet.

**The wire.** Every event is journaled in simctl and sent to the newsroom as a sequenced wire item (kind, company, sector, sentiment, the estimated effect on value, the facts, and the resulting fundamentals `fund=SYMBOL:eps_e4:growth_bps:cash_e2`). The newsroom tells the contributor at logon which event it expects next, so nothing is lost or published twice when either side restarts.

**The newsroom** (`ome-newsroom`, port 7500) is a news vendor with no market model of its own. Contributors log in with `--source NAME=SECRET` (simctl is `world`); the desk writes each event up (templates, or the local LLM) and publishes it to subscribers in order. Brokers subscribe with `--news` (they apply, or are registered with `ome news register`), resume after the last story they saw, cache recent stories and pass them to clients that ask (`ome broker ... news`). Everything is journaled, so a killed newsroom restarts with the same story numbers.

### LLM writer

The world decides every fact (what happened, the numbers, the tone, the effect on value); the newsroom's local LLM only writes the words. `scripts/llm.sh up` starts Ollama in Docker (`docker-compose.yml` profile `llm`, plus `docker-compose.gpu.yml` when Docker has the NVIDIA runtime) and pulls `qwen2.5:7b` (`OME_LLM_MODEL` picks another); omectl passes `--llm` to the newsroom and simctl when Ollama answers (`writer=auto|llm|templates`).

Each story is journaled at once with template text and handed to a writer thread, which asks the model for a JSON headline and body. The prompt gives the draft, the numbers that appear in it and the tone; the reply is rejected if it drops or changes a number, contradicts the tone ("exceeding expectations" in a miss), or is too short or long. Accepted copy is journaled as a `Rewritten` fact. Stories leave the newsroom strictly in order: a story waits for its rewrite at most `--llm-timeout-ms` (8 s) plus half a second, then goes out with the template text. Events the model invented keep their own text. If Ollama is down, the writer backs off for 10 seconds at a time and every story uses its template, so the feed never stalls. `ome news status` shows the writer: model, stories written, average latency, fallbacks, timeouts and the last problem.

On the RTX 5070 Ti, `qwen2.5:1.5b` ran fully on the GPU at about 0.2-0.3 s per story and wrote all of the first 23 stories of a test run with no fallbacks, for example "Zen Biotech Falls Short as Quarterly Profit Disappoints: Quarter 1 results: Zen Biotech earned 0.61 a share, down 5.89% from expectations of 0.65, on revenue of $58,632."

How the market uses the news:
- **Agents read it through their broker.** Every agent subscribes to its broker's news feed; a story about a symbol, its sector (learned from company stories) or the economy adds to that symbol's mood, weighted by relevance and fading with a 45-second half-life. The mood drives retail panic selling, value-investor caution, noise-trader tilt and the ML personas' sentiment feature, together with simctl scenarios.
- **News-driven funds** (`strategy=news`, group `news-fund`) trade stories under 10 seconds old: tone plus fair-value impact sets direction and size, they cross the spread with a limit order and unwind the position after `hold_ms`.
- **Listings need a prospectus.** With `--require-prospectus` (on by default under omectl) the exchange refuses an IPO until a prospectus has been filed for the symbol; simctl files each company's prospectus before it lists the IPO. `ome exchange ipo FOO ...` for a company nobody filed for fails with "no prospectus has been filed for FOO".
- **Simulation shocks are world events.** `ome sim news SYM pct` scales the company's earnings in the world, so the move reaches value, the wire, the feed and the agents the same way as any other event. Recordings keep the shock and replays make it happen again.

```bash
ome sim world                         # economy, companies: value, EPS vs next estimate, margin, growth, next report day; seeding progress
ome sim event lawsuit ACME            # make something happen now (earnings, cpi, rates, gdp or any catalogue event)
ome news status                       # stories, wire events received, subscribers, contributors, writer
ome news list 20 ACME                 # recent stories about ACME, its sector and the economy
ome news read 8                       # one story in full with its numbers
ome news watch                        # follow the feed live
ome broker --addr 127.0.0.1:7101 --user alice --secret alice-pw news 5
```

In a live run under omectl, `ome sim event lawsuit ACME` became world event #4 (-32 bps), `ome sim news NOVA -8` became #5 ("Unexpected setback for Nova Energy", -800 bps), and both reached alice's broker feed within a second. After `kill -9` of the retail broker, omectl restarted it within a second; it replayed its journal and its 56 agent sessions reconnected.

## Calibration, Measurement and Capacity

**Liquidity profiles.** `config/profiles.conf` is yours and stays out of git (`config/profiles.example.conf` shows the format). One line per symbol: shares outstanding, starting price, free float, average daily volume, typical spread, typical daily move and depth within 1% of mid. simctl sizes each company from the first three (the IPO offers the free float; EPS follows the price so the company keeps its valuation multiple). The rest are only compared with what the market did, never fed back.

**Measurement.** `ome exchange vol [SYMBOL]` reports, per symbol: typical and largest 1-minute move and the recent range; the median wick share of 1-minute candles (how much of the range is wicks); the quoted spread; resting depth within 1% and 2% of mid; today's volume, trades, range and limit up-limit down pauses (now part of the exchange's market statistics). With a profile it prints the profile's numbers underneath.

**The starting market** (`config/world.conf`, `config/roster.conf`) is about ten times the earlier demo: each company's free float is worth about 18.6M (ACME 742,500 of 1.35M shares, NOVA 1.49M of 3.3M, ZEN 234,000 of 390,000), held by 86 firms with about 96M of capital. Institutions hold most of it: two index funds (about 27% of the float), three domestic mutual funds with daily inflows, two foreign funds, a sovereign fund and two pensions. The prime broker posts 30M of collateral, because it clears the institutions and underwrites the IPOs.

**Measured** (fresh market, `compressed` days, templates, middle of trading day 1, after the IPO day): prices within 3% of the world's values (ACME 23.86 vs 24.51, NOVA 12.50 vs 12.32, ZEN 79.28 vs 78.30); typical 1-minute move 2.3-2.6% and 5-minute range 5.5-7%; spreads 1-8 bps; depth within 1% of mid 0.6-1.5M; no limit up-limit down pauses (phase 5c: ACME 7.5% a minute, 14 pauses in 6 minutes). Still above a real stock at this time scale (a 1-minute bar here is about 40 real minutes), and turnover is far higher than real (NOVA traded about half its float in a day), because most agents act on wall-clock time, not per trading day.

**Capacity** (`ome-load`, Intel Core Ultra 9 275HX, release build, journal on):

| Path | Throughput | Latency to first report |
|---|---|---|
| members straight to the exchange (8 sessions) | about 24,000-25,000 orders/s, the same with the journal off | 1.2 ms p50, 3.9 ms p99 at low queue depth |
| clients through one DMA broker (8 accounts) | about 4,500-6,000 orders/s, paced to the broker's one exchange session | 2 ms p50 unloaded |
| market data from one broker while it trades | about 180,000 messages/s to 200 subscribers (900 each) | |

The matching thread is the limit, not the disk. One broker is capped by its exchange session (5,000 messages/s by default, `--session-rate`), which the broker now respects by pacing instead of losing orders. The full demo generates about 130 orders/s, so one machine has room for a market about 100 times busier before the exchange saturates; the next limits are per-broker sessions, then the single matching thread (symbol sharding, planned).

```bash
cargo run --release -p ome-bench --bin ome-load -- exchange --sessions 8 --seconds 10
cargo run --release -p ome-bench --bin ome-load -- broker --sessions 8 --window 16 --subscribers 200
```

## Desktop Apps

The desktop apps are written in pure Rust with [egui](https://github.com/emilk/egui) (eframe, egui_extras tables and egui_dock workspaces). There is one app per party. They share `ome-ui`, which handles the look: theme, bundled font, tables, collapsible sections, widgets, toasts and the candle chart. They also share `ome-ui-data`, which handles connections and state and draws nothing. Open Sans (regular and bold, Apache-2.0, licence file in `ome-ui/fonts`) is compiled into every app, so nothing depends on installed fonts.

The look follows professional broker platforms such as thinkorswim:
- regular-weight labels with bold values, on a pure black background;
- grey strips for collapsible sections and gadget headers, and thin row lines, with almost no outlines;
- main tabs whose gadgets can be docked side by side, and a sidebar of gadgets;
- solid green and orange cells for ticks.

A light theme is available too. A unit test checks text contrast (at least 4.5:1) in both themes.

```bash
scripts/demo.sh --fresh                    # with the recorder (on when Docker runs), brokers serve chart history from PostgreSQL
cargo run --release -p ome-retail          # log in as alice / alice-pw at 127.0.0.1:7101
cargo run --release -p ome-retail -- --user alice --secret alice-pw --interval 60   # scripted start, 1m candles
```

**OME Trader (`ome-retail`)** is the retail client's platform:

- **Login**: login ID and password (with Show), "Remember login ID", and the broker address under Connection. A refused login says why. The broker address and, if asked, the login ID are remembered in `~/.config/ome/retail.conf`; the password never is.
- **Status strip** and **balances strip**: connection and data state, date and bold UTC time, the account box, the market phase with a countdown, then Stock BP, Net Liq, Cash, Unsettled, Day P/L and P/L Open. On the right are Shortcuts, Setup and Log out.
- **Workspaces**: Monitor, Trade, Charts and MarketWatch each hold a dock of gadgets.
  - Drag a gadget's tab to stack it, put it side by side, or split the area. The `+` on a tab bar adds a gadget there, and `×` closes one.
  - Trade opens with the chart beside order entry and the ladder. `Layout` also has trading layouts: ladder beside chart; watchlist, chart and ladder; and ladder, chart, Level II and Time & Sales.
  - `Layout` adds any gadget to the current tab, builds a chart grid (1, 2 side by side, or 2 × 2 with different timeframes), detaches a gadget into its own OS window (closing that window docks it back), or resets the tab.
  - Gadgets: Activity and Positions, Account Statement, All Products, Active Trader, Chart, Level II, Time & Sales, Watchlist, Quote Board, News.
- **Symbol links**: every gadget has a colour link (the `S` chip in its header, or right-click its tab). Gadgets with the same colour show the same symbol; the sidebar watchlist drives red.
- **Sidebar** (resizable; ◀ hides it): Account Info, Watchlist, Quote Details and Live News. Add gadgets with `+` in the footer; each gadget's ☰ menu moves it up or down or removes it.
- **Watchlist**: the Last cell is filled green on an uptick and red on a downtick, and Net Chng is filled green or red. Click a row to switch symbol.
- **Monitor**:
  - *Activity and Positions*: Working, Filled and Cancelled Orders, with each row tinted by side. The Position Statement shows overall totals, account status and available dollars, with Square off per position and Square off all.
  - *Account Statement*: Cash Balance, Trade History with every charge, Profits and Losses, and an Account Summary.
  - Every table's gear (⚙) chooses its columns.
- **Trade**:
  - *All Products*: a quote header, the Order Entry row with **Confirm and Send**, and Level II beside Time & Sales.
  - *Active Trader*: the price ladder with Buy MKT, Sell MKT, **Flatten** and Cancel orders.
- **Charts**: 1s to 15m candles (1m and longer load history from PostgreSQL), volume, VWAP, SMA 20 and EMA 50, a crosshair with an OHLCV readout, the previous close as a dashed line, and a "new day" separator at every session start.
  - Drag the time axis to compress or expand bars, and drag the price axis to rescale ("manual" appears).
  - Drag the chart to pan back in time, scroll to zoom, and double-click to reset (double-click the price axis to go back to auto scale).
  - Bars keep their width, with a right margin after the last bar.
- **Order confirmation**: the order priced by the broker's own fee engine (brokerage, exchange fee, taxes, GST, cash effect and buying power after). It can be turned off in Setup; orders and ladder clicks then go out at once.
- **Square off and Flatten**: a confirmation lists each symbol, the quantity, the bid and the estimated proceeds, and how many working orders and armed conditionals are cancelled first. As soon as no sell order still holds the shares, the whole position is sold at market. A symbol that is already being squared off is never sold twice.
- **Setup** (`Ctrl+,`): dark or light theme, text size (90-125%), order confirmation on or off, default quantity and default time in force.
- **Remembered per login**: every tab's dock layout, chart grids, link colours, detached windows, sidebar gadgets and their order, folded sections and hidden columns.

Keys: `B` / `S` review a buy or sell, `Enter` sends the order under review, `Esc` closes dialogs, `↑` `↓` change symbol, `C` recenters the ladder, `Ctrl+1`…`4` switch between Monitor, Trade, Charts and MarketWatch, and `?` or `F1` shows all shortcuts. Right-click a tab for its link colour and detach.

The GUI is tested headless with `egui_kittest`. The tests click and type through the AccessKit tree against a fake broker session and render wgpu snapshots into `crates/ui/apps/ome-retail/tests/snapshots`. They cover:
- login validation and refusal;
- an order reviewed with charges, sent and filled;
- square off of one position and of all positions, including the wait for a working sell to cancel;
- Flatten on the ladder;
- Setup;
- docking, chart grids, adding and detaching gadgets, and the remembered layout;
- link groups;
- sidebar gadgets;
- column choosers;
- the shortcut overlay.

Run `UPDATE_SNAPSHOTS=1 cargo test -p ome-retail` after a visual change and look at the PNGs.

## Storage: PostgreSQL and Redis

The services keep their own journals as the source of truth. `ome-recorder` is a separate observer that copies the market into databases for dashboards, analysis and the future desktop apps. It logs in to the exchange, the clearing house, the venues and the newsroom as ops and to every provider as broker admin; with a registry (`--registry`, set by omectl) it finds brokers and venues there and starts recording a new one as soon as it joins. It reports each database's state and its backlog as health. It subscribes to every listed symbol, polls the slow-moving state once a second, and writes batches every 500 ms. Snapshots are coalesced so only the latest book or account state per key is written.

| Store | Holds | Keys / tables |
|---|---|---|
| **PostgreSQL** (record) | every trade (lit and off-exchange, with venue), instruments, market stats, session changes, exchange members, clearing positions, settled cycles, client accounts per broker, 606 routing stats per broker, venue status and inventory, every news story with its sentiment, impact and numbers | `trades`, `instruments`, `market_stats`, `sessions`, `members`, `clearing_members`, `settlement_cycles`, `accounts`, `broker_routing`, `venues`, `venue_inventory`, `news` |
| **Redis** (live) | last price, full book as JSON, market stats, a capped trade stream per symbol, a pub/sub trade channel, market phase, a news stream and channel | `ome:last:SYM`, `ome:book:SYM`, `ome:stats:SYM`, `ome:trades:SYM` (stream), `ome:trades` (channel), `ome:news` (stream and channel), `ome:market`, `ome:instrument:SYM`, `ome:symbols` |

Prices and cash are stored as integer ticks (0.01), like everywhere else in the system. If a database goes away, the recorder keeps a bounded backlog (200,000 records) and writes it when the database is back. Restarting PostgreSQL in the middle of a session loses nothing.

```bash
scripts/db.sh psql -c "select venue, count(*), sum(qty) from trades group by venue"
scripts/db.sh redis GET ome:book:ACME
scripts/db.sh redis SUBSCRIBE ome:trades
```

## Reliability

- **Clearing house**: an event-sourced journal of facts (registrations, deposits, cleared trades, cycles); settlement is recomputed deterministically on replay, including each member's report feed.
- **Exchange**: a write-ahead journal of commands. Replay is deterministic and rebuilds books, halts, order ids **and each member's sequenced feed**.
- **Member recovery**: every execution report and IPO allocation sent to a member carries a per-member sequence number. A reconnecting provider sends `last_seq`, and the exchange resends everything after it, followed by `ReplayComplete`. Duplicates are ignored by sequence number. If the requested history is older than what the exchange kept, the gap is reported.
- **Lost orders**: if a provider recorded an order but crashed before the exchange received it, the order is still pending after `ReplayComplete`. It is then abandoned and its reservation released.
- **Providers**: an event-sourced journal of **facts** (account opened, order accepted, exchange report, allocation), not commands. Provider decisions read live market data, so replaying the original commands could decide differently; replaying facts always rebuilds the same state.
- **Verified by crash tests**: with the full stack running, `kill -9` the retail broker and then the exchange; after each restart all account balances are identical (no lost or double settlements), and trading resumes on its own.
- **Supervision**: under omectl a crashed service is restarted automatically (1 s, then doubling up to 30 s); the simulation master keeps its world, seeding progress and recordings in its own journal, and wire events are resent from the newsroom's last received sequence.

## IPO Flow

Today a listing is a single ops command that offers the whole issue at one price (a filed prospectus is required when the exchange runs with `--require-prospectus`). In the demo, simctl does these steps for every company in `config/world.conf` once the brokers are members: it files the prospectus, opens the issuer account at the underwriter, lists the IPO and names the designated market maker, journaling each step so a restart never repeats one. A full primary market (issuer onboarding, listing-desk approval, book-building with fund blocking, pricing, allotment with refunds, listing auction) is planned; see the roadmap.

```
ome broker --addr <prime> create-account acme_ir <secret> issuer 0      →  account 3
ome exchange ipo ACME 25.00 20000 prime-co 3 Acme Robotics          (needs a filed prospectus with --require-prospectus)
```

The exchange sends the underwriter (`prime-co`) an `Allocation` for account 3, then places the offering as a sell order owned by that account. The prime broker credits and reserves the shares, and proceeds settle into the issuer's account as investors buy. In the demo seed, market makers, HFT desks and retail accounts get their opening inventory by **buying in the IPO**; no shares are created out of thin air.

## Testing

```bash
cargo test --workspace     # 300 tests
```

Includes protocol round-trips and fuzzing, exchange matching, sequencing, gap detection and journal replay, provider risk and settlement, fact-journal replay, and **end-to-end tests** in which a real exchange and real brokers talk over TCP: an investor buys IPO shares through a retail broker while the issuer sits at a prime broker, and an HFT account can only be opened at a DMA provider, a retail stop-loss fires on a real exchange trade and sells into the bid, and a fund's TWAP fills through three child orders. A clearing end-to-end test runs exchange, clearing house and two brokers: an IPO fill shows as unsettled, settles one cycle later, collateral pays for it, and withdrawing collateral makes the exchange reject orders on the credit limit. A world end-to-end test runs exchange, newsroom, three brokers and simctl: simctl files the prospectuses, lists both IPOs, names the DMM, opens the demo and agent accounts, assigns the agents to a runner, and its events (prospectuses, calendar news, an injected lawsuit, a shock) reach a retail client through the broker's feed; after a restart nothing is seeded twice. An onboarding test starts a broker that is not a member: it applies, exchange operations approve it, and it comes online.

## Roadmap

Done:
- **1** market structure: exchange, providers, clearing, sessions and auctions, exchange order types and member controls, broker-held orders, risk controls, algos, off-exchange execution, PostgreSQL/Redis recording
- **2** tiered agents (HFT, DMM, quant, pension, prop, rule-based and ML retail), simulation control plane with scenarios and record/replay
- **3** newsroom: fundamentals, economy, earnings and news feed, news-driven agents, prospectus-gated listings, recorded news, local-LLM writer
- **4** desktop prototype: UI kit, data layer, retail app with login and chart history (GPUI, replaced in 5a)
- **5a** egui platform in the thinkorswim style: docked gadget workspaces, detachable windows, link colours, customisable sidebar and columns, draggable chart axes, setup, square off, headless GUI tests
- **5b** market realism: economics-sized news, agents valuing companies themselves (no injected fair value), capital- and inventory-aware market makers, a balanced starting market, limit up-limit down pauses, a volatility report, day separators on charts
- **5c** architecture realignment: workers and masters (the world model, seeding and the agent roster move to simctl; the newsroom becomes a news vendor fed by a sequenced wire), membership applications instead of scripted registration, a service registry with health, dependencies and metrics from every service, discovery by name, and omectl (topology, preflight, start order, supervision, admissions) replacing the bash scripts
- **5d** deep calibrated market: a 10x larger market held mostly by institutions (index, domestic and foreign funds, a sovereign fund, pensions), multi-level market makers, market order collars and broker pacing, liquidity profiles, a measurement report, session presets with a simulated clock, backing services under omectl, a capacity load test

Planned (open decisions first: IPO allotment rules, admin identity, warm-up history, short selling):
- **5e** strategy lab: a strategy SDK, backtests with the same interface, cost, impact and capacity reports
- **5f** information model and operator event desk: consensus and surprises, gradual information diffusion, scheduled macro and corporate calendars, institutional flow commands
- **5g** index, ETF and HFT strategies, latency classes and co-location; then a durable fast journal with an event bus
- **5h-5i** market data v3 (TimescaleDB, server-side candles, warm-up, indicators, drawings, markers) and the retail OMS
- **6** primary market: issuer portal and organisation onboarding, prospectus filing and listing-desk review, book-building with fund blocking and live subscription, pricing, allotment with lottery and refunds, listing-day auction, agents bidding, news coverage
- **7** back office with web consoles: named admin users with roles, maker-checker and audit trail; broker desk; exchange ops console and clearing desk; surveillance
- **8** institutional terminal, short selling and margin; **9** futures and options; **10** deployment (containers, Helm, TLS, sharing, more omectl backends)

## License

MIT
