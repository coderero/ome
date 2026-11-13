use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, OrderKind, OrderOptions, Qty, Side};
use ome_protocol::{ConditionalReport, ConditionalStatus, Trigger};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conditional {
    pub cond_id: u64,
    pub account: AccountId,
    pub client_order_id: u64,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Qty,
    pub trigger: Trigger,
    pub kind: OrderKind,
    pub options: OrderOptions,
    pub group: u64,
    pub extreme: Price,
}

impl Conditional {
    pub fn level(&self) -> Price {
        match self.trigger {
            Trigger::Above(p) | Trigger::Below(p) => p,
            Trigger::TrailingAbove(d) => self.extreme.saturating_add(d),
            Trigger::TrailingBelow(d) => self.extreme.saturating_sub(d),
            Trigger::MarketOpens => 0,
        }
    }

    pub fn is_due(&self, last: Price) -> bool {
        match self.trigger {
            Trigger::Above(_) | Trigger::TrailingAbove(_) => last >= self.level(),
            Trigger::Below(_) | Trigger::TrailingBelow(_) => last <= self.level(),
            Trigger::MarketOpens => false,
        }
    }

    pub fn new_extreme(&self, last: Price) -> Option<Price> {
        match self.trigger {
            Trigger::TrailingBelow(_) if last > self.extreme => Some(last),
            Trigger::TrailingAbove(_) if last < self.extreme => Some(last),
            _ => None,
        }
    }

    pub fn report(&self, status: ConditionalStatus, reason: &str) -> ConditionalReport {
        ConditionalReport {
            cond_id: self.cond_id,
            client_order_id: self.client_order_id,
            account: self.account,
            symbol: self.symbol,
            side: self.side,
            qty: self.qty,
            status,
            trigger: self.trigger,
            level: self.level(),
            reason: reason.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BracketPlan {
    pub take_profit: Price,
    pub stop_loss: Price,
    pub group: Option<u64>,
}

pub struct Conditionals {
    armed: BTreeMap<u64, Conditional>,
    next_id: u64,
}

impl Default for Conditionals {
    fn default() -> Self {
        Self {
            armed: BTreeMap::new(),
            next_id: 1,
        }
    }
}

impl Conditionals {
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    pub fn get(&self, cond_id: u64) -> Option<&Conditional> {
        self.armed.get(&cond_id)
    }

    pub fn insert(&mut self, conditional: Conditional) {
        self.next_id = self.next_id.max(conditional.cond_id + 1);
        self.armed.insert(conditional.cond_id, conditional);
    }

    pub fn remove(&mut self, cond_id: u64) -> Option<Conditional> {
        self.armed.remove(&cond_id)
    }

    pub fn set_extreme(&mut self, cond_id: u64, extreme: Price) -> Option<&Conditional> {
        let conditional = self.armed.get_mut(&cond_id)?;
        conditional.extreme = extreme;
        Some(conditional)
    }

    pub fn group(&self, group: u64) -> Vec<u64> {
        if group == 0 {
            return Vec::new();
        }
        self.armed
            .values()
            .filter(|c| c.group == group)
            .map(|c| c.cond_id)
            .collect()
    }

    pub fn grow(&mut self, group: u64, qty: Qty) -> Vec<&Conditional> {
        self.armed
            .values_mut()
            .filter(|c| c.group == group)
            .map(|c| {
                c.qty += qty;
                &*c
            })
            .collect()
    }

    pub fn for_account(&self, account: AccountId) -> impl Iterator<Item = &Conditional> {
        self.armed.values().filter(move |c| c.account == account)
    }

    pub fn scan(&self, symbol: Symbol, last: Price) -> (Vec<(u64, Price)>, Vec<u64>) {
        let mut trails = Vec::new();
        let mut due = Vec::new();
        for c in self.armed.values().filter(|c| c.symbol == symbol) {
            if let Some(extreme) = c.new_extreme(last) {
                trails.push((c.cond_id, extreme));
            } else if c.is_due(last) {
                due.push(c.cond_id);
            }
        }
        (trails, due)
    }

    pub fn waiting_for_open(&self) -> Vec<u64> {
        self.armed
            .values()
            .filter(|c| c.trigger == Trigger::MarketOpens)
            .map(|c| c.cond_id)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.armed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.armed.is_empty()
    }
}

pub fn exit_legs(
    side: Side,
    take_profit: Price,
    stop: Price,
    stop_limit: Option<Price>,
) -> [(Trigger, OrderKind); 2] {
    let stop_kind = stop_limit.map_or(OrderKind::Market, OrderKind::Limit);
    match side {
        Side::Sell => [
            (Trigger::Above(take_profit), OrderKind::Limit(take_profit)),
            (Trigger::Below(stop), stop_kind),
        ],
        Side::Buy => [
            (Trigger::Below(take_profit), OrderKind::Limit(take_profit)),
            (Trigger::Above(stop), stop_kind),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conditional(cond_id: u64, trigger: Trigger, extreme: Price) -> Conditional {
        Conditional {
            cond_id,
            account: 1,
            client_order_id: 0,
            symbol: Symbol::parse("ACME").unwrap(),
            side: Side::Sell,
            qty: 10,
            trigger,
            kind: OrderKind::Market,
            options: OrderOptions::default(),
            group: 0,
            extreme,
        }
    }

    #[test]
    fn fixed_triggers_fire_at_or_through_their_level() {
        let stop = conditional(1, Trigger::Below(24_00), 0);
        assert!(!stop.is_due(24_01));
        assert!(stop.is_due(24_00));
        assert!(stop.is_due(23_00));
        let target = conditional(2, Trigger::Above(26_00), 0);
        assert!(!target.is_due(25_99));
        assert!(target.is_due(26_50));
    }

    #[test]
    fn trailing_stop_follows_the_high_and_fires_on_the_pullback() {
        let mut book = Conditionals::default();
        book.insert(conditional(1, Trigger::TrailingBelow(1_00), 25_00));
        let acme = Symbol::parse("ACME").unwrap();
        assert_eq!(book.scan(acme, 24_50), (vec![], vec![]));
        assert_eq!(book.scan(acme, 27_00), (vec![(1, 27_00)], vec![]));
        book.set_extreme(1, 27_00);
        assert_eq!(book.get(1).unwrap().level(), 26_00);
        assert_eq!(book.scan(acme, 26_20), (vec![], vec![]));
        assert_eq!(book.scan(acme, 26_00), (vec![], vec![1]));
    }

    #[test]
    fn exit_legs_point_the_right_way_for_each_side() {
        let sell = exit_legs(Side::Sell, 30_00, 20_00, None);
        assert_eq!(sell[0], (Trigger::Above(30_00), OrderKind::Limit(30_00)));
        assert_eq!(sell[1], (Trigger::Below(20_00), OrderKind::Market));
        let buy = exit_legs(Side::Buy, 20_00, 30_00, Some(30_10));
        assert_eq!(buy[0], (Trigger::Below(20_00), OrderKind::Limit(20_00)));
        assert_eq!(buy[1], (Trigger::Above(30_00), OrderKind::Limit(30_10)));
    }
}
