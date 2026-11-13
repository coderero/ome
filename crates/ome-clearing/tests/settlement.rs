use ome_broker::profile::{ProfileKind, Regime};
use ome_broker::{BrokerConfig, start as start_broker};
use ome_clearing::clearing::ClearingConfig;
use ome_clearing::{ServiceConfig, start as start_clearing};
use ome_client::Client;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side};
use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start as start_exchange};
use ome_protocol::{
    ClearingStatus, ClientMessage, MemberKind, MemberMessage, OrderStatus, Role, ServerMessage,
};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

fn eventually<T>(mut attempt: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(value) = attempt() {
            return value;
        }
        assert!(Instant::now() < deadline, "condition not met within 15s");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn broker(
    exchange: SocketAddr,
    clearing: SocketAddr,
    profile: ProfileKind,
    member: &str,
) -> SocketAddr {
    start_broker(BrokerConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        profile,
        admin_name: "admin".into(),
        admin_secret: "admin".into(),
        exchange_addr: exchange.to_string(),
        member_name: member.into(),
        member_secret: "pw".into(),
        clearing_addr: Some(clearing.to_string()),
        regime: Regime::India,
    })
    .unwrap()
    .0
}

fn create(addr: SocketAddr, role: Role, name: &str, cash: u64) -> u64 {
    let (mut admin, _) = Client::connect_and_logon(&addr.to_string(), "admin", "admin").unwrap();
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

fn status(ops: &mut Client) -> ClearingStatus {
    ops.send_member(&MemberMessage::QueryClearing).unwrap();
    match ops
        .wait_for(Duration::from_secs(2), |m| {
            matches!(m, ServerMessage::ClearingStatus(_))
        })
        .unwrap()
    {
        ServerMessage::ClearingStatus(s) => s,
        _ => unreachable!(),
    }
}

#[test]
fn trades_settle_t_plus_one_and_credit_follows_collateral() {
    let exchange = start_exchange(ServerConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        admin_name: "ops".into(),
        admin_secret: "ops".into(),
        exchange: ExchangeConfig::default(),
    })
    .unwrap()
    .0;
    let (mut exchange_ops, _) =
        Client::connect_member(&exchange.to_string(), "ops", "ops").unwrap();
    for (name, kind) in [
        ("ccp", MemberKind::Clearing),
        ("retail-co", MemberKind::Broker),
        ("prime-co", MemberKind::Broker),
    ] {
        exchange_ops
            .request_member(&MemberMessage::CreateMember {
                kind,
                name: name.into(),
                secret: "pw".into(),
            })
            .unwrap();
    }

    let clearing = start_clearing(ServiceConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        ops_name: "ops".into(),
        ops_secret: "ops".into(),
        exchange_addr: exchange.to_string(),
        member_name: "ccp".into(),
        member_secret: "pw".into(),
        clearing: ClearingConfig {
            cycle_ms: 700,
            ..ClearingConfig::default()
        },
    })
    .unwrap()
    .0;
    let (mut ops, _) = Client::connect_member(&clearing.to_string(), "ops", "ops").unwrap();
    for (name, id) in [("retail-co", 3), ("prime-co", 4)] {
        eventually(|| {
            ops.request_member(&MemberMessage::CreateMember {
                kind: MemberKind::Broker,
                name: name.into(),
                secret: "pw".into(),
            })
            .ok()
        });
        ops.request_member(&MemberMessage::Deposit {
            member: id,
            amount: 1_000_000_00,
        })
        .unwrap();
    }

    eventually(|| (status(&mut ops).cycle >= 1).then_some(()));
    let retail = broker(exchange, clearing, ProfileKind::Retail, "retail-co");
    let prime = broker(exchange, clearing, ProfileKind::Prime, "prime-co");
    let issuer = create(prime, Role::Issuer, "acme_ir", 0);
    create(retail, Role::Retail, "alice", 10_000_00);

    let acme = Symbol::parse("ACME").unwrap();
    exchange_ops
        .request_member(&MemberMessage::ListInstrument {
            symbol: acme,
            ipo_price: 20_00,
            shares: 1_000,
            underwriter: 4,
            issuer_account: issuer,
            name: String::new(),
        })
        .unwrap();

    let (mut alice, _) = Client::connect_and_logon(&retail.to_string(), "alice", "pw").unwrap();
    let fill = eventually(|| {
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
    assert!(
        fill.cycle >= 1,
        "fills are tagged with the open trade cycle"
    );

    let unsettled = eventually(
        || match alice.recv_timeout(Duration::from_millis(100)).ok()? {
            Some(ServerMessage::Account(a)) if a.unsettled_cash < 0 => Some(a),
            _ => None,
        },
    );
    assert_eq!(unsettled.positions[0].unsettled, 25);

    let settled = eventually(|| {
        alice.send(&ClientMessage::QueryAccount).ok()?;
        match alice
            .wait_for(Duration::from_millis(500), |m| {
                matches!(m, ServerMessage::Account(_))
            })
            .ok()?
        {
            ServerMessage::Account(a) if a.unsettled_cash == 0 => Some(a),
            _ => None,
        }
    });
    assert_eq!(settled.positions[0].balance.available, 25);
    assert_eq!(settled.positions[0].unsettled, 0);

    let s = status(&mut ops);
    assert!(s.settled_cycle >= fill.cycle);
    let retail_row = s.members.iter().find(|m| m.name == "retail-co").unwrap();
    assert!(
        retail_row.collateral < 1_000_000_00,
        "retail paid for the shares at settlement"
    );
    assert_eq!(retail_row.fails, 0);
    assert!(s.fee_pool > 0);

    let (mut low, _) = Client::connect_member(&clearing.to_string(), "ops", "ops").unwrap();
    low.request_member(&MemberMessage::Deposit {
        member: 3,
        amount: -(retail_row.collateral - 10_00),
    })
    .unwrap();
    let rejected = eventually(|| {
        let id = alice
            .send_order(acme, Side::Buy, OrderKind::Limit(20_00), 50)
            .ok()?;
        match alice
            .wait_for(
                Duration::from_secs(2),
                |m| matches!(m, ServerMessage::Execution(r) if r.client_order_id == id),
            )
            .ok()?
        {
            ServerMessage::Execution(r) if r.reason.contains("credit limit") => Some(r),
            ServerMessage::Execution(r) if r.status == OrderStatus::New => {
                alice
                    .send(&ClientMessage::Cancel {
                        order_id: r.order_id,
                    })
                    .ok()?;
                None
            }
            _ => None,
        }
    });
    assert_eq!(rejected.status, OrderStatus::Rejected);
}
