use ome_broker::fees::FeeEngine;
use ome_broker::profile::{ProfileKind, Regime};
use ome_broker::{BrokerConfig, start as start_broker};
use ome_client::{Client, ClientError};
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side};
use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start as start_exchange};
use ome_protocol::{ClientMessage, MemberKind, MemberMessage, OrderStatus, Role, ServerMessage};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

fn exchange() -> SocketAddr {
    start_exchange(ServerConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        admin_name: "ops".into(),
        admin_secret: "ops".into(),
        exchange: ExchangeConfig::default(),
    })
    .unwrap()
    .0
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
    })
    .unwrap()
    .0
}

fn admin(addr: SocketAddr) -> Client {
    Client::connect_and_logon(&addr.to_string(), "admin", "admin")
        .unwrap()
        .0
}

fn create(admin: &mut Client, role: Role, name: &str, cash: u64) -> u64 {
    let text = admin
        .request(&ClientMessage::CreateAccount {
            role,
            cash,
            username: name.into(),
            secret: "pw".into(),
        })
        .unwrap();
    text.split_whitespace().nth(1).unwrap().parse().unwrap()
}

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

#[test]
fn investor_buys_ipo_shares_through_a_retail_broker() {
    let exchange = exchange();
    let (mut ops, _) = Client::connect_member(&exchange.to_string(), "ops", "ops").unwrap();
    for (name, kind) in [
        ("retail-co", MemberKind::Broker),
        ("prime-co", MemberKind::Broker),
    ] {
        ops.request_member(&MemberMessage::CreateMember {
            kind,
            name: name.into(),
            secret: "pw".into(),
        })
        .unwrap();
    }

    let retail = broker(exchange, ProfileKind::Retail, "retail-co");
    let prime = broker(exchange, ProfileKind::Prime, "prime-co");

    let mut prime_admin = admin(prime);
    assert!(matches!(
        prime_admin.request(&ClientMessage::CreateAccount { role: Role::Retail, cash: 0, username: "x".into(), secret: "pw".into() }),
        Err(ClientError::Rejected(reason)) if reason.contains("prime broker")
    ));
    let issuer = create(&mut prime_admin, Role::Issuer, "acme_ir", 0);
    let alice_id = create(&mut admin(retail), Role::Retail, "alice", 10_000_00);

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

    let (mut alice, session) =
        Client::connect_and_logon(&retail.to_string(), "alice", "pw").unwrap();
    assert_eq!(session.account_id, alice_id);
    let filled = eventually(|| {
        let id = alice
            .send_order(acme, Side::Buy, OrderKind::Limit(20_00), 25)
            .ok()?;
        loop {
            match alice
                .wait_for(
                    Duration::from_secs(2),
                    |m| matches!(m, ServerMessage::Execution(r) if r.client_order_id == id),
                )
                .ok()?
            {
                ServerMessage::Execution(r) if r.status == OrderStatus::Filled => return Some(r),
                ServerMessage::Execution(r) if r.status == OrderStatus::Rejected => return None,
                _ => {}
            }
        }
    });
    assert_eq!((filled.last_price, filled.filled), (20_00, 25));

    alice.send(&ClientMessage::QueryAccount).unwrap();
    let account = match alice
        .wait_for(Duration::from_secs(2), |m| {
            matches!(m, ServerMessage::Account(_))
        })
        .unwrap()
    {
        ServerMessage::Account(a) => a,
        _ => unreachable!(),
    };
    let buyer_charges = FeeEngine::default().charges(Role::Retail, Side::Buy, 20_00, 25, 8, 0);
    assert_eq!(filled.fees, buyer_charges.total());
    assert_eq!(
        account.cash.available as i64,
        10_000_00 - 500_00 - buyer_charges.total()
    );
    assert_eq!(account.cash.reserved, 0);
    assert_eq!(account.positions[0].balance.available, 25);

    let (issuer_session, _) =
        Client::connect_and_logon(&prime.to_string(), "acme_ir", "pw").unwrap();
    let issuer_account = eventually(|| {
        match issuer_session
            .recv_timeout(Duration::from_millis(200))
            .ok()?
        {
            Some(ServerMessage::Account(a)) if a.cash.available > 0 => Some(a),
            _ => None,
        }
    });
    assert_eq!(issuer_account.positions[0].balance.reserved, 975);
    let issuer_charges = FeeEngine::default().charges(Role::Issuer, Side::Sell, 20_00, 25, -4, 0);
    assert_eq!(issuer_charges.commission, 10_00);
    assert_eq!(
        issuer_account.cash.available as i64,
        500_00 - issuer_charges.total()
    );
}

#[test]
fn hft_accounts_only_exist_at_dma_providers_and_orders_need_funds() {
    let exchange = exchange();
    let (mut ops, _) = Client::connect_member(&exchange.to_string(), "ops", "ops").unwrap();
    ops.request_member(&MemberMessage::CreateMember {
        kind: MemberKind::Broker,
        name: "dma-co".into(),
        secret: "pw".into(),
    })
    .unwrap();
    ops.request_member(&MemberMessage::CreateMember {
        kind: MemberKind::Broker,
        name: "uw".into(),
        secret: "pw".into(),
    })
    .unwrap();
    let dma = broker(exchange, ProfileKind::Dma, "dma-co");
    let uw = broker(exchange, ProfileKind::Prime, "uw");
    let issuer = create(&mut admin(uw), Role::Issuer, "ir", 0);
    ops.request_member(&MemberMessage::ListInstrument {
        symbol: Symbol::parse("ZEN").unwrap(),
        ipo_price: 10_00,
        shares: 100,
        underwriter: 3,
        issuer_account: issuer,
        name: String::new(),
    })
    .unwrap();

    let mut dma_admin = admin(dma);
    assert!(
        dma_admin
            .request(&ClientMessage::CreateAccount {
                role: Role::Retail,
                cash: 0,
                username: "r".into(),
                secret: "pw".into()
            })
            .is_err()
    );
    create(&mut dma_admin, Role::Hft, "jump", 50_00);
    let (mut hft, _) = Client::connect_and_logon(&dma.to_string(), "jump", "pw").unwrap();
    let report = eventually(|| {
        let id = hft
            .send_order(
                Symbol::parse("ZEN").unwrap(),
                Side::Buy,
                OrderKind::Limit(10_00),
                10,
            )
            .ok()?;
        match hft
            .wait_for(
                Duration::from_secs(2),
                |m| matches!(m, ServerMessage::Execution(r) if r.client_order_id == id),
            )
            .ok()?
        {
            ServerMessage::Execution(r) if !r.reason.contains("link is down") => Some(r),
            _ => None,
        }
    });
    assert_eq!(report.status, OrderStatus::Rejected);
    assert!(report.reason.contains("insufficient cash"));
}
