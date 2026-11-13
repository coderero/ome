use crate::algo::Algo;
use crate::fees::FeeEngine;
use crate::ledger::Ledger;
use crate::market::MarketCache;
use crate::oms::{BracketPlan, Conditional, Conditionals, exit_legs};
use crate::profile::{Profile, Shape};
use crate::risk::AccountRisk;
use ome_core::price::Price;
use ome_core::risk::RiskContext;
use ome_core::symbol::Symbol;
use ome_core::types::{
    AccountId, OrderId, OrderKind, OrderOptions, OrderRequest, Qty, Side, TimeInForce, Timestamp,
};
use ome_net::auth::SecretHash;
use ome_protocol::{
    AccountInfo, AccountSnapshot, AlgoReport, AlgoStatus, AlgoStrategy, Allocation, Asset, Charges,
    ConditionalReport, ConditionalStatus, ContractNote, ExecutionReport, FeeSummary, MarketPhase,
    MemberMessage, OrderStatus, Position, QuoteSide, Role, ServerMessage, SettlementReport,
    Trigger, Venue,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const SYNTHETIC_REF: u64 = 1 << 63;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    AccountOpened {
        role: Role,
        username: String,
        secret: SecretHash,
        cash: u64,
    },
    BalanceSet {
        account: AccountId,
        asset: Asset,
        amount: u64,
    },
    OrderAccepted {
        order_ref: u64,
        account: AccountId,
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        price: Price,
        options: OrderOptions,
        qty: Qty,
        reserved: u64,
        fee_reserve: u64,
    },
    ExchangeReport {
        report: ExecutionReport,
        charges: Charges,
    },
    Allocated(Allocation),
    OrderAbandoned {
        order_ref: u64,
    },
    SettlementReceived(SettlementReport),
    QuoteAccepted {
        account: AccountId,
        symbol: Symbol,
        bid: Option<QuoteLeg>,
        ask: Option<QuoteLeg>,
    },
    ConditionalArmed(Vec<Conditional>),
    TrailMoved {
        cond_id: u64,
        extreme: Price,
    },
    ConditionalTriggered {
        cond_id: u64,
        price: Price,
        order: Option<Box<Fact>>,
        reason: String,
    },
    ConditionalCancelled {
        cond_id: u64,
        reason: String,
    },
    BracketAccepted {
        order: Box<Fact>,
        take_profit: Price,
        stop_loss: Price,
    },
    AccountStatusSet {
        account: AccountId,
        suspended: bool,
    },
    RestrictionSet {
        symbol: Symbol,
        restricted: bool,
    },
    LossLimitSet {
        account: AccountId,
        amount: u64,
    },
    TradingDayStarted {
        day: u64,
        marks: Vec<(Symbol, Price)>,
    },
    OrderHeld(Box<Fact>),
    RoutedToVenue {
        order_ref: u64,
        venue: Venue,
    },
    VenueReport {
        venue: Venue,
        report: ExecutionReport,
        charges: Charges,
    },
    DarkAccepted {
        order: Box<Fact>,
        limit: Option<Price>,
        min_qty: Qty,
    },
    InternalCross(Cross),
    Released {
        order_ref: u64,
    },
    AlgoStarted(Algo),
    AlgoChild {
        algo_id: u64,
        order: Box<Fact>,
    },
    AlgoFinished {
        algo_id: u64,
        status: AlgoStatus,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cross {
    pub cross_id: u64,
    pub buy_ref: u64,
    pub sell_ref: u64,
    pub price: Price,
    pub qty: Qty,
    pub cycle: u64,
    pub buy_charges: Charges,
    pub sell_charges: Charges,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuoteLeg {
    pub order_ref: u64,
    pub price: Price,
    pub qty: Qty,
    pub reserved: u64,
    pub fee_reserve: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    AccountOpened(AccountId),
    AccountChanged(AccountId),
    ToAccount {
        account: AccountId,
        message: ServerMessage,
    },
    ToExchange(MemberMessage),
    ToVenue(Venue, MemberMessage),
}

#[derive(Default)]
struct Pending {
    cash: i64,
    shares: BTreeMap<Symbol, i64>,
}

struct OrderMeta {
    account: AccountId,
    client_order_id: u64,
    symbol: Symbol,
    side: Side,
    price: Price,
    qty: Qty,
    filled: Qty,
    reserved: u64,
    fee_reserve: u64,
    commission_paid: u64,
    exchange_id: Option<OrderId>,
    venue: Venue,
    options: OrderOptions,
}

pub struct Broker {
    profile: Profile,
    fees: FeeEngine,
    fee_totals: FeeSummary,
    ledger: Ledger,
    orders: BTreeMap<u64, OrderMeta>,
    by_exchange: HashMap<OrderId, u64>,
    allocations: BTreeSet<u64>,
    next_order_ref: u64,
    last_seq: u64,
    pending: BTreeMap<(u64, AccountId), Pending>,
    last_settlement_seq: u64,
    conditionals: Conditionals,
    brackets: BTreeMap<u64, BracketPlan>,
    risk: AccountRisk,
    marks: BTreeMap<Symbol, Price>,
    algos: BTreeMap<u64, Algo>,
    algo_children: BTreeMap<u64, u64>,
    next_algo_id: u64,
    held: BTreeMap<u64, OrderOptions>,
    cycle: u64,
    next_cross_id: u64,
    venue_seqs: BTreeMap<Venue, u64>,
}

impl Broker {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            fees: FeeEngine::default(),
            fee_totals: FeeSummary::default(),
            ledger: Ledger::default(),
            orders: BTreeMap::new(),
            by_exchange: HashMap::new(),
            allocations: BTreeSet::new(),
            next_order_ref: 1,
            last_seq: 0,
            pending: BTreeMap::new(),
            last_settlement_seq: 0,
            conditionals: Conditionals::default(),
            brackets: BTreeMap::new(),
            risk: AccountRisk::default(),
            marks: BTreeMap::new(),
            algos: BTreeMap::new(),
            algo_children: BTreeMap::new(),
            next_algo_id: 1,
            held: BTreeMap::new(),
            cycle: 0,
            next_cross_id: 1,
            venue_seqs: BTreeMap::new(),
        }
    }

    pub fn mark(&mut self, symbol: Symbol, price: Price) {
        self.marks.insert(symbol, price);
    }

    pub fn risk(&self) -> &AccountRisk {
        &self.risk
    }

    pub fn day_pnl(&self, account: AccountId) -> i64 {
        self.risk
            .day_pnl(account, |symbol| self.marks.get(&symbol).copied())
    }

    pub fn decide_account_status(
        &self,
        account: AccountId,
        suspended: bool,
    ) -> Result<Fact, String> {
        match self.ledger.get(account) {
            Some(a) if a.role.can_trade() => Ok(Fact::AccountStatusSet { account, suspended }),
            Some(_) => Err(format!("account {account} does not trade")),
            None => Err(format!("unknown account {account}")),
        }
    }

    pub fn decide_restriction(
        &self,
        symbol: Symbol,
        restricted: bool,
        market: &MarketCache,
    ) -> Result<Fact, String> {
        if market.get(symbol).is_none() {
            return Err(format!("unknown symbol {symbol}"));
        }
        Ok(Fact::RestrictionSet { symbol, restricted })
    }

    pub fn decide_loss_limit(&self, account: AccountId, amount: u64) -> Result<Fact, String> {
        match self.ledger.get(account) {
            Some(a) if a.role.can_trade() => Ok(Fact::LossLimitSet { account, amount }),
            Some(_) => Err(format!("account {account} does not trade")),
            None => Err(format!("unknown account {account}")),
        }
    }

    pub fn decide_trading_day(&self, day: u64, market: &MarketCache) -> Option<Fact> {
        (day > self.risk.day()).then(|| Fact::TradingDayStarted {
            day,
            marks: market
                .symbols()
                .filter_map(|symbol| mark_of(market, symbol).map(|p| (symbol, p)))
                .collect(),
        })
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn fee_summary(&self) -> FeeSummary {
        self.fee_totals
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn is_new(&self, seq: u64) -> bool {
        seq > self.last_seq
    }

    pub fn last_settlement_seq(&self) -> u64 {
        self.last_settlement_seq
    }

    pub fn is_new_settlement(&self, seq: u64) -> bool {
        seq > self.last_settlement_seq
    }

    fn unsettled(&self, account: AccountId) -> (i64, BTreeMap<Symbol, i64>) {
        let mut cash = 0;
        let mut shares: BTreeMap<Symbol, i64> = BTreeMap::new();
        for ((_, holder), pending) in &self.pending {
            if *holder == account {
                cash += pending.cash;
                for (symbol, qty) in &pending.shares {
                    *shares.entry(*symbol).or_default() += qty;
                }
            }
        }
        (cash, shares)
    }

    pub fn decide_open_account(
        &self,
        role: Role,
        username: &str,
        secret: SecretHash,
        cash: u64,
    ) -> Result<Fact, String> {
        if !self.profile.allows(role) {
            return Err(format!(
                "a {} does not open {} accounts",
                self.profile.kind.label(),
                role.label()
            ));
        }
        if self.ledger.find(username).is_some() {
            return Err(format!("username '{username}' is taken"));
        }
        Ok(Fact::AccountOpened {
            role,
            username: username.to_string(),
            secret,
            cash,
        })
    }

    pub fn decide_set_balance(
        &self,
        account: AccountId,
        asset: Asset,
        amount: u64,
    ) -> Result<Fact, String> {
        match self.ledger.get(account) {
            None => Err(format!("unknown account {account}")),
            Some(a) if !a.role.can_trade() => {
                Err(format!("account {account} cannot hold balances"))
            }
            Some(_) => Ok(Fact::BalanceSet {
                account,
                asset,
                amount,
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decide_order(
        &self,
        account: AccountId,
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        options: OrderOptions,
        qty: Qty,
        market: &MarketCache,
        now: Timestamp,
    ) -> Result<Fact, Box<ExecutionReport>> {
        let reject = |reason: String| {
            rejection(
                account,
                client_order_id,
                symbol,
                side,
                kind,
                qty,
                now,
                reason,
            )
        };
        let Some(holder) = self.ledger.get(account) else {
            return Err(reject(format!("unknown account {account}")));
        };
        let Some(quote) = market.get(symbol) else {
            return Err(reject(format!("unknown symbol {symbol}")));
        };
        if market.phase() == MarketPhase::Closed {
            return Err(reject("market is closed".to_string()));
        }
        let tif = options.tif;
        if market.phase().is_call() && tif.is_immediate() && kind != OrderKind::Market {
            return Err(reject(format!(
                "{} orders are not accepted during the {}",
                tif.label().to_uppercase(),
                market.phase().label()
            )));
        }
        if let Err(reason) = Profile::permits(holder.role, kind, &options) {
            return Err(reject(reason));
        }
        if let Err(reason) = self
            .risk
            .check(account, symbol, side, |s| mark_of(market, s))
        {
            return Err(reject(reason));
        }
        if !market.is_trading(symbol) {
            return Err(reject(format!(
                "{symbol} is halted: {}",
                quote.info.halt_reason
            )));
        }
        let reference_price = quote.info.last_price.unwrap_or(quote.info.reference_price);
        if let Err(rejection) = Profile::risk(holder.role).check(
            &OrderRequest { side, kind, qty },
            &RiskContext { reference_price },
        ) {
            return Err(reject(rejection.to_string()));
        }
        let Some((low, high)) = market.band(symbol) else {
            return Err(reject(format!("no price band for {symbol}")));
        };
        let (price, tif) = match (kind, side) {
            (OrderKind::Limit(p), _) if p < low || p > high => {
                return Err(reject(format!(
                    "price {} outside band {} - {}",
                    ome_core::price::format_price(p),
                    ome_core::price::format_price(low),
                    ome_core::price::format_price(high)
                )));
            }
            (OrderKind::Limit(p), _) => (p, tif),
            (OrderKind::Market, _) if tif == TimeInForce::Fok => {
                (if side == Side::Buy { high } else { low }, tif)
            }
            (OrderKind::Market, Side::Buy) => (high, TimeInForce::Ioc),
            (OrderKind::Market, Side::Sell) => (low, TimeInForce::Ioc),
        };
        let options = OrderOptions { tif, ..options };
        let fee_reserve = self.fees.reserve(holder.role, side, price, qty);
        let reserved = match side {
            Side::Buy => {
                let notional = price.saturating_mul(qty);
                let needed = notional.saturating_add(fee_reserve);
                if holder.cash.available < needed {
                    return Err(reject(format!(
                        "insufficient cash: need {}, available {}",
                        ome_core::price::format_price(needed),
                        ome_core::price::format_price(holder.cash.available)
                    )));
                }
                notional
            }
            Side::Sell => {
                let available = holder.position(symbol).available;
                if available < qty {
                    return Err(reject(format!(
                        "insufficient {symbol} shares: need {qty}, available {available}"
                    )));
                }
                qty
            }
        };
        Ok(Fact::OrderAccepted {
            order_ref: self.next_order_ref,
            account,
            client_order_id,
            symbol,
            side,
            price,
            options,
            qty,
            reserved,
            fee_reserve,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn arm_checks(
        &self,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        qty: Qty,
        trigger: &Trigger,
        shape: Shape,
        market: &MarketCache,
    ) -> Result<(Role, Price), String> {
        let holder = self
            .ledger
            .get(account)
            .ok_or_else(|| format!("unknown account {account}"))?;
        Profile::permits_conditional(holder.role, trigger, shape)?;
        self.risk
            .check(account, symbol, side, |s| mark_of(market, s))?;
        let quote = market
            .get(symbol)
            .ok_or_else(|| format!("unknown symbol {symbol}"))?;
        let limits = Profile::limits(holder.role);
        if qty == 0 || qty > limits.max_qty {
            return Err(format!("quantity must be 1 - {}", limits.max_qty));
        }
        let level = match trigger {
            Trigger::Above(p)
            | Trigger::Below(p)
            | Trigger::TrailingAbove(p)
            | Trigger::TrailingBelow(p) => *p,
            Trigger::MarketOpens => 1,
        };
        if level == 0 {
            return Err("trigger price must be positive".into());
        }
        if *trigger == Trigger::MarketOpens && market.phase() != MarketPhase::Closed {
            return Err("the market is open; send a regular order".into());
        }
        let held = holder.position(symbol).total();
        if side == Side::Sell && held < qty {
            return Err(format!(
                "insufficient {symbol} shares: need {qty}, holding {held}"
            ));
        }
        Ok((
            holder.role,
            quote.info.last_price.unwrap_or(quote.info.reference_price),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decide_conditional(
        &self,
        account: AccountId,
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        qty: Qty,
        trigger: Trigger,
        kind: OrderKind,
        options: OrderOptions,
        market: &MarketCache,
    ) -> Result<Fact, String> {
        let (role, last) =
            self.arm_checks(account, symbol, side, qty, &trigger, Shape::Single, market)?;
        Profile::permits(role, kind, &options)?;
        Ok(Fact::ConditionalArmed(vec![Conditional {
            cond_id: self.conditionals.next_id(),
            account,
            client_order_id,
            symbol,
            side,
            qty,
            trigger,
            kind,
            options,
            group: 0,
            extreme: last,
        }]))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decide_oco(
        &self,
        account: AccountId,
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        qty: Qty,
        take_profit: Price,
        stop: Price,
        stop_limit: Option<Price>,
        market: &MarketCache,
    ) -> Result<Fact, String> {
        let ordered = match side {
            Side::Sell => take_profit > stop,
            Side::Buy => take_profit < stop,
        };
        if !ordered {
            return Err(format!(
                "a {} OCO needs its take-profit {} the stop",
                side_label(side),
                if side == Side::Sell { "above" } else { "below" }
            ));
        }
        let legs = exit_legs(side, take_profit, stop, stop_limit);
        let (_, last) =
            self.arm_checks(account, symbol, side, qty, &legs[1].0, Shape::Oco, market)?;
        let group = self.conditionals.next_id();
        Ok(Fact::ConditionalArmed(
            legs.into_iter()
                .enumerate()
                .map(|(i, (trigger, kind))| Conditional {
                    cond_id: group + i as u64,
                    account,
                    client_order_id,
                    symbol,
                    side,
                    qty,
                    trigger,
                    kind,
                    options: OrderOptions::default(),
                    group,
                    extreme: last,
                })
                .collect(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decide_bracket(
        &self,
        account: AccountId,
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        qty: Qty,
        entry: OrderKind,
        take_profit: Price,
        stop_loss: Price,
        market: &MarketCache,
        now: Timestamp,
    ) -> Result<Fact, Box<ExecutionReport>> {
        let order = self.decide_order(
            account,
            client_order_id,
            symbol,
            side,
            entry,
            OrderOptions::default(),
            qty,
            market,
            now,
        )?;
        let reject = |reason: String| {
            rejection(
                account,
                client_order_id,
                symbol,
                side,
                entry,
                qty,
                now,
                reason,
            )
        };
        let role = self.ledger.get(account).map(|a| a.role);
        if let Some(role) = role
            && let Err(reason) =
                Profile::permits_conditional(role, &Trigger::Below(stop_loss), Shape::Bracket)
        {
            return Err(reject(reason));
        }
        let (low, high) = match side {
            Side::Buy => (stop_loss, take_profit),
            Side::Sell => (take_profit, stop_loss),
        };
        let inside = match entry {
            OrderKind::Limit(p) => low < p && p < high,
            OrderKind::Market => low < high,
        };
        if stop_loss == 0 || take_profit == 0 || !inside {
            return Err(reject(format!(
                "a {} bracket needs stop-loss {} entry {} take-profit",
                side_label(side),
                if side == Side::Buy { "<" } else { ">" },
                if side == Side::Buy { "<" } else { ">" }
            )));
        }
        Ok(Fact::BracketAccepted {
            order: Box::new(order),
            take_profit,
            stop_loss,
        })
    }

    pub fn decide_cancel_conditional(
        &self,
        account: AccountId,
        cond_id: u64,
    ) -> Result<Fact, String> {
        let conditional = self
            .conditionals
            .get(cond_id)
            .ok_or_else(|| format!("conditional #{cond_id} is not armed"))?;
        if conditional.account != account {
            return Err(format!("conditional #{cond_id} is not yours"));
        }
        Ok(Fact::ConditionalCancelled {
            cond_id,
            reason: "cancelled by the client".into(),
        })
    }

    pub fn scan_conditionals(&self, symbol: Symbol, last: Price) -> (Vec<Fact>, Vec<u64>) {
        let (trails, due) = self.conditionals.scan(symbol, last);
        (
            trails
                .into_iter()
                .map(|(cond_id, extreme)| Fact::TrailMoved { cond_id, extreme })
                .collect(),
            due,
        )
    }

    pub fn waiting_for_open(&self) -> Vec<u64> {
        self.conditionals.waiting_for_open()
    }

    pub fn decide_trigger(
        &self,
        cond_id: u64,
        price: Price,
        market: &MarketCache,
        now: Timestamp,
    ) -> Option<Fact> {
        let c = self.conditionals.get(cond_id)?;
        let (order, reason) = match self.decide_order(
            c.account,
            c.client_order_id,
            c.symbol,
            c.side,
            c.kind,
            c.options,
            c.qty,
            market,
            now,
        ) {
            Ok(fact) => (Some(Box::new(fact)), String::new()),
            Err(report) => (None, report.reason),
        };
        Some(Fact::ConditionalTriggered {
            cond_id,
            price,
            order,
            reason,
        })
    }

    pub fn conditional_reports(&self, account: AccountId) -> Vec<ConditionalReport> {
        self.conditionals
            .for_account(account)
            .map(|c| c.report(ConditionalStatus::Armed, ""))
            .collect()
    }

    pub fn armed_conditionals(&self) -> usize {
        self.conditionals.len()
    }

    fn cancel_group(&mut self, group: u64, reason: &str) -> Vec<Effect> {
        self.conditionals
            .group(group)
            .into_iter()
            .filter_map(|id| self.conditionals.remove(id))
            .map(|c| conditional_update(&c, ConditionalStatus::Cancelled, reason))
            .collect()
    }

    fn bracket_fill(
        &mut self,
        order_ref: u64,
        (account, client_order_id, symbol, side, qty): (AccountId, u64, Symbol, Side, Qty),
    ) -> Vec<Effect> {
        let Some(plan) = self.brackets.get(&order_ref).copied() else {
            return vec![];
        };
        if let Some(group) = plan.group {
            let grown: Vec<Effect> = self
                .conditionals
                .grow(group, qty)
                .into_iter()
                .map(|c| conditional_update(c, ConditionalStatus::Armed, "bracket exit grew"))
                .collect();
            if !grown.is_empty() {
                return grown;
            }
        }
        let exit = match side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        let group = self.conditionals.next_id();
        let mut effects = Vec::new();
        for (i, (trigger, kind)) in exit_legs(exit, plan.take_profit, plan.stop_loss, None)
            .into_iter()
            .enumerate()
        {
            let c = Conditional {
                cond_id: group + i as u64,
                account,
                client_order_id,
                symbol,
                side: exit,
                qty,
                trigger,
                kind,
                options: OrderOptions::default(),
                group,
                extreme: 0,
            };
            effects.push(conditional_update(
                &c,
                ConditionalStatus::Armed,
                "bracket exit armed",
            ));
            self.conditionals.insert(c);
        }
        if let Some(plan) = self.brackets.get_mut(&order_ref) {
            plan.group = Some(group);
        }
        effects
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decide_algo(
        &self,
        account: AccountId,
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        qty: Qty,
        limit: Option<Price>,
        strategy: AlgoStrategy,
        market: &MarketCache,
        now: Timestamp,
    ) -> Result<Fact, String> {
        let holder = self
            .ledger
            .get(account)
            .ok_or_else(|| format!("unknown account {account}"))?;
        if !matches!(holder.role, Role::Quant | Role::Prop) {
            return Err(format!(
                "{} accounts may not use algorithmic orders",
                holder.role.label()
            ));
        }
        self.risk
            .check(account, symbol, side, |s| mark_of(market, s))?;
        let quote = market
            .get(symbol)
            .ok_or_else(|| format!("unknown symbol {symbol}"))?;
        if qty == 0 {
            return Err("quantity must be positive".into());
        }
        match strategy {
            AlgoStrategy::Twap {
                duration_ms,
                slices,
            } if duration_ms < 1_000 || slices == 0 || slices > 1_000 => {
                return Err("TWAP needs at least 1 second and 1 - 1000 slices".into());
            }
            AlgoStrategy::Vwap { duration_ms } if duration_ms < 1_000 => {
                return Err("VWAP needs at least 1 second".into());
            }
            AlgoStrategy::Pov {
                rate_bps,
                duration_ms,
            } if rate_bps == 0 || rate_bps > 5_000 || duration_ms < 1_000 => {
                return Err("POV needs a rate of 0.01% - 50% and at least 1 second".into());
            }
            AlgoStrategy::Iceberg { clip } if clip == 0 || limit.is_none() => {
                return Err("an iceberg slicer needs a clip size and a limit price".into());
            }
            _ => {}
        }
        if let (Some(limit), Some((low, high))) = (limit, market.band(symbol))
            && (limit < low || limit > high)
        {
            return Err(format!(
                "limit {} outside band {} - {}",
                ome_core::price::format_price(limit),
                ome_core::price::format_price(low),
                ome_core::price::format_price(high)
            ));
        }
        let held = holder.position(symbol).total();
        if side == Side::Sell && held < qty {
            return Err(format!(
                "insufficient {symbol} shares: need {qty}, holding {held}"
            ));
        }
        Ok(Fact::AlgoStarted(Algo {
            algo_id: self.next_algo_id,
            account,
            client_order_id,
            symbol,
            side,
            qty,
            limit,
            strategy,
            started_at: now,
            volume_base: quote.info.volume,
            filled: 0,
            notional: 0,
            children: 0,
            child: None,
        }))
    }

    pub fn decide_cancel_algo(&self, account: AccountId, algo_id: u64) -> Result<Fact, String> {
        let algo = self
            .algos
            .get(&algo_id)
            .ok_or_else(|| format!("algo #{algo_id} is not working"))?;
        if algo.account != account {
            return Err(format!("algo #{algo_id} is not yours"));
        }
        Ok(Fact::AlgoFinished {
            algo_id,
            status: AlgoStatus::Cancelled,
            reason: "cancelled by the client".into(),
        })
    }

    pub fn working_algos(&self) -> Vec<(u64, Timestamp)> {
        self.algos
            .values()
            .map(|a| (a.algo_id, a.check_interval()))
            .collect()
    }

    pub fn decide_algo_step(
        &self,
        algo_id: u64,
        market: &MarketCache,
        now: Timestamp,
    ) -> Option<Fact> {
        let algo = self.algos.get(&algo_id)?;
        if algo.child.is_none() && algo.expired(now) {
            return Some(Fact::AlgoFinished {
                algo_id,
                status: AlgoStatus::Expired,
                reason: format!("time is up with {} of {} filled", algo.filled, algo.qty),
            });
        }
        if market.phase() != MarketPhase::Open || !market.is_trading(algo.symbol) {
            return None;
        }
        let volume = market.get(algo.symbol)?.info.volume;
        let (kind, options, qty) = algo.next_child(now, volume)?;
        let role = self.ledger.get(algo.account)?.role;
        let qty = qty.min(Profile::limits(role).max_qty);
        Some(
            match self.decide_order(
                algo.account,
                algo.client_order_id,
                algo.symbol,
                algo.side,
                kind,
                options,
                qty,
                market,
                now,
            ) {
                Ok(order) => Fact::AlgoChild {
                    algo_id,
                    order: Box::new(order),
                },
                Err(report) => Fact::AlgoFinished {
                    algo_id,
                    status: AlgoStatus::Rejected,
                    reason: format!("child order rejected: {}", report.reason),
                },
            },
        )
    }

    pub fn algo_reports(&self, account: AccountId) -> Vec<AlgoReport> {
        self.algos
            .values()
            .filter(|a| a.account == account)
            .map(|a| a.report(AlgoStatus::Working, ""))
            .collect()
    }

    fn finish_algo(&mut self, algo_id: u64, status: AlgoStatus, reason: &str) -> Vec<Effect> {
        let Some(algo) = self.algos.remove(&algo_id) else {
            return vec![];
        };
        let mut effects = Vec::new();
        if let Some(id) = algo
            .child
            .and_then(|r| self.orders.get(&r))
            .and_then(|m| m.exchange_id)
        {
            effects.push(Effect::ToExchange(MemberMessage::Cancel { order_id: id }));
        }
        effects.push(algo_update(&algo, status, reason));
        effects
    }

    fn algo_fill(&mut self, order_ref: u64, fill: Option<(Price, Qty)>, done: bool) -> Vec<Effect> {
        let Some(&algo_id) = self.algo_children.get(&order_ref) else {
            return vec![];
        };
        if done {
            self.algo_children.remove(&order_ref);
        }
        let Some(algo) = self.algos.get_mut(&algo_id) else {
            let orphan = self
                .orders
                .get(&order_ref)
                .and_then(|m| m.exchange_id)
                .filter(|_| !done);
            return orphan
                .map(|order_id| vec![Effect::ToExchange(MemberMessage::Cancel { order_id })])
                .unwrap_or_default();
        };
        if done && algo.child == Some(order_ref) {
            algo.child = None;
        }
        let Some((price, qty)) = fill else {
            return vec![];
        };
        algo.filled += qty;
        algo.notional += price as u128 * qty as u128;
        if algo.filled >= algo.qty {
            let mut effects = self.finish_algo(algo_id, AlgoStatus::Done, "fully filled");
            effects.retain(|e| !matches!(e, Effect::ToExchange(_)));
            effects
        } else {
            vec![algo_update(algo, AlgoStatus::Working, "")]
        }
    }

    pub fn decide_quote(
        &self,
        account: AccountId,
        symbol: Symbol,
        bid: QuoteSide,
        ask: QuoteSide,
        market: &MarketCache,
    ) -> Result<Fact, String> {
        let holder = self
            .ledger
            .get(account)
            .ok_or_else(|| format!("unknown account {account}"))?;
        if !matches!(holder.role, Role::Hft | Role::LpDesk) {
            return Err(format!(
                "{} accounts may not send two-sided quotes",
                holder.role.label()
            ));
        }
        if market.phase() == MarketPhase::Closed {
            return Err("market is closed".into());
        }
        if !market.is_trading(symbol) {
            return Err(format!("{symbol} is not trading"));
        }
        let side = if bid.is_empty() {
            Side::Sell
        } else {
            Side::Buy
        };
        self.risk
            .check(account, symbol, side, |s| mark_of(market, s))?;
        if !bid.is_empty() && !ask.is_empty() && bid.price >= ask.price {
            return Err("quote bid must be below its ask".into());
        }
        let (low, high) = market
            .band(symbol)
            .ok_or_else(|| format!("no price band for {symbol}"))?;
        let limits = Profile::limits(holder.role);
        for side in [bid, ask].iter().filter(|s| !s.is_empty()) {
            if side.price < low || side.price > high {
                return Err(format!(
                    "quote price {} outside band {} - {}",
                    ome_core::price::format_price(side.price),
                    ome_core::price::format_price(low),
                    ome_core::price::format_price(high)
                ));
            }
            if side.qty > limits.max_qty {
                return Err(format!(
                    "quote quantity {} exceeds max {}",
                    side.qty, limits.max_qty
                ));
            }
        }
        let bid_leg = (!bid.is_empty()).then(|| {
            let fee_reserve = self
                .fees
                .reserve(holder.role, Side::Buy, bid.price, bid.qty);
            QuoteLeg {
                order_ref: self.next_order_ref,
                price: bid.price,
                qty: bid.qty,
                reserved: bid.price.saturating_mul(bid.qty),
                fee_reserve,
            }
        });
        if let Some(leg) = bid_leg
            && holder.cash.available < leg.reserved + leg.fee_reserve
        {
            return Err("insufficient cash for the bid side of the quote".into());
        }
        let ask_leg = (!ask.is_empty()).then(|| QuoteLeg {
            order_ref: self.next_order_ref + 1,
            price: ask.price,
            qty: ask.qty,
            reserved: ask.qty,
            fee_reserve: 0,
        });
        if ask_leg.is_some_and(|leg| holder.position(symbol).available < leg.qty) {
            return Err(format!(
                "insufficient {symbol} shares for the ask side of the quote"
            ));
        }
        Ok(Fact::QuoteAccepted {
            account,
            symbol,
            bid: bid_leg,
            ask: ask_leg,
        })
    }

    pub fn decide_mmp(
        &self,
        account: AccountId,
        window_ms: u64,
        max_qty: Qty,
    ) -> Result<MemberMessage, String> {
        let holder = self
            .ledger
            .get(account)
            .ok_or_else(|| format!("unknown account {account}"))?;
        if !matches!(holder.role, Role::Hft | Role::LpDesk) {
            return Err(format!(
                "{} accounts do not use market maker protection",
                holder.role.label()
            ));
        }
        Ok(MemberMessage::SetMmp {
            account,
            window_ms,
            max_qty,
        })
    }

    pub fn decide_cancel(
        &self,
        account: AccountId,
        order_id: OrderId,
    ) -> Result<(Venue, MemberMessage), String> {
        let meta = self
            .by_exchange
            .get(&order_id)
            .and_then(|r| self.orders.get(r))
            .ok_or_else(|| format!("order #{order_id} is not open"))?;
        if meta.account != account {
            return Err(format!("order #{order_id} is not yours"));
        }
        Ok((meta.venue, MemberMessage::Cancel { order_id }))
    }

    pub fn venue_orders(
        &self,
        account: AccountId,
        symbol: Option<Symbol>,
    ) -> Vec<(Venue, OrderId)> {
        self.orders
            .values()
            .filter(|m| m.account == account && m.venue == Venue::Ats)
            .filter(|m| symbol.is_none_or(|s| s == m.symbol))
            .filter_map(|m| m.exchange_id.map(|id| (m.venue, id)))
            .collect()
    }

    pub fn last_venue_seq(&self, venue: Venue) -> u64 {
        self.venue_seqs.get(&venue).copied().unwrap_or(0)
    }

    pub fn is_new_from(&self, venue: Venue, seq: u64) -> bool {
        seq > self.last_venue_seq(venue)
    }

    pub fn venue_charges(&self, report: &ExecutionReport) -> Charges {
        self.charges_with(report, 0)
    }

    pub fn route_held(&self, order_ref: u64, wholesaler_ready: bool) -> Fact {
        let retail = self
            .orders
            .get(&order_ref)
            .and_then(|m| self.ledger.get(m.account))
            .is_some_and(|a| a.role == Role::Retail);
        if wholesaler_ready && retail && self.profile.kind == crate::profile::ProfileKind::Retail {
            Fact::RoutedToVenue {
                order_ref,
                venue: Venue::Wholesaler,
            }
        } else {
            Fact::Released { order_ref }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decide_dark(
        &self,
        account: AccountId,
        client_order_id: u64,
        symbol: Symbol,
        side: Side,
        qty: Qty,
        limit: Option<Price>,
        min_qty: Qty,
        market: &MarketCache,
        now: Timestamp,
    ) -> Result<Fact, Box<ExecutionReport>> {
        let kind = limit.map_or(OrderKind::Market, OrderKind::Limit);
        let reject = |reason: &str| {
            rejection(
                account,
                client_order_id,
                symbol,
                side,
                kind,
                qty,
                now,
                reason.to_string(),
            )
        };
        let role = self.ledger.get(account).map(|a| a.role);
        if !matches!(role, Some(Role::Quant | Role::Prop)) {
            return Err(reject("dark orders are for quant and prop accounts"));
        }
        if !self.profile.internalize {
            return Err(reject(
                "off-exchange venues are not allowed under this market regime",
            ));
        }
        if min_qty > qty {
            return Err(reject("minimum quantity cannot exceed the order quantity"));
        }
        let Some((low, high)) = market.band(symbol) else {
            return Err(reject("unknown symbol"));
        };
        let reserve_at = limit.unwrap_or(if side == Side::Buy { high } else { low });
        let order = self.decide_order(
            account,
            client_order_id,
            symbol,
            side,
            OrderKind::Limit(reserve_at),
            OrderOptions::default(),
            qty,
            market,
            now,
        )?;
        Ok(Fact::DarkAccepted {
            order: Box::new(order),
            limit,
            min_qty,
        })
    }

    pub fn hold_for_crossing(&self, fact: Fact, market: &MarketCache) -> Fact {
        let Fact::OrderAccepted {
            symbol,
            side,
            price,
            options,
            ..
        } = &fact
        else {
            return fact;
        };
        let eligible_tif = matches!(
            options.tif,
            TimeInForce::Gtc | TimeInForce::Day | TimeInForce::Ioc | TimeInForce::Gtd(_)
        );
        let marketable = market.best(*symbol).is_some_and(|(bid, ask)| match side {
            Side::Buy => *price >= ask,
            Side::Sell => *price <= bid,
        });
        if self.profile.internalize
            && eligible_tif
            && !options.post_only
            && options.display_qty.is_none()
            && market.phase() == MarketPhase::Open
            && market.is_trading(*symbol)
            && marketable
        {
            Fact::OrderHeld(Box::new(fact))
        } else {
            fact
        }
    }

    pub fn held_orders(&self) -> Vec<u64> {
        self.held.keys().copied().collect()
    }

    pub fn next_cross(&self, market: &MarketCache, now: Timestamp) -> Option<Fact> {
        let open = |r: &u64| {
            self.orders
                .get(r)
                .filter(|m| m.filled < m.qty)
                .map(|m| (*r, m))
        };
        let held: Vec<(u64, &OrderMeta)> = self.held.keys().filter_map(open).collect();
        for (buy_ref, buy) in held.iter().filter(|(_, m)| m.side == Side::Buy) {
            if market.phase() != MarketPhase::Open || !market.is_trading(buy.symbol) {
                continue;
            }
            let Some((bid, ask)) = market.best(buy.symbol) else {
                continue;
            };
            let mid = (bid + ask) / 2;
            if buy.price < mid {
                continue;
            }
            let Some((sell_ref, sell)) = held.iter().find(|(_, s)| {
                s.side == Side::Sell
                    && s.symbol == buy.symbol
                    && s.account != buy.account
                    && s.price <= mid
            }) else {
                continue;
            };
            let qty = (buy.qty - buy.filled).min(sell.qty - sell.filled);
            let charges = |meta: &OrderMeta| {
                let role = self
                    .ledger
                    .get(meta.account)
                    .map_or(Role::Retail, |a| a.role);
                self.fees
                    .charges(role, meta.side, mid, qty, 0, meta.commission_paid)
            };
            return Some(Fact::InternalCross(Cross {
                cross_id: self.next_cross_id,
                buy_ref: *buy_ref,
                sell_ref: *sell_ref,
                price: mid,
                qty,
                cycle: self.cycle,
                buy_charges: charges(buy),
                sell_charges: charges(sell),
                timestamp: now,
            }));
        }
        None
    }

    fn cross_leg(&mut self, order_ref: u64, cross: &Cross, charges: &Charges) -> Vec<Effect> {
        let Some(meta) = self.orders.get(&order_ref) else {
            return vec![];
        };
        let filled = meta.filled + cross.qty;
        let report = ExecutionReport {
            seq: 0,
            order_id: 0,
            client_order_id: order_ref,
            account: meta.account,
            symbol: meta.symbol,
            side: meta.side,
            status: if filled >= meta.qty {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            },
            price: meta.price,
            qty: meta.qty,
            filled,
            leaves: meta.qty - filled,
            last_price: cross.price,
            last_qty: cross.qty,
            fees: 0,
            cycle: cross.cycle,
            timestamp: cross.timestamp,
            reason: "internal cross".into(),
        };
        if filled >= meta.qty {
            self.held.remove(&order_ref);
        }
        self.apply_report(&report, charges)
    }

    pub fn pending_orders(&self, venue: Venue) -> Vec<u64> {
        self.orders
            .iter()
            .filter(|(r, _)| !self.held.contains_key(r))
            .filter(|(_, m)| m.venue == venue)
            .filter(|(_, m)| m.exchange_id.is_none())
            .map(|(r, _)| *r)
            .collect()
    }

    pub fn apply(&mut self, fact: &Fact) -> Vec<Effect> {
        match fact {
            Fact::AccountOpened {
                role,
                username,
                secret,
                cash,
            } => match self.ledger.open(username, *role, *secret, *cash) {
                Ok(id) => vec![Effect::AccountOpened(id)],
                Err(_) => vec![],
            },
            Fact::BalanceSet {
                account,
                asset,
                amount,
            } => {
                let result = match asset {
                    Asset::Cash => self.ledger.set_cash(*account, *amount),
                    Asset::Shares(symbol) => self.ledger.set_shares(*account, *symbol, *amount),
                };
                result
                    .map(|_| vec![Effect::AccountChanged(*account)])
                    .unwrap_or_default()
            }
            Fact::OrderAccepted {
                order_ref,
                account,
                client_order_id,
                symbol,
                side,
                price,
                options,
                qty,
                reserved,
                fee_reserve,
            } => {
                let reserved_ok = match side {
                    Side::Buy => self
                        .ledger
                        .reserve_cash(*account, reserved + fee_reserve)
                        .is_ok(),
                    Side::Sell => self.ledger.reserve_shares(*account, *symbol, *qty).is_ok(),
                };
                self.next_order_ref = self.next_order_ref.max(order_ref + 1);
                if !reserved_ok {
                    return vec![];
                }
                self.orders.insert(
                    *order_ref,
                    OrderMeta {
                        account: *account,
                        client_order_id: *client_order_id,
                        symbol: *symbol,
                        side: *side,
                        price: *price,
                        qty: *qty,
                        filled: 0,
                        reserved: *reserved,
                        fee_reserve: *fee_reserve,
                        commission_paid: 0,
                        exchange_id: None,
                        venue: Venue::Exchange,
                        options: *options,
                    },
                );
                self.fee_totals.routed_orders += 1;
                vec![
                    Effect::AccountChanged(*account),
                    Effect::ToExchange(MemberMessage::NewOrder {
                        member_order_id: *order_ref,
                        account: *account,
                        symbol: *symbol,
                        side: *side,
                        kind: OrderKind::Limit(*price),
                        options: *options,
                        qty: *qty,
                    }),
                ]
            }
            Fact::QuoteAccepted {
                account,
                symbol,
                bid,
                ask,
            } => {
                let mut sides = [QuoteSide::default(), QuoteSide::default()];
                let mut refs = [0, 0];
                for (slot, (side, leg)) in [(Side::Buy, bid), (Side::Sell, ask)]
                    .into_iter()
                    .enumerate()
                {
                    let Some(leg) = leg else {
                        continue;
                    };
                    self.next_order_ref = self.next_order_ref.max(leg.order_ref + 1);
                    let reserved_ok = match side {
                        Side::Buy => self
                            .ledger
                            .reserve_cash(*account, leg.reserved + leg.fee_reserve)
                            .is_ok(),
                        Side::Sell => self
                            .ledger
                            .reserve_shares(*account, *symbol, leg.qty)
                            .is_ok(),
                    };
                    if !reserved_ok {
                        continue;
                    }
                    self.orders.insert(
                        leg.order_ref,
                        OrderMeta {
                            account: *account,
                            client_order_id: 0,
                            symbol: *symbol,
                            side,
                            price: leg.price,
                            qty: leg.qty,
                            filled: 0,
                            reserved: leg.reserved,
                            fee_reserve: leg.fee_reserve,
                            commission_paid: 0,
                            exchange_id: None,
                            venue: Venue::Exchange,
                            options: OrderOptions::default(),
                        },
                    );
                    sides[slot] = QuoteSide {
                        price: leg.price,
                        qty: leg.qty,
                    };
                    refs[slot] = leg.order_ref;
                }
                vec![
                    Effect::AccountChanged(*account),
                    Effect::ToExchange(MemberMessage::Quote {
                        account: *account,
                        symbol: *symbol,
                        bid_ref: refs[0],
                        bid: sides[0],
                        ask_ref: refs[1],
                        ask: sides[1],
                    }),
                ]
            }
            Fact::ExchangeReport { report, charges } => {
                self.last_seq = self.last_seq.max(report.seq);
                self.cycle = self.cycle.max(report.cycle);
                self.apply_report(report, charges)
            }
            Fact::Allocated(allocation) => {
                self.last_seq = self.last_seq.max(allocation.seq);
                if !self.allocations.insert(allocation.allocation_id) {
                    return vec![];
                }
                match self.ledger.credit_shares(
                    allocation.account,
                    allocation.symbol,
                    allocation.shares,
                ) {
                    Ok(()) => vec![Effect::AccountChanged(allocation.account)],
                    Err(_) => vec![],
                }
            }
            Fact::SettlementReceived(report) => {
                self.last_settlement_seq = self.last_settlement_seq.max(report.seq);
                let settled: Vec<(u64, AccountId)> = self
                    .pending
                    .range(..=(report.cycle, AccountId::MAX))
                    .map(|(key, _)| *key)
                    .collect();
                let mut accounts = BTreeSet::new();
                for key in settled {
                    self.pending.remove(&key);
                    accounts.insert(key.1);
                }
                accounts.into_iter().map(Effect::AccountChanged).collect()
            }
            Fact::ConditionalArmed(legs) => legs
                .iter()
                .map(|c| {
                    self.conditionals.insert(c.clone());
                    conditional_update(c, ConditionalStatus::Armed, "")
                })
                .collect(),
            Fact::TrailMoved { cond_id, extreme } => self
                .conditionals
                .set_extreme(*cond_id, *extreme)
                .map(|c| {
                    vec![conditional_update(
                        c,
                        ConditionalStatus::Armed,
                        "trail moved",
                    )]
                })
                .unwrap_or_default(),
            Fact::ConditionalTriggered {
                cond_id,
                price,
                order,
                reason,
            } => {
                let Some(c) = self.conditionals.remove(*cond_id) else {
                    return vec![];
                };
                let mut effects = self.cancel_group(c.group, "the other leg triggered");
                let note = format!("triggered at {}", ome_core::price::format_price(*price));
                match order {
                    Some(order) => {
                        effects.push(conditional_update(&c, ConditionalStatus::Triggered, &note));
                        effects.extend(self.apply(order));
                    }
                    None => effects.push(conditional_update(
                        &c,
                        ConditionalStatus::Rejected,
                        &format!("{note}, order rejected: {reason}"),
                    )),
                }
                effects
            }
            Fact::ConditionalCancelled { cond_id, reason } => {
                let Some(c) = self.conditionals.remove(*cond_id) else {
                    return vec![];
                };
                let mut effects = self.cancel_group(c.group, reason);
                effects.push(conditional_update(&c, ConditionalStatus::Cancelled, reason));
                effects
            }
            Fact::BracketAccepted {
                order,
                take_profit,
                stop_loss,
            } => {
                let effects = self.apply(order);
                if let Fact::OrderAccepted { order_ref, .. } = order.as_ref()
                    && self.orders.contains_key(order_ref)
                {
                    self.brackets.insert(
                        *order_ref,
                        BracketPlan {
                            take_profit: *take_profit,
                            stop_loss: *stop_loss,
                            group: None,
                        },
                    );
                }
                effects
            }
            Fact::OrderHeld(order) => {
                let mut effects = self.apply(order);
                effects.retain(|e| !matches!(e, Effect::ToExchange(_)));
                if let Fact::OrderAccepted {
                    order_ref, options, ..
                } = order.as_ref()
                    && self.orders.contains_key(order_ref)
                {
                    self.fee_totals.routed_orders -= 1;
                    self.held.insert(*order_ref, *options);
                }
                effects
            }
            Fact::InternalCross(cross) => {
                self.next_cross_id = self.next_cross_id.max(cross.cross_id + 1);
                let Some(symbol) = self.orders.get(&cross.buy_ref).map(|m| m.symbol) else {
                    return vec![];
                };
                if !self.orders.contains_key(&cross.sell_ref) {
                    return vec![];
                }
                let mut effects = self.cross_leg(cross.buy_ref, cross, &cross.buy_charges);
                effects.extend(self.cross_leg(cross.sell_ref, cross, &cross.sell_charges));
                self.fee_totals.internal_crosses += 1;
                self.fee_totals.internalized_shares += cross.qty;
                effects.push(Effect::ToExchange(MemberMessage::TradeReport {
                    report_id: cross.cross_id,
                    symbol,
                    price: cross.price,
                    qty: cross.qty,
                    venue: Venue::Internal,
                    buy_member: 0,
                    sell_member: 0,
                }));
                effects
            }
            Fact::RoutedToVenue { order_ref, venue } => {
                self.held.remove(order_ref);
                let Some(meta) = self.orders.get_mut(order_ref) else {
                    return vec![];
                };
                meta.venue = *venue;
                self.fee_totals.wholesaler_orders += 1;
                vec![Effect::ToVenue(
                    *venue,
                    MemberMessage::NewOrder {
                        member_order_id: *order_ref,
                        account: meta.account,
                        symbol: meta.symbol,
                        side: meta.side,
                        kind: OrderKind::Limit(meta.price),
                        options: OrderOptions::tif(TimeInForce::Ioc),
                        qty: meta.qty - meta.filled,
                    },
                )]
            }
            Fact::VenueReport {
                venue,
                report,
                charges,
            } => {
                let seq = self.venue_seqs.entry(*venue).or_default();
                *seq = (*seq).max(report.seq);
                if *venue == Venue::Wholesaler && report.status == OrderStatus::Rejected {
                    let Some(meta) = self.orders.get_mut(&report.client_order_id) else {
                        return vec![];
                    };
                    meta.venue = Venue::Exchange;
                    self.fee_totals.routed_orders += 1;
                    return vec![Effect::ToExchange(MemberMessage::NewOrder {
                        member_order_id: report.client_order_id,
                        account: meta.account,
                        symbol: meta.symbol,
                        side: meta.side,
                        kind: OrderKind::Limit(meta.price),
                        options: meta.options,
                        qty: meta.qty - meta.filled,
                    })];
                }
                if report.last_qty > 0 {
                    match venue {
                        Venue::Wholesaler => {
                            self.fee_totals.wholesaler_shares += report.last_qty;
                            self.fee_totals.pfof_earned -= report.fees;
                        }
                        Venue::Ats => self.fee_totals.dark_shares += report.last_qty,
                        _ => {}
                    }
                }
                let report = ExecutionReport {
                    cycle: report.cycle.max(self.cycle),
                    fees: 0,
                    ..report.clone()
                };
                self.apply_report(&report, charges)
            }
            Fact::DarkAccepted {
                order,
                limit,
                min_qty,
            } => {
                let mut effects = self.apply(order);
                effects.retain(|e| !matches!(e, Effect::ToExchange(_)));
                let Fact::OrderAccepted { order_ref, .. } = order.as_ref() else {
                    return effects;
                };
                let Some(meta) = self.orders.get_mut(order_ref) else {
                    return effects;
                };
                meta.venue = Venue::Ats;
                self.fee_totals.routed_orders -= 1;
                effects.push(Effect::ToVenue(
                    Venue::Ats,
                    MemberMessage::DarkOrder {
                        member_order_id: *order_ref,
                        account: meta.account,
                        symbol: meta.symbol,
                        side: meta.side,
                        limit: *limit,
                        qty: meta.qty,
                        min_qty: *min_qty,
                    },
                ));
                effects
            }
            Fact::Released { order_ref } => {
                let Some(options) = self.held.remove(order_ref) else {
                    return vec![];
                };
                let Some(meta) = self.orders.get(order_ref) else {
                    return vec![];
                };
                self.fee_totals.routed_orders += 1;
                vec![Effect::ToExchange(MemberMessage::NewOrder {
                    member_order_id: *order_ref,
                    account: meta.account,
                    symbol: meta.symbol,
                    side: meta.side,
                    kind: OrderKind::Limit(meta.price),
                    options,
                    qty: meta.qty - meta.filled,
                })]
            }
            Fact::AlgoStarted(algo) => {
                self.next_algo_id = self.next_algo_id.max(algo.algo_id + 1);
                self.algos.insert(algo.algo_id, algo.clone());
                vec![algo_update(algo, AlgoStatus::Working, "started")]
            }
            Fact::AlgoChild { algo_id, order } => {
                let effects = self.apply(order);
                if let Fact::OrderAccepted { order_ref, .. } = order.as_ref()
                    && self.orders.contains_key(order_ref)
                    && let Some(algo) = self.algos.get_mut(algo_id)
                {
                    algo.child = Some(*order_ref);
                    algo.children += 1;
                    self.algo_children.insert(*order_ref, *algo_id);
                }
                effects
            }
            Fact::AlgoFinished {
                algo_id,
                status,
                reason,
            } => self.finish_algo(*algo_id, *status, reason),
            Fact::AccountStatusSet { account, suspended } => {
                self.risk.set_suspended(*account, *suspended);
                if !*suspended {
                    return vec![Effect::AccountChanged(*account)];
                }
                let algos: Vec<u64> = self
                    .algos
                    .values()
                    .filter(|a| a.account == *account)
                    .map(|a| a.algo_id)
                    .collect();
                let mut stopped: Vec<Effect> = algos
                    .into_iter()
                    .flat_map(|id| self.finish_algo(id, AlgoStatus::Cancelled, "account suspended"))
                    .filter(|e| !matches!(e, Effect::ToExchange(_)))
                    .collect();
                let ids: Vec<u64> = self
                    .conditionals
                    .for_account(*account)
                    .map(|c| c.cond_id)
                    .collect();
                let mut effects: Vec<Effect> = ids
                    .into_iter()
                    .filter_map(|id| self.conditionals.remove(id))
                    .map(|c| {
                        conditional_update(&c, ConditionalStatus::Cancelled, "account suspended")
                    })
                    .collect();
                effects.push(Effect::ToExchange(MemberMessage::CancelAll {
                    account: Some(*account),
                    symbol: None,
                }));
                for (venue, order_id) in self.venue_orders(*account, None) {
                    effects.push(Effect::ToVenue(venue, MemberMessage::Cancel { order_id }));
                }
                effects.push(Effect::AccountChanged(*account));
                effects.append(&mut stopped);
                effects
            }
            Fact::RestrictionSet { symbol, restricted } => {
                self.risk.set_restricted(*symbol, *restricted);
                vec![]
            }
            Fact::LossLimitSet { account, amount } => {
                self.risk.set_loss_limit(*account, *amount);
                vec![Effect::AccountChanged(*account)]
            }
            Fact::TradingDayStarted { day, marks } => {
                self.risk.start_day(*day, marks);
                vec![]
            }
            Fact::OrderAbandoned { order_ref } => {
                self.brackets.remove(order_ref);
                let mut algo_effects = self.algo_fill(*order_ref, None, true);
                let Some(meta) = self.orders.remove(order_ref) else {
                    return vec![];
                };
                self.release(&meta);
                let report = client_report(
                    0,
                    &meta,
                    OrderStatus::Rejected,
                    (0, 0, 0),
                    0,
                    "order lost while the exchange link was down",
                );
                let mut effects = vec![
                    Effect::AccountChanged(meta.account),
                    Effect::ToAccount {
                        account: meta.account,
                        message: ServerMessage::Execution(report),
                    },
                ];
                effects.append(&mut algo_effects);
                effects
            }
        }
    }

    fn order_ref_of(&self, report: &ExecutionReport) -> Option<u64> {
        if report.client_order_id != 0 {
            Some(report.client_order_id)
        } else {
            self.by_exchange.get(&report.order_id).copied()
        }
    }

    pub fn charges_for(&self, report: &ExecutionReport) -> Charges {
        self.charges_with(report, report.fees)
    }

    fn charges_with(&self, report: &ExecutionReport, exchange_fee: i64) -> Charges {
        if report.last_qty == 0 {
            return Charges::default();
        }
        let Some(meta) = self.order_ref_of(report).and_then(|r| self.orders.get(&r)) else {
            return Charges::default();
        };
        let Some(holder) = self.ledger.get(meta.account) else {
            return Charges::default();
        };
        self.fees.charges(
            holder.role,
            meta.side,
            report.last_price,
            report.last_qty,
            exchange_fee,
            meta.commission_paid,
        )
    }

    fn apply_report(&mut self, report: &ExecutionReport, charges: &Charges) -> Vec<Effect> {
        let order_ref = if report.client_order_id != 0 {
            report.client_order_id
        } else if let Some(r) = self.by_exchange.get(&report.order_id) {
            *r
        } else {
            let order_ref = SYNTHETIC_REF | report.order_id;
            if report.status != OrderStatus::New || report.side != Side::Sell {
                return vec![];
            }
            if self
                .ledger
                .reserve_shares(report.account, report.symbol, report.qty)
                .is_err()
            {
                return vec![];
            }
            self.orders.insert(
                order_ref,
                OrderMeta {
                    account: report.account,
                    client_order_id: 0,
                    symbol: report.symbol,
                    side: Side::Sell,
                    price: report.price,
                    qty: report.qty,
                    filled: 0,
                    reserved: report.qty,
                    fee_reserve: 0,
                    commission_paid: 0,
                    exchange_id: None,
                    venue: Venue::Exchange,
                    options: OrderOptions::default(),
                },
            );
            order_ref
        };
        let Some(meta) = self.orders.get_mut(&order_ref) else {
            return vec![];
        };
        if report.order_id != 0 && meta.exchange_id.is_none() {
            meta.exchange_id = Some(report.order_id);
            self.by_exchange.insert(report.order_id, order_ref);
        }

        let mut effects = Vec::new();
        let fill = (report.last_qty > 0).then_some((
            meta.account,
            meta.client_order_id,
            meta.symbol,
            meta.side,
            report.last_qty,
        ));
        if report.last_qty > 0 {
            let qty = report.last_qty;
            let cost = (report.last_price * qty) as i64;
            let remaining = meta.qty - meta.filled;
            meta.filled += qty;
            meta.commission_paid += charges.commission;
            let net_amount = match meta.side {
                Side::Buy => {
                    let consumed = (meta.price * qty).min(meta.reserved);
                    let fee_part = if remaining == 0 {
                        meta.fee_reserve
                    } else {
                        (meta.fee_reserve as u128 * qty.min(remaining) as u128 / remaining as u128)
                            as u64
                    };
                    meta.reserved -= consumed;
                    meta.fee_reserve -= fee_part;
                    let paid = cost + charges.total();
                    self.ledger.settle_buy(
                        meta.account,
                        meta.symbol,
                        consumed + fee_part,
                        paid,
                        qty,
                    );
                    -paid
                }
                Side::Sell => {
                    let consumed = qty.min(meta.reserved);
                    meta.reserved -= consumed;
                    let proceeds = cost - charges.total();
                    self.ledger
                        .settle_sell(meta.account, meta.symbol, consumed, proceeds);
                    proceeds
                }
            };
            self.fee_totals.charges.add(charges);
            self.fee_totals.fills += 1;
            self.risk.on_fill(
                meta.account,
                meta.symbol,
                meta.side,
                (report.last_price, qty),
                charges.total(),
            );
            let pending = self
                .pending
                .entry((report.cycle, meta.account))
                .or_default();
            pending.cash += net_amount;
            let signed = match meta.side {
                Side::Buy => qty as i64,
                Side::Sell => -(qty as i64),
            };
            *pending.shares.entry(meta.symbol).or_default() += signed;
            effects.push(Effect::ToAccount {
                account: meta.account,
                message: ServerMessage::ContractNote(ContractNote {
                    account: meta.account,
                    order_id: report.order_id,
                    symbol: meta.symbol,
                    side: meta.side,
                    qty,
                    price: report.last_price,
                    charges: *charges,
                    net_amount,
                    timestamp: report.timestamp,
                }),
            });
        }

        let account = meta.account;
        let mut client = client_report(
            report.order_id,
            meta,
            report.status,
            (report.last_price, report.last_qty, charges.total()),
            report.timestamp,
            &report.reason,
        );
        client.cycle = report.cycle;
        let bracket = fill
            .map(|f| self.bracket_fill(order_ref, f))
            .unwrap_or_default();
        let algo = self.algo_fill(
            order_ref,
            fill.map(|f| (report.last_price, f.4)),
            !report.status.is_open(),
        );
        if !report.status.is_open() {
            self.brackets.remove(&order_ref);
        }
        if !report.status.is_open()
            && let Some(meta) = self.orders.remove(&order_ref)
        {
            self.release(&meta);
            if let Some(id) = meta.exchange_id {
                self.by_exchange.remove(&id);
            }
        }
        effects.push(Effect::AccountChanged(account));
        effects.push(Effect::ToAccount {
            account,
            message: ServerMessage::Execution(client),
        });
        effects.extend(bracket);
        effects.extend(algo);
        effects
    }

    fn release(&mut self, meta: &OrderMeta) {
        match meta.side {
            Side::Buy => self
                .ledger
                .release_cash(meta.account, meta.reserved + meta.fee_reserve),
            Side::Sell => self
                .ledger
                .release_shares(meta.account, meta.symbol, meta.reserved),
        }
    }

    pub fn account_snapshot(&self, id: AccountId) -> Option<AccountSnapshot> {
        let account = self.ledger.get(id)?;
        let (unsettled_cash, unsettled_shares) = self.unsettled(id);
        Some(AccountSnapshot {
            account_id: id,
            role: account.role,
            cash: account.cash,
            unsettled_cash,
            day_pnl: self.day_pnl(id),
            positions: account
                .positions
                .iter()
                .map(|(symbol, balance)| Position {
                    symbol: *symbol,
                    balance: *balance,
                    unsettled: unsettled_shares.get(symbol).copied().unwrap_or(0),
                })
                .filter(|p| p.balance.total() > 0 || p.unsettled != 0)
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
                suspended: self.risk.is_suspended(a.id),
                loss_limit: self.risk.loss_limit(a.id),
                day_pnl: self.day_pnl(a.id),
                username: a.username.clone(),
            })
            .collect()
    }

    pub fn open_order_reports(&self, account: AccountId, now: Timestamp) -> Vec<ExecutionReport> {
        self.orders
            .values()
            .filter(|m| m.account == account)
            .filter_map(|m| {
                let status = if m.filled > 0 {
                    OrderStatus::PartiallyFilled
                } else {
                    OrderStatus::New
                };
                m.exchange_id
                    .map(|id| client_report(id, m, status, (0, 0, 0), now, ""))
            })
            .collect()
    }
}

fn algo_update(algo: &Algo, status: AlgoStatus, reason: &str) -> Effect {
    Effect::ToAccount {
        account: algo.account,
        message: ServerMessage::AlgoUpdate(algo.report(status, reason)),
    }
}

fn mark_of(market: &MarketCache, symbol: Symbol) -> Option<Price> {
    market
        .get(symbol)
        .map(|q| q.info.last_price.unwrap_or(q.info.reference_price))
}

fn side_label(side: Side) -> &'static str {
    match side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

fn conditional_update(c: &Conditional, status: ConditionalStatus, reason: &str) -> Effect {
    Effect::ToAccount {
        account: c.account,
        message: ServerMessage::ConditionalUpdate(c.report(status, reason)),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn rejection(
    account: AccountId,
    client_order_id: u64,
    symbol: Symbol,
    side: Side,
    kind: OrderKind,
    qty: Qty,
    now: Timestamp,
    reason: String,
) -> Box<ExecutionReport> {
    Box::new(ExecutionReport {
        seq: 0,
        order_id: 0,
        client_order_id,
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
        cycle: 0,
        timestamp: now,
        reason,
    })
}

fn client_report(
    order_id: OrderId,
    meta: &OrderMeta,
    status: OrderStatus,
    last: (Price, Qty, i64),
    timestamp: Timestamp,
    reason: &str,
) -> ExecutionReport {
    ExecutionReport {
        seq: 0,
        order_id,
        client_order_id: meta.client_order_id,
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
        cycle: 0,
        timestamp,
        reason: reason.to_string(),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::profile::ProfileKind;
    use ome_net::auth::hash_secret;
    use ome_protocol::{InstrumentInfo, InstrumentStatus};

    pub fn sym() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    pub fn market() -> MarketCache {
        let mut cache = MarketCache::default();
        cache.apply(&ServerMessage::Instrument(InstrumentInfo {
            symbol: sym(),
            status: InstrumentStatus::Trading,
            reference_price: 25_00,
            last_price: None,
            low_limit: 17_50,
            high_limit: 32_50,
            band_bps: 1_000,
            volume: 0,
            name: "Acme".into(),
            halt_reason: String::new(),
        }));
        cache
    }

    fn open(broker: &mut Broker, name: &str, role: Role, cash: u64) -> AccountId {
        let fact = broker
            .decide_open_account(role, name, hash_secret(name, "pw"), cash)
            .unwrap();
        match broker.apply(&fact)[0] {
            Effect::AccountOpened(id) => id,
            _ => unreachable!(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn exchange_report(
        order_id: OrderId,
        order_ref: u64,
        account: AccountId,
        side: Side,
        status: OrderStatus,
        fill: (Price, Qty),
        filled: Qty,
        qty: Qty,
        seq: u64,
    ) -> ExecutionReport {
        ExecutionReport {
            seq,
            order_id,
            client_order_id: order_ref,
            account,
            symbol: sym(),
            side,
            status,
            price: 26_00,
            qty,
            filled,
            leaves: qty - filled,
            last_price: fill.0,
            last_qty: fill.1,
            fees: 0,
            cycle: 1,
            timestamp: 1,
            reason: String::new(),
        }
    }

    pub fn deliver(broker: &mut Broker, report: ExecutionReport) -> Vec<Effect> {
        let charges = broker.charges_for(&report);
        broker.apply(&Fact::ExchangeReport { report, charges })
    }

    #[test]
    fn profiles_restrict_which_accounts_can_be_opened() {
        let broker = Broker::new(Profile::new(ProfileKind::Retail));
        assert!(
            broker
                .decide_open_account(Role::Hft, "jump", [0; 32], 0)
                .is_err()
        );
        assert!(
            broker
                .decide_open_account(Role::Retail, "alice", [0; 32], 0)
                .is_ok()
        );
    }

    #[test]
    fn accepted_order_reserves_and_routes_a_protected_limit_to_the_exchange() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut broker, "alice", Role::Retail, 10_000_00);
        let fact = broker
            .decide_order(
                alice,
                5,
                sym(),
                Side::Buy,
                OrderKind::Market,
                OrderOptions::default(),
                10,
                &market(),
                0,
            )
            .unwrap();
        let effects = broker.apply(&fact);

        assert_eq!(
            broker.ledger().get(alice).unwrap().cash.reserved,
            27_50 * 10 + FeeEngine::default().reserve(Role::Retail, Side::Buy, 27_50, 10)
        );
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::ToExchange(MemberMessage::NewOrder {
                kind: OrderKind::Limit(27_50),
                options: OrderOptions {
                    tif: TimeInForce::Ioc,
                    ..
                },
                member_order_id: 1,
                ..
            })
        )));
        assert_eq!(broker.pending_orders(Venue::Exchange), vec![1]);
    }

    #[test]
    fn fills_settle_at_the_trade_price_and_refund_the_difference() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut broker, "alice", Role::Retail, 10_000_00);
        let fact = broker
            .decide_order(
                alice,
                5,
                sym(),
                Side::Buy,
                OrderKind::Limit(26_00),
                OrderOptions::default(),
                10,
                &market(),
                0,
            )
            .unwrap();
        broker.apply(&fact);
        deliver(
            &mut broker,
            exchange_report(900, 1, alice, Side::Buy, OrderStatus::New, (0, 0), 0, 10, 1),
        );
        let effects = deliver(
            &mut broker,
            exchange_report(
                900,
                1,
                alice,
                Side::Buy,
                OrderStatus::Filled,
                (25_00, 10),
                10,
                10,
                2,
            ),
        );

        let a = broker.ledger().get(alice).unwrap();
        let charges = FeeEngine::default().charges(Role::Retail, Side::Buy, 25_00, 10, 0, 0);
        assert_eq!(
            a.cash.available as i64,
            10_000_00 - 250_00 - charges.total()
        );
        assert_eq!(a.cash.reserved, 0);
        assert_eq!(a.position(sym()).available, 10);
        assert!(effects.iter().any(|e| matches!(e, Effect::ToAccount { message: ServerMessage::Execution(r), .. } if r.client_order_id == 5 && r.order_id == 900)));
        assert_eq!(broker.last_seq(), 2);
        assert!(broker.pending_orders(Venue::Exchange).is_empty());
    }

    #[test]
    fn rejections_cover_funds_shares_band_and_limits() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut broker, "alice", Role::Retail, 100_00);
        let m = market();
        let reason = |r: Result<Fact, Box<ExecutionReport>>| r.unwrap_err().reason;
        assert!(
            reason(broker.decide_order(
                alice,
                1,
                sym(),
                Side::Buy,
                OrderKind::Limit(25_00),
                OrderOptions::default(),
                10,
                &m,
                0
            ))
            .contains("insufficient cash")
        );
        assert!(
            reason(broker.decide_order(
                alice,
                1,
                sym(),
                Side::Sell,
                OrderKind::Limit(25_00),
                OrderOptions::default(),
                1,
                &m,
                0
            ))
            .contains("insufficient ACME")
        );
        assert!(
            reason(broker.decide_order(
                alice,
                1,
                sym(),
                Side::Buy,
                OrderKind::Limit(99_00),
                OrderOptions::default(),
                1,
                &m,
                0
            ))
            .contains("outside band")
        );
        assert!(
            reason(broker.decide_order(
                alice,
                1,
                sym(),
                Side::Buy,
                OrderKind::Limit(25_00),
                OrderOptions::default(),
                5_000,
                &m,
                0
            ))
            .contains("exceeds max")
        );
    }

    #[test]
    fn ipo_allocation_and_exchange_originated_order_settle_for_the_issuer() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Prime));
        let issuer = open(&mut broker, "acme_ir", Role::Issuer, 0);
        broker.apply(&Fact::Allocated(Allocation {
            member: 3,
            seq: 1,
            allocation_id: 1,
            symbol: sym(),
            account: issuer,
            shares: 100,
        }));
        deliver(
            &mut broker,
            exchange_report(
                5,
                0,
                issuer,
                Side::Sell,
                OrderStatus::New,
                (0, 0),
                0,
                100,
                2,
            ),
        );
        assert_eq!(
            broker
                .ledger()
                .get(issuer)
                .unwrap()
                .position(sym())
                .reserved,
            100
        );

        deliver(
            &mut broker,
            exchange_report(
                5,
                0,
                issuer,
                Side::Sell,
                OrderStatus::PartiallyFilled,
                (25_00, 40),
                40,
                100,
                3,
            ),
        );
        let a = broker.ledger().get(issuer).unwrap();
        let charges = FeeEngine::default().charges(Role::Issuer, Side::Sell, 25_00, 40, 0, 0);
        assert_eq!(charges.commission, 20_00);
        assert_eq!(a.cash.available as i64, 1_000_00 - charges.total());
        assert_eq!(a.position(sym()).reserved, 60);

        broker.apply(&Fact::Allocated(Allocation {
            member: 3,
            seq: 4,
            allocation_id: 1,
            symbol: sym(),
            account: issuer,
            shares: 100,
        }));
        assert_eq!(
            broker.ledger().get(issuer).unwrap().position(sym()).total(),
            60,
            "allocations are idempotent"
        );
    }

    #[test]
    fn abandoned_orders_release_their_reservation() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut broker, "alice", Role::Retail, 10_000_00);
        let fact = broker
            .decide_order(
                alice,
                5,
                sym(),
                Side::Buy,
                OrderKind::Limit(25_00),
                OrderOptions::default(),
                10,
                &market(),
                0,
            )
            .unwrap();
        broker.apply(&fact);
        broker.apply(&Fact::OrderAbandoned { order_ref: 1 });
        assert_eq!(
            broker.ledger().get(alice).unwrap().cash.available,
            10_000_00
        );
        assert!(broker.pending_orders(Venue::Exchange).is_empty());
    }

    #[test]
    fn cancels_are_checked_for_ownership_before_routing() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut broker, "alice", Role::Retail, 10_000_00);
        let bob = open(&mut broker, "bob", Role::Retail, 10_000_00);
        let fact = broker
            .decide_order(
                alice,
                5,
                sym(),
                Side::Buy,
                OrderKind::Limit(25_00),
                OrderOptions::default(),
                10,
                &market(),
                0,
            )
            .unwrap();
        broker.apply(&fact);
        deliver(
            &mut broker,
            exchange_report(900, 1, alice, Side::Buy, OrderStatus::New, (0, 0), 0, 10, 1),
        );

        assert!(
            broker
                .decide_cancel(bob, 900)
                .unwrap_err()
                .contains("not yours")
        );
        assert_eq!(
            broker.decide_cancel(alice, 900),
            Ok((Venue::Exchange, MemberMessage::Cancel { order_id: 900 }))
        );
        deliver(
            &mut broker,
            exchange_report(
                900,
                1,
                alice,
                Side::Buy,
                OrderStatus::Cancelled,
                (0, 0),
                0,
                10,
                2,
            ),
        );
        assert_eq!(broker.ledger().get(alice).unwrap().cash.reserved, 0);
        assert!(broker.decide_cancel(alice, 900).is_err());
    }

    #[test]
    fn fills_produce_contract_notes_and_fee_totals_balance_the_books() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut broker, "alice", Role::Retail, 10_000_00);
        let bob = open(&mut broker, "bob", Role::Retail, 0);
        broker.apply(&Fact::BalanceSet {
            account: bob,
            asset: Asset::Shares(sym()),
            amount: 50,
        });
        let start_cash: u64 = 10_000_00;

        let buy = broker
            .decide_order(
                alice,
                1,
                sym(),
                Side::Buy,
                OrderKind::Limit(25_50),
                OrderOptions::default(),
                20,
                &market(),
                0,
            )
            .unwrap();
        broker.apply(&buy);
        let sell = broker
            .decide_order(
                bob,
                2,
                sym(),
                Side::Sell,
                OrderKind::Limit(25_00),
                OrderOptions::default(),
                20,
                &market(),
                0,
            )
            .unwrap();
        broker.apply(&sell);
        deliver(
            &mut broker,
            exchange_report(70, 2, bob, Side::Sell, OrderStatus::New, (0, 0), 0, 20, 1),
        );
        let mut taker = exchange_report(
            71,
            1,
            alice,
            Side::Buy,
            OrderStatus::Filled,
            (25_00, 20),
            20,
            20,
            2,
        );
        taker.fees = 6;
        let effects = deliver(&mut broker, taker);
        let mut maker = exchange_report(
            70,
            2,
            bob,
            Side::Sell,
            OrderStatus::Filled,
            (25_00, 20),
            20,
            20,
            3,
        );
        maker.fees = -3;
        deliver(&mut broker, maker);

        let note = effects
            .iter()
            .find_map(|e| match e {
                Effect::ToAccount {
                    message: ServerMessage::ContractNote(n),
                    ..
                } => Some(n.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(note.qty, 20);
        assert_eq!(note.charges.exchange_fee, 6);
        assert_eq!(note.net_amount, -(500_00 + note.charges.total()));

        let totals = broker.fee_summary();
        assert_eq!(totals.fills, 2);
        let cash: i64 = [alice, bob]
            .iter()
            .map(|id| broker.ledger().get(*id).unwrap().cash.total() as i64)
            .sum();
        assert_eq!(cash + totals.charges.total(), start_cash as i64);
        assert_eq!(broker.ledger().get(alice).unwrap().cash.reserved, 0);
    }

    #[test]
    fn commission_cap_applies_across_partial_fills_of_one_order() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut broker, "alice", Role::Retail, 1_000_000_00);
        let fact = broker
            .decide_order(
                alice,
                1,
                sym(),
                Side::Buy,
                OrderKind::Limit(25_00),
                OrderOptions::default(),
                1_000,
                &market(),
                0,
            )
            .unwrap();
        broker.apply(&fact);
        deliver(
            &mut broker,
            exchange_report(
                80,
                1,
                alice,
                Side::Buy,
                OrderStatus::PartiallyFilled,
                (25_00, 600),
                600,
                1_000,
                1,
            ),
        );
        deliver(
            &mut broker,
            exchange_report(
                80,
                1,
                alice,
                Side::Buy,
                OrderStatus::Filled,
                (25_00, 400),
                1_000,
                1_000,
                2,
            ),
        );
        assert_eq!(broker.fee_summary().charges.commission, 7_50);
        let big = broker.decide_order(
            alice,
            2,
            sym(),
            Side::Buy,
            OrderKind::Limit(27_00),
            OrderOptions::default(),
            1_000,
            &market(),
            0,
        );
        assert!(big.is_ok());
    }

    #[test]
    fn fills_stay_unsettled_until_clearing_reports_their_cycle() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut broker, "alice", Role::Retail, 10_000_00);
        let fact = broker
            .decide_order(
                alice,
                1,
                sym(),
                Side::Buy,
                OrderKind::Limit(25_00),
                OrderOptions::default(),
                10,
                &market(),
                0,
            )
            .unwrap();
        broker.apply(&fact);
        let report = exchange_report(
            90,
            1,
            alice,
            Side::Buy,
            OrderStatus::Filled,
            (25_00, 10),
            10,
            10,
            1,
        );
        let paid = 250_00 + broker.charges_for(&report).total();
        deliver(&mut broker, report);

        let snapshot = broker.account_snapshot(alice).unwrap();
        assert_eq!(snapshot.unsettled_cash, -paid);
        assert_eq!(snapshot.positions[0].unsettled, 10);

        let settlement = |seq, cycle| {
            Fact::SettlementReceived(SettlementReport {
                seq,
                cycle,
                member: 2,
                cash: 0,
                collateral: 0,
                margin_required: 0,
                positions: vec![],
            })
        };
        broker.apply(&settlement(1, 0));
        assert_eq!(
            broker.account_snapshot(alice).unwrap().unsettled_cash,
            -paid
        );
        let effects = broker.apply(&settlement(2, 1));
        assert_eq!(effects, vec![Effect::AccountChanged(alice)]);
        let snapshot = broker.account_snapshot(alice).unwrap();
        assert_eq!(
            (snapshot.unsettled_cash, snapshot.positions[0].unsettled),
            (0, 0)
        );
        assert_eq!(broker.last_settlement_seq(), 2);
        assert!(!broker.is_new_settlement(2));
    }

    #[test]
    fn session_rules_apply_at_the_broker_before_routing() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut broker, "alice", Role::Retail, 10_000_00);
        let mut m = market();
        let status = |phase| {
            ServerMessage::MarketStatus(ome_protocol::MarketStatus {
                phase,
                day: 1,
                since: 0,
                next_change_at: 0,
            })
        };

        m.apply(&status(MarketPhase::Closed));
        let closed = broker.decide_order(
            alice,
            1,
            sym(),
            Side::Buy,
            OrderKind::Limit(25_00),
            OrderOptions::tif(TimeInForce::Day),
            1,
            &m,
            0,
        );
        assert!(closed.unwrap_err().reason.contains("market is closed"));

        m.apply(&status(MarketPhase::PreOpen));
        let ioc = broker.decide_order(
            alice,
            1,
            sym(),
            Side::Buy,
            OrderKind::Limit(25_00),
            OrderOptions::tif(TimeInForce::Ioc),
            1,
            &m,
            0,
        );
        assert!(ioc.unwrap_err().reason.contains("IOC"));
        let day = broker
            .decide_order(
                alice,
                1,
                sym(),
                Side::Buy,
                OrderKind::Limit(25_00),
                OrderOptions::tif(TimeInForce::Day),
                1,
                &m,
                0,
            )
            .unwrap();
        let effects = broker.apply(&day);
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::ToExchange(MemberMessage::NewOrder {
                options: OrderOptions {
                    tif: TimeInForce::Day,
                    ..
                },
                ..
            })
        )));
    }

    #[test]
    fn quotes_reserve_both_sides_and_go_out_as_one_message() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Lp));
        let desk = open(&mut broker, "desk", Role::LpDesk, 100_000_00);
        broker.apply(&Fact::BalanceSet {
            account: desk,
            asset: Asset::Shares(sym()),
            amount: 100,
        });
        let m = market();
        let fact = broker
            .decide_quote(
                desk,
                sym(),
                QuoteSide {
                    price: 24_90,
                    qty: 20,
                },
                QuoteSide {
                    price: 25_10,
                    qty: 30,
                },
                &m,
            )
            .unwrap();
        let effects = broker.apply(&fact);
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::ToExchange(MemberMessage::Quote {
                bid_ref: 1,
                ask_ref: 2,
                ..
            })
        )));
        let holder = broker.ledger().get(desk).unwrap();
        assert!(holder.cash.reserved >= 24_90 * 20);
        assert_eq!(holder.position(sym()).reserved, 30);

        assert!(
            broker
                .decide_quote(
                    desk,
                    sym(),
                    QuoteSide {
                        price: 25_10,
                        qty: 1
                    },
                    QuoteSide {
                        price: 25_00,
                        qty: 1
                    },
                    &m
                )
                .is_err()
        );
        let mut retail = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut retail, "alice", Role::Retail, 100_000_00);
        assert!(
            retail
                .decide_quote(
                    alice,
                    sym(),
                    QuoteSide {
                        price: 24_00,
                        qty: 1
                    },
                    QuoteSide::default(),
                    &m
                )
                .is_err()
        );
        assert!(retail.decide_mmp(alice, 1_000, 10).is_err());
        assert!(broker.decide_mmp(desk, 1_000, 10).is_ok());
    }

    fn holding(broker: &mut Broker, name: &str, role: Role, shares: u64) -> AccountId {
        let id = open(broker, name, role, 10_000_00);
        let fact = broker
            .decide_set_balance(id, Asset::Shares(sym()), shares)
            .unwrap();
        broker.apply(&fact);
        id
    }

    fn statuses(effects: &[Effect]) -> Vec<(u64, ConditionalStatus)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::ToAccount {
                    message: ServerMessage::ConditionalUpdate(c),
                    ..
                } => Some((c.cond_id, c.status)),
                _ => None,
            })
            .collect()
    }

    fn routed(effects: &[Effect]) -> Vec<(Side, OrderKind, Qty)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::ToExchange(MemberMessage::NewOrder {
                    side, kind, qty, ..
                }) => Some((*side, *kind, *qty)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn stop_loss_waits_for_the_trigger_then_releases_a_protected_market_sell() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = holding(&mut broker, "alice", Role::Retail, 100);
        let fact = broker
            .decide_conditional(
                alice,
                7,
                sym(),
                Side::Sell,
                40,
                Trigger::Below(24_00),
                OrderKind::Market,
                OrderOptions::default(),
                &market(),
            )
            .unwrap();
        assert_eq!(
            statuses(&broker.apply(&fact)),
            vec![(1, ConditionalStatus::Armed)]
        );
        assert_eq!(
            broker
                .ledger()
                .get(alice)
                .unwrap()
                .position(sym())
                .available,
            100,
            "an armed stop does not lock shares"
        );

        assert_eq!(broker.scan_conditionals(sym(), 24_50), (vec![], vec![]));
        let (_, due) = broker.scan_conditionals(sym(), 23_90);
        assert_eq!(due, vec![1]);
        let fact = broker.decide_trigger(1, 23_90, &market(), 5).unwrap();
        let effects = broker.apply(&fact);
        assert_eq!(statuses(&effects), vec![(1, ConditionalStatus::Triggered)]);
        assert_eq!(
            routed(&effects),
            vec![(Side::Sell, OrderKind::Limit(22_50), 40)]
        );
        assert_eq!(broker.armed_conditionals(), 0);
        assert_eq!(
            broker.ledger().get(alice).unwrap().position(sym()).reserved,
            40
        );
    }

    #[test]
    fn oco_cancels_the_other_leg_when_one_triggers() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = holding(&mut broker, "alice", Role::Retail, 50);
        assert!(
            broker
                .decide_oco(
                    alice,
                    3,
                    sym(),
                    Side::Sell,
                    50,
                    24_00,
                    26_00,
                    None,
                    &market()
                )
                .is_err(),
            "take-profit below the stop is nonsense for a sell OCO"
        );
        let fact = broker
            .decide_oco(
                alice,
                3,
                sym(),
                Side::Sell,
                50,
                27_00,
                24_00,
                Some(23_90),
                &market(),
            )
            .unwrap();
        broker.apply(&fact);
        assert_eq!(broker.armed_conditionals(), 2);

        let (_, due) = broker.scan_conditionals(sym(), 27_10);
        assert_eq!(due, vec![1]);
        let effects = broker.apply(&broker.decide_trigger(1, 27_10, &market(), 1).unwrap());
        assert_eq!(
            statuses(&effects),
            vec![
                (2, ConditionalStatus::Cancelled),
                (1, ConditionalStatus::Triggered)
            ]
        );
        assert_eq!(
            routed(&effects),
            vec![(Side::Sell, OrderKind::Limit(27_00), 50)]
        );
        assert_eq!(broker.armed_conditionals(), 0);
    }

    #[test]
    fn released_order_is_rejected_when_the_shares_are_gone() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = holding(&mut broker, "alice", Role::Retail, 10);
        let fact = broker
            .decide_conditional(
                alice,
                1,
                sym(),
                Side::Sell,
                10,
                Trigger::Below(24_00),
                OrderKind::Market,
                OrderOptions::default(),
                &market(),
            )
            .unwrap();
        broker.apply(&fact);
        broker.apply(
            &broker
                .decide_set_balance(alice, Asset::Shares(sym()), 0)
                .unwrap(),
        );
        let effects = broker.apply(&broker.decide_trigger(1, 23_00, &market(), 1).unwrap());
        assert_eq!(statuses(&effects), vec![(1, ConditionalStatus::Rejected)]);
        assert!(routed(&effects).is_empty());
        assert_eq!(broker.armed_conditionals(), 0);
    }

    #[test]
    fn bracket_arms_exits_as_the_entry_fills_and_grows_them() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut broker, "alice", Role::Retail, 10_000_00);
        assert!(
            broker
                .decide_bracket(
                    alice,
                    9,
                    sym(),
                    Side::Buy,
                    10,
                    OrderKind::Limit(25_00),
                    24_00,
                    26_00,
                    &market(),
                    0,
                )
                .is_err()
        );
        let fact = broker
            .decide_bracket(
                alice,
                9,
                sym(),
                Side::Buy,
                10,
                OrderKind::Limit(25_00),
                27_00,
                24_00,
                &market(),
                0,
            )
            .unwrap();
        broker.apply(&fact);
        assert_eq!(broker.armed_conditionals(), 0);
        let mut first = exchange_report(
            70,
            1,
            alice,
            Side::Buy,
            OrderStatus::PartiallyFilled,
            (25_00, 4),
            4,
            10,
            1,
        );
        first.client_order_id = 1;
        let effects = deliver(&mut broker, first);
        assert_eq!(
            statuses(&effects),
            vec![(1, ConditionalStatus::Armed), (2, ConditionalStatus::Armed)]
        );
        let legs = broker.conditional_reports(alice);
        assert_eq!(legs[0].trigger, Trigger::Above(27_00));
        assert_eq!(legs[1].trigger, Trigger::Below(24_00));
        assert!(legs.iter().all(|c| c.qty == 4 && c.side == Side::Sell));

        deliver(
            &mut broker,
            exchange_report(
                70,
                1,
                alice,
                Side::Buy,
                OrderStatus::Filled,
                (25_00, 6),
                10,
                10,
                2,
            ),
        );
        assert!(
            broker
                .conditional_reports(alice)
                .iter()
                .all(|c| c.qty == 10)
        );
        assert!(broker.brackets.is_empty());
    }

    #[test]
    fn after_market_orders_and_trailing_stops_follow_role_and_session_rules() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = holding(&mut broker, "alice", Role::Retail, 10);
        let amo = |broker: &Broker, m: &MarketCache| {
            broker.decide_conditional(
                alice,
                1,
                sym(),
                Side::Buy,
                5,
                Trigger::MarketOpens,
                OrderKind::Limit(25_00),
                OrderOptions::default(),
                m,
            )
        };
        assert!(amo(&broker, &market()).is_err());
        let mut closed = market();
        closed.apply(&ServerMessage::MarketStatus(ome_protocol::MarketStatus {
            phase: MarketPhase::Closed,
            day: 1,
            since: 0,
            next_change_at: 0,
        }));
        broker.apply(&amo(&broker, &closed).unwrap());
        assert_eq!(broker.waiting_for_open(), vec![1]);
        let effects = broker.apply(&broker.decide_trigger(1, 0, &market(), 1).unwrap());
        assert_eq!(
            routed(&effects),
            vec![(Side::Buy, OrderKind::Limit(25_00), 5)]
        );

        let mut dma = Broker::new(Profile::new(ProfileKind::Dma));
        let hft = holding(&mut dma, "hft", Role::Hft, 10);
        let trailing = dma.decide_conditional(
            hft,
            1,
            sym(),
            Side::Sell,
            5,
            Trigger::TrailingBelow(50),
            OrderKind::Market,
            OrderOptions::default(),
            &market(),
        );
        assert!(trailing.unwrap_err().contains("trailing"));
    }

    #[test]
    fn suspension_cancels_everything_and_loss_limit_follows_real_fills() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = holding(&mut broker, "alice", Role::Retail, 10);
        let fact = broker
            .decide_conditional(
                alice,
                1,
                sym(),
                Side::Sell,
                10,
                Trigger::Below(20_00),
                OrderKind::Market,
                OrderOptions::default(),
                &market(),
            )
            .unwrap();
        broker.apply(&fact);
        let effects = broker.apply(&broker.decide_account_status(alice, true).unwrap());
        assert_eq!(statuses(&effects), vec![(1, ConditionalStatus::Cancelled)]);
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::ToExchange(MemberMessage::CancelAll { account: Some(a), symbol: None }) if *a == alice
        )));
        let blocked = broker.decide_order(
            alice,
            2,
            sym(),
            Side::Sell,
            OrderKind::Limit(25_00),
            OrderOptions::default(),
            1,
            &market(),
            0,
        );
        assert!(blocked.unwrap_err().reason.contains("suspended"));
        broker.apply(&broker.decide_account_status(alice, false).unwrap());

        broker.apply(&broker.decide_loss_limit(alice, 30_00).unwrap());
        let buy = |broker: &Broker, id| {
            broker.decide_order(
                alice,
                id,
                sym(),
                Side::Buy,
                OrderKind::Limit(26_00),
                OrderOptions::default(),
                10,
                &market(),
                0,
            )
        };
        broker.apply(&buy(&broker, 3).unwrap());
        deliver(
            &mut broker,
            exchange_report(
                80,
                1,
                alice,
                Side::Buy,
                OrderStatus::Filled,
                (26_00, 10),
                10,
                10,
                1,
            ),
        );
        assert!(
            buy(&broker, 4).is_ok(),
            "marked at the 25.00 reference: down 10.00 plus fees"
        );
        broker.mark(sym(), 23_00);
        assert!(broker.day_pnl(alice) < -30_00);
        let mut lower = market();
        lower.apply(&ServerMessage::Trade(ome_protocol::TradeEvent {
            symbol: sym(),
            trade_id: 1,
            price: 23_00,
            qty: 1,
            aggressor: Side::Sell,
            timestamp: 0,
            venue: ome_protocol::Venue::Exchange,
        }));
        let refused = broker.decide_order(
            alice,
            5,
            sym(),
            Side::Buy,
            OrderKind::Limit(23_00),
            OrderOptions::default(),
            1,
            &lower,
            0,
        );
        assert!(refused.unwrap_err().reason.contains("daily loss limit"));
        assert_eq!(broker.account_list()[0].loss_limit, 30_00);

        broker.apply(&broker.decide_trading_day(1, &lower).unwrap());
        assert_eq!(broker.day_pnl(alice), 0);
        assert!(broker.decide_trading_day(1, &lower).is_none());
    }

    fn algo_statuses(effects: &[Effect]) -> Vec<(AlgoStatus, Qty)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::ToAccount {
                    message: ServerMessage::AlgoUpdate(a),
                    ..
                } => Some((a.status, a.filled)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn twap_parent_sends_children_on_schedule_and_completes_on_fills() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Prime));
        let fund = open(&mut broker, "fund", Role::Quant, 1_000_000_00);
        let twap = AlgoStrategy::Twap {
            duration_ms: 10_000,
            slices: 2,
        };
        let fact = broker
            .decide_algo(
                fund,
                5,
                sym(),
                Side::Buy,
                100,
                Some(26_00),
                twap,
                &market(),
                1_000,
            )
            .unwrap();
        assert_eq!(
            algo_statuses(&broker.apply(&fact)),
            vec![(AlgoStatus::Working, 0)]
        );

        let step = broker.decide_algo_step(1, &market(), 1_000).unwrap();
        let effects = broker.apply(&step);
        assert_eq!(
            routed(&effects),
            vec![(Side::Buy, OrderKind::Limit(26_00), 50)]
        );
        assert!(
            broker.decide_algo_step(1, &market(), 1_100).is_none(),
            "one child at a time"
        );
        let effects = deliver(
            &mut broker,
            exchange_report(
                10,
                1,
                fund,
                Side::Buy,
                OrderStatus::Filled,
                (25_50, 50),
                50,
                50,
                1,
            ),
        );
        assert_eq!(algo_statuses(&effects), vec![(AlgoStatus::Working, 50)]);
        assert!(
            broker.decide_algo_step(1, &market(), 3_000).is_none(),
            "next slice not due"
        );

        let effects = broker.apply(&broker.decide_algo_step(1, &market(), 6_000).unwrap());
        assert_eq!(
            routed(&effects),
            vec![(Side::Buy, OrderKind::Limit(26_00), 50)]
        );
        let effects = deliver(
            &mut broker,
            exchange_report(
                11,
                2,
                fund,
                Side::Buy,
                OrderStatus::Filled,
                (25_70, 50),
                50,
                50,
                2,
            ),
        );
        let done = algo_statuses(&effects);
        assert_eq!(done, vec![(AlgoStatus::Done, 100)]);
        assert!(broker.algo_reports(fund).is_empty());
        let report = effects
            .iter()
            .find_map(|e| match e {
                Effect::ToAccount {
                    message: ServerMessage::AlgoUpdate(a),
                    ..
                } => Some(a.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!((report.avg_price, report.children), (25_60, 2));
    }

    #[test]
    fn cancelling_an_iceberg_slicer_pulls_its_resting_child_and_algos_are_role_gated() {
        let mut broker = Broker::new(Profile::new(ProfileKind::Prime));
        let desk = open(&mut broker, "desk", Role::Prop, 1_000_000_00);
        let fact = broker
            .decide_algo(
                desk,
                1,
                sym(),
                Side::Buy,
                1_000,
                Some(24_00),
                AlgoStrategy::Iceberg { clip: 200 },
                &market(),
                0,
            )
            .unwrap();
        broker.apply(&fact);
        let effects = broker.apply(&broker.decide_algo_step(1, &market(), 0).unwrap());
        assert_eq!(
            routed(&effects),
            vec![(Side::Buy, OrderKind::Limit(24_00), 200)]
        );
        deliver(
            &mut broker,
            exchange_report(42, 1, desk, Side::Buy, OrderStatus::New, (0, 0), 0, 200, 1),
        );
        let effects = broker.apply(&broker.decide_cancel_algo(desk, 1).unwrap());
        assert!(effects.contains(&Effect::ToExchange(MemberMessage::Cancel { order_id: 42 })));
        assert_eq!(algo_statuses(&effects), vec![(AlgoStatus::Cancelled, 0)]);

        let expired = AlgoStrategy::Vwap { duration_ms: 5_000 };
        broker.apply(
            &broker
                .decide_algo(desk, 2, sym(), Side::Buy, 10, None, expired, &market(), 0)
                .unwrap(),
        );
        let mut closed = market();
        closed.apply(&ServerMessage::MarketStatus(ome_protocol::MarketStatus {
            phase: MarketPhase::Closed,
            day: 1,
            since: 0,
            next_change_at: 0,
        }));
        assert!(broker.decide_algo_step(2, &closed, 1_000).is_none());
        let effects = broker.apply(&broker.decide_algo_step(2, &closed, 5_000).unwrap());
        assert_eq!(algo_statuses(&effects), vec![(AlgoStatus::Expired, 0)]);

        let mut retail = Broker::new(Profile::new(ProfileKind::Retail));
        let alice = open(&mut retail, "alice", Role::Retail, 10_000_00);
        let refused = retail.decide_algo(
            alice,
            1,
            sym(),
            Side::Buy,
            10,
            None,
            AlgoStrategy::Vwap {
                duration_ms: 60_000,
            },
            &market(),
            0,
        );
        assert!(refused.unwrap_err().contains("algorithmic"));
    }

    fn quoted_market() -> MarketCache {
        let mut m = market();
        let level = |price| ome_core::order_book::Level {
            price,
            qty: 100,
            orders: 1,
        };
        m.apply(&ServerMessage::Book(ome_protocol::BookSnapshot {
            symbol: sym(),
            timestamp: 0,
            bids: vec![level(24_90)],
            asks: vec![level(25_10)],
        }));
        m
    }

    #[test]
    fn internalizer_crosses_marketable_orders_at_the_midpoint_and_routes_the_rest() {
        use crate::profile::Regime;
        let mut broker = Broker::new(Profile::with_regime(ProfileKind::Retail, Regime::Us));
        let alice = open(&mut broker, "alice", Role::Retail, 10_000_00);
        let bob = holding(&mut broker, "bob", Role::Retail, 50);
        let m = quoted_market();
        let order = |broker: &Broker, account, side, kind, qty| {
            let fact = broker
                .decide_order(
                    account,
                    1,
                    sym(),
                    side,
                    kind,
                    OrderOptions::default(),
                    qty,
                    &m,
                    0,
                )
                .unwrap();
            broker.hold_for_crossing(fact, &m)
        };

        let resting = order(&broker, alice, Side::Buy, OrderKind::Limit(25_00), 3);
        assert!(
            matches!(resting, Fact::OrderAccepted { .. }),
            "non-marketable goes to the exchange"
        );
        let buy = order(&broker, alice, Side::Buy, OrderKind::Market, 10);
        assert!(matches!(buy, Fact::OrderHeld(_)));
        assert!(routed(&broker.apply(&buy)).is_empty());
        let sell = order(&broker, bob, Side::Sell, OrderKind::Limit(24_90), 6);
        broker.apply(&sell);
        assert_eq!(broker.held_orders(), vec![1, 2]);

        let cross = broker.next_cross(&m, 5).unwrap();
        let Fact::InternalCross(c) = &cross else {
            unreachable!()
        };
        assert_eq!((c.price, c.qty, c.buy_charges.exchange_fee), (25_00, 6, 0));
        let effects = broker.apply(&cross);
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::ToExchange(MemberMessage::TradeReport {
                price: 25_00,
                qty: 6,
                venue: Venue::Internal,
                ..
            })
        )));
        assert_eq!(broker.next_cross(&m, 5), None);
        assert_eq!(broker.held_orders(), vec![1], "bob was filled completely");
        assert_eq!(
            broker.ledger().get(bob).unwrap().position(sym()).total(),
            44
        );

        let effects = broker.apply(&Fact::Released { order_ref: 1 });
        assert_eq!(
            routed(&effects),
            vec![(Side::Buy, OrderKind::Limit(27_50), 4)]
        );
        assert!(broker.held_orders().is_empty());
        let stats = broker.fee_summary();
        assert_eq!(
            (
                stats.internal_crosses,
                stats.internalized_shares,
                stats.routed_orders
            ),
            (1, 6, 1)
        );

        let mut india = Broker::new(Profile::with_regime(ProfileKind::Retail, Regime::India));
        let carol = open(&mut india, "carol", Role::Retail, 10_000_00);
        let fact = india
            .decide_order(
                carol,
                1,
                sym(),
                Side::Buy,
                OrderKind::Market,
                OrderOptions::default(),
                1,
                &m,
                0,
            )
            .unwrap();
        assert!(matches!(
            india.hold_for_crossing(fact, &m),
            Fact::OrderAccepted { .. }
        ));
    }
}
