# OME: Order Matching Exchange

A simulated equities market built like the real thing: an **exchange** that only deals with member firms, **brokers and liquidity providers** that hold client accounts and route orders, and **trading agents** that can only reach the market through a provider. Services talk over a binary **SBE over TCP** protocol, every service recovers from crashes through its own journal, and prebuilt bots generate real traffic.

Every price, balance and trade comes from real orders matching in the engine. Nothing is mocked.

## Quick Start

```bash
scripts/demo.sh --fresh       # build; start exchange + 4 providers; seed accounts and 3 IPOs; start 24 bots
scripts/stop.sh
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
├── ome-client     client library (sessions, background links) + `ome` CLI
└── ome-bots       agents: market maker, momentum, mean reversion, noise
scripts/  env.sh · demo.sh · seed.sh · bots.sh · stop.sh
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
| client | agent → provider | Logon, NewOrder, Cancel, CancelAll, Subscribe, QueryAccount, QueryInstruments; admin: CreateAccount, SetBalance, QueryAccounts |
| member | provider → exchange | Logon (with `last_seq`), NewOrder (account ref + time in force), Cancel, CancelAll, Subscribe; ops: CreateMember, ListInstrument, Halt, Resume, QueryMembers |
| server | both directions back | LogonAck, MemberLogonAck, ReplayComplete, Execution, Allocation, Account, InstrumentList, Instrument, Book, Trade, Candles, AccountList, MemberList, Ack, Reject, Heartbeat |

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
ome exchange ipo ACME 25.00 20000 prime-co 3 Acme Robotics
```

The exchange sends the underwriter (`prime-co`) an `Allocation` for account 3, then places the offering as a sell order owned by that account. The prime broker credits and reserves the shares, and proceeds settle into the issuer's account as investors buy. In the demo seed, market makers, HFT desks and retail accounts get their opening inventory by **buying in the IPO**; no shares are created out of thin air.

## Agents

`ome-bots --file data/bots.conf`, where each line is `strategy:SYMBOL:user:secret@broker-host:port`.

| Strategy | Seeded at | Behaviour |
|---|---|---|
| `mm` | lp-co, dma-co | two-sided quotes around the last trade, skewed against inventory, pulled during halts |
| `momentum` | prime-co (quant) | fast/slow EMA crossover with market orders |
| `meanrev` | prime-co (prop) | z-score of recent trades; fades extremes with expiring limit orders |
| `noise` | retail-co | small random orders driven by a drifting sentiment with rare shocks |

## Testing

```bash
cargo test --workspace     # 139 tests
```

Includes protocol round-trips and fuzzing, exchange matching, sequencing, gap detection and journal replay, provider risk and settlement, fact-journal replay, and **end-to-end tests** in which a real exchange and real brokers talk over TCP: an investor buys IPO shares through a retail broker while the issuer sits at a prime broker, and an HFT account can only be opened at a DMA provider. A clearing end-to-end test runs exchange, clearing house and two brokers: an IPO fill shows as unsettled, settles one cycle later, collateral pays for it, and withdrawing collateral makes the exchange reject orders on the credit limit.

## Roadmap

- **1e-ii** member controls: kill switch, cancel-on-disconnect, message throttling, bulk two-sided quotes with market-maker protection
- **1f** broker-held orders (stop, trailing stop, OCO, bracket, GTT) and institutional algorithms (TWAP, VWAP, POV)
- **1g** off-exchange execution: broker internalization, wholesaler with payment for order flow, dark pool
- **1h** PostgreSQL (accounts, trades, settlements) and Redis (market data)
- **2** tiered agents: HFT and quant institutions, prop desks, ML-managed retail accounts, simulation control plane
- **3** local-LLM newsroom: company reports, IPO prospectuses, news and macro events
- **4** GPUI desktop apps, one per party (the terminal UI was retired)

## License

MIT
