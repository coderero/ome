use ome_broker::profile::{ProfileKind, Regime};
use ome_broker::{BrokerConfig, start as start_broker};
use ome_client::Client;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, Side};
use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start as start_exchange};
use ome_protocol::{
    ClientMessage, ExecutionReport, MemberKind, MemberMessage, OrderStatus, Role, ServerMessage,
    Venue,
};
use ome_venue::wholesaler::WholesalerConfig;
use ome_venue::{VenueConfig, start as start_venue};
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

fn venue(kind: Venue, exchange: SocketAddr, member: &str) -> SocketAddr {
    start_venue(VenueConfig {
        kind,
        listen: "127.0.0.1:0".into(),
        journal: None,
        ops_name: "ops".into(),
        ops_secret: "ops".into(),
        exchange_addr: exchange.to_string(),
        member_name: member.into(),
        member_secret: "pw".into(),
        wholesaler: WholesalerConfig {
            accept_pct: 100,
            ..WholesalerConfig::default()
        },
    })
    .unwrap()
    .0
}

fn broker(
    exchange: SocketAddr,
    profile: ProfileKind,
    member: &str,
    venues: (Option<SocketAddr>, Option<SocketAddr>),
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
        clearing_addr: None,
        regime: Regime::Us,
        wholesaler_addr: venues.0.map(|a| a.to_string()),
        ats_addr: venues.1.map(|a| a.to_string()),
        news_addr: None,
    })
    .unwrap()
    .0
}

fn create(addr: SocketAddr, role: Role, name: &str, cash: u64) -> u64 {
    let (mut admin, _) = Client::connect_and_logon(&addr.to_string(), "admin", "admin").unwrap();
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
}

fn register(venue: SocketAddr, member: &str) {
    let (mut ops, _) = Client::connect_member(&venue.to_string(), "ops", "ops").unwrap();
    eventually(|| {
        ops.request_member(&MemberMessage::CreateMember {
            kind: MemberKind::Broker,
            name: member.into(),
            secret: "pw".into(),
        })
        .ok()
    });
}

fn final_report(client: &mut Client, client_order_id: u64) -> ExecutionReport {
    loop {
        match client
            .wait_for(Duration::from_secs(5), |m| {
                matches!(m, ServerMessage::Execution(r) if r.client_order_id == client_order_id)
            })
            .unwrap()
        {
            ServerMessage::Execution(r) if !r.status.is_open() => return r,
            _ => {}
        }
    }
}

#[test]
fn retail_flow_fills_at_the_wholesaler_and_institutions_meet_in_the_dark_pool() {
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
    for (name, kind) in [
        ("retail-co", MemberKind::Broker),
        ("prime-co", MemberKind::Broker),
        ("wholesale-co", MemberKind::LiquidityProvider),
        ("ats-co", MemberKind::Broker),
    ] {
        ops.request_member(&MemberMessage::CreateMember {
            kind,
            name: name.into(),
            secret: "pw".into(),
        })
        .unwrap();
    }
    let wholesaler = venue(Venue::Wholesaler, exchange, "wholesale-co");
    let ats = venue(Venue::Ats, exchange, "ats-co");
    register(wholesaler, "retail-co");
    register(ats, "prime-co");
    let retail = broker(
        exchange,
        ProfileKind::Retail,
        "retail-co",
        (Some(wholesaler), None),
    );
    let prime = broker(exchange, ProfileKind::Prime, "prime-co", (None, Some(ats)));

    let issuer = create(prime, Role::Issuer, "acme_ir", 0);
    create(prime, Role::Quant, "fund", 1_000_000_00);
    let desk_id = create(prime, Role::Prop, "desk", 1_000_000_00);
    let (mut prime_admin, _) =
        Client::connect_and_logon(&prime.to_string(), "admin", "admin").unwrap();
    prime_admin
        .request(&ClientMessage::SetBalance {
            account_id: desk_id,
            asset: ome_protocol::Asset::Shares(Symbol::parse("ACME").unwrap()),
            amount: 500,
        })
        .unwrap();
    create(retail, Role::Retail, "alice", 10_000_00);
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
    let (mut desk, _) = Client::connect_and_logon(&prime.to_string(), "desk", "pw").unwrap();
    eventually(|| {
        let id = fund
            .send_order(acme, Side::Buy, OrderKind::Limit(20_00), 400)
            .ok()?;
        (final_report(&mut fund, id).status == OrderStatus::Filled).then_some(())
    });
    eventually(|| {
        let id = fund
            .send_order(acme, Side::Buy, OrderKind::Limit(19_00), 10)
            .ok()?;
        match fund
            .wait_for(
                Duration::from_secs(2),
                |m| matches!(m, ServerMessage::Execution(r) if r.client_order_id == id),
            )
            .ok()?
        {
            ServerMessage::Execution(r) if r.status == OrderStatus::New => Some(()),
            _ => None,
        }
    });

    let (mut alice, _) = Client::connect_and_logon(&retail.to_string(), "alice", "pw").unwrap();
    alice
        .send(&ClientMessage::Subscribe { symbol: acme })
        .unwrap();
    alice
        .wait_for(
            Duration::from_secs(10),
            |m| matches!(m, ServerMessage::Book(b) if !b.bids.is_empty() && !b.asks.is_empty()),
        )
        .unwrap();
    let fill = eventually(|| {
        let id = alice
            .send_order(acme, Side::Buy, OrderKind::Market, 10)
            .ok()?;
        let r = final_report(&mut alice, id);
        (r.status == OrderStatus::Filled).then_some(r)
    });
    assert_eq!(
        (fill.last_price, fill.filled),
        (19_80, 10),
        "{}",
        fill.reason
    );
    assert!(fill.reason.contains("wholesaler"));

    let (mut admin, _) = Client::connect_and_logon(&retail.to_string(), "admin", "admin").unwrap();
    admin.send(&ClientMessage::QueryFees).unwrap();
    let ServerMessage::FeeSummary(stats) = admin
        .wait_for(Duration::from_secs(5), |m| {
            matches!(m, ServerMessage::FeeSummary(_))
        })
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(
        (
            stats.wholesaler_orders,
            stats.wholesaler_shares,
            stats.pfof_earned
        ),
        (1, 10, 2)
    );

    let dark = |client: &mut Client, id, side, qty, min_qty| {
        client
            .send(&ClientMessage::DarkOrder {
                client_order_id: id,
                symbol: acme,
                side,
                qty,
                limit: None,
                min_qty,
            })
            .unwrap();
    };
    dark(&mut fund, 900, Side::Buy, 300, 100);
    dark(&mut desk, 901, Side::Sell, 50, 0);
    std::thread::sleep(Duration::from_millis(300));
    dark(&mut desk, 902, Side::Sell, 200, 0);
    let sold = final_report(&mut desk, 902);
    assert_eq!(
        (sold.status, sold.filled, sold.last_price),
        (OrderStatus::Filled, 200, 19_50)
    );
    let (mut ats_ops, _) = Client::connect_member(&ats.to_string(), "ops", "ops").unwrap();
    ats_ops.send_member(&MemberMessage::QueryVenue).unwrap();
    let ServerMessage::VenueStatus(status) = ats_ops
        .wait_for(Duration::from_secs(5), |m| {
            matches!(m, ServerMessage::VenueStatus(_))
        })
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!((status.fills, status.shares, status.resting), (1, 200, 2));
}
