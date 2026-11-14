use crate::observation::Observation;
use crate::sink::Sink;
use ome_protocol::InstrumentStatus;
use postgres::{Client, NoTls, Transaction};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS instruments (
    symbol TEXT PRIMARY KEY, name TEXT NOT NULL, status TEXT NOT NULL,
    reference_price BIGINT NOT NULL, last_price BIGINT, low_limit BIGINT NOT NULL,
    high_limit BIGINT NOT NULL, volume BIGINT NOT NULL, halt_reason TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now());
CREATE TABLE IF NOT EXISTS trades (
    trade_id BIGINT PRIMARY KEY, symbol TEXT NOT NULL, price BIGINT NOT NULL, qty BIGINT NOT NULL,
    aggressor TEXT NOT NULL, venue TEXT NOT NULL, traded_at TIMESTAMPTZ NOT NULL);
CREATE INDEX IF NOT EXISTS trades_by_symbol ON trades (symbol, traded_at);
CREATE TABLE IF NOT EXISTS market_stats (
    symbol TEXT PRIMARY KEY, open BIGINT NOT NULL, high BIGINT NOT NULL, low BIGINT NOT NULL,
    last BIGINT NOT NULL, vwap BIGINT NOT NULL, trades BIGINT NOT NULL, turnover BIGINT NOT NULL,
    exchange_volume BIGINT NOT NULL, internal_volume BIGINT NOT NULL,
    wholesaler_volume BIGINT NOT NULL, ats_volume BIGINT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now());
CREATE TABLE IF NOT EXISTS sessions (
    id BIGSERIAL PRIMARY KEY, day BIGINT NOT NULL, phase TEXT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now());
CREATE TABLE IF NOT EXISTS members (
    member_id BIGINT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, connected BOOLEAN NOT NULL,
    suspended BOOLEAN NOT NULL, open_orders INTEGER NOT NULL, net_fees BIGINT NOT NULL,
    credit_limit BIGINT, exposure BIGINT NOT NULL, updated_at TIMESTAMPTZ NOT NULL DEFAULT now());
CREATE TABLE IF NOT EXISTS clearing_members (
    member_id BIGINT PRIMARY KEY, name TEXT NOT NULL, collateral BIGINT NOT NULL,
    margin_required BIGINT NOT NULL, unsettled_gross BIGINT NOT NULL, credit_limit BIGINT,
    fails INTEGER NOT NULL, updated_at TIMESTAMPTZ NOT NULL DEFAULT now());
CREATE TABLE IF NOT EXISTS settlement_cycles (
    cycle BIGINT PRIMARY KEY, fee_pool BIGINT NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now());
CREATE TABLE IF NOT EXISTS accounts (
    broker TEXT NOT NULL, account_id BIGINT NOT NULL, username TEXT NOT NULL, role TEXT NOT NULL,
    cash BIGINT NOT NULL, reserved BIGINT NOT NULL, open_orders INTEGER NOT NULL,
    day_pnl BIGINT NOT NULL, loss_limit BIGINT NOT NULL, suspended BOOLEAN NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), PRIMARY KEY (broker, account_id));
CREATE TABLE IF NOT EXISTS broker_routing (
    broker TEXT PRIMARY KEY, fills BIGINT NOT NULL, charges BIGINT NOT NULL,
    routed_orders BIGINT NOT NULL, internal_crosses BIGINT NOT NULL,
    internalized_shares BIGINT NOT NULL, wholesaler_orders BIGINT NOT NULL,
    wholesaler_shares BIGINT NOT NULL, pfof_earned BIGINT NOT NULL, dark_shares BIGINT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now());
CREATE TABLE IF NOT EXISTS venues (
    venue TEXT PRIMARY KEY, fills BIGINT NOT NULL, declined BIGINT NOT NULL, shares BIGINT NOT NULL,
    pfof_paid BIGINT NOT NULL, cash BIGINT NOT NULL, resting BIGINT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now());
CREATE TABLE IF NOT EXISTS news (
    seq BIGINT PRIMARY KEY, day BIGINT NOT NULL, kind TEXT NOT NULL, symbol TEXT, sector TEXT NOT NULL,
    sentiment INTEGER NOT NULL, impact_bps INTEGER NOT NULL, headline TEXT NOT NULL, body TEXT NOT NULL,
    data TEXT NOT NULL, published_at TIMESTAMPTZ NOT NULL);
CREATE INDEX IF NOT EXISTS news_by_symbol ON news (symbol, published_at);
CREATE TABLE IF NOT EXISTS venue_inventory (
    venue TEXT NOT NULL, symbol TEXT NOT NULL, qty BIGINT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(), PRIMARY KEY (venue, symbol));
";

fn int(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn limit(v: u64) -> Option<i64> {
    i64::try_from(v).ok().filter(|&l| l != i64::MAX)
}

pub struct PostgresSink {
    url: String,
    client: Option<Client>,
}

impl PostgresSink {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            client: None,
        }
    }

    fn connect(&mut self) -> Result<&mut Client, String> {
        if self.client.as_ref().is_none_or(|c| c.is_closed()) {
            let mut client = Client::connect(&self.url, NoTls).map_err(|e| e.to_string())?;
            client.batch_execute(SCHEMA).map_err(|e| e.to_string())?;
            self.client = Some(client);
        }
        Ok(self.client.as_mut().expect("connected above"))
    }
}

impl Sink for PostgresSink {
    fn name(&self) -> &str {
        "postgres"
    }

    fn write(&mut self, batch: &[Observation]) -> Result<(), String> {
        let client = self.connect()?;
        let result = client
            .transaction()
            .and_then(|mut tx| {
                for observation in batch {
                    record(&mut tx, observation)?;
                }
                tx.commit()
            })
            .map_err(|e| e.to_string());
        if result.is_err() {
            self.client = None;
        }
        result
    }
}

fn record(tx: &mut Transaction<'_>, observation: &Observation) -> Result<(), postgres::Error> {
    match observation {
        Observation::Instrument(i) => {
            tx.execute(
                "INSERT INTO instruments (symbol, name, status, reference_price, last_price, low_limit,
                     high_limit, volume, halt_reason)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT (symbol) DO UPDATE SET name = $2, status = $3, reference_price = $4,
                     last_price = $5, low_limit = $6, high_limit = $7, volume = $8, halt_reason = $9,
                     updated_at = now()",
                &[
                    &i.symbol.to_string(),
                    &i.name,
                    &if i.status == InstrumentStatus::Halted { "halted" } else { "trading" },
                    &int(i.reference_price),
                    &i.last_price.map(int),
                    &int(i.low_limit),
                    &int(i.high_limit),
                    &int(i.volume),
                    &i.halt_reason,
                ],
            )?;
        }
        Observation::News(n) => {
            tx.execute(
                "INSERT INTO news (seq, day, kind, symbol, sector, sentiment, impact_bps, headline, body,
                                   data, published_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, to_timestamp($11::double precision / 1000))
                 ON CONFLICT (seq) DO NOTHING",
                &[
                    &int(n.seq),
                    &int(n.day),
                    &n.kind.label(),
                    &n.symbol.map(|s| s.to_string()),
                    &n.sector,
                    &(n.sentiment as i32),
                    &(n.impact_bps.clamp(i32::MIN as i64, i32::MAX as i64) as i32),
                    &n.headline,
                    &n.body,
                    &n.data,
                    &(n.time as f64),
                ],
            )?;
        }
        Observation::Trade(t) => {
            tx.execute(
                "INSERT INTO trades (trade_id, symbol, price, qty, aggressor, venue, traded_at)
                 VALUES ($1, $2, $3, $4, $5, $6, to_timestamp($7::double precision / 1000))
                 ON CONFLICT (trade_id) DO NOTHING",
                &[
                    &int(t.trade_id),
                    &t.symbol.to_string(),
                    &int(t.price),
                    &int(t.qty),
                    &t.aggressor.label(),
                    &t.venue.label(),
                    &(t.timestamp as f64),
                ],
            )?;
        }
        Observation::Stats(s) => {
            tx.execute(
                "INSERT INTO market_stats (symbol, open, high, low, last, vwap, trades, turnover,
                     exchange_volume, internal_volume, wholesaler_volume, ats_volume)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
                 ON CONFLICT (symbol) DO UPDATE SET open = $2, high = $3, low = $4, last = $5,
                     vwap = $6, trades = $7, turnover = $8, exchange_volume = $9,
                     internal_volume = $10, wholesaler_volume = $11, ats_volume = $12,
                     updated_at = now()",
                &[
                    &s.symbol.to_string(),
                    &int(s.open),
                    &int(s.high),
                    &int(s.low),
                    &int(s.last),
                    &int(s.vwap),
                    &int(s.trades),
                    &int(s.turnover),
                    &int(s.exchange_volume),
                    &int(s.internal_volume),
                    &int(s.wholesaler_volume),
                    &int(s.ats_volume),
                ],
            )?;
        }
        Observation::Market(m) => {
            tx.execute(
                "INSERT INTO sessions (day, phase) SELECT $1, $2
                 WHERE NOT EXISTS (SELECT 1 FROM (SELECT day, phase FROM sessions ORDER BY id DESC
                     LIMIT 1) last WHERE last.day = $1 AND last.phase = $2)",
                &[&int(m.day), &m.phase.label()],
            )?;
        }
        Observation::Members(list) => {
            for m in list {
                tx.execute(
                    "INSERT INTO members (member_id, name, kind, connected, suspended, open_orders,
                         net_fees, credit_limit, exposure)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                     ON CONFLICT (member_id) DO UPDATE SET name = $2, kind = $3, connected = $4,
                         suspended = $5, open_orders = $6, net_fees = $7, credit_limit = $8,
                         exposure = $9, updated_at = now()",
                    &[
                        &int(m.member_id),
                        &m.name,
                        &m.kind.label(),
                        &m.connected,
                        &m.suspended,
                        &(m.open_orders as i32),
                        &m.net_fees,
                        &limit(m.credit_limit),
                        &int(m.exposure),
                    ],
                )?;
            }
        }
        Observation::Clearing(c) => {
            for m in &c.members {
                tx.execute(
                    "INSERT INTO clearing_members (member_id, name, collateral, margin_required,
                         unsettled_gross, credit_limit, fails)
                     VALUES ($1, $2, $3, $4, $5, $6, $7)
                     ON CONFLICT (member_id) DO UPDATE SET name = $2, collateral = $3,
                         margin_required = $4, unsettled_gross = $5, credit_limit = $6, fails = $7,
                         updated_at = now()",
                    &[
                        &int(m.member_id),
                        &m.name,
                        &m.collateral,
                        &int(m.margin_required),
                        &int(m.unsettled_gross),
                        &limit(m.credit_limit),
                        &(m.fails as i32),
                    ],
                )?;
            }
            if c.settled_cycle > 0 {
                tx.execute(
                    "INSERT INTO settlement_cycles (cycle, fee_pool) VALUES ($1, $2)
                     ON CONFLICT (cycle) DO NOTHING",
                    &[&int(c.settled_cycle), &c.fee_pool],
                )?;
            }
        }
        Observation::Accounts { broker, list } => {
            for a in list {
                tx.execute(
                    "INSERT INTO accounts (broker, account_id, username, role, cash, reserved,
                         open_orders, day_pnl, loss_limit, suspended)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                     ON CONFLICT (broker, account_id) DO UPDATE SET username = $3, role = $4,
                         cash = $5, reserved = $6, open_orders = $7, day_pnl = $8,
                         loss_limit = $9, suspended = $10, updated_at = now()",
                    &[
                        broker,
                        &int(a.account_id),
                        &a.username,
                        &a.role.label(),
                        &int(a.cash.available),
                        &int(a.cash.reserved),
                        &(a.open_orders as i32),
                        &a.day_pnl,
                        &int(a.loss_limit),
                        &a.suspended,
                    ],
                )?;
            }
        }
        Observation::Routing { broker, fees } => {
            tx.execute(
                "INSERT INTO broker_routing (broker, fills, charges, routed_orders, internal_crosses,
                     internalized_shares, wholesaler_orders, wholesaler_shares, pfof_earned,
                     dark_shares)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                 ON CONFLICT (broker) DO UPDATE SET fills = $2, charges = $3, routed_orders = $4,
                     internal_crosses = $5, internalized_shares = $6, wholesaler_orders = $7,
                     wholesaler_shares = $8, pfof_earned = $9, dark_shares = $10,
                     updated_at = now()",
                &[
                    broker,
                    &int(fees.fills),
                    &fees.charges.total(),
                    &int(fees.routed_orders),
                    &int(fees.internal_crosses),
                    &int(fees.internalized_shares),
                    &int(fees.wholesaler_orders),
                    &int(fees.wholesaler_shares),
                    &fees.pfof_earned,
                    &int(fees.dark_shares),
                ],
            )?;
        }
        Observation::Venue(v) => {
            let venue = v.venue.label();
            tx.execute(
                "INSERT INTO venues (venue, fills, declined, shares, pfof_paid, cash, resting)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (venue) DO UPDATE SET fills = $2, declined = $3, shares = $4,
                     pfof_paid = $5, cash = $6, resting = $7, updated_at = now()",
                &[
                    &venue,
                    &int(v.fills),
                    &int(v.declined),
                    &int(v.shares),
                    &v.pfof_paid,
                    &v.cash,
                    &int(v.resting),
                ],
            )?;
            tx.execute("DELETE FROM venue_inventory WHERE venue = $1", &[&venue])?;
            for (symbol, qty) in &v.inventory {
                tx.execute(
                    "INSERT INTO venue_inventory (venue, symbol, qty) VALUES ($1, $2, $3)",
                    &[&venue, &symbol.to_string(), qty],
                )?;
            }
        }
        Observation::Book(_) => {}
    }
    Ok(())
}
