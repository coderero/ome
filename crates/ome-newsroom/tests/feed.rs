use ome_broker::profile::{ProfileKind, Regime};
use ome_broker::{BrokerConfig, start as start_broker};
use ome_client::Client;
use ome_core::symbol::Symbol;
use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start as start_exchange};
use ome_newsroom::company::parse_companies;
use ome_newsroom::{NewsConfig, start as start_newsroom};
use ome_protocol::{
    ClientMessage, MemberKind, MemberMessage, NewsItem, NewsKind, Role, ServerMessage, SimMessage,
    SimRole,
};
use ome_simctl::{SimConfig, start as start_simctl};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const COMPANIES: &str = "company ACME sector=robotics price=25.00 shares=52000 eps=1.25 margin=12 growth=9 beta=1.2 name=Acme Robotics
company NOVA sector=energy price=12.50 shares=104000 eps=0.80 margin=8 growth=4 beta=0.9 name=Nova Energy";

fn newsroom(
    journal: Option<PathBuf>,
    exchange: Option<SocketAddr>,
    simctl: Option<SocketAddr>,
    news_per_minute: f64,
) -> SocketAddr {
    start_newsroom(NewsConfig {
        listen: "127.0.0.1:0".into(),
        journal,
        companies: parse_companies(COMPANIES).unwrap(),
        ops_name: "ops".into(),
        ops_secret: "ops".into(),
        exchange: exchange.map(|a| (a.to_string(), "ops".into(), "ops".into())),
        simctl: simctl.map(|a| (a.to_string(), "sim".into())),
        quarter_days: 2,
        news_per_minute,
        seed: Some(7),
    })
    .unwrap()
    .0
}

fn ops(addr: SocketAddr) -> Client {
    Client::connect_member(&addr.to_string(), "ops", "ops")
        .unwrap()
        .0
}

fn status(client: &mut Client) -> ome_protocol::NewsroomStatus {
    client.send_member(&MemberMessage::QueryNewsroom).unwrap();
    match client
        .wait_for(Duration::from_secs(3), |m| {
            matches!(m, ServerMessage::NewsroomStatus(_))
        })
        .unwrap()
    {
        ServerMessage::NewsroomStatus(s) => s,
        _ => unreachable!(),
    }
}

fn collect(client: &Client, until: impl Fn(&[NewsItem]) -> bool) -> Vec<NewsItem> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut items = Vec::new();
    while !until(&items) {
        assert!(Instant::now() < deadline, "only got {items:#?}");
        if let Ok(Some(ServerMessage::News(item))) = client.recv_timeout(Duration::from_millis(100))
        {
            items.push(item);
        }
    }
    items
}

#[test]
fn stories_flow_from_the_newsroom_through_a_broker_and_fundamentals_drive_fair_values() {
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
    exchange_ops
        .request_member(&MemberMessage::CreateMember {
            kind: MemberKind::Broker,
            name: "retail-co".into(),
            secret: "pw".into(),
        })
        .unwrap();
    let simctl = start_simctl(SimConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        operator: ("operator".into(), "sim".into()),
        runner: ("runner".into(), "runner".into()),
        exchange_addr: exchange.to_string(),
        exchange_name: "ops".into(),
        exchange_secret: "ops".into(),
    })
    .unwrap()
    .0;
    let news = newsroom(None, Some(exchange), Some(simctl), 300.0);
    let mut desk = ops(news);
    desk.request_member(&MemberMessage::CreateMember {
        kind: MemberKind::Broker,
        name: "retail-co".into(),
        secret: "pw".into(),
    })
    .unwrap();
    assert!(
        desk.request_member(&MemberMessage::InjectNews {
            symbol: None,
            event: "lawsuit".into()
        })
        .is_err()
    );

    let retail = start_broker(BrokerConfig {
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
        news_addr: Some(news.to_string()),
    })
    .unwrap()
    .0;
    let (mut admin, _) = Client::connect_and_logon(&retail.to_string(), "admin", "admin").unwrap();
    admin
        .request(&ClientMessage::CreateAccount {
            role: Role::Retail,
            cash: 1_000_00,
            username: "alice".into(),
            secret: "pw".into(),
        })
        .unwrap();
    let (mut alice, _) = Client::connect_and_logon(&retail.to_string(), "alice", "pw").unwrap();
    let prospectuses = {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "broker never received the backlog"
            );
            alice
                .send(&ClientMessage::SubscribeNews { recent: 50 })
                .ok();
            let mut got = Vec::new();
            let window = Instant::now() + Duration::from_millis(400);
            while Instant::now() < window {
                if let Ok(Some(ServerMessage::News(item))) =
                    alice.recv_timeout(Duration::from_millis(50))
                    && item.kind == NewsKind::Prospectus
                {
                    got.push(item);
                }
            }
            if got.len() >= 2 {
                break got;
            }
        }
    };
    assert_eq!(prospectuses[0].value("price"), Some("25.00"));

    exchange_ops
        .request_member(&MemberMessage::SetSchedule {
            pre_open_ms: 300,
            continuous_ms: 1_200,
            closing_ms: 200,
            closed_ms: 300,
        })
        .unwrap();
    let live = collect(&alice, |items| {
        items.iter().any(|i| i.kind == NewsKind::Earnings)
            && items.iter().any(|i| i.kind == NewsKind::Macro)
            && items
                .iter()
                .any(|i| matches!(i.kind, NewsKind::Company | NewsKind::Sector))
    });
    let seqs: Vec<u64> = live.iter().map(|i| i.seq).collect();
    assert!(
        seqs.windows(2).all(|w| w[1] == w[0] + 1),
        "gap-free {seqs:?}"
    );
    let earnings = live.iter().find(|i| i.kind == NewsKind::Earnings).unwrap();
    assert!(earnings.value("eps").is_some() && earnings.day >= 1);

    let acme = Symbol::parse("ACME").unwrap();
    let mut operator =
        Client::connect_sim(&simctl.to_string(), SimRole::Operator, "operator", "sim").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let room_fair = status(&mut desk)
            .companies
            .iter()
            .find(|c| c.symbol == Some(acme))
            .unwrap()
            .fair;
        operator.send_sim(&SimMessage::QueryStatus).unwrap();
        let ServerMessage::SimStatus(sim) = operator
            .wait_for(Duration::from_secs(3), |m| {
                matches!(m, ServerMessage::SimStatus(_))
            })
            .unwrap()
        else {
            unreachable!()
        };
        let sim_fair = sim.fair.iter().find(|(s, _, _)| *s == acme).map(|f| f.1);
        let close = sim_fair.is_some_and(|f| (f as f64 / room_fair as f64 - 1.0).abs() < 0.001);
        if close {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "simctl fair {sim_fair:?} vs newsroom {room_fair}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let s = status(&mut desk);
    assert!(s.day >= 1 && s.subscribers == 1, "{s:?}");
    assert_eq!(s.writer, "templates");
}

#[test]
fn the_newsroom_recovers_from_its_journal_and_subscribers_resume_after_their_last_story() {
    let dir = std::env::temp_dir().join(format!("ome-newsroom-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let journal = dir.join("newsroom.journal");
    let _ = std::fs::remove_file(&journal);
    let first = newsroom(Some(journal.clone()), None, None, 0.0);
    let mut desk = ops(first);
    desk.request_member(&MemberMessage::CreateMember {
        kind: MemberKind::Broker,
        name: "prime-co".into(),
        secret: "pw".into(),
    })
    .unwrap();
    let acme = Symbol::parse("ACME").unwrap();
    for event in ["lawsuit", "earnings", "rates"] {
        let text = desk
            .request_member(&MemberMessage::InjectNews {
                symbol: Some(acme),
                event: event.into(),
            })
            .unwrap();
        assert!(text.starts_with('#'), "{text}");
    }
    let before = status(&mut desk);
    assert_eq!(before.published, 5, "two prospectuses and three stories");

    let second = newsroom(Some(journal.clone()), None, None, 0.0);
    let mut again = ops(second);
    let after = status(&mut again);
    assert_eq!(after.published, 5, "prospectuses are not filed twice");
    assert_eq!(after.companies, before.companies);
    assert_eq!(after.economy, before.economy);

    let mut subscriber = Client::connect(&second.to_string()).unwrap();
    subscriber.logon_member("prime-co", "pw", 3).unwrap();
    let mut seqs = Vec::new();
    while let Ok(Some(message)) = subscriber.recv_timeout(Duration::from_millis(300)) {
        if let ServerMessage::News(item) = message {
            seqs.push(item.seq);
        }
    }
    assert_eq!(
        seqs,
        vec![4, 5],
        "replay starts after the subscriber's last story"
    );
    assert!(Client::connect_member(&second.to_string(), "prime-co", "wrong").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
