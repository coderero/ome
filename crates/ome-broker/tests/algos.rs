use ome_broker::profile::{ProfileKind, Regime};
use ome_broker::{BrokerConfig, start as start_broker};
use ome_client::Client;
use ome_core::symbol::Symbol;
use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start as start_exchange};
use ome_protocol::{
    AlgoStatus, AlgoStrategy, ClientMessage, MemberKind, MemberMessage, Role, ServerMessage,
};
use std::time::{Duration, Instant};

#[test]
fn twap_parent_fills_through_child_orders_on_the_real_exchange() {
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
    for name in ["retail-co", "prime-co"] {
        ops.request_member(&MemberMessage::CreateMember {
            kind: MemberKind::Broker,
            name: name.into(),
            secret: "pw".into(),
        })
        .unwrap();
    }
    let prime = start_broker(BrokerConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        profile: ProfileKind::Prime,
        admin_name: "admin".into(),
        admin_secret: "admin".into(),
        exchange_addr: exchange.to_string(),
        member_name: "prime-co".into(),
        member_secret: "pw".into(),
        clearing_addr: None,
        regime: Regime::India,
        wholesaler_addr: None,
        ats_addr: None,
    })
    .unwrap()
    .0;
    let (mut admin, _) = Client::connect_and_logon(&prime.to_string(), "admin", "admin").unwrap();
    let mut create = |role, name: &str, cash| -> u64 {
        admin
            .request(&ClientMessage::CreateAccount {
                role,
                cash,
                username: name.into(),
                secret: "pw".into(),
            })
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    };
    let issuer = create(Role::Issuer, "acme_ir", 0);
    create(Role::Quant, "fund", 1_000_000_00);
    let acme = Symbol::parse("ACME").unwrap();
    ops.request_member(&MemberMessage::ListInstrument {
        symbol: acme,
        ipo_price: 20_00,
        shares: 1_000,
        underwriter: 3,
        issuer_account: issuer,
        name: "Acme".into(),
    })
    .unwrap();

    let (mut fund, _) = Client::connect_and_logon(&prime.to_string(), "fund", "pw").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let started = loop {
        assert!(Instant::now() < deadline, "algo never started");
        fund.send(&ClientMessage::AlgoOrder {
            client_order_id: 9,
            symbol: acme,
            side: ome_core::types::Side::Buy,
            qty: 120,
            limit: Some(20_00),
            strategy: AlgoStrategy::Twap {
                duration_ms: 1_000,
                slices: 3,
            },
        })
        .unwrap();
        match fund
            .wait_for(Duration::from_secs(2), |m| {
                matches!(
                    m,
                    ServerMessage::AlgoUpdate(_) | ServerMessage::Reject { .. }
                )
            })
            .unwrap()
        {
            ServerMessage::AlgoUpdate(a) => break a,
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    assert_eq!(started.status, AlgoStatus::Working);
    let done = fund
        .wait_for(
            Duration::from_secs(10),
            |m| matches!(m, ServerMessage::AlgoUpdate(a) if a.status != AlgoStatus::Working),
        )
        .unwrap();
    let ServerMessage::AlgoUpdate(done) = done else {
        unreachable!()
    };
    assert_eq!(done.status, AlgoStatus::Done, "{}", done.reason);
    assert_eq!(
        (done.filled, done.avg_price, done.children),
        (120, 20_00, 3)
    );
}
