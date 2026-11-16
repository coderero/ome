use ome_core::price::{Price, parse_price};
use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;
use ome_protocol::{NewsItem, NewsKind};
use std::collections::{BTreeMap, VecDeque};

const KEEP: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    pub time: Timestamp,
    pub forward_e4: f64,
    pub cash_e2: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Book {
    pub anchor: Option<(Price, f64, f64)>,
    pub history: VecDeque<Snapshot>,
}

#[derive(Debug, Clone, Default)]
pub struct Valuations {
    books: BTreeMap<Symbol, Book>,
}

pub fn forward_e4(eps_e4: i64, growth_bps: i64) -> f64 {
    eps_e4 as f64 * 4.0 * (1.0 + growth_bps.clamp(-2_000, 3_000) as f64 / 10_000.0)
}

fn entries(data: &str) -> Vec<(Symbol, i64, i64, i64)> {
    let Some(fund) = data.split_whitespace().find_map(|w| w.strip_prefix("fund=")) else {
        return Vec::new();
    };
    fund.split(';')
        .filter_map(|e| {
            let mut parts = e.split(':');
            let symbol = Symbol::parse(parts.next()?)?;
            let eps = parts.next()?.parse().ok()?;
            let growth = parts.next()?.parse().ok()?;
            let cash = parts.next()?.parse().ok()?;
            Some((symbol, eps, growth, cash))
        })
        .collect()
}

impl Valuations {
    pub fn absorb(&mut self, item: &NewsItem) {
        let listing = (item.kind == NewsKind::Prospectus)
            .then(|| item.value("price").and_then(parse_price))
            .flatten();
        for (symbol, eps, growth, cash) in entries(&item.data) {
            let book = self.books.entry(symbol).or_default();
            let snapshot = Snapshot { time: item.time, forward_e4: forward_e4(eps, growth), cash_e2: cash as f64 };
            if let Some(price) = listing.filter(|_| item.symbol == Some(symbol)) {
                book.anchor = Some((price, snapshot.forward_e4, snapshot.cash_e2));
            }
            if book.history.len() == KEEP {
                book.history.pop_front();
            }
            book.history.push_back(snapshot);
        }
    }

    pub fn knows(&self, symbol: Symbol) -> bool {
        self.books.get(&symbol).is_some_and(|b| !b.history.is_empty())
    }

    pub fn value(&self, symbol: Symbol, as_of: Timestamp, market_anchor: Price) -> Option<f64> {
        let book = self.books.get(&symbol)?;
        let first = *book.history.front()?;
        let now = book.history.iter().rev().find(|s| s.time <= as_of).copied().unwrap_or(first);
        let (price, forward, cash) = book.anchor.unwrap_or((market_anchor, first.forward_e4, first.cash_e2));
        if forward <= 0.0 || price == 0 {
            return None;
        }
        Some((price as f64 * (now.forward_e4 / forward).max(0.05) + now.cash_e2 - cash).max(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(seq: u64, time: Timestamp, kind: NewsKind, data: &str) -> NewsItem {
        NewsItem {
            seq,
            time,
            day: 1,
            kind,
            symbol: Symbol::parse("ACME"),
            sentiment: 0,
            impact_bps: 0,
            sector: "robotics".into(),
            headline: String::new(),
            body: String::new(),
            data: data.into(),
        }
    }

    #[test]
    fn values_come_from_published_fundamentals_with_a_lag() {
        let acme = Symbol::parse("ACME").unwrap();
        let mut book = Valuations::default();
        assert_eq!(book.value(acme, 0, 25_00), None, "nothing known yet");
        book.absorb(&item(1, 1_000, NewsKind::Prospectus, "price=25.00 fund=ACME:3125:900:0"));
        assert!((book.value(acme, 1_000, 99_00).unwrap() - 25_00.0).abs() < 0.5, "the listing anchors the value");
        book.absorb(&item(2, 5_000, NewsKind::Company, "event=contract-win fund=ACME:3125:915:62"));
        let later = book.value(acme, 5_000, 99_00).unwrap();
        assert!(later > 25_60.0 && later < 25_80.0, "growth and the one-off profit add a little: {later}");
        assert!((book.value(acme, 4_999, 99_00).unwrap() - 25_00.0).abs() < 0.5, "an agent that has not read it yet still sees the old value");

        let nova = Symbol::parse("NOVA").unwrap();
        book.absorb(&item(3, 6_000, NewsKind::Earnings, "eps=0.20 fund=NOVA:2000:400:0"));
        assert_eq!(book.value(nova, 6_000, 12_00), Some(12_00.0), "without a prospectus the market price anchors the first reading");
        book.absorb(&item(4, 7_000, NewsKind::Earnings, "fund=NOVA:2200:400:0"));
        assert!((book.value(nova, 7_000, 12_00).unwrap() - 13_20.0).abs() < 0.5, "and later readings move it");
    }
}
