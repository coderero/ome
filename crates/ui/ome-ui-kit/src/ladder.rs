use ome_core::price::Price;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LadderRow {
    pub price: Price,
    pub bid: u64,
    pub ask: u64,
    pub own_bid: u64,
    pub own_ask: u64,
    pub traded: u64,
    pub last: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LadderInput {
    pub bids: Vec<(Price, u64)>,
    pub asks: Vec<(Price, u64)>,
    pub own_bids: Vec<(Price, u64)>,
    pub own_asks: Vec<(Price, u64)>,
    pub prints: Vec<(Price, u64)>,
    pub last: Option<Price>,
    pub tick: Price,
}

fn add(map: &mut BTreeMap<Price, u64>, levels: &[(Price, u64)]) {
    for (price, qty) in levels {
        *map.entry(*price).or_default() += qty;
    }
}

impl LadderInput {
    pub fn center(&self) -> Option<Price> {
        match (self.bids.first(), self.asks.first()) {
            (Some((bid, _)), Some((ask, _))) => Some((bid + ask) / 2),
            _ => self
                .last
                .or(self.bids.first().map(|b| b.0))
                .or(self.asks.first().map(|a| a.0)),
        }
    }

    pub fn rows(&self, center: Price, count: usize) -> Vec<LadderRow> {
        let tick = self.tick.max(1);
        let center = center / tick * tick;
        let half = (count / 2) as u64;
        let top = center + half * tick;
        let maps = |levels: &[(Price, u64)]| {
            let mut map = BTreeMap::new();
            add(&mut map, levels);
            map
        };
        let (bids, asks) = (maps(&self.bids), maps(&self.asks));
        let (own_bids, own_asks) = (maps(&self.own_bids), maps(&self.own_asks));
        let prints = maps(&self.prints);
        (0..count as u64)
            .filter_map(|i| top.checked_sub(i * tick))
            .filter(|p| *p > 0)
            .map(|price| LadderRow {
                price,
                bid: bids.get(&price).copied().unwrap_or(0),
                ask: asks.get(&price).copied().unwrap_or(0),
                own_bid: own_bids.get(&price).copied().unwrap_or(0),
                own_ask: own_asks.get(&price).copied().unwrap_or(0),
                traded: prints.get(&price).copied().unwrap_or(0),
                last: self.last == Some(price),
            })
            .collect()
    }
}

pub fn depth_fraction(qty: u64, rows: &[LadderRow]) -> f32 {
    let max = rows.iter().map(|r| r.bid.max(r.ask)).max().unwrap_or(0);
    if max == 0 {
        0.0
    } else {
        qty as f32 / max as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_centres_on_the_spread_and_merges_book_orders_and_prints() {
        let input = LadderInput {
            bids: vec![(24_98, 100), (24_97, 300)],
            asks: vec![(25_02, 50)],
            own_bids: vec![(24_97, 10)],
            own_asks: vec![],
            prints: vec![(25_02, 5), (25_02, 7)],
            last: Some(25_02),
            tick: 1,
        };
        assert_eq!(input.center(), Some(25_00));
        let rows = input.rows(25_00, 7);
        assert_eq!(rows.len(), 7);
        assert_eq!((rows[0].price, rows[6].price), (25_03, 24_97));
        let ask = rows.iter().find(|r| r.price == 25_02).unwrap();
        assert_eq!((ask.ask, ask.traded, ask.last), (50, 12, true));
        let bid = rows.iter().find(|r| r.price == 24_97).unwrap();
        assert_eq!((bid.bid, bid.own_bid), (300, 10));
        assert_eq!(depth_fraction(150, &rows), 0.5);
        let empty = LadderInput {
            last: Some(10_00),
            ..LadderInput::default()
        };
        assert_eq!(empty.center(), Some(10_00));
        assert!(empty.rows(1, 5).iter().all(|r| r.price > 0));
    }
}
