use crate::fees::FeeSchedule;
use crate::instrument::{Halt, Instrument};
use crate::members::Members;
use ome_core::price::{Price, format_price};
use ome_core::risk::{MaxQuantity, PositiveQuantity, PriceBand, RiskContext, RiskEngine};
use ome_core::symbol::Symbol;
use ome_core::types::{
    AccountId, Order, OrderId, OrderKind, OrderRequest, Qty, Side, TimeInForce, Timestamp, Trade,
};
use ome_net::auth::SecretHash;
use ome_protocol::{
    Allocation, ExecutionReport, InstrumentInfo, MemberId, MemberInfo, MemberKind, OrderStatus,
    ServerMessage, TradeEvent,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;

const ACCOUNT_BITS: u32 = 40;

#[derive(Debug, Clone)]
pub struct ExchangeConfig {
    pub band_bps: u64,
    pub circuit_bps: u64,
    pub halt_cooldown_ms: Timestamp,
    pub max_order_qty: Qty,
    pub candle_interval: Timestamp,
    pub candle_capacity: usize,
    pub tape_capacity: usize,
    pub depth_levels: usize,
    pub feed_capacity: usize,
    pub fees: FeeSchedule,
}

impl Default for ExchangeConfig {
    fn default() -> Self {
        Self {
            band_bps: 1_000,
            circuit_bps: 3_000,
            halt_cooldown_ms: 30_000,
            max_order_qty: 10_000_000,
            candle_interval: 1_000,
            candle_capacity: 600,
            tape_capacity: 200,
            depth_levels: 12,
            feed_capacity: 100_000,
            fees: FeeSchedule::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    CreateMember {
        kind: MemberKind,
        name: String,
        secret: SecretHash,
    },
    ListInstrument {
        symbol: Symbol,
        name: String,
        ipo_price: Price,
        shares: Qty,
        underwriter: MemberId,
        issuer_account: AccountId,
    },
    Halt {
        symbol: Symbol,
        reason: String,
    },
    Resume {
        symbol: Symbol,
    },
    NewOrder {
        member: MemberId,
        member_order_id: u64,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        tif: TimeInForce,
        qty: Qty,
    },
    Cancel {
        member: MemberId,
        order_id: OrderId,
    },
    CancelAll {
        member: MemberId,
        account: Option<AccountId>,
        symbol: Option<Symbol>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    MemberCreated(MemberId),
    ToMember {
        member: MemberId,
        message: ServerMessage,
    },
    Trade(TradeEvent),
    BookChanged(Symbol),
    InstrumentChanged(Symbol),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeError {
    DuplicateMember(String),
    UnknownMember(MemberId),
    NotATradingMember(MemberId),
    InvalidAccount(AccountId),
    UnknownSymbol(Symbol),
    DuplicateSymbol(Symbol),
    InvalidListing(&'static str),
    AlreadyHalted(Symbol),
    NotHalted(Symbol),
    UnknownOrder(OrderId),
    NotOwner(OrderId),
}

impl fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExchangeError::DuplicateMember(name) => write!(f, "member '{name}' already exists"),
            ExchangeError::UnknownMember(id) => write!(f, "unknown member {id}"),
            ExchangeError::NotATradingMember(id) => {
                write!(f, "member {id} is not a trading member")
            }
            ExchangeError::InvalidAccount(id) => write!(f, "account reference {id} out of range"),
            ExchangeError::UnknownSymbol(s) => write!(f, "unknown symbol {s}"),
            ExchangeError::DuplicateSymbol(s) => write!(f, "symbol {s} is already listed"),
            ExchangeError::InvalidListing(why) => write!(f, "invalid listing: {why}"),
            ExchangeError::AlreadyHalted(s) => write!(f, "{s} is already halted"),
            ExchangeError::NotHalted(s) => write!(f, "{s} is not halted"),
            ExchangeError::UnknownOrder(id) => write!(f, "order #{id} is not open"),
            ExchangeError::NotOwner(id) => write!(f, "order #{id} belongs to another member"),
        }
    }
}

struct OrderMeta {
    member: MemberId,
    account: AccountId,
    member_order_id: u64,
    symbol: Symbol,
    side: Side,
    price: Price,
    qty: Qty,
    filled: Qty,
}

#[derive(Default)]
struct MemberFeed {
    next_seq: u64,
    retained: VecDeque<ServerMessage>,
}

pub struct Replay {
    pub messages: Vec<ServerMessage>,
    pub next_seq: u64,
    pub gap: bool,
}

pub struct Exchange {
    config: ExchangeConfig,
    members: Members,
    instruments: BTreeMap<Symbol, Instrument>,
    orders: HashMap<OrderId, OrderMeta>,
    feeds: HashMap<MemberId, MemberFeed>,
    member_fees: HashMap<MemberId, i64>,
    risk: RiskEngine,
    next_order_id: OrderId,
    next_trade_id: u64,
    next_allocation_id: u64,
}

fn owner_key(member: MemberId, account: AccountId) -> u64 {
    (member << ACCOUNT_BITS) | account
}

impl Exchange {
    pub fn new(config: ExchangeConfig) -> Self {
        let risk = RiskEngine::new()
            .with(PositiveQuantity)
            .with(MaxQuantity(config.max_order_qty))
            .with(PriceBand {
                bps: config.band_bps,
            });
        Self {
            config,
            members: Members::default(),
            instruments: BTreeMap::new(),
            orders: HashMap::new(),
            feeds: HashMap::new(),
            member_fees: HashMap::new(),
            risk,
            next_order_id: 1,
            next_trade_id: 1,
            next_allocation_id: 1,
        }
    }

    pub fn config(&self) -> &ExchangeConfig {
        &self.config
    }

    pub fn members(&self) -> &Members {
        &self.members
    }

    pub fn instrument(&self, symbol: Symbol) -> Option<&Instrument> {
        self.instruments.get(&symbol)
    }

    pub fn execute(
        &mut self,
        command: &Command,
        now: Timestamp,
    ) -> Result<Vec<Event>, ExchangeError> {
        match command {
            Command::CreateMember { kind, name, secret } => self
                .members
                .admit(name, *kind, *secret)
                .map(|id| vec![Event::MemberCreated(id)])
                .ok_or_else(|| ExchangeError::DuplicateMember(name.clone())),
            Command::ListInstrument {
                symbol,
                name,
                ipo_price,
                shares,
                underwriter,
                issuer_account,
            } => self.list_instrument(
                *symbol,
                name,
                *ipo_price,
                *shares,
                *underwriter,
                *issuer_account,
                now,
            ),
            Command::Halt { symbol, reason } => self.halt(*symbol, reason.clone(), None),
            Command::Resume { symbol } => self.resume(*symbol),
            Command::NewOrder {
                member,
                member_order_id,
                account,
                symbol,
                side,
                kind,
                tif,
                qty,
            } => self.new_order(
                *member,
                *member_order_id,
                *account,
                *symbol,
                *side,
                *kind,
                *tif,
                *qty,
                now,
            ),
            Command::Cancel { member, order_id } => self.cancel(*member, *order_id, now),
            Command::CancelAll {
                member,
                account,
                symbol,
            } => Ok(self.cancel_all(*member, *account, *symbol, now)),
        }
    }

    pub fn due_commands(&self, now: Timestamp) -> Vec<Command> {
        self.instruments
            .values()
            .filter(|i| {
                i.halt
                    .as_ref()
                    .and_then(|h| h.auto_resume_at)
                    .is_some_and(|at| at <= now)
            })
            .map(|i| Command::Resume { symbol: i.symbol })
            .collect()
    }

    pub fn replay(&self, member: MemberId, after_seq: u64) -> Replay {
        let Some(feed) = self.feeds.get(&member) else {
            return Replay {
                messages: Vec::new(),
                next_seq: 1,
                gap: false,
            };
        };
        let oldest = feed.next_seq - feed.retained.len() as u64;
        Replay {
            messages: feed
                .retained
                .iter()
                .skip(after_seq.saturating_sub(oldest - 1) as usize)
                .cloned()
                .collect(),
            next_seq: feed.next_seq,
            gap: after_seq.saturating_add(1) < oldest,
        }
    }

    pub fn roll_candles(&mut self, now: Timestamp) {
        for instrument in self.instruments.values_mut() {
            instrument.candles.roll_to(now);
        }
    }

    pub fn instrument_info(&self, symbol: Symbol) -> Option<InstrumentInfo> {
        self.instruments
            .get(&symbol)
            .map(|i| i.info(self.config.band_bps, self.config.circuit_bps))
    }

    pub fn instrument_list(&self) -> Vec<InstrumentInfo> {
        self.instruments
            .values()
            .map(|i| i.info(self.config.band_bps, self.config.circuit_bps))
            .collect()
    }

    pub fn member_list(&self, connected: impl Fn(MemberId) -> bool) -> Vec<MemberInfo> {
        let mut open: HashMap<MemberId, u32> = HashMap::new();
        for meta in self.orders.values() {
            *open.entry(meta.member).or_default() += 1;
        }
        self.members
            .iter()
            .map(|m| MemberInfo {
                member_id: m.id,
                kind: m.kind,
                connected: connected(m.id),
                open_orders: open.get(&m.id).copied().unwrap_or(0),
                net_fees: self.member_fees.get(&m.id).copied().unwrap_or(0),
                name: m.name.clone(),
            })
            .collect()
    }

    fn trading_member(&self, member: MemberId) -> Result<(), ExchangeError> {
        match self.members.get(member) {
            None => Err(ExchangeError::UnknownMember(member)),
            Some(m) if m.kind == MemberKind::Admin => Err(ExchangeError::NotATradingMember(member)),
            Some(_) => Ok(()),
        }
    }

    fn publish(&mut self, member: MemberId, mut message: ServerMessage) -> Event {
        let capacity = self.config.feed_capacity;
        let feed = self.feeds.entry(member).or_insert_with(|| MemberFeed {
            next_seq: 1,
            retained: VecDeque::new(),
        });
        let seq = feed.next_seq;
        feed.next_seq += 1;
        match &mut message {
            ServerMessage::Execution(report) => report.seq = seq,
            ServerMessage::Allocation(allocation) => allocation.seq = seq,
            _ => {}
        }
        if feed.retained.len() == capacity {
            feed.retained.pop_front();
        }
        feed.retained.push_back(message.clone());
        Event::ToMember { member, message }
    }

    fn report(
        &mut self,
        order_id: OrderId,
        status: OrderStatus,
        last: (Price, Qty, i64),
        now: Timestamp,
        reason: &str,
    ) -> Option<Event> {
        let meta = self.orders.get(&order_id)?;
        let member = meta.member;
        let report = ExecutionReport {
            seq: 0,
            order_id,
            client_order_id: meta.member_order_id,
            account: meta.account,
            symbol: meta.symbol,
            side: meta.side,
            status,
            price: meta.price,
            qty: meta.qty,
            filled: meta.filled,
            leaves: if status.is_open() {
                meta.qty - meta.filled
            } else {
                0
            },
            last_price: last.0,
            last_qty: last.1,
            fees: last.2,
            timestamp: now,
            reason: reason.to_string(),
        };
        Some(self.publish(member, ServerMessage::Execution(report)))
    }

    #[allow(clippy::too_many_arguments)]
    fn list_instrument(
        &mut self,
        symbol: Symbol,
        name: &str,
        ipo_price: Price,
        shares: Qty,
        underwriter: MemberId,
        issuer_account: AccountId,
        now: Timestamp,
    ) -> Result<Vec<Event>, ExchangeError> {
        if self.instruments.contains_key(&symbol) {
            return Err(ExchangeError::DuplicateSymbol(symbol));
        }
        if ipo_price == 0 {
            return Err(ExchangeError::InvalidListing("IPO price must be positive"));
        }
        if shares == 0 {
            return Err(ExchangeError::InvalidListing(
                "share count must be positive",
            ));
        }
        self.trading_member(underwriter)?;
        if issuer_account >> ACCOUNT_BITS != 0 {
            return Err(ExchangeError::InvalidAccount(issuer_account));
        }

        let name = if name.trim().is_empty() {
            symbol.to_string()
        } else {
            name.trim().to_string()
        };
        self.instruments.insert(
            symbol,
            Instrument::new(
                symbol,
                name,
                ipo_price,
                self.config.candle_interval,
                self.config.candle_capacity,
                self.config.tape_capacity,
            ),
        );

        let allocation_id = self.next_allocation_id;
        self.next_allocation_id += 1;
        let mut events = vec![
            Event::InstrumentChanged(symbol),
            self.publish(
                underwriter,
                ServerMessage::Allocation(Allocation {
                    seq: 0,
                    allocation_id,
                    symbol,
                    account: issuer_account,
                    shares,
                }),
            ),
        ];
        events.extend(self.place(
            underwriter,
            0,
            issuer_account,
            symbol,
            Side::Sell,
            ipo_price,
            TimeInForce::Gtc,
            shares,
            now,
        ));
        Ok(events)
    }

    fn halt(
        &mut self,
        symbol: Symbol,
        reason: String,
        auto_resume_at: Option<Timestamp>,
    ) -> Result<Vec<Event>, ExchangeError> {
        let instrument = self
            .instruments
            .get_mut(&symbol)
            .ok_or(ExchangeError::UnknownSymbol(symbol))?;
        if instrument.halt.is_some() {
            return Err(ExchangeError::AlreadyHalted(symbol));
        }
        let reason = if reason.trim().is_empty() {
            "halted by exchange".to_string()
        } else {
            reason
        };
        instrument.halt = Some(Halt {
            reason,
            auto_resume_at,
        });
        Ok(vec![Event::InstrumentChanged(symbol)])
    }

    fn resume(&mut self, symbol: Symbol) -> Result<Vec<Event>, ExchangeError> {
        let instrument = self
            .instruments
            .get_mut(&symbol)
            .ok_or(ExchangeError::UnknownSymbol(symbol))?;
        if instrument.halt.take().is_none() {
            return Err(ExchangeError::NotHalted(symbol));
        }
        instrument.reference_price = instrument.last_price.unwrap_or(instrument.reference_price);
        Ok(vec![Event::InstrumentChanged(symbol)])
    }

    #[allow(clippy::too_many_arguments)]
    fn new_order(
        &mut self,
        member: MemberId,
        member_order_id: u64,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        tif: TimeInForce,
        qty: Qty,
        now: Timestamp,
    ) -> Result<Vec<Event>, ExchangeError> {
        self.trading_member(member)?;
        let reject = |this: &mut Self, reason: String| {
            let report = ExecutionReport {
                seq: 0,
                order_id: 0,
                client_order_id: member_order_id,
                account,
                symbol,
                side,
                status: OrderStatus::Rejected,
                price: match kind {
                    OrderKind::Limit(p) => p,
                    OrderKind::Market => 0,
                },
                qty,
                filled: 0,
                leaves: 0,
                last_price: 0,
                last_qty: 0,
                fees: 0,
                timestamp: now,
                reason,
            };
            vec![this.publish(member, ServerMessage::Execution(report))]
        };

        if account >> ACCOUNT_BITS != 0 {
            return Ok(reject(
                self,
                format!("account reference {account} out of range"),
            ));
        }
        let Some(instrument) = self.instruments.get(&symbol) else {
            return Ok(reject(self, format!("unknown symbol {symbol}")));
        };
        if let Some(halt) = &instrument.halt {
            let reason = format!("{symbol} is halted: {}", halt.reason);
            return Ok(reject(self, reason));
        }
        let reference_price = instrument.last_price.unwrap_or(instrument.reference_price);
        let (low, high) = instrument.band(self.config.band_bps);
        if let Err(rejection) = self.risk.check(
            &OrderRequest { side, kind, qty },
            &RiskContext { reference_price },
        ) {
            return Ok(reject(self, rejection.to_string()));
        }

        let (price, tif) = match (kind, side) {
            (OrderKind::Limit(price), _) => (price, tif),
            (OrderKind::Market, Side::Buy) => (high, TimeInForce::Ioc),
            (OrderKind::Market, Side::Sell) => (low, TimeInForce::Ioc),
        };
        Ok(self.place(
            member,
            member_order_id,
            account,
            symbol,
            side,
            price,
            tif,
            qty,
            now,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn place(
        &mut self,
        member: MemberId,
        member_order_id: u64,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        price: Price,
        tif: TimeInForce,
        qty: Qty,
        now: Timestamp,
    ) -> Vec<Event> {
        let order_id = self.next_order_id;
        self.next_order_id += 1;
        self.orders.insert(
            order_id,
            OrderMeta {
                member,
                account,
                member_order_id,
                symbol,
                side,
                price,
                qty,
                filled: 0,
            },
        );
        let mut events: Vec<Event> = self
            .report(order_id, OrderStatus::New, (0, 0, 0), now, "")
            .into_iter()
            .collect();

        let Some(instrument) = self.instruments.get_mut(&symbol) else {
            return events;
        };
        let execution = instrument.book.submit(
            Order {
                id: order_id,
                account: owner_key(member, account),
                side,
                price,
                qty,
                timestamp: now,
            },
            tif,
        );

        for trade in &execution.trades {
            self.fill(symbol, trade, now, &mut events);
        }
        for cancelled in &execution.self_trade_cancelled {
            events.extend(self.close(
                cancelled.id,
                OrderStatus::Cancelled,
                now,
                "self-trade prevention",
            ));
        }
        if execution.expired > 0 {
            events.extend(self.close(
                order_id,
                OrderStatus::Expired,
                now,
                "no liquidity at the limit price",
            ));
        }
        events.push(Event::BookChanged(symbol));
        self.check_circuit(symbol, &mut events, now);
        events
    }

    fn fill(&mut self, symbol: Symbol, trade: &Trade, now: Timestamp, events: &mut Vec<Event>) {
        let trade_id = self.next_trade_id;
        self.next_trade_id += 1;
        if let Some(instrument) = self.instruments.get_mut(&symbol) {
            instrument.record_trade(trade);
        }
        events.push(Event::Trade(TradeEvent {
            symbol,
            trade_id,
            price: trade.price,
            qty: trade.qty,
            aggressor: trade.taker_side,
            timestamp: now,
        }));
        for (order_id, maker) in [(trade.maker_order, true), (trade.taker_order, false)] {
            let Some(meta) = self.orders.get_mut(&order_id) else {
                continue;
            };
            meta.filled += trade.qty;
            let member = meta.member;
            let kind = self
                .members
                .get(member)
                .map_or(MemberKind::Broker, |m| m.kind);
            let fee = self.config.fees.fee(kind, maker, trade.qty);
            *self.member_fees.entry(member).or_default() += fee;
            let Some(meta) = self.orders.get(&order_id) else {
                continue;
            };
            let done = meta.filled == meta.qty;
            let status = if done {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            };
            events.extend(self.report(order_id, status, (trade.price, trade.qty, fee), now, ""));
            if done {
                self.orders.remove(&order_id);
            }
        }
    }

    fn close(
        &mut self,
        order_id: OrderId,
        status: OrderStatus,
        now: Timestamp,
        reason: &str,
    ) -> Option<Event> {
        let event = self.report(order_id, status, (0, 0, 0), now, reason);
        self.orders.remove(&order_id);
        event
    }

    fn cancel(
        &mut self,
        member: MemberId,
        order_id: OrderId,
        now: Timestamp,
    ) -> Result<Vec<Event>, ExchangeError> {
        let meta = self
            .orders
            .get(&order_id)
            .ok_or(ExchangeError::UnknownOrder(order_id))?;
        if meta.member != member {
            return Err(ExchangeError::NotOwner(order_id));
        }
        let symbol = meta.symbol;
        self.instruments
            .get_mut(&symbol)
            .and_then(|i| i.book.cancel(order_id))
            .ok_or(ExchangeError::UnknownOrder(order_id))?;
        let mut events: Vec<Event> = self
            .close(order_id, OrderStatus::Cancelled, now, "cancelled")
            .into_iter()
            .collect();
        events.push(Event::BookChanged(symbol));
        Ok(events)
    }

    fn cancel_all(
        &mut self,
        member: MemberId,
        account: Option<AccountId>,
        symbol: Option<Symbol>,
        now: Timestamp,
    ) -> Vec<Event> {
        let mut ids: Vec<OrderId> = self
            .orders
            .iter()
            .filter(|(_, m)| {
                m.member == member
                    && account.is_none_or(|a| a == m.account)
                    && symbol.is_none_or(|s| s == m.symbol)
            })
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| self.cancel(member, id, now).ok())
            .flatten()
            .collect()
    }

    fn check_circuit(&mut self, symbol: Symbol, events: &mut Vec<Event>, now: Timestamp) {
        let bps = self.config.circuit_bps;
        let Some(instrument) = self.instruments.get(&symbol) else {
            return;
        };
        if instrument.halt.is_some() || !instrument.breached_circuit(bps) {
            return;
        }
        let last = instrument.last_price.unwrap_or(instrument.reference_price);
        let reference = instrument.reference_price;
        let change = (last as f64 / reference as f64 - 1.0) * 100.0;
        let reason = format!(
            "circuit breaker: {change:+.1}% vs reference {}",
            format_price(reference)
        );
        if let Ok(halted) = self.halt(symbol, reason, Some(now + self.config.halt_cooldown_ms)) {
            events.extend(halted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ome_net::auth::hash_secret;
    use ome_protocol::InstrumentStatus;

    pub fn sym(s: &str) -> Symbol {
        Symbol::parse(s).unwrap()
    }

    struct Market {
        ex: Exchange,
        broker: MemberId,
        lp: MemberId,
    }

    fn admit(ex: &mut Exchange, name: &str, kind: MemberKind) -> MemberId {
        match ex
            .execute(
                &Command::CreateMember {
                    kind,
                    name: name.into(),
                    secret: hash_secret(name, "pw"),
                },
                0,
            )
            .unwrap()[0]
        {
            Event::MemberCreated(id) => id,
            _ => unreachable!(),
        }
    }

    fn market_with(config: ExchangeConfig) -> Market {
        let mut ex = Exchange::new(config);
        admit(&mut ex, "ops", MemberKind::Admin);
        let broker = admit(&mut ex, "broker", MemberKind::Broker);
        let lp = admit(&mut ex, "lp", MemberKind::LiquidityProvider);
        ex.execute(
            &Command::ListInstrument {
                symbol: sym("ACME"),
                name: "Acme".into(),
                ipo_price: 25_00,
                shares: 1_000,
                underwriter: broker,
                issuer_account: 9,
            },
            0,
        )
        .unwrap();
        Market { ex, broker, lp }
    }

    fn market() -> Market {
        market_with(ExchangeConfig::default())
    }

    fn order(
        m: &mut Market,
        member: MemberId,
        account: AccountId,
        side: Side,
        price: Price,
        qty: Qty,
    ) -> Vec<Event> {
        m.ex.execute(
            &Command::NewOrder {
                member,
                member_order_id: 77,
                account,
                symbol: sym("ACME"),
                side,
                kind: OrderKind::Limit(price),
                tif: TimeInForce::Gtc,
                qty,
            },
            1,
        )
        .unwrap()
    }

    fn reports_for(events: &[Event], who: MemberId) -> Vec<ExecutionReport> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::ToMember {
                    member,
                    message: ServerMessage::Execution(r),
                } if *member == who => Some(r.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn ipo_allocates_to_underwriter_and_offers_shares() {
        let m = market();
        let replay = m.ex.replay(m.broker, 0);
        assert!(
            matches!(&replay.messages[0], ServerMessage::Allocation(a) if a.account == 9 && a.shares == 1_000 && a.seq == 1)
        );
        assert!(
            matches!(&replay.messages[1], ServerMessage::Execution(r) if r.status == OrderStatus::New && r.client_order_id == 0 && r.seq == 2)
        );
        assert_eq!(
            m.ex.instrument(sym("ACME")).unwrap().book.best_ask(),
            Some(25_00)
        );
    }

    #[test]
    fn fills_are_reported_to_both_members_with_their_own_ids() {
        let mut m = market();
        let lp = m.lp;
        let events = order(&mut m, lp, 4, Side::Buy, 25_00, 10);

        let lp_reports = reports_for(&events, m.lp);
        assert_eq!(lp_reports[0].status, OrderStatus::New);
        assert!(
            lp_reports.iter().any(|r| r.status == OrderStatus::Filled
                && r.client_order_id == 77
                && r.account == 4)
        );
        let broker_reports = reports_for(&events, m.broker);
        assert!(
            broker_reports
                .iter()
                .any(|r| r.status == OrderStatus::PartiallyFilled
                    && r.account == 9
                    && r.leaves == 990)
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Trade(t) if t.qty == 10 && t.price == 25_00))
        );
    }

    #[test]
    fn takers_pay_fees_and_makers_earn_rebates() {
        let mut m = market();
        let lp = m.lp;
        order(&mut m, lp, 4, Side::Sell, 24_90, 100);
        let broker = m.broker;
        let events = order(&mut m, broker, 7, Side::Buy, 24_90, 100);

        let taker = reports_for(&events, m.broker)
            .into_iter()
            .find(|r| r.last_qty == 100)
            .unwrap();
        assert_eq!(taker.fees, 30);
        let maker = reports_for(&events, m.lp)
            .into_iter()
            .find(|r| r.last_qty == 100)
            .unwrap();
        assert_eq!(maker.fees, -25);

        let fees: Vec<(MemberId, i64)> =
            m.ex.member_list(|_| false)
                .iter()
                .map(|i| (i.member_id, i.net_fees))
                .collect();
        assert!(fees.contains(&(m.broker, 30)));
        assert!(fees.contains(&(m.lp, -25)));
    }

    #[test]
    fn sequence_numbers_are_per_member_and_gapless() {
        let mut m = market();
        let lp = m.lp;
        order(&mut m, lp, 4, Side::Buy, 24_00, 1);
        order(&mut m, lp, 4, Side::Buy, 24_10, 1);
        let seqs: Vec<u64> =
            m.ex.replay(m.lp, 0)
                .messages
                .iter()
                .map(|msg| match msg {
                    ServerMessage::Execution(r) => r.seq,
                    _ => 0,
                })
                .collect();
        assert_eq!(seqs, vec![1, 2]);

        let partial = m.ex.replay(m.lp, 1);
        assert_eq!(partial.messages.len(), 1);
        assert_eq!(partial.next_seq, 3);
        assert!(!partial.gap);
    }

    #[test]
    fn replay_reports_a_gap_when_history_was_dropped() {
        let mut m = market_with(ExchangeConfig {
            feed_capacity: 2,
            ..ExchangeConfig::default()
        });
        let lp = m.lp;
        for i in 0..5 {
            order(&mut m, lp, 4, Side::Buy, 24_00 + i, 1);
        }
        let replay = m.ex.replay(m.lp, 0);
        assert!(replay.gap);
        assert_eq!(replay.messages.len(), 2);
        assert!(!m.ex.replay(m.lp, 3).gap);
    }

    #[test]
    fn self_trade_prevention_is_per_beneficial_account() {
        let mut m = market();
        let lp = m.lp;
        order(&mut m, lp, 4, Side::Buy, 24_00, 5);
        let events = order(&mut m, lp, 5, Side::Sell, 24_00, 5);
        assert!(
            events.iter().any(|e| matches!(e, Event::Trade(_))),
            "different accounts of one member may trade"
        );

        order(&mut m, lp, 4, Side::Buy, 24_00, 5);
        let events = order(&mut m, lp, 4, Side::Sell, 24_00, 5);
        assert!(
            reports_for(&events, m.lp)
                .iter()
                .any(|r| r.reason == "self-trade prevention")
        );
    }

    #[test]
    fn members_can_only_cancel_their_own_orders() {
        let mut m = market();
        let lp = m.lp;
        let id = reports_for(&order(&mut m, lp, 4, Side::Buy, 24_00, 5), m.lp)[0].order_id;
        assert_eq!(
            m.ex.execute(
                &Command::Cancel {
                    member: m.broker,
                    order_id: id
                },
                2
            ),
            Err(ExchangeError::NotOwner(id))
        );
        assert!(
            m.ex.execute(
                &Command::Cancel {
                    member: m.lp,
                    order_id: id
                },
                2
            )
            .is_ok()
        );
    }

    #[test]
    fn admin_members_cannot_trade() {
        let mut m = market();
        let result = m.ex.execute(
            &Command::NewOrder {
                member: 1,
                member_order_id: 0,
                account: 0,
                symbol: sym("ACME"),
                side: Side::Buy,
                kind: OrderKind::Market,
                tif: TimeInForce::Ioc,
                qty: 1,
            },
            0,
        );
        assert_eq!(result, Err(ExchangeError::NotATradingMember(1)));
    }

    #[test]
    fn band_and_halt_rejections_are_sequenced_reports() {
        let mut m = market();
        let lp = m.lp;
        let events = order(&mut m, lp, 4, Side::Buy, 99_00, 1);
        assert!(
            reports_for(&events, m.lp)[0]
                .reason
                .contains("outside band")
        );

        m.ex.execute(
            &Command::Halt {
                symbol: sym("ACME"),
                reason: "news".into(),
            },
            2,
        )
        .unwrap();
        let events = order(&mut m, lp, 4, Side::Buy, 25_00, 1);
        assert!(
            reports_for(&events, m.lp)[0]
                .reason
                .contains("halted: news")
        );
    }

    #[test]
    fn ioc_limit_expires_unfilled_remainder() {
        let mut m = market();
        let events =
            m.ex.execute(
                &Command::NewOrder {
                    member: m.lp,
                    member_order_id: 5,
                    account: 4,
                    symbol: sym("ACME"),
                    side: Side::Buy,
                    kind: OrderKind::Limit(25_00),
                    tif: TimeInForce::Ioc,
                    qty: 2_000,
                },
                1,
            )
            .unwrap();
        let last = reports_for(&events, m.lp).pop().unwrap();
        assert_eq!(last.status, OrderStatus::Expired);
        assert_eq!(last.filled, 1_000);
    }

    #[test]
    fn circuit_breaker_halts_and_auto_resumes() {
        let mut m = market_with(ExchangeConfig {
            circuit_bps: 500,
            ..ExchangeConfig::default()
        });
        let (lp, broker) = (m.lp, m.broker);
        order(&mut m, lp, 4, Side::Buy, 25_00, 1_000);
        order(&mut m, lp, 4, Side::Sell, 27_00, 10);
        order(&mut m, broker, 7, Side::Buy, 27_00, 10);

        let info = m.ex.instrument_info(sym("ACME")).unwrap();
        assert_eq!(info.status, InstrumentStatus::Halted);
        assert!(info.halt_reason.contains("+8.0%"));
        assert!(m.ex.due_commands(30_000).is_empty());
        let due = m.ex.due_commands(30_001);
        m.ex.execute(&due[0], 30_001).unwrap();
        assert_eq!(
            m.ex.instrument_info(sym("ACME")).unwrap().reference_price,
            27_00
        );
    }

    #[test]
    fn listing_validation() {
        let mut m = market();
        let list =
            |symbol: &str, price, shares, underwriter, issuer_account| Command::ListInstrument {
                symbol: sym(symbol),
                name: String::new(),
                ipo_price: price,
                shares,
                underwriter,
                issuer_account,
            };
        assert!(m.ex.execute(&list("ACME", 1, 1, 2, 1), 0).is_err());
        assert!(m.ex.execute(&list("NEW", 0, 1, 2, 1), 0).is_err());
        assert!(m.ex.execute(&list("NEW", 1, 0, 2, 1), 0).is_err());
        assert_eq!(
            m.ex.execute(&list("NEW", 1, 1, 1, 1), 0),
            Err(ExchangeError::NotATradingMember(1))
        );
        assert_eq!(
            m.ex.execute(&list("NEW", 1, 1, 2, 1 << 41), 0),
            Err(ExchangeError::InvalidAccount(1 << 41))
        );
        assert!(m.ex.execute(&list("NEW", 1_00, 10, 2, 1), 0).is_ok());
    }
}
