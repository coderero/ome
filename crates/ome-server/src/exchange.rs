use crate::auth::SecretHash;
use crate::instrument::{Halt, Instrument};
use crate::ledger::{Ledger, LedgerError};
use ome_core::price::{Price, format_price};
use ome_core::risk::{
    MaxNotional, MaxQuantity, PositiveQuantity, PriceBand, RiskContext, RiskEngine,
};
use ome_core::symbol::Symbol;
use ome_core::types::{
    AccountId, Order, OrderId, OrderKind, OrderRequest, Qty, Side, TimeInForce, Timestamp, Trade,
};
use ome_protocol::{
    AccountInfo, AccountSnapshot, Asset, ExecutionReport, InstrumentInfo, OrderStatus, Position,
    Role, TradeEvent,
};
use std::collections::{BTreeMap, HashMap};
use std::fmt;

#[derive(Debug, Clone)]
pub struct ExchangeConfig {
    pub band_bps: u64,
    pub circuit_bps: u64,
    pub halt_cooldown_ms: Timestamp,
    pub candle_interval: Timestamp,
    pub candle_capacity: usize,
    pub tape_capacity: usize,
    pub depth_levels: usize,
}

impl Default for ExchangeConfig {
    fn default() -> Self {
        Self {
            band_bps: 1_000,
            circuit_bps: 3_000,
            halt_cooldown_ms: 30_000,
            candle_interval: 1_000,
            candle_capacity: 600,
            tape_capacity: 200,
            depth_levels: 12,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    CreateAccount {
        role: Role,
        username: String,
        secret: SecretHash,
        cash: u64,
    },
    SetBalance {
        account: AccountId,
        asset: Asset,
        amount: u64,
    },
    ListInstrument {
        symbol: Symbol,
        name: String,
        ipo_price: Price,
        shares: Qty,
        issuer: AccountId,
    },
    Halt {
        symbol: Symbol,
        reason: String,
    },
    Resume {
        symbol: Symbol,
    },
    NewOrder {
        account: AccountId,
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        qty: Qty,
    },
    Cancel {
        account: AccountId,
        order_id: OrderId,
    },
    CancelAll {
        account: AccountId,
        symbol: Option<Symbol>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    AccountCreated(AccountId),
    Execution {
        account: AccountId,
        report: ExecutionReport,
    },
    AccountChanged(AccountId),
    Trade(TradeEvent),
    BookChanged(Symbol),
    InstrumentChanged(Symbol),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExchangeError {
    Ledger(LedgerError),
    UnknownSymbol(Symbol),
    DuplicateSymbol(Symbol),
    InvalidListing(&'static str),
    NotTradingAccount(AccountId),
    AlreadyHalted(Symbol),
    NotHalted(Symbol),
    UnknownOrder(OrderId),
    NotOwner(OrderId),
}

impl fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExchangeError::Ledger(e) => write!(f, "{e}"),
            ExchangeError::UnknownSymbol(s) => write!(f, "unknown symbol {s}"),
            ExchangeError::DuplicateSymbol(s) => write!(f, "symbol {s} is already listed"),
            ExchangeError::InvalidListing(why) => write!(f, "invalid listing: {why}"),
            ExchangeError::NotTradingAccount(id) => {
                write!(f, "account {id} cannot hold balances or trade")
            }
            ExchangeError::AlreadyHalted(s) => write!(f, "{s} is already halted"),
            ExchangeError::NotHalted(s) => write!(f, "{s} is not halted"),
            ExchangeError::UnknownOrder(id) => write!(f, "order #{id} is not open"),
            ExchangeError::NotOwner(id) => write!(f, "order #{id} is not yours"),
        }
    }
}

impl From<LedgerError> for ExchangeError {
    fn from(e: LedgerError) -> Self {
        ExchangeError::Ledger(e)
    }
}

struct OrderMeta {
    account: AccountId,
    symbol: Symbol,
    client_order_id: u64,
    side: Side,
    price: Price,
    qty: Qty,
    filled: Qty,
    reserved: u64,
}

struct RiskProfiles {
    retail: RiskEngine,
    institutional: RiskEngine,
    bot: RiskEngine,
}

impl RiskProfiles {
    fn new(band_bps: u64) -> Self {
        let profile = |max_qty: Qty, max_notional: u64| {
            RiskEngine::new()
                .with(PositiveQuantity)
                .with(MaxQuantity(max_qty))
                .with(PriceBand { bps: band_bps })
                .with(MaxNotional(max_notional))
        };
        Self {
            retail: profile(1_000, 10_000_000),
            institutional: profile(1_000_000, 10_000_000_000),
            bot: profile(10_000, 1_000_000_000),
        }
    }

    fn for_role(&self, role: Role) -> &RiskEngine {
        match role {
            Role::Institutional => &self.institutional,
            Role::Bot => &self.bot,
            Role::Retail | Role::Admin => &self.retail,
        }
    }
}

pub struct Exchange {
    config: ExchangeConfig,
    ledger: Ledger,
    instruments: BTreeMap<Symbol, Instrument>,
    orders: HashMap<OrderId, OrderMeta>,
    risk: RiskProfiles,
    next_order_id: OrderId,
    next_trade_id: u64,
}

impl Exchange {
    pub fn new(config: ExchangeConfig) -> Self {
        Self {
            risk: RiskProfiles::new(config.band_bps),
            config,
            ledger: Ledger::default(),
            instruments: BTreeMap::new(),
            orders: HashMap::new(),
            next_order_id: 1,
            next_trade_id: 1,
        }
    }

    pub fn config(&self) -> &ExchangeConfig {
        &self.config
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
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
            Command::CreateAccount {
                role,
                username,
                secret,
                cash,
            } => {
                let id = self.ledger.open(username, *role, *secret, *cash)?;
                Ok(vec![Event::AccountCreated(id)])
            }
            Command::SetBalance {
                account,
                asset,
                amount,
            } => self.set_balance(*account, *asset, *amount),
            Command::ListInstrument {
                symbol,
                name,
                ipo_price,
                shares,
                issuer,
            } => self.list_instrument(*symbol, name, *ipo_price, *shares, *issuer, now),
            Command::Halt { symbol, reason } => self.halt(*symbol, reason.clone(), None),
            Command::Resume { symbol } => self.resume(*symbol),
            Command::NewOrder {
                account,
                client_order_id,
                symbol,
                side,
                kind,
                qty,
            } => self.new_order(*account, *client_order_id, *symbol, *side, *kind, *qty, now),
            Command::Cancel { account, order_id } => self.cancel(*account, *order_id, now),
            Command::CancelAll { account, symbol } => Ok(self.cancel_all(*account, *symbol, now)),
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

    pub fn roll_candles(&mut self, now: Timestamp) {
        for instrument in self.instruments.values_mut() {
            instrument.candles.roll_to(now);
        }
    }

    pub fn instrument_info(&self, symbol: Symbol) -> Option<InstrumentInfo> {
        self.instruments
            .get(&symbol)
            .map(|i| i.info(self.config.circuit_bps))
    }

    pub fn instrument_list(&self) -> Vec<InstrumentInfo> {
        self.instruments
            .values()
            .map(|i| i.info(self.config.circuit_bps))
            .collect()
    }

    pub fn account_snapshot(&self, id: AccountId) -> Option<AccountSnapshot> {
        let account = self.ledger.get(id)?;
        Some(AccountSnapshot {
            account_id: id,
            role: account.role,
            cash: account.cash,
            positions: account
                .positions
                .iter()
                .filter(|(_, b)| b.total() > 0)
                .map(|(symbol, balance)| Position {
                    symbol: *symbol,
                    balance: *balance,
                })
                .collect(),
        })
    }

    pub fn account_list(&self) -> Vec<AccountInfo> {
        let mut open: HashMap<AccountId, u32> = HashMap::new();
        for meta in self.orders.values() {
            *open.entry(meta.account).or_default() += 1;
        }
        self.ledger
            .accounts()
            .map(|a| AccountInfo {
                account_id: a.id,
                role: a.role,
                cash: a.cash,
                open_orders: open.get(&a.id).copied().unwrap_or(0),
                username: a.username.clone(),
            })
            .collect()
    }

    pub fn open_order_reports(&self, account: AccountId, now: Timestamp) -> Vec<ExecutionReport> {
        let mut ids: Vec<OrderId> = self
            .orders
            .iter()
            .filter(|(_, m)| m.account == account)
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| {
                let meta = self.orders.get(&id)?;
                let status = if meta.filled > 0 {
                    OrderStatus::PartiallyFilled
                } else {
                    OrderStatus::New
                };
                Some(report_from(id, meta, status, 0, 0, now, ""))
            })
            .collect()
    }

    fn set_balance(
        &mut self,
        account: AccountId,
        asset: Asset,
        amount: u64,
    ) -> Result<Vec<Event>, ExchangeError> {
        let role = self
            .ledger
            .get(account)
            .ok_or(LedgerError::UnknownAccount(account))?
            .role;
        if !role.can_trade() {
            return Err(ExchangeError::NotTradingAccount(account));
        }
        match asset {
            Asset::Cash => self.ledger.set_cash(account, amount)?,
            Asset::Shares(symbol) => {
                if !self.instruments.contains_key(&symbol) {
                    return Err(ExchangeError::UnknownSymbol(symbol));
                }
                self.ledger.set_shares(account, symbol, amount)?
            }
        }
        Ok(vec![Event::AccountChanged(account)])
    }

    fn list_instrument(
        &mut self,
        symbol: Symbol,
        name: &str,
        ipo_price: Price,
        shares: Qty,
        issuer: AccountId,
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
        let role = self
            .ledger
            .get(issuer)
            .ok_or(LedgerError::UnknownAccount(issuer))?
            .role;
        if !role.can_trade() {
            return Err(ExchangeError::NotTradingAccount(issuer));
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
        self.ledger.credit_shares(issuer, symbol, shares)?;

        let mut events = vec![
            Event::InstrumentChanged(symbol),
            Event::AccountChanged(issuer),
        ];
        match self.place(
            issuer,
            0,
            symbol,
            Side::Sell,
            OrderKind::Limit(ipo_price),
            shares,
            now,
        ) {
            Ok(placed) => events.extend(placed),
            Err(reason) => unreachable!("IPO order for freshly credited shares failed: {reason}"),
        }
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
            "halted by admin".to_string()
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
        account: AccountId,
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        qty: Qty,
        now: Timestamp,
    ) -> Result<Vec<Event>, ExchangeError> {
        let role = self
            .ledger
            .get(account)
            .ok_or(LedgerError::UnknownAccount(account))?
            .role;
        if !role.can_trade() {
            return Err(ExchangeError::NotTradingAccount(account));
        }

        let reject = |reason: String| {
            vec![Event::Execution {
                account,
                report: ExecutionReport {
                    order_id: 0,
                    client_order_id,
                    symbol,
                    side,
                    status: OrderStatus::Rejected,
                    price: match kind {
                        OrderKind::Limit(price) => price,
                        OrderKind::Market => 0,
                    },
                    qty,
                    filled: 0,
                    leaves: 0,
                    last_price: 0,
                    last_qty: 0,
                    timestamp: now,
                    reason,
                },
            }]
        };

        let Some(instrument) = self.instruments.get(&symbol) else {
            return Ok(reject(format!("unknown symbol {symbol}")));
        };
        if let Some(halt) = &instrument.halt {
            return Ok(reject(format!("{symbol} is halted: {}", halt.reason)));
        }
        let reference_price = instrument.last_price.unwrap_or(instrument.reference_price);
        let request = OrderRequest { side, kind, qty };
        if let Err(rejection) = self
            .risk
            .for_role(role)
            .check(&request, &RiskContext { reference_price })
        {
            return Ok(reject(rejection.to_string()));
        }

        Ok(self
            .place(account, client_order_id, symbol, side, kind, qty, now)
            .unwrap_or_else(reject))
    }

    #[allow(clippy::too_many_arguments)]
    fn place(
        &mut self,
        account: AccountId,
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        qty: Qty,
        now: Timestamp,
    ) -> Result<Vec<Event>, String> {
        let instrument = self
            .instruments
            .get(&symbol)
            .ok_or_else(|| format!("unknown symbol {symbol}"))?;
        let (low, high) = instrument.band(self.config.band_bps);
        let (price, tif) = match (kind, side) {
            (OrderKind::Limit(price), _) => (price, TimeInForce::Gtc),
            (OrderKind::Market, Side::Buy) => (high, TimeInForce::Ioc),
            (OrderKind::Market, Side::Sell) => (low, TimeInForce::Ioc),
        };

        let reserved = match side {
            Side::Buy => {
                let amount = price.checked_mul(qty).ok_or("order notional overflows")?;
                self.ledger
                    .reserve_cash(account, amount)
                    .map_err(|e| e.to_string())?;
                amount
            }
            Side::Sell => {
                self.ledger
                    .reserve_shares(account, symbol, qty)
                    .map_err(|e| e.to_string())?;
                qty
            }
        };

        let order_id = self.next_order_id;
        self.next_order_id += 1;
        let meta = OrderMeta {
            account,
            symbol,
            client_order_id,
            side,
            price,
            qty,
            filled: 0,
            reserved,
        };
        let mut events = vec![
            Event::Execution {
                account,
                report: report_from(order_id, &meta, OrderStatus::New, 0, 0, now, ""),
            },
            Event::AccountChanged(account),
        ];
        self.orders.insert(order_id, meta);

        let execution = self
            .instruments
            .get_mut(&symbol)
            .expect("instrument checked above")
            .book
            .submit(
                Order {
                    id: order_id,
                    account,
                    side,
                    price,
                    qty,
                    timestamp: now,
                },
                tif,
            );

        for trade in &execution.trades {
            self.settle(symbol, trade, now, &mut events);
        }
        for cancelled in &execution.self_trade_cancelled {
            self.close_order(
                cancelled.id,
                OrderStatus::Cancelled,
                now,
                "self-trade prevention",
                &mut events,
            );
        }
        if execution.expired > 0 {
            self.close_order(
                order_id,
                OrderStatus::Expired,
                now,
                "no liquidity inside the price band",
                &mut events,
            );
        }

        events.push(Event::BookChanged(symbol));
        self.check_circuit(symbol, now, &mut events);
        Ok(events)
    }

    fn settle(&mut self, symbol: Symbol, trade: &Trade, now: Timestamp, events: &mut Vec<Event>) {
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

        let (buy_order, sell_order) = match trade.taker_side {
            Side::Buy => (trade.taker_order, trade.maker_order),
            Side::Sell => (trade.maker_order, trade.taker_order),
        };
        let cost = trade.price * trade.qty;
        if let Some(meta) = self.orders.get_mut(&buy_order) {
            let consumed = meta.price * trade.qty;
            meta.reserved -= consumed;
            meta.filled += trade.qty;
            self.ledger
                .settle_buy(meta.account, symbol, consumed, cost, trade.qty);
        }
        if let Some(meta) = self.orders.get_mut(&sell_order) {
            meta.reserved -= trade.qty;
            meta.filled += trade.qty;
            self.ledger
                .settle_sell(meta.account, symbol, trade.qty, cost);
        }

        for order_id in [trade.maker_order, trade.taker_order] {
            let Some(meta) = self.orders.get(&order_id) else {
                continue;
            };
            let done = meta.filled == meta.qty;
            let status = if done {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            };
            events.push(Event::Execution {
                account: meta.account,
                report: report_from(order_id, meta, status, trade.price, trade.qty, now, ""),
            });
            events.push(Event::AccountChanged(meta.account));
            if done {
                self.orders.remove(&order_id);
            }
        }
    }

    fn close_order(
        &mut self,
        order_id: OrderId,
        status: OrderStatus,
        now: Timestamp,
        reason: &str,
        events: &mut Vec<Event>,
    ) {
        let Some(meta) = self.orders.remove(&order_id) else {
            return;
        };
        match meta.side {
            Side::Buy => self.ledger.release_cash(meta.account, meta.reserved),
            Side::Sell => self
                .ledger
                .release_shares(meta.account, meta.symbol, meta.reserved),
        }
        let mut report = report_from(order_id, &meta, status, 0, 0, now, reason);
        report.leaves = 0;
        events.push(Event::Execution {
            account: meta.account,
            report,
        });
        events.push(Event::AccountChanged(meta.account));
    }

    fn cancel(
        &mut self,
        account: AccountId,
        order_id: OrderId,
        now: Timestamp,
    ) -> Result<Vec<Event>, ExchangeError> {
        let meta = self
            .orders
            .get(&order_id)
            .ok_or(ExchangeError::UnknownOrder(order_id))?;
        if meta.account != account {
            return Err(ExchangeError::NotOwner(order_id));
        }
        let symbol = meta.symbol;
        self.instruments
            .get_mut(&symbol)
            .and_then(|i| i.book.cancel(order_id))
            .ok_or(ExchangeError::UnknownOrder(order_id))?;

        let mut events = Vec::new();
        self.close_order(
            order_id,
            OrderStatus::Cancelled,
            now,
            "cancelled",
            &mut events,
        );
        events.push(Event::BookChanged(symbol));
        Ok(events)
    }

    fn cancel_all(
        &mut self,
        account: AccountId,
        symbol: Option<Symbol>,
        now: Timestamp,
    ) -> Vec<Event> {
        let mut ids: Vec<OrderId> = self
            .orders
            .iter()
            .filter(|(_, m)| m.account == account && symbol.is_none_or(|s| s == m.symbol))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| self.cancel(account, id, now).ok())
            .flatten()
            .collect()
    }

    fn check_circuit(&mut self, symbol: Symbol, now: Timestamp, events: &mut Vec<Event>) {
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
        let resume_at = now + self.config.halt_cooldown_ms;
        if let Ok(halted) = self.halt(symbol, reason, Some(resume_at)) {
            events.extend(halted);
        }
    }
}

fn report_from(
    order_id: OrderId,
    meta: &OrderMeta,
    status: OrderStatus,
    last_price: Price,
    last_qty: Qty,
    timestamp: Timestamp,
    reason: &str,
) -> ExecutionReport {
    ExecutionReport {
        order_id,
        client_order_id: meta.client_order_id,
        symbol: meta.symbol,
        side: meta.side,
        status,
        price: meta.price,
        qty: meta.qty,
        filled: meta.filled,
        leaves: meta.qty - meta.filled,
        last_price,
        last_qty,
        timestamp,
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::hash_secret;
    use ome_protocol::InstrumentStatus;

    pub(crate) fn sym(s: &str) -> Symbol {
        Symbol::parse(s).unwrap()
    }

    struct Market {
        ex: Exchange,
        issuer: AccountId,
        alice: AccountId,
        bob: AccountId,
    }

    fn open(ex: &mut Exchange, name: &str, role: Role, cash: u64) -> AccountId {
        let events = ex
            .execute(
                &Command::CreateAccount {
                    role,
                    username: name.into(),
                    secret: hash_secret(name, "pw"),
                    cash,
                },
                0,
            )
            .unwrap();
        match events[0] {
            Event::AccountCreated(id) => id,
            _ => unreachable!(),
        }
    }

    fn market_with(config: ExchangeConfig) -> Market {
        let mut ex = Exchange::new(config);
        let issuer = open(&mut ex, "issuer", Role::Institutional, 0);
        let alice = open(&mut ex, "alice", Role::Retail, 100_000_00);
        let bob = open(&mut ex, "bob", Role::Institutional, 1_000_000_00);
        ex.execute(
            &Command::ListInstrument {
                symbol: sym("ACME"),
                name: "Acme".into(),
                ipo_price: 25_00,
                shares: 1_000,
                issuer,
            },
            0,
        )
        .unwrap();
        Market {
            ex,
            issuer,
            alice,
            bob,
        }
    }

    fn market() -> Market {
        market_with(ExchangeConfig::default())
    }

    fn order(
        ex: &mut Exchange,
        account: AccountId,
        side: Side,
        kind: OrderKind,
        qty: Qty,
    ) -> Vec<Event> {
        ex.execute(
            &Command::NewOrder {
                account,
                client_order_id: 1,
                symbol: sym("ACME"),
                side,
                kind,
                qty,
            },
            1,
        )
        .unwrap()
    }

    fn reports(events: &[Event]) -> Vec<&ExecutionReport> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Execution { report, .. } => Some(report),
                _ => None,
            })
            .collect()
    }

    fn assert_conserved(m: &Market, cash: u64) {
        assert_eq!(m.ex.ledger().total_cash(), cash, "cash not conserved");
        assert_eq!(
            m.ex.ledger().total_shares(sym("ACME")),
            1_000,
            "shares not conserved"
        );
    }

    const TOTAL_CASH: u64 = 100_000_00 + 1_000_000_00;

    #[test]
    fn ipo_lists_symbol_and_offers_all_shares_from_issuer() {
        let m = market();
        let instrument = m.ex.instrument(sym("ACME")).unwrap();
        assert_eq!(instrument.book.best_ask(), Some(25_00));
        assert_eq!(instrument.reference_price, 25_00);
        let issuer = m.ex.ledger().get(m.issuer).unwrap();
        assert_eq!(issuer.position(sym("ACME")).reserved, 1_000);
        assert_conserved(&m, TOTAL_CASH);
    }

    #[test]
    fn buying_from_ipo_settles_both_sides() {
        let mut m = market();
        let events = order(&mut m.ex, m.alice, Side::Buy, OrderKind::Limit(26_00), 10);

        let r = reports(&events);
        assert_eq!(r[0].status, OrderStatus::New);
        assert!(
            r.iter()
                .any(|r| r.status == OrderStatus::Filled && r.last_price == 25_00)
        );

        let alice = m.ex.ledger().get(m.alice).unwrap();
        assert_eq!(alice.cash.available, 100_000_00 - 250_00);
        assert_eq!(alice.cash.reserved, 0);
        assert_eq!(alice.position(sym("ACME")).available, 10);
        assert_eq!(m.ex.ledger().get(m.issuer).unwrap().cash.available, 250_00);
        assert_conserved(&m, TOTAL_CASH);
    }

    #[test]
    fn insufficient_funds_and_shares_are_rejected() {
        let mut m = market();
        let events = order(&mut m.ex, m.alice, Side::Sell, OrderKind::Limit(25_00), 5);
        assert_eq!(reports(&events)[0].status, OrderStatus::Rejected);
        assert!(reports(&events)[0].reason.contains("insufficient ACME"));

        m.ex.execute(
            &Command::SetBalance {
                account: m.alice,
                asset: Asset::Cash,
                amount: 100,
            },
            1,
        )
        .unwrap();
        let events = order(&mut m.ex, m.alice, Side::Buy, OrderKind::Limit(25_00), 5);
        assert!(reports(&events)[0].reason.contains("insufficient cash"));
    }

    #[test]
    fn market_buy_reserves_at_band_and_refunds_the_difference() {
        let mut m = market();
        let events = order(&mut m.ex, m.alice, Side::Buy, OrderKind::Market, 4);

        assert!(
            reports(&events)
                .iter()
                .any(|r| r.status == OrderStatus::Filled)
        );
        let alice = m.ex.ledger().get(m.alice).unwrap();
        assert_eq!(alice.cash.available, 100_000_00 - 4 * 25_00);
        assert_eq!(alice.cash.reserved, 0);
    }

    #[test]
    fn market_order_with_no_liquidity_expires_and_releases() {
        let mut m = market();
        let events = order(&mut m.ex, m.alice, Side::Sell, OrderKind::Market, 1);
        assert_eq!(reports(&events)[0].status, OrderStatus::Rejected);

        order(&mut m.ex, m.alice, Side::Buy, OrderKind::Limit(25_00), 2);
        let events = order(&mut m.ex, m.alice, Side::Sell, OrderKind::Market, 2);
        assert_eq!(
            reports(&events).last().unwrap().status,
            OrderStatus::Expired
        );
        assert_eq!(
            m.ex.ledger()
                .get(m.alice)
                .unwrap()
                .position(sym("ACME"))
                .available,
            2
        );
    }

    #[test]
    fn cancel_releases_reservation_and_checks_owner() {
        let mut m = market();
        let events = order(&mut m.ex, m.alice, Side::Buy, OrderKind::Limit(24_00), 10);
        let id = reports(&events)[0].order_id;
        assert_eq!(m.ex.ledger().get(m.alice).unwrap().cash.reserved, 240_00);

        let cancel = |account| Command::Cancel {
            account,
            order_id: id,
        };
        assert_eq!(
            m.ex.execute(&cancel(m.bob), 2),
            Err(ExchangeError::NotOwner(id))
        );
        m.ex.execute(&cancel(m.alice), 2).unwrap();

        assert_eq!(m.ex.ledger().get(m.alice).unwrap().cash.reserved, 0);
        assert_eq!(
            m.ex.execute(&cancel(m.alice), 2),
            Err(ExchangeError::UnknownOrder(id))
        );
        assert_conserved(&m, TOTAL_CASH);
    }

    #[test]
    fn retail_limits_are_tighter_than_institutional() {
        let mut m = market();
        let events = order(
            &mut m.ex,
            m.alice,
            Side::Buy,
            OrderKind::Limit(25_00),
            5_000,
        );
        assert!(reports(&events)[0].reason.contains("exceeds max"));

        let events = order(&mut m.ex, m.bob, Side::Buy, OrderKind::Limit(25_00), 5_000);
        assert_ne!(reports(&events)[0].status, OrderStatus::Rejected);
    }

    #[test]
    fn absurd_prices_are_rejected_by_the_band() {
        let mut m = market();
        for price in [1, 250_00, 22_49, 27_51] {
            let events = order(&mut m.ex, m.bob, Side::Buy, OrderKind::Limit(price), 1);
            assert!(
                reports(&events)[0].reason.contains("outside band"),
                "{price}"
            );
        }
    }

    #[test]
    fn halted_symbol_rejects_orders_but_allows_cancels() {
        let mut m = market();
        let events = order(&mut m.ex, m.alice, Side::Buy, OrderKind::Limit(24_00), 1);
        let id = reports(&events)[0].order_id;
        m.ex.execute(
            &Command::Halt {
                symbol: sym("ACME"),
                reason: "news".into(),
            },
            2,
        )
        .unwrap();

        let events = order(&mut m.ex, m.alice, Side::Buy, OrderKind::Limit(24_00), 1);
        assert!(reports(&events)[0].reason.contains("halted: news"));
        assert!(
            m.ex.execute(
                &Command::Cancel {
                    account: m.alice,
                    order_id: id
                },
                3
            )
            .is_ok()
        );
        assert_eq!(
            m.ex.execute(
                &Command::Halt {
                    symbol: sym("ACME"),
                    reason: String::new()
                },
                3
            ),
            Err(ExchangeError::AlreadyHalted(sym("ACME")))
        );

        m.ex.execute(
            &Command::Resume {
                symbol: sym("ACME"),
            },
            4,
        )
        .unwrap();
        assert!(m.ex.due_commands(u64::MAX).is_empty());
    }

    #[test]
    fn circuit_breaker_halts_on_large_move_and_auto_resumes() {
        let mut m = market_with(ExchangeConfig {
            circuit_bps: 500,
            ..ExchangeConfig::default()
        });
        order(&mut m.ex, m.bob, Side::Buy, OrderKind::Limit(25_00), 1_000);
        order(&mut m.ex, m.bob, Side::Sell, OrderKind::Limit(27_00), 10);
        let events = order(&mut m.ex, m.alice, Side::Buy, OrderKind::Limit(27_00), 10);

        assert!(events.contains(&Event::InstrumentChanged(sym("ACME"))));
        let info = m.ex.instrument_info(sym("ACME")).unwrap();
        assert_eq!(info.status, InstrumentStatus::Halted);
        assert!(info.halt_reason.contains("circuit breaker: +8.0%"));

        assert!(m.ex.due_commands(1 + 29_999).is_empty());
        let due = m.ex.due_commands(1 + 30_000);
        assert_eq!(
            due,
            vec![Command::Resume {
                symbol: sym("ACME")
            }]
        );
        m.ex.execute(&due[0], 30_001).unwrap();
        let info = m.ex.instrument_info(sym("ACME")).unwrap();
        assert_eq!(info.status, InstrumentStatus::Trading);
        assert_eq!(info.reference_price, 27_00);
    }

    #[test]
    fn self_trade_cancels_resting_and_releases_shares() {
        let mut m = market();
        order(&mut m.ex, m.bob, Side::Buy, OrderKind::Limit(25_00), 100);
        let resting = order(&mut m.ex, m.bob, Side::Buy, OrderKind::Limit(24_00), 10);
        let resting_id = reports(&resting)[0].order_id;

        let events = order(&mut m.ex, m.bob, Side::Sell, OrderKind::Limit(24_00), 10);

        let cancelled = reports(&events)
            .into_iter()
            .find(|r| r.status == OrderStatus::Cancelled)
            .unwrap();
        assert_eq!(cancelled.order_id, resting_id);
        assert_eq!(cancelled.reason, "self-trade prevention");
        assert!(!events.iter().any(|e| matches!(e, Event::Trade(_))));
        let bob = m.ex.ledger().get(m.bob).unwrap();
        assert_eq!(bob.cash.reserved, 0);
        assert_eq!(bob.position(sym("ACME")).reserved, 10);
        assert_conserved(&m, TOTAL_CASH);
    }

    #[test]
    fn admin_accounts_cannot_trade_or_hold_balances() {
        let mut m = market();
        let admin = open(&mut m.ex, "root", Role::Admin, 0);
        assert_eq!(
            m.ex.execute(
                &Command::NewOrder {
                    account: admin,
                    client_order_id: 0,
                    symbol: sym("ACME"),
                    side: Side::Buy,
                    kind: OrderKind::Market,
                    qty: 1
                },
                1
            ),
            Err(ExchangeError::NotTradingAccount(admin))
        );
        assert!(
            m.ex.execute(
                &Command::SetBalance {
                    account: admin,
                    asset: Asset::Cash,
                    amount: 1
                },
                1
            )
            .is_err()
        );
    }

    #[test]
    fn listing_validation() {
        let mut m = market();
        let list = |symbol: &str, price, shares, issuer| Command::ListInstrument {
            symbol: sym(symbol),
            name: String::new(),
            ipo_price: price,
            shares,
            issuer,
        };
        assert_eq!(
            m.ex.execute(&list("ACME", 1, 1, m.issuer), 0),
            Err(ExchangeError::DuplicateSymbol(sym("ACME")))
        );
        assert!(m.ex.execute(&list("NEW", 0, 1, m.issuer), 0).is_err());
        assert!(m.ex.execute(&list("NEW", 1, 0, m.issuer), 0).is_err());
        assert!(m.ex.execute(&list("NEW", 1, 1, 999), 0).is_err());
        m.ex.execute(&list("NEW", 1_00, 10, m.issuer), 0).unwrap();
        assert_eq!(m.ex.instrument_info(sym("NEW")).unwrap().name, "NEW");
    }

    #[test]
    fn open_orders_are_reported_for_logon_replay() {
        let mut m = market();
        order(&mut m.ex, m.alice, Side::Buy, OrderKind::Limit(24_00), 3);
        order(&mut m.ex, m.alice, Side::Buy, OrderKind::Limit(24_50), 3);

        let open = m.ex.open_order_reports(m.alice, 5);
        assert_eq!(open.len(), 2);
        assert!(
            open.iter()
                .all(|r| r.status == OrderStatus::New && r.leaves == 3)
        );
        let alice =
            m.ex.account_list()
                .into_iter()
                .find(|a| a.account_id == m.alice)
                .unwrap();
        assert_eq!(alice.open_orders, 2);
    }
}
