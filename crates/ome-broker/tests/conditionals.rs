use ome_broker::profile::{ProfileKind, Regime};
use ome_broker::{BrokerConfig, start as start_broker};
use ome_client::Client;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, OrderOptions, Side};
use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start as start_exchange};
use ome_protocol::{
    Asset, ClientMessage, ConditionalStatus, MemberKind, MemberMessage, OrderStatus, Role,
    ServerMessage, Trigger,
};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

fn eventually<T>(mut attempt: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = attempt() {
            return value;
        }
        assert!(Instant::now() < deadline, "condition not met within 10s");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn broker(exchange: SocketAddr, profile: ProfileKind, member: &str) -> SocketAddr {
    start_broker(BrokerConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        profile,
        admin_name: "admin".into(),
        admin_secret: "admin".into(),
        exchange_addr: exchange.to_string(),
        member_name: member.into(),
        member_secret: "pw".into(),
        clearing_addr: None,
        regime: Regime::India,
        wholesaler_addr: None,
        ats_addr: None,
        news_addr: None,
    })
    .unwrap()
    .0
}

fn create(admin: &mut Client, name: &str, shares: u64) -> u64 {
    let text = admin
        .request(&ClientMessage::CreateAccount {
            role: Role::Retail,
            cash: 10_000_00,
            username: name.into(),
            secret: "pw".into(),
        })
        .unwrap();
    let id = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    if shares > 0 {
        admin
            .request(&ClientMessage::SetBalance {
                account_id: id,
                asset: Asset::Shares(Symbol::parse("ACME").unwrap()),
                amount: shares,
            })
            .unwrap();
    }
    id
}

fn resting(client: &mut Client, side: Side, price: u64, qty: u64) {
    let acme = Symbol::parse("ACME").unwrap();
    eventually(|| {
        let id = client
            .send_order(acme, side, OrderKind::Limit(price), qty)
            .ok()?;
        match client
            .wait_for(
                Duration::from_secs(2),
                |m| matches!(m, ServerMessage::Execution(r) if r.client_order_id == id),
            )
            .ok()?
        {
            ServerMessage::Execution(r) if r.status != OrderStatus::Rejected => Some(()),
            _ => None,
        }
    });
}

#[test]
fn stop_loss_fires_on_an_exchange_trade_and_sells_into_the_bid() {
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
    let retail = broker(exchange, ProfileKind::Retail, "retail-co");
    let prime = broker(exchange, ProfileKind::Prime, "prime-co");
    let (mut prime_admin, _) =
        Client::connect_and_logon(&prime.to_string(), "admin", "admin").unwrap();
    let issuer = prime_admin
        .request(&ClientMessage::CreateAccount {
            role: Role::Issuer,
            cash: 0,
            username: "acme_ir".into(),
            secret: "pw".into(),
        })
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
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

    let (mut admin, _) = Client::connect_and_logon(&retail.to_string(), "admin", "admin").unwrap();
    create(&mut admin, "alice", 25);
    create(&mut admin, "bob", 10);
    create(&mut admin, "carol", 0);
    let logon = |name: &str| {
        Client::connect_and_logon(&retail.to_string(), name, "pw")
            .unwrap()
            .0
    };
    let (mut alice, mut bob, mut carol) = (logon("alice"), logon("bob"), logon("carol"));

    let armed = eventually(|| {
        alice
            .send(&ClientMessage::Conditional {
                client_order_id: 77,
                symbol: acme,
                side: Side::Sell,
                qty: 25,
                trigger: Trigger::Below(19_50),
                kind: OrderKind::Market,
                options: OrderOptions::default(),
            })
            .ok()?;
        match alice
            .wait_for(Duration::from_secs(2), |m| {
                matches!(
                    m,
                    ServerMessage::ConditionalUpdate(_) | ServerMessage::Reject { .. }
                )
            })
            .ok()?
        {
            ServerMessage::ConditionalUpdate(c) => Some(c),
            _ => None,
        }
    });
    assert_eq!(armed.status, ConditionalStatus::Armed);

    resting(&mut carol, Side::Buy, 19_50, 40);
    resting(&mut bob, Side::Sell, 19_50, 10);

    let triggered = alice
        .wait_for(Duration::from_secs(5), |m| {
            matches!(m, ServerMessage::ConditionalUpdate(c) if c.status != ConditionalStatus::Armed)
        })
        .unwrap();
    let ServerMessage::ConditionalUpdate(triggered) = triggered else {
        unreachable!()
    };
    assert_eq!(
        triggered.status,
        ConditionalStatus::Triggered,
        "{}",
        triggered.reason
    );
    let fill = alice
        .wait_for(Duration::from_secs(5), |m| {
            matches!(m, ServerMessage::Execution(r) if r.client_order_id == 77 && !r.status.is_open())
        })
        .unwrap();
    let ServerMessage::Execution(fill) = fill else {
        unreachable!()
    };
    assert_eq!(fill.status, OrderStatus::Filled);
    assert_eq!((fill.filled, fill.last_price), (25, 19_50));
}
