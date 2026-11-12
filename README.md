# OME: Order Matching Exchange

A small but complete stock exchange written in Rust: a matching engine with accounts and fixed balances, IPO listings, admin halts and an automatic circuit breaker, a binary **SBE over TCP** API, an event journal for crash recovery, prebuilt trading bots that generate real traffic, a scriptable CLI, and a terminal UI with separate admin and trader consoles.

Every price, balance and trade you see comes from real orders matching in the engine. Nothing is mocked.

## Quick Start

```bash
scripts/demo.sh --fresh                 # build, start server, seed accounts + 3 IPOs, start 21 bots
scripts/tui.sh admin admin              # admin console
scripts/tui.sh alice alice-pw           # retail trader (in another terminal)
scripts/stop.sh                         # stop bots and server
```

Run `scripts/demo.sh` without `--fresh` to restart from the existing journal. State survives restarts.

## Workspace

```
crates/
├── ome-core       matching engine, risk checks, candles, trade tape (pure library, no I/O)
├── ome-protocol   SBE codec, message definitions, SOFH framing
├── ome-server     exchange daemon: accounts, IPOs, halts, journal, TCP gateway
├── ome-client     client library + `ome` CLI
├── ome-bots       bot runner with prebuilt strategies
└── ome-tui        terminal UI (admin and trader consoles)
scripts/
├── demo.sh        one command: build, start, seed (first run), start bots
├── seed.sh        create users, issuers, IPOs and bot accounts; writes data/bots.conf
├── bots.sh        start ome-bots from a bots file
├── tui.sh         open the terminal UI as a given user
└── stop.sh        stop bots and server
```

Dependencies between crates point one way: `core ← protocol ← client ← {bots, tui}` and `core + protocol ← server`. The server never depends on any client, and the UI has no business logic: it only sends requests and renders what the server publishes.

## Server Architecture

```
TCP clients ──► reader thread per session ──┐
                                            ▼
                               ┌─────────── mpsc ───────────┐
                               │  engine thread (single)    │
                               │  Gateway: logon, roles,    │
                               │    routing, conflation     │
                               │  Journal: write-ahead log  │
                               │  Exchange: ledger, books,  │
                               │    IPOs, halts, breaker    │
                               └──────────┬─────────────────┘
                                          ▼
TCP clients ◄── writer thread per session ◄── bounded queue (slow consumers are dropped)
```

- **One engine thread owns all state**, in the style of the LMAX architecture. There are no locks around books or balances, every request is processed in order, and the results are deterministic.
- **The `Exchange` is pure**: `execute(command, timestamp) -> Result<Vec<Event>>`. No sockets, no clock reads, no randomness. That makes it fully unit-testable and lets the journal replay it exactly.
- **The `Gateway`** turns protocol messages into commands, checks permissions, writes the journal, and routes events: executions to the owning account, trades to subscribers, status changes to everyone.
- **Book updates are conflated**: snapshots are published at most once per 50 ms tick per symbol, however many orders hit the book in between.
- **Fan-out is zero-copy**: each outbound message is encoded once into an `Arc<[u8]>` and shared by every recipient.
- **Back-pressure**: each session has a bounded outbound queue (16k frames). A client that can't keep up is disconnected instead of stalling the exchange.

### Event journal (crash recovery)

Every state-changing command is appended to `data/ome.journal` **before** it executes (write-ahead), along with the timestamp the engine used. On startup the server replays the journal through the same `Exchange::execute`, which rebuilds identical accounts, balances, books, halts and order ids. Journal entries are SBE-encoded and SOFH-framed, the same format as the wire. A torn final entry from a crash is detected and truncated. Read-only queries are not journaled.

## Roles

| Role | Can do | Order limits |
|---|---|---|
| **admin** | create accounts, set cash and share balances, list IPOs, halt/resume symbols, view all accounts | cannot trade or hold balances |
| **institutional** | trade, view own account | 1,000,000 shares and 100,000,000.00 notional per order |
| **retail** | trade, view own account | 1,000 shares and 100,000.00 notional per order |
| **bot** | trade via the API | 10,000 shares and 10,000,000.00 notional per order |

The first admin is created automatically on first start (`--admin-user`, `--admin-secret` or `OME_ADMIN_SECRET`, default `admin`/`admin`). Secrets are stored as SHA-256 hashes salted with the username.

## Market Rules

- **Fixed balances**: a buy reserves `price × qty` cash and a sell reserves the shares, both up front. No margin, no shorting. Reservations are released on cancel or expiry, and price improvement is refunded on fill.
- **Price band**: limit prices must be within ±10% of the last trade. Market orders become IOC limits at the band edge, so they can never fill at an absurd price.
- **Circuit breaker**: if a trade moves the price more than **30% from the reference price**, the symbol halts automatically for 30 seconds and then resumes with the last price as the new reference. The reference is the IPO price, or the last price at the most recent resume.
- **Admin halt**: halts until an admin resumes. While halted, new orders are rejected but cancels are allowed.
- **Self-trade prevention**: if your order would match your own resting order, the resting order is cancelled instead.
- **IPO**: `ipo <SYMBOL> <price> <shares> <issuer> <name>` creates the instrument, credits the shares to the issuer, and places one sell order for all of them at the IPO price.
- **Settlement** is immediate and conserves totals: the sum of all cash and the sum of each symbol's shares never change except through admin balance changes (tested).

## Protocol: SBE over TCP

Messages use **Simple Binary Encoding** (the FIX Trading Community standard used by CME and other exchanges), framed with the **Simple Open Framing Header**.

```
SOFH (6 bytes, big-endian)      SBE message header (8 bytes, little-endian)
┌──────────────┬──────────────┐ ┌─────────────┬────────────┬──────────┬─────────┐
│ frame length │ encoding     │ │ blockLength │ templateId │ schemaId │ version │
│ u32 (incl.)  │ 0x5BE0 (LE)  │ │ u16         │ u16        │ u16 = 7  │ u16 = 1 │
└──────────────┴──────────────┘ └─────────────┴────────────┴──────────┴─────────┘
then: fixed root block · repeating groups (u16 blockLength, u16 count) · var data (u16 length + UTF-8)
```

- All integers are little-endian. Prices are `u64` ticks (1 tick = 0.01). Symbols are `char[8]`, zero-padded.
- Decoders read the fields they know and then jump to `blockLength`, so newer senders can append fields without breaking older readers.
- A `NewOrder` is 42 bytes, compared with roughly 150+ bytes for the same order in FIX tag=value.

| Client → server | ID | | Server → client | ID |
|---|---|---|---|---|
| Logon | 1 | | LogonAck | 100 |
| Heartbeat | 2 | | Heartbeat (every 1 s) | 101 |
| NewOrder | 10 | | Reject (with ref template + reason) | 102 |
| Cancel | 11 | | Ack (admin actions) | 103 |
| CancelAll | 12 | | ExecutionReport | 110 |
| Subscribe / Unsubscribe | 20 / 21 | | AccountSnapshot | 111 |
| QueryAccount / QueryInstruments | 22 / 23 | | InstrumentList / Instrument | 120 / 121 |
| CreateAccount (admin) | 30 | | BookSnapshot (12 levels) | 122 |
| SetBalance (admin) | 31 | | Trade | 123 |
| ListInstrument (admin) | 32 | | CandleSnapshot (1 s bars) | 124 |
| Halt / Resume (admin) | 33 / 34 | | AccountList (admin) | 130 |
| QueryAccounts (admin) | 35 | | | |

Session flow: `Logon` → `LogonAck`, then the server sends the instrument list, your account snapshot and a replay of your open orders. `Subscribe` to a symbol to receive its instrument status, the last 300 candles, recent trades, a book snapshot, and then live trades and conflated book updates.

## Bots

`ome-bots` runs one thread and one TCP session per bot. Each bot is a normal `bot` account with a fixed balance and no special access. A strategy is a pure function `on_tick(&state, &mut rng) -> Vec<Action>`, which makes it unit-testable without a network.

| Strategy | Behaviour |
|---|---|
| `mm`: market maker | quotes both sides around the last trade every 120 ms, skews quotes against its inventory, re-quotes when the price moves, pulls quotes when halted |
| `momentum` | fast/slow EMA crossover on trade prices; buys breakouts and sells breakdowns with market orders |
| `meanrev`: mean reversion | z-score of the price over the last 150 trades; bids dips and offers spikes with limit orders that expire after 6 s |
| `noise`: retail flow | random small market and limit orders, biased by a slowly drifting "sentiment" with rare shocks, which creates trends and occasional breaker trips |

```bash
ome-bots --file data/bots.conf
ome-bots mm:ACME:acme_mm1:bot-pw noise:ACME:acme_noise1:bot-pw
```

Bots reconnect automatically if the server restarts.

## CLI

```bash
ome [--addr HOST:PORT] [--user NAME] [--secret SECRET] <command>      # or OME_ADDR / OME_USER / OME_SECRET

ome accounts
ome create-account carol carol-pw retail 5000
ome set-cash carol 7500.00
ome set-shares carol ACME 100
ome ipo ZEN 12.50 50000 zen_ir Zen Robotics
ome halt ACME pending news
ome resume ACME
ome --user alice --secret alice-pw buy ACME 24.90 10
ome --user alice --secret alice-pw sell ACME mkt 5
ome --user alice --secret alice-pw balance
ome instruments
ome watch ACME
```

Accounts can be given by id or by username.

## Terminal UI

`ome-tui --user NAME --secret SECRET [--symbol SYMBOL]`

- **Everyone**: symbol tabs (◀ ▶ to switch), 1-second candlestick chart with volume or a braille line chart (Tab), 12-level depth ladder with spread in bps, trade tape, halt banner, log, command history (↑ ↓), help (F1 / ?).
- **Traders** also get My Orders (◆ marks your levels in the book), Portfolio (cash, held cash, positions, equity), and the commands `buy <price> <qty>`, `sell mkt <qty>`, `cancel <id>`, `cancel all`, `sym <SYMBOL>`.
- **Admins** get Instruments (status, last, change, volume) and Accounts (refreshed every 2 s) panels, and the commands `create`, `cash`, `shares`, `ipo`, `halt [SYM] [reason]`, `resume [SYM]`.

## Server Options

```
ome-server [--listen 127.0.0.1:7000] [--journal data/ome.journal] [--admin-user admin] [--admin-secret admin]
```

Environment: `OME_LISTEN`, `OME_JOURNAL`, `OME_ADMIN_SECRET`.

## Testing

```bash
cargo test --workspace
```

93 tests, including:
- **core**: matching priority, partial fills, IOC, cancel, self-trade prevention, risk rules, candles, symbols
- **protocol**: round-trips for every message, forward compatibility, truncated and bit-flipped input never panics, framing errors
- **server**: IPO flow, settlement conserves cash and shares, insufficient funds and shares, market order refunds, cancel ownership, role limits, band rejection, manual halt, circuit breaker with auto-resume, journal round-trip, **replay reproduces identical state**, torn-tail recovery, gateway logon, permissions, routing, conflation, slow-consumer disconnect
- **bots**: quoting, inventory skew, halt behaviour, momentum and mean-reversion signals, no selling without shares
- **tui**: command parsing, role enforcement, order and halt handling, full-screen renders of both consoles

## Limitations

- No TLS. Run it on localhost or a trusted network.
- The journal grows without bound. Snapshots and compaction would be the next step.
- One matching thread serves all symbols. Sharding by symbol would scale further.
- No order amend, stop or iceberg orders, or fees.

## License

MIT
