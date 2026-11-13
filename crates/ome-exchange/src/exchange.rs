use crate::fees::FeeSchedule;
use crate::instrument::{Halt, Instrument};
use crate::members::Members;
use crate::session::{Schedule, Session};
use ome_core::price::{Price, format_price};
use ome_core::risk::{MaxQuantity, PositiveQuantity, PriceBand, RiskContext, RiskEngine};
use ome_core::symbol::Symbol;
use ome_core::types::{
    AccountId, Order, OrderId, OrderKind, OrderRequest, Qty, Side, TimeInForce, Timestamp, Trade,
};
use ome_net::auth::SecretHash;
use ome_net::feed::{Feeds, Replay};
use ome_protocol::{
    Allocation, AuctionInfo, ClearedTrade, ExecutionReport, InstrumentInfo, MarketPhase,
    MarketStatus, MemberId, MemberInfo, MemberKind, OrderStatus, ServerMessage, TradeEvent,
};
use std::collections::{BTreeMap, HashMap};
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
    OpenCycle {
        cycle: u64,
    },
    Settled {
        cycle: u64,
    },
    SetCreditLimit {
        member: MemberId,
        limit: u64,
    },
    SetPhase {
        phase: MarketPhase,
    },
    SetSchedule {
        schedule: Option<Schedule>,
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
    SessionChanged,
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
    tif: TimeInForce,
}

pub struct Exchange {
    config: ExchangeConfig,
    members: Members,
    instruments: BTreeMap<Symbol, Instrument>,
    orders: HashMap<OrderId, OrderMeta>,
    feeds: Feeds,
    member_fees: HashMap<MemberId, i64>,
    credit_limits: HashMap<MemberId, u64>,
    open_notional: HashMap<MemberId, u64>,
    traded: HashMap<MemberId, BTreeMap<u64, u64>>,
    cycle: u64,
    session: Session,
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
        let feeds = Feeds::new(config.feed_capacity);
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
            feeds,
            member_fees: HashMap::new(),
            credit_limits: HashMap::new(),
            open_notional: HashMap::new(),
            traded: HashMap::new(),
            cycle: 0,
            session: Session::default(),
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
            Command::OpenCycle { cycle } => {
                self.cycle = self.cycle.max(*cycle);
                Ok(vec![])
            }
            Command::Settled { cycle } => {
                for by_cycle in self.traded.values_mut() {
                    by_cycle.retain(|c, _| c > cycle);
                }
                Ok(vec![])
            }
            Command::SetCreditLimit { member, limit } => {
                if self.members.get(*member).is_none() {
                    return Err(ExchangeError::UnknownMember(*member));
                }
                self.credit_limits.insert(*member, *limit);
                Ok(vec![])
            }
            Command::SetPhase { phase } => Ok(self.set_phase(*phase, now)),
            Command::SetSchedule { schedule } => {
                self.session.set_schedule(*schedule, now);
                Ok(vec![Event::SessionChanged])
            }
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

    pub fn market_status(&self) -> MarketStatus {
        self.session.status()
    }

    pub fn auction_info(&self, symbol: Symbol) -> Option<AuctionInfo> {
        if !self.session.phase.is_call() {
            return None;
        }
        let instrument = self.instruments.get(&symbol)?;
        let reference = instrument.last_price.unwrap_or(instrument.reference_price);
        let indication = instrument.book.indicative(reference);
        Some(AuctionInfo {
            symbol,
            price: indication.map_or(0, |i| i.price),
            volume: indication.map_or(0, |i| i.volume),
            imbalance: indication.map_or(0, |i| i.imbalance),
        })
    }

    pub fn due_commands(&self, now: Timestamp) -> Vec<Command> {
        let session = self
            .session
            .due(now)
            .map(|phase| Command::SetPhase { phase });
        session
            .into_iter()
            .chain(
                self.instruments
                    .values()
                    .filter(|i| {
                        i.halt
                            .as_ref()
                            .and_then(|h| h.auto_resume_at)
                            .is_some_and(|at| at <= now)
                    })
                    .map(|i| Command::Resume { symbol: i.symbol }),
            )
            .collect()
    }

    fn set_phase(&mut self, phase: MarketPhase, now: Timestamp) -> Vec<Event> {
        let previous = self.session.phase;
        let mut events = Vec::new();
        if previous.is_call() && phase != previous {
            self.uncross_all(now, &mut events);
        }
        if phase == MarketPhase::Closed && previous != MarketPhase::Closed {
            self.expire_day_orders(now, &mut events);
        }
        if phase == MarketPhase::PreOpen
            || (phase == MarketPhase::Open && previous == MarketPhase::Closed)
        {
            for instrument in self.instruments.values_mut() {
                instrument.reference_price =
                    instrument.last_price.unwrap_or(instrument.reference_price);
                events.push(Event::InstrumentChanged(instrument.symbol));
            }
        }
        self.session.enter(phase, now);
        events.push(Event::SessionChanged);
        events
    }

    fn uncross_all(&mut self, now: Timestamp, events: &mut Vec<Event>) {
        let symbols: Vec<Symbol> = self.instruments.keys().copied().collect();
        for symbol in symbols {
            let Some(instrument) = self.instruments.get_mut(&symbol) else {
                continue;
            };
            if instrument.halt.is_some() {
                continue;
            }
            let reference = instrument.last_price.unwrap_or(instrument.reference_price);
            let Some(indication) = instrument.book.indicative(reference) else {
                continue;
            };
            let execution = instrument.book.uncross(indication.price, now);
            for trade in &execution.trades {
                self.fill(symbol, trade, now, events);
            }
            for cancelled in &execution.self_trade_cancelled {
                events.extend(self.close(
                    cancelled.id,
                    OrderStatus::Cancelled,
                    now,
                    "self-trade prevention",
                ));
            }
            events.push(Event::BookChanged(symbol));
            self.check_circuit(symbol, events, now);
        }
    }

    fn expire_day_orders(&mut self, now: Timestamp, events: &mut Vec<Event>) {
        let mut ids: Vec<(OrderId, Symbol)> = self
            .orders
            .iter()
            .filter(|(_, m)| m.tif == TimeInForce::Day)
            .map(|(id, m)| (*id, m.symbol))
            .collect();
        ids.sort_unstable();
        let mut touched = std::collections::BTreeSet::new();
        for (id, symbol) in ids {
            if let Some(instrument) = self.instruments.get_mut(&symbol) {
                instrument.book.cancel(id);
            }
            events.extend(self.close(id, OrderStatus::Expired, now, "end of trading day"));
            touched.insert(symbol);
        }
        events.extend(touched.into_iter().map(Event::BookChanged));
    }

    pub fn replay(&self, member: MemberId, after_seq: u64) -> Replay {
        self.feeds.replay(member, after_seq)
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
                credit_limit: self.credit_limits.get(&m.id).copied().unwrap_or(u64::MAX),
                exposure: self.exposure(m.id),
                name: m.name.clone(),
            })
            .collect()
    }

    pub fn cycle(&self) -> u64 {
        self.cycle
    }

    pub fn exposure(&self, member: MemberId) -> u64 {
        let traded: u64 = self.traded.get(&member).map_or(0, |c| c.values().sum());
        self.open_notional.get(&member).copied().unwrap_or(0) + traded
    }

    fn clearing_members(&self) -> Vec<MemberId> {
        self.members
            .iter()
            .filter(|m| m.kind == MemberKind::Clearing)
            .map(|m| m.id)
            .collect()
    }

    fn trading_member(&self, member: MemberId) -> Result<(), ExchangeError> {
        match self.members.get(member) {
            None => Err(ExchangeError::UnknownMember(member)),
            Some(m) if matches!(m.kind, MemberKind::Admin | MemberKind::Clearing) => {
                Err(ExchangeError::NotATradingMember(member))
            }
            Some(_) => Ok(()),
        }
    }

    fn publish(&mut self, member: MemberId, message: ServerMessage) -> Event {
        let message = self.feeds.publish(member, message);
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
            cycle: self.cycle,
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
        let allocation = Allocation {
            seq: 0,
            allocation_id,
            member: underwriter,
            symbol,
            account: issuer_account,
            shares,
        };
        let mut events = vec![
            Event::InstrumentChanged(symbol),
            self.publish(underwriter, ServerMessage::Allocation(allocation.clone())),
        ];
        for clearing in self.clearing_members() {
            events.push(self.publish(clearing, ServerMessage::Allocation(allocation.clone())));
        }
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
                cycle: this.cycle,
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
        let phase = self.session.phase;
        if phase == MarketPhase::Closed {
            return Ok(reject(self, "market is closed".to_string()));
        }
        if phase.is_call() && tif == TimeInForce::Ioc && kind != OrderKind::Market {
            return Ok(reject(
                self,
                format!("IOC orders are not accepted during the {}", phase.label()),
            ));
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
        let tif = if phase.is_call() && tif == TimeInForce::Ioc {
            TimeInForce::Day
        } else {
            tif
        };
        if let Some(&limit) = self.credit_limits.get(&member) {
            let exposure = self.exposure(member);
            let notional = price.saturating_mul(qty);
            if exposure.saturating_add(notional) > limit {
                return Ok(reject(
                    self,
                    format!(
                        "member credit limit exceeded: exposure {} + order {} > limit {}",
                        format_price(exposure),
                        format_price(notional),
                        format_price(limit)
                    ),
                ));
            }
        }
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
        *self.open_notional.entry(member).or_default() += price.saturating_mul(qty);
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
                tif,
            },
        );
        let mut events: Vec<Event> = self
            .report(order_id, OrderStatus::New, (0, 0, 0), now, "")
            .into_iter()
            .collect();

        let calling = self.session.phase.is_call();
        let Some(instrument) = self.instruments.get_mut(&symbol) else {
            return events;
        };
        let order = Order {
            id: order_id,
            account: owner_key(member, account),
            side,
            price,
            qty,
            timestamp: now,
        };
        if calling {
            instrument.book.rest(order);
            events.push(Event::BookChanged(symbol));
            return events;
        }
        let execution = instrument.book.submit(order, tif);

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
        let mut legs = Vec::with_capacity(2);
        for (order_id, maker) in [(trade.maker_order, true), (trade.taker_order, false)] {
            let Some(meta) = self.orders.get_mut(&order_id) else {
                continue;
            };
            meta.filled += trade.qty;
            let member = meta.member;
            let (side, account, limit_price) = (meta.side, meta.account, meta.price);
            if let Some(open) = self.open_notional.get_mut(&member) {
                *open = open.saturating_sub(limit_price.saturating_mul(trade.qty));
            }
            *self
                .traded
                .entry(member)
                .or_default()
                .entry(self.cycle)
                .or_default() += trade.price.saturating_mul(trade.qty);
            let kind = self
                .members
                .get(member)
                .map_or(MemberKind::Broker, |m| m.kind);
            let fee = self.config.fees.fee(kind, maker, trade.qty);
            *self.member_fees.entry(member).or_default() += fee;
            legs.push((side, member, account, fee));
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
        let leg = |side: Side| legs.iter().find(|l| l.0 == side).copied();
        if let (Some(buy), Some(sell)) = (leg(Side::Buy), leg(Side::Sell)) {
            let cleared = ClearedTrade {
                seq: 0,
                trade_id,
                cycle: self.cycle,
                symbol,
                price: trade.price,
                qty: trade.qty,
                buy_member: buy.1,
                buy_account: buy.2,
                buy_fee: buy.3,
                sell_member: sell.1,
                sell_account: sell.2,
                sell_fee: sell.3,
                timestamp: now,
            };
            for clearing in self.clearing_members() {
                events.push(self.publish(clearing, ServerMessage::ClearedTrade(cleared.clone())));
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
        if let Some(meta) = self.orders.remove(&order_id)
            && let Some(open) = self.open_notional.get_mut(&meta.member)
        {
            *open = open.saturating_sub(meta.price.saturating_mul(meta.qty - meta.filled));
        }
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
    fn trades_and_allocations_are_sent_to_clearing_with_the_cycle() {
        let mut ex = Exchange::new(ExchangeConfig::default());
        admit(&mut ex, "ops", MemberKind::Admin);
        let broker = admit(&mut ex, "broker", MemberKind::Broker);
        let lp = admit(&mut ex, "lp", MemberKind::LiquidityProvider);
        let ccp = admit(&mut ex, "ccp", MemberKind::Clearing);
        ex.execute(&Command::OpenCycle { cycle: 4 }, 0).unwrap();
        ex.execute(
            &Command::ListInstrument {
                symbol: sym("ACME"),
                name: String::new(),
                ipo_price: 25_00,
                shares: 100,
                underwriter: broker,
                issuer_account: 9,
            },
            0,
        )
        .unwrap();
        let events = ex
            .execute(
                &Command::NewOrder {
                    member: lp,
                    member_order_id: 1,
                    account: 4,
                    symbol: sym("ACME"),
                    side: Side::Buy,
                    kind: OrderKind::Limit(25_00),
                    tif: TimeInForce::Gtc,
                    qty: 10,
                },
                1,
            )
            .unwrap();

        assert!(reports_for(&events, lp).iter().all(|r| r.cycle == 4));
        let feed = ex.replay(ccp, 0).messages;
        assert!(
            matches!(&feed[0], ServerMessage::Allocation(a) if a.member == broker && a.shares == 100)
        );
        let ServerMessage::ClearedTrade(t) = &feed[1] else {
            panic!("expected cleared trade")
        };
        assert_eq!((t.cycle, t.qty, t.price), (4, 10, 25_00));
        assert_eq!(
            (t.buy_member, t.buy_account, t.sell_member, t.sell_account),
            (lp, 4, broker, 9)
        );
        assert_eq!((t.buy_fee, t.sell_fee), (3, -2));
        assert!(
            ex.execute(
                &Command::NewOrder {
                    member: ccp,
                    member_order_id: 0,
                    account: 0,
                    symbol: sym("ACME"),
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    tif: TimeInForce::Ioc,
                    qty: 1,
                },
                2
            )
            .is_err()
        );
    }

    #[test]
    fn credit_limits_cap_exposure_until_the_cycle_settles() {
        let mut m = market();
        let lp = m.lp;
        m.ex.execute(&Command::OpenCycle { cycle: 1 }, 0).unwrap();
        m.ex.execute(
            &Command::SetCreditLimit {
                member: lp,
                limit: 600_00,
            },
            0,
        )
        .unwrap();

        order(&mut m, lp, 4, Side::Buy, 25_00, 20);
        assert_eq!(m.ex.exposure(lp), 500_00);
        let events = order(&mut m, lp, 4, Side::Buy, 24_00, 5);
        assert!(
            reports_for(&events, lp)[0]
                .reason
                .contains("credit limit exceeded")
        );

        m.ex.execute(&Command::OpenCycle { cycle: 2 }, 0).unwrap();
        m.ex.execute(&Command::Settled { cycle: 1 }, 0).unwrap();
        assert_eq!(m.ex.exposure(lp), 0);
        let events = order(&mut m, lp, 4, Side::Buy, 24_00, 5);
        assert_eq!(reports_for(&events, lp)[0].status, OrderStatus::New);
        assert_eq!(m.ex.exposure(lp), 120_00);

        let id = reports_for(&events, lp)[0].order_id;
        m.ex.execute(
            &Command::Cancel {
                member: lp,
                order_id: id,
            },
            1,
        )
        .unwrap();
        assert_eq!(m.ex.exposure(lp), 0);
        let info =
            m.ex.member_list(|_| false)
                .into_iter()
                .find(|i| i.member_id == lp)
                .unwrap();
        assert_eq!(info.credit_limit, 600_00);
    }

    fn order_tif(
        m: &mut Market,
        member: MemberId,
        account: AccountId,
        side: Side,
        kind: OrderKind,
        tif: TimeInForce,
        qty: Qty,
    ) -> Vec<Event> {
        m.ex.execute(
            &Command::NewOrder {
                member,
                member_order_id: 5,
                account,
                symbol: sym("ACME"),
                side,
                kind,
                tif,
                qty,
            },
            10,
        )
        .unwrap()
    }

    fn phase(m: &mut Market, phase: MarketPhase) -> Vec<Event> {
        m.ex.execute(&Command::SetPhase { phase }, 20).unwrap()
    }

    #[test]
    fn pre_open_collects_orders_and_the_opening_auction_uncrosses_at_one_price() {
        let mut m = market();
        let (lp, broker) = (m.lp, m.broker);
        phase(&mut m, MarketPhase::Closed);
        let events = order(&mut m, lp, 4, Side::Buy, 25_00, 10);
        assert!(
            reports_for(&events, lp)[0]
                .reason
                .contains("market is closed")
        );

        phase(&mut m, MarketPhase::PreOpen);
        let events = order(&mut m, lp, 4, Side::Buy, 26_00, 300);
        assert!(
            !events.iter().any(|e| matches!(e, Event::Trade(_))),
            "no matching during pre-open"
        );
        order(&mut m, broker, 7, Side::Sell, 24_50, 200);
        let events = order_tif(
            &mut m,
            lp,
            4,
            Side::Buy,
            OrderKind::Limit(25_50),
            TimeInForce::Ioc,
            5,
        );
        assert!(reports_for(&events, lp)[0].reason.contains("IOC"));
        let info = m.ex.auction_info(sym("ACME")).unwrap();
        assert_eq!((info.price, info.volume), (25_00, 300));

        let events = phase(&mut m, MarketPhase::Open);
        let prices: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                Event::Trade(t) => Some(t.price),
                _ => None,
            })
            .collect();
        assert!(!prices.is_empty());
        assert!(prices.iter().all(|p| *p == 25_00));
        assert!(m.ex.auction_info(sym("ACME")).is_none());
        let book = &m.ex.instrument(sym("ACME")).unwrap().book;
        assert!(book.best_bid() < book.best_ask() || book.best_ask().is_none());
        assert_eq!(m.ex.market_status().day, 1);
    }

    #[test]
    fn day_orders_expire_at_the_close_and_gtc_orders_survive() {
        let mut m = market();
        let lp = m.lp;
        let day = reports_for(
            &order_tif(
                &mut m,
                lp,
                4,
                Side::Buy,
                OrderKind::Limit(24_00),
                TimeInForce::Day,
                5,
            ),
            lp,
        )[0]
        .order_id;
        let gtc = reports_for(&order(&mut m, lp, 4, Side::Buy, 24_10, 5), lp)[0].order_id;
        let events = phase(&mut m, MarketPhase::Closed);
        let expired = reports_for(&events, lp);
        assert!(expired.iter().any(|r| r.order_id == day
            && r.status == OrderStatus::Expired
            && r.reason == "end of trading day"));
        assert!(!expired.iter().any(|r| r.order_id == gtc));
        assert!(
            m.ex.instrument(sym("ACME"))
                .unwrap()
                .book
                .get(gtc)
                .is_some()
        );
        assert_eq!(m.ex.exposure(lp), 24_10 * 5);
    }

    #[test]
    fn market_orders_join_the_auction_and_the_reference_resets_each_day() {
        let mut m = market();
        let (lp, broker) = (m.lp, m.broker);
        order(&mut m, lp, 4, Side::Buy, 25_00, 10);
        phase(&mut m, MarketPhase::ClosingAuction);
        order_tif(
            &mut m,
            lp,
            4,
            Side::Buy,
            OrderKind::Market,
            TimeInForce::Ioc,
            20,
        );
        order(&mut m, broker, 7, Side::Sell, 26_00, 20);
        let events = phase(&mut m, MarketPhase::Closed);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Trade(t) if t.qty == 20))
        );
        let close = m.ex.instrument(sym("ACME")).unwrap().last_price.unwrap();
        phase(&mut m, MarketPhase::PreOpen);
        assert_eq!(m.ex.instrument(sym("ACME")).unwrap().reference_price, close);
    }

    #[test]
    fn schedules_drive_phase_changes_through_due_commands() {
        let mut m = market();
        m.ex.execute(
            &Command::SetSchedule {
                schedule: Some(Schedule {
                    pre_open_ms: 5,
                    continuous_ms: 40,
                    closing_ms: 5,
                    closed_ms: 10,
                }),
            },
            100,
        )
        .unwrap();
        assert!(m.ex.due_commands(139).is_empty());
        assert_eq!(
            m.ex.due_commands(140),
            vec![Command::SetPhase {
                phase: MarketPhase::ClosingAuction
            }]
        );
        let status = m.ex.market_status();
        assert_eq!(
            (status.phase, status.next_change_at),
            (MarketPhase::Open, 140)
        );
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
