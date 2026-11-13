use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{AccountId, Qty, Side, Timestamp};
use ome_protocol::MemberId;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DarkOrder {
    pub order_id: u64,
    pub member: MemberId,
    pub member_order_id: u64,
    pub account: AccountId,
    pub symbol: Symbol,
    pub side: Side,
    pub limit: Option<Price>,
    pub qty: Qty,
    pub min_qty: Qty,
    pub filled: Qty,
    pub timestamp: Timestamp,
}

impl DarkOrder {
    pub fn remaining(&self) -> Qty {
        self.qty - self.filled
    }

    fn accepts(&self, price: Price) -> bool {
        match (self.side, self.limit) {
            (_, None) => true,
            (Side::Buy, Some(limit)) => limit >= price,
            (Side::Sell, Some(limit)) => limit <= price,
        }
    }

    fn minimum(&self) -> Qty {
        self.min_qty.min(self.remaining()).max(1)
    }
}

#[derive(Default)]
pub struct DarkBook {
    pub orders: BTreeMap<u64, DarkOrder>,
}

impl DarkBook {
    pub fn next_match(&self, symbol: Symbol, mid: Price) -> Option<(u64, u64, Qty)> {
        let resting: Vec<&DarkOrder> = self
            .orders
            .values()
            .filter(|o| o.symbol == symbol && o.remaining() > 0 && o.accepts(mid))
            .collect();
        for buy in resting.iter().filter(|o| o.side == Side::Buy) {
            for sell in resting.iter().filter(|o| o.side == Side::Sell) {
                if (buy.member, buy.account) == (sell.member, sell.account) {
                    continue;
                }
                let qty = buy.remaining().min(sell.remaining());
                if qty >= buy.minimum() && qty >= sell.minimum() {
                    return Some((buy.order_id, sell.order_id, qty));
                }
            }
        }
        None
    }

    pub fn symbols(&self) -> Vec<Symbol> {
        let mut symbols: Vec<Symbol> = self.orders.values().map(|o| o.symbol).collect();
        symbols.sort();
        symbols.dedup();
        symbols
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(order_id: u64, member: MemberId, side: Side, qty: Qty, min_qty: Qty) -> DarkOrder {
        DarkOrder {
            order_id,
            member,
            member_order_id: order_id,
            account: 1,
            symbol: Symbol::parse("ACME").unwrap(),
            side,
            limit: None,
            qty,
            min_qty,
            filled: 0,
            timestamp: 0,
        }
    }

    #[test]
    fn matches_respect_minimum_quantity_limits_and_ownership() {
        let acme = Symbol::parse("ACME").unwrap();
        let mut book = DarkBook::default();
        book.orders.insert(1, order(1, 2, Side::Buy, 1_000, 500));
        book.orders.insert(2, order(2, 2, Side::Sell, 1_000, 0));
        assert_eq!(
            book.next_match(acme, 25_00),
            None,
            "same account never crosses"
        );
        book.orders.insert(3, order(3, 3, Side::Sell, 200, 0));
        assert_eq!(
            book.next_match(acme, 25_00),
            None,
            "200 is below the buyer's minimum"
        );
        book.orders.insert(4, order(4, 4, Side::Sell, 600, 0));
        assert_eq!(book.next_match(acme, 25_00), Some((1, 4, 600)));
        let mut capped = order(5, 5, Side::Sell, 800, 0);
        capped.limit = Some(25_10);
        book.orders.remove(&4);
        book.orders.insert(5, capped);
        assert_eq!(
            book.next_match(acme, 25_00),
            None,
            "seller wants at least 25.10"
        );
        assert_eq!(book.next_match(acme, 25_10), Some((1, 5, 800)));
    }
}
