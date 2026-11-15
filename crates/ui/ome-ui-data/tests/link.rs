use ome_broker::profile::{ProfileKind, Regime};
use ome_broker::{BrokerConfig, start as start_broker};
use ome_client::Client;
use ome_core::symbol::Symbol;
use ome_core::types::{Side, TimeInForce};
use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start as start_exchange};
use ome_protocol::{ClientMessage, MemberKind, MemberMessage, Role};
use ome_ui_data::{
    AccountView, BrokerLink, Connection, Credentials, LinkUpdate, MarketData, MarketStore,
    OrderEntry, OrderRequest,
};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn pump(
    store: &mut MarketStore,
    updates: &mpsc::Receiver<LinkUpdate>,
    until: impl Fn(&MarketStore) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !until(store) {
        assert!(
            Instant::now() < deadline,
            "store never reached the expected state"
        );
        match updates.recv_timeout(Duration::from_millis(100)) {
            Ok(LinkUpdate::Message(message)) => store.apply(message),
            Ok(LinkUpdate::Down(_)) => store.disconnected(),
            Ok(LinkUpdate::Rejected(reason)) => panic!("rejected: {reason}"),
            Err(_) => {}
        }
    }
}

#[test]
fn a_ui_link_streams_the_market_and_trades_through_a_real_broker() {
    let exchange = start_exchange(ServerConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        admin_name: "ops".into(),
        admin_secret: "ops".into(),
        exchange: ExchangeConfig::default(),
    })
    .unwrap()
    .0;
    let (mut ops, _) = Client::connect_member(&exchange.to_string(), "ops", "ops").unwrap();
    let member = ops
        .request_member(&MemberMessage::CreateMember {
            kind: MemberKind::Broker,
            name: "retail-co".into(),
            secret: "pw".into(),
        })
        .unwrap();
    assert!(!member.is_empty());
    let broker = start_broker(BrokerConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        profile: ProfileKind::Retail,
        admin_name: "admin".into(),
        admin_secret: "admin".into(),
        exchange_addr: exchange.to_string(),
        member_name: "retail-co".into(),
        member_secret: "pw".into(),
        clearing_addr: None,
        regime: Regime::India,
        wholesaler_addr: None,
        ats_addr: None,
        news_addr: None,
        postgres: None,
    })
    .unwrap()
    .0;
    ops.request_member(&MemberMessage::ListInstrument {
        symbol: Symbol::parse("ACME").unwrap(),
        ipo_price: 25_00,
        shares: 1_000,
        underwriter: 2,
        issuer_account: 1,
        name: "Acme Robotics".into(),
    })
    .unwrap();
    let (mut admin, _) = Client::connect_and_logon(&broker.to_string(), "admin", "admin").unwrap();
    admin
        .request(&ClientMessage::CreateAccount {
            role: Role::Retail,
            cash: 10_000_00,
            username: "alice".into(),
            secret: "pw".into(),
        })
        .unwrap();

    let (tx, rx) = mpsc::channel();
    let link = BrokerLink::start(
        Credentials {
            addr: broker.to_string(),
            user: "alice".into(),
            secret: "pw".into(),
        },
        tx,
    );
    let mut store = MarketStore::default();
    let acme = Symbol::parse("ACME").unwrap();
    pump(&mut store, &rx, |s| {
        matches!(s.connection, Connection::Online { .. })
            && s.instrument(acme).is_some()
            && s.book(acme).is_some()
            && s.account().is_some()
    });
    assert_eq!(store.account().unwrap().cash.available, 10_000_00);
    assert!(link.submit(OrderRequest::Limit {
        symbol: acme,
        side: Side::Buy,
        price: 24_00,
        qty: 10,
        tif: TimeInForce::Gtc,
    }));
    pump(&mut store, &rx, |s| {
        s.open_orders().len() == 1 && s.book(acme).is_some_and(|b| !b.bids.is_empty())
    });
    let order = store.open_orders()[0].order_id;
    assert!(link.submit(OrderRequest::Cancel(order)));
    pump(&mut store, &rx, |s| s.open_orders().is_empty());
    assert!(link.submit(OrderRequest::Market {
        symbol: acme,
        side: Side::Buy,
        qty: 4,
    }));
    pump(&mut store, &rx, |s| {
        s.account().is_some_and(|a| {
            a.positions
                .iter()
                .any(|p| p.symbol == acme && p.balance.available + p.balance.reserved == 4)
        })
    });
    assert_eq!(store.tape(acme)[0].price, 25_00);
    assert!(!store.contract_notes().is_empty());
    let held = store
        .account()
        .unwrap()
        .positions
        .iter()
        .find(|p| p.symbol == acme)
        .cloned()
        .unwrap();
    assert_eq!((held.avg_cost, held.day_cost), (25_00, 25_00));

    assert!(link.query(ClientMessage::EstimateCharges {
        symbol: acme,
        side: Side::Buy,
        qty: 10,
        price: 25_00,
    }));
    pump(&mut store, &rx, |s| s.estimate.is_some());
    let estimate = store.estimate.unwrap();
    assert!(estimate.charges.total() > 0);
    assert_eq!(estimate.cash_effect, -(25_000 + estimate.charges.total()));
    assert!(link.query(ClientMessage::QueryHistory {
        symbol: acme,
        interval_ms: 60_000,
        limit: 100,
    }));
    pump(&mut store, &rx, |s| s.intervals(acme).contains(&60_000));
    assert!(!store.candles(acme, 60_000).is_empty());

    let (tx, rejected) = mpsc::channel();
    let _wrong = BrokerLink::start(
        Credentials {
            addr: broker.to_string(),
            user: "alice".into(),
            secret: "wrong".into(),
        },
        tx,
    );
    assert!(matches!(
        rejected.recv_timeout(Duration::from_secs(5)),
        Ok(LinkUpdate::Rejected(_))
    ));
    assert!(
        rejected.recv_timeout(Duration::from_secs(3)).is_err(),
        "no retry after a rejected logon"
    );
}
