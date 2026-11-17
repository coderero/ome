use crate::writer::{Copy, Story, Writer};
use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;
use ome_net::auth::SecretHash;
use ome_protocol::{NewsItem, NewsroomStatus, WireItem};
use rand::rngs::StdRng;
use std::collections::{BTreeMap, VecDeque};

const RETAINED: usize = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    SubscriberRegistered {
        name: String,
        secret: SecretHash,
    },
    Published {
        item: NewsItem,
        source: String,
        source_seq: u64,
    },
    Rewritten {
        seq: u64,
        headline: String,
        body: String,
    },
}

#[derive(Default)]
pub struct Newsroom {
    news: VecDeque<NewsItem>,
    next_seq: u64,
    day: u64,
    subscribers: BTreeMap<String, SecretHash>,
    received: BTreeMap<String, u64>,
    sectors: BTreeMap<Symbol, String>,
}

impl Newsroom {
    pub fn new() -> Self {
        Self {
            next_seq: 1,
            ..Self::default()
        }
    }

    pub fn apply(&mut self, fact: &Fact) {
        match fact {
            Fact::SubscriberRegistered { name, secret } => {
                self.subscribers.insert(name.clone(), *secret);
            }
            Fact::Rewritten {
                seq,
                headline,
                body,
            } => {
                if let Some(item) = self.news.iter_mut().find(|n| n.seq == *seq) {
                    item.headline = headline.clone();
                    item.body = body.clone();
                }
            }
            Fact::Published {
                item,
                source,
                source_seq,
            } => {
                let last = self.received.entry(source.clone()).or_default();
                *last = (*last).max(*source_seq);
                self.day = self.day.max(item.day);
                if let Some(symbol) = item.symbol {
                    self.sectors.insert(symbol, item.sector.clone());
                }
                self.next_seq = item.seq + 1;
                if self.news.len() == RETAINED {
                    self.news.pop_front();
                }
                self.news.push_back(item.clone());
            }
        }
    }

    pub fn day(&self) -> u64 {
        self.day
    }

    pub fn subscriber(&self, name: &str) -> Option<SecretHash> {
        self.subscribers.get(name).copied()
    }

    pub fn received(&self, source: &str) -> u64 {
        self.received.get(source).copied().unwrap_or(0)
    }

    pub fn received_total(&self) -> u64 {
        self.received.values().sum()
    }

    pub fn since(&self, last_seq: u64) -> Vec<NewsItem> {
        self.news
            .iter()
            .filter(|n| n.seq > last_seq)
            .cloned()
            .collect()
    }

    pub fn oldest_retained(&self) -> u64 {
        self.news.front().map_or(self.next_seq, |n| n.seq)
    }

    pub fn item(&self, seq: u64) -> Option<&NewsItem> {
        self.news.iter().rev().find(|n| n.seq == seq)
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn recent(&self, limit: usize, symbol: Option<Symbol>) -> Vec<NewsItem> {
        let sector = symbol.and_then(|s| self.sectors.get(&s)).cloned();
        let mut items: Vec<NewsItem> = self
            .news
            .iter()
            .rev()
            .filter(|n| match (symbol, &sector) {
                (Some(s), Some(sector)) => n.about(s, sector),
                (Some(s), None) => n.symbol == Some(s),
                (None, _) => true,
            })
            .take(limit)
            .cloned()
            .collect();
        items.reverse();
        items
    }

    pub fn status(&self, subscribers: u32, sources: u32, writer: String) -> NewsroomStatus {
        NewsroomStatus {
            day: self.day,
            published: self.next_seq - 1,
            subscribers,
            sources,
            received: self.received_total(),
            writer,
        }
    }

    pub fn decide_register(&self, name: &str, secret: SecretHash) -> Result<Fact, String> {
        if self.subscribers.contains_key(name) {
            return Err(format!("{name} already subscribes"));
        }
        Ok(Fact::SubscriberRegistered {
            name: name.to_string(),
            secret,
        })
    }

    pub fn decide_wire(
        &self,
        source: &str,
        wire: &WireItem,
        now: Timestamp,
        rng: &mut StdRng,
        writer: &mut dyn Writer,
    ) -> Result<Option<Fact>, String> {
        let last = self.received(source);
        if wire.seq <= last {
            return Ok(None);
        }
        if !(-100..=100).contains(&wire.sentiment) {
            return Err(format!("wire #{} has sentiment {} outside -100..100", wire.seq, wire.sentiment));
        }
        let story = Story {
            kind: wire.kind,
            event: wire.event.clone(),
            symbol: wire.symbol,
            company: wire.company.clone(),
            sector: wire.sector.clone(),
            sentiment: wire.sentiment,
            facts: wire.facts.clone(),
        };
        let copy = if wire.headline.is_empty() {
            writer.write(&story, rng)
        } else {
            Copy {
                headline: wire.headline.clone(),
                body: wire.body.clone(),
            }
        };
        let mut data = story.data();
        if !wire.data.is_empty() {
            if !data.is_empty() {
                data.push(' ');
            }
            data.push_str(&wire.data);
        }
        Ok(Some(Fact::Published {
            item: NewsItem {
                seq: self.next_seq,
                time: now,
                day: wire.day,
                kind: wire.kind,
                symbol: wire.symbol,
                sentiment: wire.sentiment,
                impact_bps: wire.impact_bps,
                sector: wire.sector.clone(),
                headline: copy.headline,
                body: copy.body,
                data,
            },
            source: source.to_string(),
            source_seq: wire.seq,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::TemplateWriter;
    use ome_protocol::NewsKind;
    use rand::SeedableRng;

    fn wire(seq: u64, kind: NewsKind, event: &str, facts: &[(&str, &str)]) -> WireItem {
        WireItem {
            seq,
            day: 3,
            kind,
            symbol: Symbol::parse("ACME"),
            sentiment: -40,
            impact_bps: -63,
            event: event.into(),
            sector: "robotics".into(),
            company: "Acme Robotics".into(),
            facts: facts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            data: "fund=ACME:3000:900:0".into(),
            headline: String::new(),
            body: String::new(),
        }
    }

    #[test]
    fn wire_events_become_written_stories_once_and_in_order() {
        let mut room = Newsroom::new();
        let mut rng = StdRng::seed_from_u64(1);
        let mut w = TemplateWriter;
        let earnings = wire(1, NewsKind::Earnings, "earnings", &[("eps", "0.63"), ("consensus", "0.65"), ("surprise", "-3.08%")]);
        let fact = room.decide_wire("world", &earnings, 10, &mut rng, &mut w).unwrap().unwrap();
        room.apply(&fact);
        let item = room.item(1).unwrap().clone();
        assert!(item.body.contains("0.63") && item.body.contains("0.65"), "{}", item.body);
        assert_eq!(item.value("eps"), Some("0.63"));
        assert!(item.data.ends_with("fund=ACME:3000:900:0"));
        assert_eq!((item.day, item.time, item.impact_bps), (3, 10, -63));
        assert!(room.decide_wire("world", &earnings, 11, &mut rng, &mut w).unwrap().is_none(), "a resent event is not published twice");
        assert_eq!(room.received("world"), 1);
        let mut invented = wire(2, NewsKind::Company, "warehouse-deal", &[("source", "llm")]);
        invented.headline = "Acme Robotics lands a warehouse automation deal".into();
        invented.body = "Acme Robotics will automate the warehouses of a regional grocer.".into();
        let fact = room.decide_wire("world", &invented, 12, &mut rng, &mut w).unwrap().unwrap();
        room.apply(&fact);
        assert_eq!(room.item(2).unwrap().headline, invented.headline);
        assert_eq!(room.recent(10, Symbol::parse("ACME")).len(), 2);
        assert_eq!(room.status(1, 1, "templates".into()).received, 2);
        let mut bad = wire(3, NewsKind::Company, "x", &[]);
        bad.sentiment = 900;
        assert!(room.decide_wire("world", &bad, 13, &mut rng, &mut w).is_err());
    }
}
