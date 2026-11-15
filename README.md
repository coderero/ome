# OME: Order Matching Exchange

A simulated equities market built like the real thing: an **exchange** that only deals with member firms, **brokers and liquidity providers** that hold client accounts and route orders, and **trading agents** that can only reach the market through a provider. Services talk over a binary **SBE over TCP** protocol, every service recovers from crashes through its own journal, and prebuilt bots generate real traffic.

Every price, balance and trade comes from real orders matching in the engine. Nothing is mocked.

## Quick Start

```bash
scripts/demo.sh --fresh       # build; start exchange, clearing, simctl, newsroom, 2 venues, 4 providers; seed 3 IPOs; start agents
scripts/stop.sh

scripts/db.sh up              # PostgreSQL + Redis in Docker
OME_RECORD=1 scripts/demo.sh --fresh   # also record the market into them
scripts/llm.sh up             # Ollama with qwen2.5:1.5b (GPU when the NVIDIA runtime is installed); the newsroom then writes with it
```

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
├── ome-protocol   SBE codec + SOFH framing; client, member and server message families
├── ome-store      generic crash-safe journal (write-ahead log of SBE frames)
├── ome-net        TCP session server, outbound back-pressure, auth hashing, logging
├── ome-exchange   exchange service (lib + `ome-exchange` binary)
├── ome-clearing   clearing house (lib + `ome-clearing` binary)
├── ome-broker     provider service with retail / prime / dma / lp profiles (lib + `ome-broker`)
├── ome-venue      off-exchange venues: wholesaler and dark pool (lib + `ome-venue`)
├── ome-simctl     simulation control plane for the artificial participants (lib + `ome-simctl`)
├── ome-newsroom   news vendor: company fundamentals, economy, earnings and news feed (lib + `ome-newsroom`)
├── ome-recorder   market recorder into PostgreSQL and Redis (lib + `ome-recorder`)
├── ome-client     client library (sessions, background links) + `ome` CLI
├── ui/ome-ui-kit  GPUI components: bundled fonts, theme, chart with studies, ladder, inputs, tables, dialogs
├── ui/ome-ui-data data layer for the apps: broker link thread and the market/account/news store
├── ui/apps/ome-retail   OME Trader, the retail client's desktop platform
└── ome-agents     trading firms from config/roster.conf: HFT, DMM, quant, pension, prop, retail personas
scripts/  env.sh · demo.sh · seed.sh · agents.sh · stop.sh · db.sh · llm.sh
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
2. The provider checks the role's limits, the instrument status and the price band, and confirms buying power. It then records an `OrderAccepted` fact, reserves the cash or shares, and forwards the order to the exchange. Market orders are sent as **IOC limits at the band edge**, so a fill can never exceed what was reserved.
3. The exchange matches and sends **sequenced** execution reports to every member involved.
4. Each provider settles its own client's side (refunding price improvement, releasing reservations on cancel or expiry) and forwards a client-facing report.

## Protocol

SBE (schema 7, version 2) with SOFH framing on every connection. All integers are little-endian; prices are `u64` ticks (0.01).

| Family | Direction | Messages |
|---|---|---|
| client | agent → provider | Logon, NewOrder, Cancel, CancelAll, Quote, SetMmp, Conditional, Oco, Bracket, CancelConditional, AlgoOrder, CancelAlgo, Subscribe, QueryAccount, QueryInstruments; admin: CreateAccount, SetBalance, SetAccountStatus, SetRestricted, SetLossLimit, QueryAccounts |
| member | provider → exchange | Logon (with `last_seq`), NewOrder (account ref + time in force), Cancel, CancelAll, Quote, SetMmp, TradeReport, Subscribe; ops: CreateMember, ListInstrument, Halt, Resume, QueryMembers |
| server | both directions back | LogonAck, MemberLogonAck, ReplayComplete, Execution, Allocation, Account, InstrumentList, Instrument, Book, Trade, Candles, AccountList, MemberList, ConditionalUpdate, AlgoUpdate, ContractNote, Ack, Reject, Heartbeat |

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

```bash
ome exchange status                         # market open · trading day 2 · next change in 8s
ome exchange schedule 5 45 5 5              # pre-open 5s, open 45s, closing auction 5s, closed 5s
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

`ome-agents` runs the firms listed in `config/roster.conf` (a group per line: broker, role, strategy, symbols, cash, starting inventory and strategy parameters). It opens their accounts through each broker's admin login, buys starting inventory with real orders, and then trades each firm through its broker with the order types of its tier:

| Tier | Strategies |
|---|---|
| HFT (DMA) | Avellaneda-Stoikov market making: spread widens with volatility, quotes skew away from inventory, two-sided quotes with market maker protection |
| Liquidity providers | designated market maker quoting round lots within the 2% obligation |
| Quant funds (prime) | pairs stat-arb executed with POV algos or in the dark pool; factor model (momentum, reversal, volatility) rebalanced with VWAP parents; value investing around fair value; news-driven funds trading fresh stories by tone and fair value impact, unwound after a minute |
| Slow money (prime) | pension funds rebalancing to target weights with slow VWAP parents |
| Prop desks (prime) | momentum, breakout and mean reversion entries, each position protected by an OCO take-profit/stop |
| Retail | day traders (protective stops, flat before the close), swing traders (brackets on dips), investors (GTT dip buys, after-market orders, trailing stops), ML personas, value-minded retail, noise traders |

**ML retail personas** (`strategy=ml-retail`, groups `ml-cautious` and `ml-bold`) each train their own online logistic regression on the live tape: features are the 5, 20 and 100-tick returns, volatility, z-score, news sentiment and the spread, and the label is whether the price is higher a horizon later. They stay out of the market until the model has seen enough samples, then trade the edge of the predicted probability over the model's base rate: a strong edge enters with a bracket, a moderate one with a GTT dip buy, holdings get a trailing stop, and a negative edge exits at market (often filled by the wholesaler). Every agent draws its own horizon, learning rate and sample subset, so agents watching the same tape learn different models. `style=cautious|balanced|bold` sets thresholds and size; `enter`, `strong`, `exit`, `horizon`, `lr`, `diversity` and others can be changed live, and the model is kept across changes. Each agent's status line shows samples seen, hit rate, base rate and its strongest feature. In a live run the models learned short-term mean reversion (a negative weight on the 5-tick return), as expected in a market anchored by value investors.

`ome-simctl` (port 7400) is the control plane for these artificial participants only; the exchange never sees it. Operators use `ome sim ...`, runners follow it with `--simctl`, and every command is journaled, so a restarted simctl or a reconnecting runner gets the current settings back.

```bash
ome sim status                        # groups, running/target, paused, fills, equity, parameters, fair values, scenarios
ome sim pause noise                   # cancel the group's open orders and stop it (resume noise | pause *)
ome sim count swing 25                # scale a group; new accounts are opened automatically
ome sim param hft-vega gamma=0.6      # live parameter change (rebuilt, except ML personas, which keep their model)
ome sim fund daytrader 1000           # add cash to every account in a group
ome sim fair ZEN 85.00 2              # fair value and drift in bps per minute, which value investors trade around
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

Fair values start at the listing prices (the seed script sets them). In a test run a flash crash took ACME from 24.60 to 23.06 and value buyers brought it back to 25.12 in the next session; a -8% news shock moved NOVA from 12.10 to its new fair value of 11.50 within 15 seconds.

## News, Fundamentals and the Economy

`ome-newsroom` (port 7500) is a news vendor. It follows the companies in `config/companies.conf` (sector, listing price, shares, EPS, net margin, revenue growth, sensitivity to the economy) and runs the economy behind them: policy rate, inflation, growth and risk appetite. It takes the trading calendar from the exchange and publishes:

| Story | When | Effect |
|---|---|---|
| prospectus | when a company is first covered | financial baseline: revenue, margin, EPS, growth, shares, price |
| earnings | pre-open, every `--quarter-days` trading days, staggered by company | new revenue, margin and EPS against the analyst consensus; the surprise sets the tone |
| inflation (CPI), GDP | pre-open on a calendar | actual vs expected moves risk appetite |
| central bank decision | pre-open on a calendar | hikes against inflation, cuts for weak growth, sometimes a surprise |
| company news | during the session (`--news-per-minute`) | contract wins, launches, lawsuits, recalls, executive departures, buybacks, guidance changes, analyst ratings, takeover rumours |
| sector news | during the session | regulation, demand booms, supply shocks, subsidies for every company in the sector |

Each story carries a structured sentiment (-100 to 100), its effect on fair value in bps and its numbers (`eps=0.63 consensus=0.65 ...`) next to the headline and body. Analyst ratings and rumours carry sentiment but do not change fundamentals. Fair value is the listing price scaled by forward earnings and a re-rating from rates, growth and risk appetite; the newsroom keeps ome-simctl's fair values in step, so value investors trade around them. ### LLM writer

The engine decides every fact (what happened, the numbers, the tone, the effect on fair value); a local LLM only writes the words. `scripts/llm.sh up` starts Ollama in Docker (`docker-compose.yml` profile `llm`, plus `docker-compose.gpu.yml` when Docker has the NVIDIA runtime) and pulls `qwen2.5:1.5b`; `scripts/demo.sh` passes `--llm` to the newsroom when Ollama answers (`OME_WRITER=auto|llm|templates`).

Each story is journaled at once with template text and handed to a writer thread, which asks the model for a JSON headline and body. The prompt gives the draft, the numbers that appear in it and the tone; the reply is rejected if it drops or changes a number, contradicts the tone ("exceeding expectations" in a miss), or is too short or long. Accepted copy is journaled as a `Rewritten` fact. Stories leave the newsroom strictly in order: a story waits for its rewrite at most `--llm-timeout-ms` (8 s) plus half a second, then goes out with the template text. If Ollama is down, the writer backs off for 10 seconds at a time and every story uses its template, so the feed never stalls. `ome news status` shows the writer: model, stories written, average latency, fallbacks, timeouts and the last problem.

On the RTX 5070 Ti the model runs fully on the GPU at about 0.2-0.3 s per story. In a test run it wrote all of the first 23 stories with no fallbacks, for example "Zen Biotech Falls Short as Quarterly Profit Disappoints: Quarter 1 results: Zen Biotech earned 0.61 a share, down 5.89% from expectations of 0.65, on revenue of $58,632." With Ollama stopped, two stories went out with their template text without delay, and the writer was back within ten seconds of restarting it.

How the market uses the news:
- **Agents read it through their broker.** Every agent subscribes to its broker's news feed; a story about a symbol, its sector (learned from company stories) or the economy adds to that symbol's mood, weighted by relevance and fading with a 45-second half-life. The mood drives retail panic selling, value-investor caution, noise-trader tilt and the ML personas' sentiment feature, together with simctl scenarios.
- **News-driven funds** (`strategy=news`, group `news-fund`) trade stories under 10 seconds old: tone plus fair-value impact sets direction and size, they cross the spread with a limit order and unwind the position after `hold_ms`.
- **Listings need a prospectus.** With `--require-prospectus` (on in the demo) the exchange refuses an IPO until a prospectus has been filed for the symbol. The newsroom files every prospectus it publishes with the exchange, and files them again whenever it reconnects. `ome exchange ipo FOO ...` for a company the newsroom does not cover fails with "no prospectus has been filed for FOO".
- **Simulation news shocks are stories.** With `--newsroom`, ome-simctl hands `ome sim news SYM pct` to the newsroom as a shock that scales the company's earnings, so the move reaches fair value, the feed and the agents through the same path as any other story (it falls back to the old direct shock if the newsroom is down). Recordings keep the shock and replays publish it again.

In a test run a `-8%` shock on NOVA became story #8 ("Unexpected setback for Nova Energy", -803 bps) and NOVA traded from 12.74 to 11.62 within 20 seconds; the two news funds had 30 fills by then, and the recorder had every story in PostgreSQL and the `ome:news` stream.

Brokers subscribe with `--news` (registered once with `ome news register`), resume after the last story they saw, cache recent stories and pass them to clients that ask (`ome broker ... news`). Everything is journaled, so a killed newsroom restarts with the same economy, fundamentals and story numbers.

```bash
ome news status                       # economy, companies: fair value, EPS vs next estimate, margin, growth, next report day
ome news list 20 ACME                 # recent stories about ACME, its sector and the economy
ome news read 8                       # one story in full with its numbers
ome news watch                        # follow the feed live
ome news inject lawsuit ACME          # publish a story now (earnings, cpi, rates, gdp or any catalogue event)
ome broker --addr 127.0.0.1:7101 --user alice --secret alice-pw news 5
```

In a test run the newsroom published a hot CPI print (-56 bps on fair values), a rate hold, a Zen Biotech takeover rumour and a Zen earnings miss (0.63 vs 0.65, -63 bps); after a kill -9 it came back at the same day and story count, and an injected lawsuit (-199 bps on ACME) reached the brokers' clients with no duplicates.

## Desktop Apps (GPUI)

The desktop apps are built with GPUI (the community fork `gpui-ce`, since Zed's crates.io release no longer builds with current `libc`). There is one app per party, sharing `ome-ui-kit` (visual components, no networking) and `ome-ui-data` (connections and state, no drawing). Fonts are bundled: Inter for text and JetBrains Mono for numbers ship inside `ome-ui-kit/fonts` with their SIL Open Font Licence files and are compiled into every app, so nothing depends on installed fonts. Linux builds need `fontconfig-devel` and `libxkbcommon-x11-devel`.

```bash
OME_RECORD=1 scripts/demo.sh --fresh       # with the recorder, brokers serve chart history from PostgreSQL
cargo run --release -p ome-retail          # log in as alice / alice-pw at 127.0.0.1:7101
cargo run --release -p ome-retail -- --user alice --secret alice-pw --tab charts --interval 60   # scripted start
```

**OME Trader (`ome-retail`)** is the retail client's platform, laid out like a full broker desktop platform:

- **Login**: broker address, login ID and a masked password; a refused login says why, the broker address and login ID are remembered (`~/.config/ome/retail.conf`), the password never is; Log out returns here.
- **Header**: account, net liquidating value, cash, buying power, day P/L, open P/L, unsettled cash, market session with a countdown.
- **Sidebar gadgets**: watchlist (last, net and % change, ticks flash green/red, halts marked), time and sales with the venue of every print, live news.
- **Trade**: quote header (last, change, bid × size, ask × size, volume, open/high/low, VWAP, status); the Active Trader ladder (volume at price, your buy orders, bid size, price, ask size, your sell orders): click a bid or ask size to place a limit there, drag your order to another price to move it or onto itself to cancel it, click a price to load it into the ticket; Level II; order entry (side, typed quantity, market/limit/stop/bracket, typed price with steppers and bid/ask fill, DAY/GTC, bracket target and stop %) with **Confirm and Send**, which opens an order confirmation priced by the broker's own fee engine (brokerage, exchange fee, taxes, GST, cash effect, buying power after); working orders and armed conditionals with Cancel. Confirmation can be switched to auto send on the ladder.
- **Monitor**: account info, position statement (quantity, average trade price, mark, change, P/L open, P/L %, P/L day, value, unsettled) with totals, and activity: working orders and conditions, fills with charges, cancelled and rejected orders with the reason.
- **Charts**: 1s to 15m candles (1m and longer load history from PostgreSQL through the broker), volume, VWAP, SMA 20, EMA 50, crosshair with an OHLCV readout, scroll to zoom.
- **News**: every story with tone and fair-value impact, and a reader with the structured numbers.

Keys: `F1`-`F4` tabs, `↑` `↓` symbol, `B` / `S` open the ticket as a buy or sell, `Enter` confirms, `Esc` cancels, `C` recenters the ladder; letters are ignored while typing in a field.

## Storage: PostgreSQL and Redis

The services keep their own journals as the source of truth. `ome-recorder` is a separate observer that copies the market into databases for dashboards, analysis and the future desktop apps. It logs in to the exchange, the clearing house, the venues and the newsroom as ops and to every provider as broker admin. It subscribes to every listed symbol, polls the slow-moving state once a second, and writes batches every 500 ms. Snapshots are coalesced so only the latest book or account state per key is written.

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

## IPO Flow

```
ome broker --addr <prime> create-account acme_ir <secret> issuer 0      →  account 3
ome exchange ipo ACME 25.00 20000 prime-co 3 Acme Robotics          (needs a filed prospectus with --require-prospectus)
```

The exchange sends the underwriter (`prime-co`) an `Allocation` for account 3, then places the offering as a sell order owned by that account. The prime broker credits and reserves the shares, and proceeds settle into the issuer's account as investors buy. In the demo seed, market makers, HFT desks and retail accounts get their opening inventory by **buying in the IPO**; no shares are created out of thin air.

## Testing

```bash
cargo test --workspace     # 249 tests
```

Includes protocol round-trips and fuzzing, exchange matching, sequencing, gap detection and journal replay, provider risk and settlement, fact-journal replay, and **end-to-end tests** in which a real exchange and real brokers talk over TCP: an investor buys IPO shares through a retail broker while the issuer sits at a prime broker, and an HFT account can only be opened at a DMA provider, a retail stop-loss fires on a real exchange trade and sells into the bid, and a fund's TWAP fills through three child orders. A clearing end-to-end test runs exchange, clearing house and two brokers: an IPO fill shows as unsettled, settles one cycle later, collateral pays for it, and withdrawing collateral makes the exchange reject orders on the credit limit.

## Roadmap

- **1f** ~~broker-held orders, broker risk controls, algorithmic orders (TWAP, VWAP, POV, iceberg slicer)~~ done
- **1g** ~~off-exchange execution: internalization, tape reporting, wholesaler with PFOF, dark pool, market stats, designated market makers~~ done
- **1h** ~~PostgreSQL (accounts, trades, settlements) and Redis (market data)~~ done
- **2** ~~tiered agents (HFT, DMM, quant, pension, prop, rule-based and ML retail), simulation control plane with scenarios and record/replay~~ done
- **3** ~~newsroom: fundamentals, economy, earnings and news feed, news-driven agents, prospectus-gated listings, recorded news, local-LLM writer~~ done
- **4** GPUI desktop apps, one per party: ~~UI kit, data layer, retail platform with login and chart history (4a, 4b)~~ done; institutional terminal, HFT monitor, LP desk (4c); operator apps, simulation control, issuer portal (4d)

## License

MIT
