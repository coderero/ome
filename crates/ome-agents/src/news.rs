use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;
use ome_protocol::{NewsItem, NewsKind};
use std::collections::{BTreeMap, VecDeque};

const KEEP: usize = 200;
const HALF_LIFE_MS: f64 = 45_000.0;

#[derive(Debug, Clone, Default)]
pub struct NewsDesk {
    pub valuations: crate::valuation::Valuations,
    sectors: BTreeMap<Symbol, String>,
    items: VecDeque<NewsItem>,
    last_seq: u64,
}

impl NewsDesk {
    pub fn absorb(&mut self, item: &NewsItem) -> bool {
        if let Some(symbol) = item.symbol
            && !item.sector.is_empty()
        {
            self.sectors.insert(symbol, item.sector.clone());
        }
        if item.seq <= self.last_seq {
            return false;
        }
        self.last_seq = item.seq;
        self.valuations.absorb(item);
        if self.items.len() == KEEP {
            self.items.pop_front();
        }
        self.items.push_back(item.clone());
        true
    }

    pub fn sector(&self, symbol: Symbol) -> &str {
        self.sectors.get(&symbol).map_or("", String::as_str)
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn about(&self, item: &NewsItem, symbol: Symbol) -> bool {
        match item.kind {
            NewsKind::Sector => !item.sector.is_empty() && item.sector == self.sector(symbol),
            _ => item.about(symbol, self.sector(symbol)),
        }
    }

    pub fn after(&self, seq: u64) -> impl Iterator<Item = &NewsItem> {
        self.items.iter().filter(move |i| i.seq > seq)
    }

    pub fn mood(&self, symbol: Symbol, now: Timestamp) -> f64 {
        self.items
            .iter()
            .filter(|i| self.about(i, symbol))
            .map(|i| {
                let weight = match i.kind {
                    NewsKind::Company | NewsKind::Earnings => 1.0,
                    NewsKind::Sector => 0.6,
                    NewsKind::Macro => 0.4,
                    NewsKind::Prospectus => 0.0,
                };
                let age = now.saturating_sub(i.time) as f64;
                weight * i.sentiment as f64 / 100.0 * 0.5_f64.powf(age / HALF_LIFE_MS)
            })
            .sum::<f64>()
            .clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn story(
        seq: u64,
        kind: NewsKind,
        symbol: &str,
        sentiment: i64,
        time: Timestamp,
    ) -> NewsItem {
        NewsItem {
            seq,
            time,
            day: 1,
            kind,
            symbol: Symbol::parse(symbol),
            sentiment,
            impact_bps: sentiment * 3,
            sector: "robotics".into(),
            headline: format!("story {seq}"),
            body: String::new(),
            data: String::new(),
        }
    }

    #[test]
    fn news_mood_weights_relevance_and_fades_with_time() {
        let acme = Symbol::parse("ACME").unwrap();
        let bolt = Symbol::parse("BOLT").unwrap();
        let mut desk = NewsDesk::default();
        assert!(desk.absorb(&story(1, NewsKind::Earnings, "ACME", 80, 0)));
        assert!(
            !desk.absorb(&story(1, NewsKind::Earnings, "ACME", 80, 0)),
            "seen already"
        );
        assert!((desk.mood(acme, 0) - 0.8).abs() < 1e-9);
        assert!((desk.mood(acme, 45_000) - 0.4).abs() < 1e-9, "half-life");
        assert_eq!(
            desk.mood(bolt, 0),
            0.0,
            "sector of BOLT unknown and not about it"
        );
        desk.absorb(&story(2, NewsKind::Sector, "", -50, 0));
        assert!(desk.mood(acme, 0) < 0.8 && desk.mood(bolt, 0) == 0.0);
        desk.absorb(&story(3, NewsKind::Company, "BOLT", 10, 0));
        assert!(
            desk.mood(bolt, 0) < 0.0,
            "BOLT is in robotics now, so the sector story counts"
        );
        desk.absorb(&story(4, NewsKind::Macro, "", -100, 0));
        desk.absorb(&story(5, NewsKind::Macro, "", -100, 0));
        desk.absorb(&story(6, NewsKind::Macro, "", -100, 0));
        assert!(desk.mood(bolt, 0) >= -1.0);
        assert_eq!(desk.after(4).count(), 2);
        assert_eq!(desk.last_seq(), 6);
    }
}
