pub mod link;
pub mod store;

pub use link::{BrokerLink, Credentials, LinkUpdate, OrderEntry, OrderRequest};
pub use store::{AccountView, Connection, MarketData, MarketStore, NewsFeed, Notice};

#[cfg(test)]
mod tests {
    use super::*;
    use ome_core::candles::Candle;
    use ome_core::symbol::Symbol;
    use ome_core::types::Side;
    use ome_protocol::{
        CandleSnapshot, ExecutionReport, InstrumentInfo, InstrumentStatus, NewsItem, NewsKind,
        OrderStatus, Role, ServerMessage, TradeEvent, Venue,
    };

    fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    fn trade(id: u64, price: u64, at: u64) -> ServerMessage {
        ServerMessage::Trade(TradeEvent {
            symbol: acme(),
            trade_id: id,
            price,
            qty: 5,
            aggressor: Side::Buy,
            timestamp: at,
            venue: Venue::Exchange,
        })
    }

    fn report(order_id: u64, status: OrderStatus) -> ServerMessage {
        ServerMessage::Execution(ExecutionReport {
            seq: 0,
            order_id,
            client_order_id: 1,
            account: 7,
            symbol: acme(),
            side: Side::Buy,
            status,
            price: 25_00,
            qty: 10,
            filled: 0,
            leaves: 10,
            last_price: 0,
            last_qty: 0,
            fees: 0,
            cycle: 0,
            timestamp: 0,
            reason: if status == OrderStatus::Rejected {
                "insufficient cash".into()
            } else {
                String::new()
            },
        })
    }

    #[test]
    fn the_store_follows_the_broker_feed() {
        let mut store = MarketStore::default();
        store.apply(ServerMessage::LogonAck {
            account_id: 7,
            role: Role::Retail,
            server_time: 1_000,
        });
        assert_eq!(
            store.connection,
            Connection::Online {
                account: 7,
                role: Role::Retail
            }
        );
        store.apply(ServerMessage::InstrumentList(vec![InstrumentInfo {
            symbol: acme(),
            status: InstrumentStatus::Trading,
            reference_price: 25_00,
            last_price: None,
            low_limit: 22_50,
            high_limit: 27_50,
            band_bps: 1_000,
            volume: 0,
            name: "Acme".into(),
            halt_reason: String::new(),
        }]));
        assert_eq!(store.last(acme()), Some(25_00));
        store.apply(ServerMessage::Candles(CandleSnapshot {
            symbol: acme(),
            interval: 1_000,
            candles: vec![Candle::new(1_000, 25_00, 1)],
        }));
        store.apply(trade(0, 24_00, 1_100));
        assert!(
            store.tape(acme()).is_empty(),
            "snapshot prints are not tape"
        );
        store.apply(trade(1, 25_10, 1_500));
        store.apply(trade(2, 25_20, 2_200));
        assert_eq!(store.tape(acme())[0].trade_id, 2, "newest first");
        assert_eq!(store.last(acme()), Some(25_20));
        let candles = store.candles(acme(), 1_000);
        assert_eq!(candles.len(), 2);
        assert_eq!((candles[0].high, candles[1].close), (25_10, 25_20));
        store.roll(5_000);
        assert_eq!(
            store.candles(acme(), 1_000).len(),
            5,
            "flat candles fill quiet time"
        );
        assert_eq!(
            store.candles(acme(), 5_000).len(),
            2,
            "coarser intervals aggregate the finest"
        );
        store.apply(ServerMessage::Candles(CandleSnapshot {
            symbol: acme(),
            interval: 60_000,
            candles: vec![Candle::new(0, 24_00, 9)],
        }));
        store.apply(trade(3, 25_30, 5_500));
        assert_eq!(
            store.candles(acme(), 60_000)[0].high,
            25_30,
            "live trades extend history too"
        );
        assert_eq!(store.intervals(acme()), vec![1_000, 60_000]);

        store.apply(report(9, OrderStatus::New));
        assert_eq!(store.open_orders().len(), 1);
        store.apply(report(9, OrderStatus::Cancelled));
        assert!(store.open_orders().is_empty());
        assert_eq!(store.closed_orders()[0].status, OrderStatus::Cancelled);
        store.apply(report(10, OrderStatus::Rejected));
        assert!(store.notices()[0].error && store.notices()[0].text.contains("insufficient"));

        let story = NewsItem {
            seq: 3,
            time: 0,
            day: 1,
            kind: NewsKind::Company,
            symbol: Some(acme()),
            sentiment: 40,
            impact_bps: 120,
            sector: "robotics".into(),
            headline: "Acme wins".into(),
            body: String::new(),
            data: String::new(),
        };
        store.apply(ServerMessage::News(story.clone()));
        store.apply(ServerMessage::News(story));
        assert_eq!(store.news().len(), 1, "replayed stories are not doubled");
        store.disconnected();
        assert_eq!(store.connection, Connection::Offline);
    }
}
