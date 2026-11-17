use ome_core::order_book::Level;
use ome_core::symbol::Symbol;
use ome_core::types::Side;
use ome_protocol::{
    BookSnapshot, InstrumentInfo, InstrumentStatus, MarketPhase, MarketStats, MarketStatus,
    NewsItem, NewsKind, TradeEvent, Venue,
};
use ome_recorder::observation::Observation;
use ome_recorder::postgres_sink::PostgresSink;
use ome_recorder::redis_sink::RedisSink;
use ome_recorder::sink::Sink;

fn urls() -> Option<(String, String)> {
    Some((
        std::env::var("OME_POSTGRES").ok()?,
        std::env::var("OME_REDIS").ok()?,
    ))
}

#[test]
fn observations_land_in_postgres_and_redis() {
    let Some((pg, rd)) = urls() else {
        eprintln!("skipped: set OME_POSTGRES and OME_REDIS (docker compose up -d)");
        return;
    };
    let symbol = Symbol::parse("TESTX").unwrap();
    let trade_id = 9_000_000_000 + std::process::id() as u64;
    let batch = vec![
        Observation::Instrument(InstrumentInfo {
            symbol,
            status: InstrumentStatus::Trading,
            reference_price: 10_00,
            last_price: Some(10_05),
            low_limit: 7_00,
            high_limit: 13_00,
            band_bps: 1_000,
            volume: 5,
            name: "Test Corp".into(),
            halt_reason: String::new(),
        }),
        Observation::Trade(TradeEvent {
            symbol,
            trade_id,
            price: 10_05,
            qty: 5,
            aggressor: Side::Buy,
            timestamp: 1_700_000_000_000,
            venue: Venue::Wholesaler,
        }),
        Observation::Book(BookSnapshot {
            symbol,
            timestamp: 1,
            bids: vec![Level {
                price: 10_00,
                qty: 50,
                orders: 1,
            }],
            asks: vec![],
        }),
        Observation::Stats(MarketStats {
            symbol,
            open: 10_00,
            high: 10_05,
            low: 10_00,
            last: 10_05,
            vwap: 10_03,
            trades: 2,
            turnover: 100_30,
            exchange_volume: 5,
            internal_volume: 0,
            wholesaler_volume: 5,
            ats_volume: 0,
            pauses: 0,
        }),
        Observation::Market(MarketStatus {
            phase: MarketPhase::Open,
            day: 4,
            since: 0,
            next_change_at: 0,
        }),
        Observation::News(NewsItem {
            seq: trade_id,
            time: 1_700_000_000_000,
            day: 4,
            kind: NewsKind::Earnings,
            symbol: Some(symbol),
            sentiment: -30,
            impact_bps: -120,
            sector: "testing".into(),
            headline: "Test Corp misses".into(),
            body: "EPS 0.10 vs 0.12".into(),
            data: "eps=0.10 consensus=0.12".into(),
        }),
    ];
    let mut postgres = PostgresSink::new(&pg);
    postgres.write(&batch).unwrap();
    postgres.write(&batch).unwrap();
    let mut redis = RedisSink::new(&rd);
    redis.write(&batch).unwrap();

    let mut client = postgres::Client::connect(&pg, postgres::NoTls).unwrap();
    let row = client
        .query_one(
            "SELECT price, qty, venue, extract(epoch FROM traded_at)::bigint FROM trades WHERE trade_id = $1",
            &[&(trade_id as i64)],
        )
        .unwrap();
    assert_eq!(
        (
            row.get::<_, i64>(0),
            row.get::<_, i64>(1),
            row.get::<_, String>(2),
            row.get::<_, i64>(3)
        ),
        (10_05, 5, "wholesaler".to_string(), 1_700_000_000)
    );
    let stats = client
        .query_one(
            "SELECT vwap, wholesaler_volume FROM market_stats WHERE symbol = 'TESTX'",
            &[],
        )
        .unwrap();
    assert_eq!((stats.get::<_, i64>(0), stats.get::<_, i64>(1)), (10_03, 5));
    let news = client
        .query_one(
            "SELECT kind, symbol, sentiment, headline FROM news WHERE seq = $1",
            &[&(trade_id as i64)],
        )
        .unwrap();
    assert_eq!(
        (
            news.get::<_, String>(0),
            news.get::<_, Option<String>>(1),
            news.get::<_, i32>(2),
            news.get::<_, String>(3)
        ),
        (
            "earnings".to_string(),
            Some("TESTX".to_string()),
            -30,
            "Test Corp misses".to_string()
        )
    );

    let mut con = redis::Client::open(rd.as_str())
        .unwrap()
        .get_connection()
        .unwrap();
    let last: String = redis::cmd("GET")
        .arg("ome:last:TESTX")
        .query(&mut con)
        .unwrap();
    let book: String = redis::cmd("GET")
        .arg("ome:book:TESTX")
        .query(&mut con)
        .unwrap();
    let vwap: String = redis::cmd("HGET")
        .arg("ome:stats:TESTX")
        .arg("vwap")
        .query(&mut con)
        .unwrap();
    let trades: usize = redis::cmd("XLEN")
        .arg("ome:trades:TESTX")
        .query(&mut con)
        .unwrap();
    assert_eq!((last.as_str(), vwap.as_str()), ("1005", "1003"));
    assert!(book.contains("[1000,50,1]"));
    assert!(trades >= 1);
    let stories: usize = redis::cmd("XLEN").arg("ome:news").query(&mut con).unwrap();
    assert!(stories >= 1);
}
