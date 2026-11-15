use ome_agents::provision::provision;
use ome_agents::roster::parse;
use ome_agents::runner::{AgentContext, supervise};
use ome_broker::profile::{ProfileKind, Regime};
use ome_broker::{BrokerConfig, start as start_broker};
use ome_client::Client;
use ome_core::symbol::Symbol;
use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start as start_exchange};
use ome_protocol::{ClientMessage, MemberKind, MemberMessage, Role, ServerMessage};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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
        regime: Regime::Us,
        wholesaler_addr: None,
        ats_addr: None,
        news_addr: None,
        postgres: None,
    })
    .unwrap()
    .0
}

#[test]
fn roster_agents_provision_bootstrap_inventory_and_use_broker_held_orders() {
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
    let (mut admin, _) = Client::connect_and_logon(&prime.to_string(), "admin", "admin").unwrap();
    let issuer = admin
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
        shares: 10_000,
        underwriter: 3,
        issuer_account: issuer,
        name: "Acme".into(),
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));

    let roster = "
        group noise count=1 broker=retail-co role=retail strategy=noise cash=20000 seed=2000 activity=0
        group investor count=1 broker=retail-co role=retail strategy=investor cash=20000 activity=1
    ";
    let agents = parse(roster, 1.0).unwrap();
    let brokers = BTreeMap::from([("retail-co".to_string(), retail.to_string())]);
    assert_eq!(
        provision(&agents, &brokers, "admin", "agent-pw").unwrap(),
        2
    );
    assert_eq!(
        provision(&agents, &brokers, "admin", "agent-pw").unwrap(),
        0,
        "idempotent"
    );

    let stop = Arc::new(AtomicBool::new(false));
    let threads: Vec<_> = agents
        .into_iter()
        .map(|agent| {
            let (addr, stop) = (retail.to_string(), stop.clone());
            let ctx = AgentContext::standalone(&agent, stop);
            std::thread::spawn(move || supervise(agent, addr, "agent-pw".into(), ctx))
        })
        .collect();

    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut seeded, mut armed) = (false, false);
    while Instant::now() < deadline && !(seeded && armed) {
        std::thread::sleep(Duration::from_millis(500));
        let (mut noise, _) =
            Client::connect_and_logon(&retail.to_string(), "noise-1", "agent-pw").unwrap();
        noise.send(&ClientMessage::QueryAccount).unwrap();
        if let Ok(ServerMessage::Account(a)) = noise.wait_for(Duration::from_secs(2), |m| {
            matches!(m, ServerMessage::Account(_))
        }) {
            seeded = a
                .positions
                .iter()
                .any(|p| p.symbol == acme && p.balance.total() >= 80);
        }
        let (investor, _) =
            Client::connect_and_logon(&retail.to_string(), "investor-1", "agent-pw").unwrap();
        armed = investor
            .wait_for(Duration::from_millis(800), |m| {
                matches!(m, ServerMessage::ConditionalUpdate(_))
            })
            .is_ok();
    }
    stop.store(true, Ordering::Relaxed);
    for thread in threads {
        let _ = thread.join();
    }
    assert!(seeded, "the noise trader bought its starting inventory");
    assert!(armed, "the investor armed a GTT dip buy at the broker");
}
