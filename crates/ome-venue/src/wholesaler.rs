use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Qty, Side};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WholesalerConfig {
    pub improvement_bps: u64,
    pub pfof_per_share: u64,
    pub max_order: Qty,
    pub inventory_limit: Qty,
    pub hedge_threshold: Qty,
}

impl Default for WholesalerConfig {
    fn default() -> Self {
        Self {
            improvement_bps: 2_000,
            pfof_per_share: 20,
            max_order: 5_000,
            inventory_limit: 20_000,
            hedge_threshold: 500,
        }
    }
}

#[derive(Default)]
pub struct Book {
    pub inventory: BTreeMap<Symbol, i64>,
    pub cash: i64,
    pub hedges: BTreeMap<u64, Symbol>,
}

pub fn fill_price(side: Side, (bid, ask): (Price, Price), improvement_bps: u64) -> Price {
    let improvement = (ask - bid) * improvement_bps / 10_000;
    match side {
        Side::Buy => ask - improvement,
        Side::Sell => bid + improvement,
    }
}

pub fn pfof(qty: Qty, per_share: u64) -> u64 {
    qty * per_share / 100
}

pub fn quote(
    config: &WholesalerConfig,
    book: &Book,
    symbol: Symbol,
    side: Side,
    kind: OrderKind,
    qty: Qty,
    best: Option<(Price, Price)>,
) -> Result<Price, String> {
    let best = best.ok_or("no two-sided exchange quote to price against")?;
    if qty == 0 || qty > config.max_order {
        return Err(format!("size must be 1 - {}", config.max_order));
    }
    let held = book.inventory.get(&symbol).copied().unwrap_or(0);
    let after = match side {
        Side::Buy => held - qty as i64,
        Side::Sell => held + qty as i64,
    };
    if after.unsigned_abs() > config.inventory_limit {
        return Err("inventory limit reached".into());
    }
    let price = fill_price(side, best, config.improvement_bps);
    let acceptable = match (kind, side) {
        (OrderKind::Market, _) => true,
        (OrderKind::Limit(limit), Side::Buy) => limit >= price,
        (OrderKind::Limit(limit), Side::Sell) => limit <= price,
    };
    if !acceptable {
        return Err("limit is not marketable at the wholesaler's price".into());
    }
    Ok(price)
}

pub fn hedge_need(
    config: &WholesalerConfig,
    book: &Book,
    symbol: Symbol,
    flatten: bool,
) -> Option<(Side, Qty)> {
    if book.hedges.values().any(|s| *s == symbol) {
        return None;
    }
    let held = book.inventory.get(&symbol).copied().unwrap_or(0);
    let threshold = if flatten { 0 } else { config.hedge_threshold };
    if held == 0 || held.unsigned_abs() <= threshold {
        return None;
    }
    Some(if held > 0 {
        (Side::Sell, held as Qty)
    } else {
        (Side::Buy, held.unsigned_abs())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    #[test]
    fn fills_improve_on_the_exchange_quote_by_a_share_of_the_spread() {
        assert_eq!(fill_price(Side::Buy, (24_90, 25_10), 2_000), 25_06);
        assert_eq!(fill_price(Side::Sell, (24_90, 25_10), 2_000), 24_94);
        assert_eq!(fill_price(Side::Buy, (24_99, 25_00), 2_000), 25_00);
        assert_eq!(pfof(100, 20), 20);
    }

    #[test]
    fn quotes_respect_size_inventory_and_limit() {
        let config = WholesalerConfig::default();
        let mut book = Book::default();
        let best = Some((24_90, 25_10));
        let q = |book: &Book, side, kind, qty| quote(&config, book, acme(), side, kind, qty, best);
        assert_eq!(q(&book, Side::Buy, OrderKind::Market, 100), Ok(25_06));
        assert!(q(&book, Side::Buy, OrderKind::Limit(25_05), 100).is_err());
        assert!(q(&book, Side::Buy, OrderKind::Market, 6_000).is_err());
        assert!(
            quote(
                &config,
                &book,
                acme(),
                Side::Buy,
                OrderKind::Market,
                1,
                None
            )
            .is_err()
        );
        book.inventory.insert(acme(), -19_950);
        assert!(q(&book, Side::Buy, OrderKind::Market, 100).is_err());
        assert!(q(&book, Side::Sell, OrderKind::Market, 100).is_ok());
    }

    #[test]
    fn hedges_trigger_past_the_threshold_one_at_a_time() {
        let config = WholesalerConfig::default();
        let mut book = Book::default();
        book.inventory.insert(acme(), -300);
        assert_eq!(hedge_need(&config, &book, acme(), false), None);
        assert_eq!(
            hedge_need(&config, &book, acme(), true),
            Some((Side::Buy, 300))
        );
        book.inventory.insert(acme(), -800);
        assert_eq!(
            hedge_need(&config, &book, acme(), false),
            Some((Side::Buy, 800))
        );
        book.hedges.insert(1, acme());
        assert_eq!(hedge_need(&config, &book, acme(), false), None);
    }
}
