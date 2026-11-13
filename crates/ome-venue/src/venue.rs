use crate::dark::{DarkBook, DarkOrder};
use crate::quotes::Quotes;
use crate::wholesaler::{self, Book, WholesalerConfig};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderKind, OrderOptions, Qty, Side, TimeInForce, Timestamp};
use ome_net::auth::SecretHash;
use ome_net::feed::{Feeds, Replay};
use ome_protocol::{
    ExecutionReport, MemberId, MemberInfo, MemberMessage, OrderStatus, ServerMessage, Venue,
    VenueMemberInfo, VenueStatus,
};
use std::collections::BTreeMap;

const FILL_IDS: u64 = 2 << 60;
const DARK_IDS: u64 = 3 << 60;
const FEED_CAPACITY: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    MemberRegistered {
        member: MemberId,
        name: String,
        secret: SecretHash,
    },
    Filled {
        fill_id: u64,
        member: MemberId,
        member_order_id: u64,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        price: Price,
        qty: Qty,
        pfof: u64,
        cycle: u64,
        timestamp: Timestamp,
    },
    Declined {
        member: MemberId,
        member_order_id: u64,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        qty: Qty,
        reason: String,
        timestamp: Timestamp,
    },
    HedgeSent {
        order_ref: u64,
        symbol: Symbol,
        side: Side,
        price: Price,
        qty: Qty,
    },
    ExchangeReport(ExecutionReport),
    HedgeAbandoned {
        order_ref: u64,
    },
    DarkAccepted(DarkOrder),
    DarkMatched {
        buy_id: u64,
        sell_id: u64,
        price: Price,
        qty: Qty,
        cycle: u64,
        timestamp: Timestamp,
    },
    DarkCancelled {
        order_id: u64,
        reason: String,
        timestamp: Timestamp,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    ToMember {
        member: MemberId,
        message: ServerMessage,
    },
    ToExchange(MemberMessage),
}

pub struct VenueState {
    kind: Venue,
    config: WholesalerConfig,
    members: BTreeMap<MemberId, (String, SecretHash)>,
    activity: BTreeMap<MemberId, (u64, u64)>,
    feeds: Feeds,
    book: Book,
    dark: DarkBook,
    fills: u64,
    declined: u64,
    shares: u64,
    pfof_paid: i64,
    next_id: u64,
    last_exchange_seq: u64,
    self_member: MemberId,
}

#[allow(clippy::too_many_arguments)]
fn report(
    (order_id, member_order_id, account): (u64, u64, AccountId),
    symbol: Symbol,
    side: Side,
    status: OrderStatus,
    (price, qty, filled): (Price, Qty, Qty),
    (last_price, last_qty, fees): (Price, Qty, i64),
    cycle: u64,
    timestamp: Timestamp,
    reason: &str,
) -> ServerMessage {
    ServerMessage::Execution(ExecutionReport {
        seq: 0,
        order_id,
        client_order_id: member_order_id,
        account,
        symbol,
        side,
        status,
        price,
        qty,
        filled,
        leaves: if status.is_open() { qty - filled } else { 0 },
        last_price,
        last_qty,
        fees,
        cycle,
        timestamp,
        reason: reason.to_string(),
    })
}

impl VenueState {
    pub fn new(kind: Venue, config: WholesalerConfig) -> Self {
        Self {
            kind,
            config,
            members: BTreeMap::new(),
            activity: BTreeMap::new(),
            feeds: Feeds::new(FEED_CAPACITY),
            book: Book::default(),
            dark: DarkBook::default(),
            fills: 0,
            declined: 0,
            shares: 0,
            pfof_paid: 0,
            next_id: 1,
            last_exchange_seq: 0,
            self_member: 0,
        }
    }

    pub fn kind(&self) -> Venue {
        self.kind
    }

    pub fn set_self_member(&mut self, member: MemberId) {
        self.self_member = member;
    }

    pub fn last_exchange_seq(&self) -> u64 {
        self.last_exchange_seq
    }

    pub fn is_new(&self, seq: u64) -> bool {
        seq > self.last_exchange_seq
    }

    pub fn find(&self, name: &str) -> Option<(MemberId, SecretHash)> {
        self.members
            .iter()
            .find(|(_, (n, _))| n == name)
            .map(|(id, (_, secret))| (*id, *secret))
    }

    pub fn replay(&self, member: MemberId, after_seq: u64) -> Replay {
        self.feeds.replay(member, after_seq)
    }

    pub fn decide_register(
        &self,
        name: &str,
        secret: SecretHash,
        exchange_members: &[MemberInfo],
    ) -> Result<Fact, String> {
        let member = exchange_members
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| format!("'{name}' is not an exchange member"))?;
        if self.members.contains_key(&member.member_id) {
            return Err(format!("'{name}' is already registered"));
        }
        Ok(Fact::MemberRegistered {
            member: member.member_id,
            name: name.to_string(),
            secret,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decide_order(
        &self,
        member: MemberId,
        member_order_id: u64,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        kind: OrderKind,
        qty: Qty,
        quotes: &Quotes,
        now: Timestamp,
    ) -> Fact {
        let decline = |reason: String| Fact::Declined {
            member,
            member_order_id,
            account,
            symbol,
            side,
            qty,
            reason: format!("declined: {reason}"),
            timestamp: now,
        };
        if self.kind != Venue::Wholesaler {
            return decline("this venue takes dark orders only".into());
        }
        if !quotes.tradable(symbol) {
            return decline(format!("{symbol} is not in continuous trading"));
        }
        match wholesaler::quote(
            &self.config,
            &self.book,
            symbol,
            side,
            kind,
            qty,
            quotes.best(symbol),
        ) {
            Ok(price) => Fact::Filled {
                fill_id: self.next_id,
                member,
                member_order_id,
                account,
                symbol,
                side,
                price,
                qty,
                pfof: wholesaler::pfof(qty, self.config.pfof_per_share),
                cycle: quotes.cycle(),
                timestamp: now,
            },
            Err(reason) => decline(reason),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn decide_dark(
        &self,
        member: MemberId,
        member_order_id: u64,
        account: AccountId,
        symbol: Symbol,
        side: Side,
        (limit, qty, min_qty): (Option<Price>, Qty, Qty),
        quotes: &Quotes,
        now: Timestamp,
    ) -> Fact {
        let decline = |reason: &str| Fact::Declined {
            member,
            member_order_id,
            account,
            symbol,
            side,
            qty,
            reason: format!("declined: {reason}"),
            timestamp: now,
        };
        if self.kind != Venue::Ats {
            return decline("this venue does not run a dark book");
        }
        if !quotes.knows(symbol) {
            return decline("unknown symbol");
        }
        if qty == 0 || min_qty > qty {
            return decline("quantity must be positive and at least the minimum");
        }
        Fact::DarkAccepted(DarkOrder {
            order_id: DARK_IDS | self.next_id,
            member,
            member_order_id,
            account,
            symbol,
            side,
            limit,
            qty,
            min_qty,
            filled: 0,
            timestamp: now,
        })
    }

    pub fn decide_cancel(
        &self,
        member: MemberId,
        order_id: u64,
        now: Timestamp,
    ) -> Result<Fact, String> {
        match self.dark.orders.get(&order_id) {
            Some(order) if order.member == member => Ok(Fact::DarkCancelled {
                order_id,
                reason: "cancelled".into(),
                timestamp: now,
            }),
            Some(_) => Err(format!("order #{order_id} belongs to another member")),
            None => Err(format!("order #{order_id} is not resting here")),
        }
    }

    pub fn decide_cancel_all(
        &self,
        member: MemberId,
        account: Option<AccountId>,
        symbol: Option<Symbol>,
        now: Timestamp,
    ) -> Vec<Fact> {
        self.dark
            .orders
            .values()
            .filter(|o| o.member == member)
            .filter(|o| account.is_none_or(|a| a == o.account))
            .filter(|o| symbol.is_none_or(|s| s == o.symbol))
            .map(|o| Fact::DarkCancelled {
                order_id: o.order_id,
                reason: "cancelled".into(),
                timestamp: now,
            })
            .collect()
    }

    pub fn expire_dark(&self, now: Timestamp) -> Vec<Fact> {
        self.dark
            .orders
            .keys()
            .map(|order_id| Fact::DarkCancelled {
                order_id: *order_id,
                reason: "expired: trading day ended".into(),
                timestamp: now,
            })
            .collect()
    }

    pub fn next_dark_match(&self, quotes: &Quotes, now: Timestamp) -> Option<Fact> {
        for symbol in self.dark.symbols() {
            if !quotes.tradable(symbol) {
                continue;
            }
            let Some((bid, ask)) = quotes.best(symbol) else {
                continue;
            };
            let mid = (bid + ask) / 2;
            if let Some((buy_id, sell_id, qty)) = self.dark.next_match(symbol, mid) {
                return Some(Fact::DarkMatched {
                    buy_id,
                    sell_id,
                    price: mid,
                    qty,
                    cycle: quotes.cycle(),
                    timestamp: now,
                });
            }
        }
        None
    }

    pub fn hedges(&self, quotes: &Quotes, now: Timestamp) -> Vec<Fact> {
        if self.kind != Venue::Wholesaler {
            return vec![];
        }
        let mut next = self.next_id;
        let mut facts = Vec::new();
        for symbol in self.book.inventory.keys() {
            if !quotes.tradable(*symbol) {
                continue;
            }
            let (Some((side, qty)), Some((low, high))) = (
                wholesaler::hedge_need(&self.config, &self.book, *symbol, quotes.closing_soon(now)),
                quotes.band(*symbol),
            ) else {
                continue;
            };
            facts.push(Fact::HedgeSent {
                order_ref: next,
                symbol: *symbol,
                side,
                price: if side == Side::Buy { high } else { low },
                qty: qty.min(self.config.max_order),
            });
            next += 1;
        }
        facts
    }

    pub fn pending_hedges(&self) -> Vec<u64> {
        self.book.hedges.keys().copied().collect()
    }

    pub fn status(&self) -> VenueStatus {
        VenueStatus {
            venue: self.kind,
            fills: self.fills,
            declined: self.declined,
            shares: self.shares,
            pfof_paid: self.pfof_paid,
            cash: self.book.cash,
            resting: self.dark.orders.len() as u64,
            inventory: self
                .book
                .inventory
                .iter()
                .filter(|(_, q)| **q != 0)
                .map(|(s, q)| (*s, *q))
                .collect(),
            members: self
                .members
                .iter()
                .map(|(id, (name, _))| {
                    let (orders, shares) = self.activity.get(id).copied().unwrap_or_default();
                    VenueMemberInfo {
                        member_id: *id,
                        orders,
                        shares,
                        name: name.clone(),
                    }
                })
                .collect(),
        }
    }

    fn publish(&mut self, member: MemberId, message: ServerMessage) -> Effect {
        let message = self.feeds.publish(member, message);
        Effect::ToMember { member, message }
    }

    fn trade_report(
        &self,
        report_id: u64,
        symbol: Symbol,
        (price, qty): (Price, Qty),
        (buy_member, sell_member): (MemberId, MemberId),
    ) -> Effect {
        Effect::ToExchange(MemberMessage::TradeReport {
            report_id,
            symbol,
            price,
            qty,
            venue: self.kind,
            buy_member,
            sell_member,
        })
    }

    pub fn apply(&mut self, fact: &Fact) -> Vec<Effect> {
        match fact {
            Fact::MemberRegistered {
                member,
                name,
                secret,
            } => {
                self.members.insert(*member, (name.clone(), *secret));
                vec![]
            }
            Fact::Filled {
                fill_id,
                member,
                member_order_id,
                account,
                symbol,
                side,
                price,
                qty,
                pfof,
                cycle,
                timestamp,
            } => {
                self.next_id = self.next_id.max(fill_id + 1);
                let signed = *qty as i64;
                let notional = (*price * *qty) as i64;
                let held = self.book.inventory.entry(*symbol).or_default();
                match side {
                    Side::Buy => {
                        *held -= signed;
                        self.book.cash += notional;
                    }
                    Side::Sell => {
                        *held += signed;
                        self.book.cash -= notional;
                    }
                }
                self.book.cash -= *pfof as i64;
                self.fills += 1;
                self.shares += qty;
                self.pfof_paid += *pfof as i64;
                let activity = self.activity.entry(*member).or_default();
                activity.0 += 1;
                activity.1 += qty;
                let message = report(
                    (FILL_IDS | fill_id, *member_order_id, *account),
                    *symbol,
                    *side,
                    OrderStatus::Filled,
                    (*price, *qty, *qty),
                    (*price, *qty, -(*pfof as i64)),
                    *cycle,
                    *timestamp,
                    "wholesaler fill",
                );
                let (buyer, seller) = match side {
                    Side::Buy => (*member, self.self_member),
                    Side::Sell => (self.self_member, *member),
                };
                let mut effects = vec![self.publish(*member, message)];
                if self.self_member != 0 {
                    effects.push(self.trade_report(
                        FILL_IDS | fill_id,
                        *symbol,
                        (*price, *qty),
                        (buyer, seller),
                    ));
                }
                effects
            }
            Fact::Declined {
                member,
                member_order_id,
                account,
                symbol,
                side,
                qty,
                reason,
                timestamp,
            } => {
                self.declined += 1;
                self.activity.entry(*member).or_default().0 += 1;
                let message = report(
                    (0, *member_order_id, *account),
                    *symbol,
                    *side,
                    OrderStatus::Rejected,
                    (0, *qty, 0),
                    (0, 0, 0),
                    0,
                    *timestamp,
                    reason,
                );
                vec![self.publish(*member, message)]
            }
            Fact::HedgeSent {
                order_ref,
                symbol,
                side,
                price,
                qty,
            } => {
                self.next_id = self.next_id.max(order_ref + 1);
                self.book.hedges.insert(*order_ref, *symbol);
                vec![Effect::ToExchange(MemberMessage::NewOrder {
                    member_order_id: *order_ref,
                    account: 1,
                    symbol: *symbol,
                    side: *side,
                    kind: OrderKind::Limit(*price),
                    options: OrderOptions::tif(TimeInForce::Ioc),
                    qty: *qty,
                })]
            }
            Fact::ExchangeReport(r) => {
                self.last_exchange_seq = self.last_exchange_seq.max(r.seq);
                if !self.book.hedges.contains_key(&r.client_order_id) {
                    return vec![];
                }
                if r.last_qty > 0 {
                    let held = self.book.inventory.entry(r.symbol).or_default();
                    let notional = (r.last_price * r.last_qty) as i64;
                    match r.side {
                        Side::Buy => {
                            *held += r.last_qty as i64;
                            self.book.cash -= notional;
                        }
                        Side::Sell => {
                            *held -= r.last_qty as i64;
                            self.book.cash += notional;
                        }
                    }
                    self.book.cash -= r.fees;
                }
                if !r.status.is_open() {
                    self.book.hedges.remove(&r.client_order_id);
                }
                vec![]
            }
            Fact::HedgeAbandoned { order_ref } => {
                self.book.hedges.remove(order_ref);
                vec![]
            }
            Fact::DarkAccepted(order) => {
                self.next_id = self.next_id.max((order.order_id & !DARK_IDS) + 1);
                self.activity.entry(order.member).or_default().0 += 1;
                self.dark.orders.insert(order.order_id, order.clone());
                let message = report(
                    (order.order_id, order.member_order_id, order.account),
                    order.symbol,
                    order.side,
                    OrderStatus::New,
                    (order.limit.unwrap_or(0), order.qty, 0),
                    (0, 0, 0),
                    0,
                    order.timestamp,
                    "resting in the dark pool",
                );
                vec![self.publish(order.member, message)]
            }
            Fact::DarkMatched {
                buy_id,
                sell_id,
                price,
                qty,
                cycle,
                timestamp,
            } => {
                let mut effects = Vec::new();
                let mut parties = [0, 0];
                let mut symbol = None;
                for (slot, id) in [*buy_id, *sell_id].into_iter().enumerate() {
                    let Some(order) = self.dark.orders.get_mut(&id) else {
                        return effects;
                    };
                    order.filled += qty;
                    parties[slot] = order.member;
                    symbol = Some(order.symbol);
                    let done = order.remaining() == 0;
                    let order = order.clone();
                    if done {
                        self.dark.orders.remove(&id);
                    }
                    let activity = self.activity.entry(order.member).or_default();
                    activity.1 += qty;
                    let message = report(
                        (order.order_id, order.member_order_id, order.account),
                        order.symbol,
                        order.side,
                        if done {
                            OrderStatus::Filled
                        } else {
                            OrderStatus::PartiallyFilled
                        },
                        (order.limit.unwrap_or(0), order.qty, order.filled),
                        (*price, *qty, 0),
                        *cycle,
                        *timestamp,
                        "dark pool match",
                    );
                    effects.push(self.publish(order.member, message));
                }
                self.fills += 1;
                self.shares += qty;
                if let Some(symbol) = symbol {
                    effects.push(self.trade_report(
                        *buy_id,
                        symbol,
                        (*price, *qty),
                        (parties[0], parties[1]),
                    ));
                }
                effects
            }
            Fact::DarkCancelled {
                order_id,
                reason,
                timestamp,
            } => {
                let Some(order) = self.dark.orders.remove(order_id) else {
                    return vec![];
                };
                let status = if reason.starts_with("expired") {
                    OrderStatus::Expired
                } else {
                    OrderStatus::Cancelled
                };
                let message = report(
                    (order.order_id, order.member_order_id, order.account),
                    order.symbol,
                    order.side,
                    status,
                    (order.limit.unwrap_or(0), order.qty, order.filled),
                    (0, 0, 0),
                    0,
                    *timestamp,
                    reason,
                );
                vec![self.publish(order.member, message)]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ome_protocol::{BookSnapshot, InstrumentInfo, InstrumentStatus, MemberKind};

    pub fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    pub fn quotes() -> Quotes {
        let mut quotes = Quotes::default();
        quotes.apply(&ServerMessage::Instrument(InstrumentInfo {
            symbol: acme(),
            status: InstrumentStatus::Trading,
            reference_price: 25_00,
            last_price: None,
            low_limit: 17_50,
            high_limit: 32_50,
            band_bps: 1_000,
            volume: 0,
            name: String::new(),
            halt_reason: String::new(),
        }));
        let level = |price| ome_core::order_book::Level {
            price,
            qty: 100,
            orders: 1,
        };
        quotes.apply(&ServerMessage::Book(BookSnapshot {
            symbol: acme(),
            timestamp: 0,
            bids: vec![level(24_90)],
            asks: vec![level(25_10)],
        }));
        quotes
    }

    fn reports(effects: &[Effect]) -> Vec<(MemberId, ExecutionReport)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::ToMember {
                    member,
                    message: ServerMessage::Execution(r),
                } => Some((*member, r.clone())),
                _ => None,
            })
            .collect()
    }

    fn registered(kind: Venue) -> VenueState {
        let mut venue = VenueState::new(kind, WholesalerConfig::default());
        let members = [("retail-co", 2), ("prime-co", 3)].map(|(name, id)| MemberInfo {
            member_id: id,
            kind: MemberKind::Broker,
            connected: true,
            open_orders: 0,
            net_fees: 0,
            credit_limit: 0,
            exposure: 0,
            suspended: false,
            cancel_on_disconnect: false,
            name: name.into(),
        });
        for name in ["retail-co", "prime-co"] {
            let fact = venue
                .decide_register(name, ome_net::auth::hash_secret(name, "pw"), &members)
                .unwrap();
            venue.apply(&fact);
        }
        assert!(venue.decide_register("nobody", [0; 32], &members).is_err());
        venue.set_self_member(9);
        venue
    }

    #[test]
    fn wholesaler_fills_with_improvement_pays_pfof_reports_and_hedges() {
        let mut venue = registered(Venue::Wholesaler);
        let q = quotes();
        let fact = venue.decide_order(2, 11, 4, acme(), Side::Buy, OrderKind::Market, 600, &q, 1);
        let effects = venue.apply(&fact);
        let (member, r) = &reports(&effects)[0];
        assert_eq!(
            (*member, r.status, r.last_price, r.last_qty),
            (2, OrderStatus::Filled, 25_06, 600)
        );
        assert_eq!((r.client_order_id, r.fees, r.seq), (11, -120, 1));
        assert!(
            effects.contains(&Effect::ToExchange(MemberMessage::TradeReport {
                report_id: r.order_id,
                symbol: acme(),
                price: 25_06,
                qty: 600,
                venue: Venue::Wholesaler,
                buy_member: 2,
                sell_member: 9,
            }))
        );

        let declined = venue.decide_order(
            2,
            12,
            4,
            acme(),
            Side::Buy,
            OrderKind::Limit(25_00),
            10,
            &q,
            2,
        );
        let r = &reports(&venue.apply(&declined))[0].1;
        assert_eq!(r.status, OrderStatus::Rejected);
        assert!(r.reason.starts_with("declined"));

        let hedges = venue.hedges(&q, 0);
        assert_eq!(
            hedges,
            vec![Fact::HedgeSent {
                order_ref: 2,
                symbol: acme(),
                side: Side::Buy,
                price: 27_50,
                qty: 600
            }]
        );
        venue.apply(&hedges[0]);
        assert!(
            venue.hedges(&q, 0).is_empty(),
            "one hedge in flight per symbol"
        );
        venue.apply(&Fact::ExchangeReport(ExecutionReport {
            seq: 1,
            order_id: 70,
            client_order_id: 2,
            account: 1,
            symbol: acme(),
            side: Side::Buy,
            status: OrderStatus::Filled,
            price: 27_50,
            qty: 600,
            filled: 600,
            leaves: 0,
            last_price: 25_10,
            last_qty: 600,
            fees: 180,
            cycle: 1,
            timestamp: 3,
            reason: String::new(),
        }));
        let status = venue.status();
        assert!(status.inventory.is_empty());
        assert_eq!(status.cash, 25_06 * 600 - 120 - 25_10 * 600 - 180);
        assert_eq!(
            (status.fills, status.declined, status.pfof_paid),
            (1, 1, 120)
        );
        assert_eq!(venue.replay(2, 0).messages.len(), 2);
    }

    #[test]
    fn dark_pool_rests_matches_at_the_midpoint_and_reports_both_sides() {
        let mut venue = registered(Venue::Ats);
        let q = quotes();
        let buy = venue.decide_dark(2, 1, 4, acme(), Side::Buy, (Some(25_20), 1_000, 300), &q, 1);
        venue.apply(&buy);
        assert_eq!(venue.next_dark_match(&q, 2), None);
        let sell = venue.decide_dark(3, 7, 5, acme(), Side::Sell, (None, 400, 0), &q, 2);
        venue.apply(&sell);
        let matched = venue.next_dark_match(&q, 3).unwrap();
        let effects = venue.apply(&matched);
        let fills = reports(&effects);
        assert_eq!(fills.len(), 2);
        assert_eq!(
            (fills[0].0, fills[0].1.status, fills[0].1.last_price),
            (2, OrderStatus::PartiallyFilled, 25_00)
        );
        assert_eq!(
            (fills[1].0, fills[1].1.status, fills[1].1.client_order_id),
            (3, OrderStatus::Filled, 7)
        );
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::ToExchange(MemberMessage::TradeReport {
                venue: Venue::Ats,
                buy_member: 2,
                sell_member: 3,
                qty: 400,
                ..
            })
        )));
        assert!(
            venue.decide_cancel(3, fills[0].1.order_id, 4).is_err(),
            "not the owner"
        );
        let cancel = venue.decide_cancel(2, fills[0].1.order_id, 4).unwrap();
        let r = &reports(&venue.apply(&cancel))[0].1;
        assert_eq!((r.status, r.filled), (OrderStatus::Cancelled, 400));
        assert_eq!(venue.status().resting, 0);
        let wrong = venue.decide_order(2, 9, 4, acme(), Side::Buy, OrderKind::Market, 1, &q, 5);
        assert!(matches!(wrong, Fact::Declined { .. }));
    }
}
