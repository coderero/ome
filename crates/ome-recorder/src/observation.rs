use ome_core::symbol::Symbol;
use ome_protocol::{
    AccountInfo, BookSnapshot, ClearingStatus, FeeSummary, InstrumentInfo, MarketStats,
    MarketStatus, MemberInfo, NewsItem, ServerMessage, TradeEvent, VenueStatus,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    Instrument(InstrumentInfo),
    Trade(TradeEvent),
    Book(BookSnapshot),
    Stats(MarketStats),
    Market(MarketStatus),
    Members(Vec<MemberInfo>),
    Clearing(ClearingStatus),
    Accounts {
        broker: String,
        list: Vec<AccountInfo>,
    },
    Routing {
        broker: String,
        fees: FeeSummary,
    },
    Venue(VenueStatus),
    News(NewsItem),
}

impl Observation {
    pub fn from_market(message: ServerMessage) -> Vec<Observation> {
        match message {
            ServerMessage::InstrumentList(list) => {
                list.into_iter().map(Observation::Instrument).collect()
            }
            ServerMessage::Instrument(info) => vec![Observation::Instrument(info)],
            ServerMessage::Trade(trade) if trade.trade_id != 0 => vec![Observation::Trade(trade)],
            ServerMessage::Book(book) => vec![Observation::Book(book)],
            ServerMessage::MarketStats(stats) => vec![Observation::Stats(stats)],
            ServerMessage::MarketStatus(status) => vec![Observation::Market(status)],
            ServerMessage::MemberList(list) => vec![Observation::Members(list)],
            ServerMessage::ClearingStatus(status) => vec![Observation::Clearing(status)],
            ServerMessage::VenueStatus(status) => vec![Observation::Venue(status)],
            ServerMessage::News(item) => vec![Observation::News(item)],
            _ => vec![],
        }
    }

    fn key(&self) -> Option<String> {
        Some(match self {
            Observation::Instrument(i) => format!("instrument:{}", i.symbol),
            Observation::Book(b) => format!("book:{}", b.symbol),
            Observation::Stats(s) => format!("stats:{}", s.symbol),
            Observation::Members(_) => "members".into(),
            Observation::Clearing(_) => "clearing".into(),
            Observation::Accounts { broker, .. } => format!("accounts:{broker}"),
            Observation::Routing { broker, .. } => format!("routing:{broker}"),
            Observation::Venue(v) => format!("venue:{}", v.venue as u8),
            Observation::Trade(_) | Observation::Market(_) | Observation::News(_) => return None,
        })
    }

    pub fn symbol(&self) -> Option<Symbol> {
        match self {
            Observation::Instrument(i) => Some(i.symbol),
            Observation::Trade(t) => Some(t.symbol),
            Observation::Book(b) => Some(b.symbol),
            Observation::Stats(s) => Some(s.symbol),
            _ => None,
        }
    }
}

pub fn coalesce(batch: Vec<Observation>) -> Vec<Observation> {
    let mut latest: BTreeMap<String, usize> = BTreeMap::new();
    for (i, observation) in batch.iter().enumerate() {
        if let Some(key) = observation.key() {
            latest.insert(key, i);
        }
    }
    batch
        .into_iter()
        .enumerate()
        .filter(|(i, o)| o.key().is_none_or(|k| latest.get(&k) == Some(i)))
        .map(|(_, o)| o)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ome_core::types::Side;
    use ome_protocol::Venue;

    fn book(symbol: &str, timestamp: u64) -> Observation {
        Observation::Book(BookSnapshot {
            symbol: Symbol::parse(symbol).unwrap(),
            timestamp,
            bids: vec![],
            asks: vec![],
        })
    }

    fn trade(trade_id: u64) -> Observation {
        Observation::Trade(TradeEvent {
            symbol: Symbol::parse("ACME").unwrap(),
            trade_id,
            price: 25_00,
            qty: 1,
            aggressor: Side::Buy,
            timestamp: trade_id,
            venue: Venue::Exchange,
        })
    }

    #[test]
    fn coalescing_keeps_every_trade_and_only_the_latest_snapshot_per_key() {
        let batch = vec![
            book("ACME", 1),
            trade(1),
            book("NOVA", 2),
            book("ACME", 3),
            trade(2),
        ];
        assert_eq!(
            coalesce(batch),
            vec![trade(1), book("NOVA", 2), book("ACME", 3), trade(2)]
        );
    }

    #[test]
    fn snapshot_trades_from_subscriptions_are_not_recorded_twice() {
        let backfill = TradeEvent {
            symbol: Symbol::parse("ACME").unwrap(),
            trade_id: 0,
            price: 1,
            qty: 1,
            aggressor: Side::Sell,
            timestamp: 0,
            venue: Venue::Exchange,
        };
        assert!(Observation::from_market(ServerMessage::Trade(backfill)).is_empty());
        assert_eq!(
            Observation::from_market(ServerMessage::Heartbeat { server_time: 1 }),
            vec![]
        );
    }
}
