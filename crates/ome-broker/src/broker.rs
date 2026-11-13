use crate::fees::FeeEngine;
use crate::ledger::Ledger;
use crate::market::MarketCache;
use crate::profile::Profile;
use ome_core::price::Price;
use ome_core::risk::RiskContext;
use ome_core::symbol::Symbol;
use ome_core::types::{
    AccountId, OrderId, OrderKind, OrderRequest, Qty, Side, TimeInForce, Timestamp,
};
use ome_net::auth::SecretHash;
use ome_protocol::{
    AccountInfo, AccountSnapshot, Allocation, Asset, Charges, ContractNote, ExecutionReport,
    FeeSummary, MemberMessage, OrderStatus, Position, Role, ServerMessage,
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
        tif: TimeInForce,
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
        }
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
        qty: Qty,
        market: &MarketCache,
        now: Timestamp,
    ) -> Result<Fact, Box<ExecutionReport>> {
        let reject = |reason: String| {
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
                timestamp: now,
                reason,
            })
        };
        let Some(holder) = self.ledger.get(account) else {
            return Err(reject(format!("unknown account {account}")));
        };
        let Some(quote) = market.get(symbol) else {
            return Err(reject(format!("unknown symbol {symbol}")));
        };
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
            (OrderKind::Limit(p), _) => (p, TimeInForce::Gtc),
            (OrderKind::Market, Side::Buy) => (high, TimeInForce::Ioc),
            (OrderKind::Market, Side::Sell) => (low, TimeInForce::Ioc),
        };
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
            tif,
            qty,
            reserved,
            fee_reserve,
        })
    }

    pub fn decide_cancel(
        &self,
        account: AccountId,
        order_id: OrderId,
    ) -> Result<MemberMessage, String> {
        let meta = self
            .by_exchange
            .get(&order_id)
            .and_then(|r| self.orders.get(r))
            .ok_or_else(|| format!("order #{order_id} is not open"))?;
        if meta.account != account {
            return Err(format!("order #{order_id} is not yours"));
        }
        Ok(MemberMessage::Cancel { order_id })
    }

    pub fn pending_orders(&self) -> Vec<u64> {
        self.orders
            .iter()
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
                tif,
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
                    },
                );
                vec![
                    Effect::AccountChanged(*account),
                    Effect::ToExchange(MemberMessage::NewOrder {
                        member_order_id: *order_ref,
                        account: *account,
                        symbol: *symbol,
                        side: *side,
                        kind: OrderKind::Limit(*price),
                        tif: *tif,
                        qty: *qty,
                    }),
                ]
            }
            Fact::ExchangeReport { report, charges } => {
                self.last_seq = self.last_seq.max(report.seq);
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
            Fact::OrderAbandoned { order_ref } => {
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
                vec![
                    Effect::AccountChanged(meta.account),
                    Effect::ToAccount {
                        account: meta.account,
                        message: ServerMessage::Execution(report),
                    },
                ]
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
            report.fees,
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
        let client = client_report(
            report.order_id,
            meta,
            report.status,
            (report.last_price, report.last_qty, charges.total()),
            report.timestamp,
            &report.reason,
        );
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
                tif: TimeInForce::Ioc,
                member_order_id: 1,
                ..
            })
        )));
        assert_eq!(broker.pending_orders(), vec![1]);
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
        assert!(broker.pending_orders().is_empty());
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
        assert!(broker.pending_orders().is_empty());
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
            Ok(MemberMessage::Cancel { order_id: 900 })
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
            1_000,
            &market(),
            0,
        );
        assert!(big.is_ok());
    }
}
