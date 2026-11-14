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
        llm: None,
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
        exchange: ExchangeConfig {
            require_prospectus: true,
            ..ExchangeConfig::default()
        },
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
        newsroom: None,
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
    let listing = |symbol: &str| MemberMessage::ListInstrument {
        symbol: Symbol::parse(symbol).unwrap(),
        ipo_price: 25_00,
        shares: 1_000,
        underwriter: 2,
        issuer_account: 1,
        name: symbol.into(),
    };
    let refused = exchange_ops.request_member(&listing("ZZZ")).unwrap_err();
    assert!(refused.to_string().contains("no prospectus"), "{refused}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while exchange_ops.request_member(&listing("ACME")).is_err() {
        assert!(
            Instant::now() < deadline,
            "the newsroom never filed ACME's prospectus"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
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

    let steering = start_simctl(SimConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        operator: ("operator".into(), "sim".into()),
        runner: ("runner".into(), "runner".into()),
        exchange_addr: exchange.to_string(),
        exchange_name: "ops".into(),
        exchange_secret: "ops".into(),
        newsroom: Some((news.to_string(), "ops".into())),
    })
    .unwrap()
    .0;
    let mut steer =
        Client::connect_sim(&steering.to_string(), SimRole::Operator, "operator", "sim").unwrap();
    let text = steer
        .request_sim(&SimMessage::Command(ome_protocol::SimCommand::Scenario(
            ome_protocol::Scenario::NewsShock {
                symbol: acme,
                move_bps: -800,
                seconds: 30,
            },
        )))
        .unwrap();
    assert!(text.contains("newsroom story"), "{text}");
    let shock = collect(&alice, |items| {
        items.iter().any(|i| i.value("event") == Some("shock"))
    });
    let shock = shock
        .iter()
        .find(|i| i.value("event") == Some("shock"))
        .unwrap();
    assert_eq!((shock.symbol, shock.sentiment), (Some(acme), -80));
    assert!((shock.impact_bps + 800).abs() <= 5);
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

fn fake_llm() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let read = stream.read(&mut buf).unwrap_or(0);
                request.extend_from_slice(&buf[..read]);
                let text = String::from_utf8_lossy(&request);
                if let Some(split) = text.find("\r\n\r\n") {
                    let length: usize = text[..split]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= split + 4 + length {
                        break;
                    }
                }
                if read == 0 {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request).to_string();
            let body = &text[text.find("\r\n\r\n").unwrap() + 4..];
            let payload: serde_json::Value = serde_json::from_str(body).unwrap();
            let content = payload["messages"][0]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let between = |from: &str, to: &str| {
                content
                    .split_once(from)
                    .and_then(|(_, rest)| rest.split_once(to))
                    .map(|(v, _)| v.trim().to_string())
                    .unwrap_or_default()
            };
            let reply = if content.is_empty() {
                serde_json::json!({"message": {"role": "assistant", "content": ""}, "done": true})
            } else if n % 3 == 2 {
                serde_json::json!({"message": {"role": "assistant", "content": "not json at all"}})
            } else {
                let copy = serde_json::json!({
                    "headline": format!("Wire: {}", between("Draft headline:", "\n")),
                    "body": between("Draft body:", "\nRules:"),
                });
                serde_json::json!({"message": {"role": "assistant", "content": copy.to_string()}, "done": true})
            };
            let reply = reply.to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    addr
}

#[test]
fn an_llm_rewrites_stories_in_order_and_bad_replies_fall_back_to_the_template() {
    let dir = std::env::temp_dir().join(format!("ome-newsroom-llm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let journal = dir.join("newsroom.journal");
    let _ = std::fs::remove_file(&journal);
    let llm = fake_llm();
    let start = |journal: PathBuf| {
        start_newsroom(NewsConfig {
            listen: "127.0.0.1:0".into(),
            journal: Some(journal),
            companies: parse_companies(COMPANIES).unwrap(),
            ops_name: "ops".into(),
            ops_secret: "ops".into(),
            exchange: None,
            simctl: None,
            quarter_days: 2,
            news_per_minute: 0.0,
            seed: Some(3),
            llm: Some(ome_newsroom::llm::LlmConfig {
                url: format!("http://{llm}"),
                model: "fake".into(),
                timeout: Duration::from_secs(2),
            }),
        })
        .unwrap()
        .0
    };
    let news = start(journal.clone());
    let mut desk = ops(news);
    desk.request_member(&MemberMessage::CreateMember {
        kind: MemberKind::Broker,
        name: "prime-co".into(),
        secret: "pw".into(),
    })
    .unwrap();
    let mut subscriber = Client::connect(&news.to_string()).unwrap();
    subscriber.logon_member("prime-co", "pw", 0).unwrap();
    let acme = Symbol::parse("ACME").unwrap();
    for event in ["upgrade", "contract-win", "lawsuit", "earnings"] {
        desk.request_member(&MemberMessage::InjectNews {
            symbol: Some(acme),
            event: event.into(),
        })
        .unwrap();
    }
    let items = collect(&subscriber, |items| items.len() >= 6);
    let seqs: Vec<u64> = items.iter().map(|i| i.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5, 6], "published strictly in order");
    let rewritten = items
        .iter()
        .filter(|i| i.headline.starts_with("Wire: "))
        .count();
    assert!(
        (3..=5).contains(&rewritten),
        "{rewritten} of 6 rewritten: {items:#?}"
    );
    let writer = status(&mut desk).writer;
    assert!(
        writer.starts_with("llm fake") && writer.contains("fallback"),
        "{writer}"
    );

    let again = start(journal.clone());
    let mut reader = ops(again);
    reader
        .send_member(&MemberMessage::QueryNews {
            limit: 10,
            symbol: None,
        })
        .unwrap();
    let mut replayed = Vec::new();
    while let Ok(Some(message)) = reader.recv_timeout(Duration::from_millis(500)) {
        match message {
            ServerMessage::News(item) => replayed.push(item.headline),
            ServerMessage::Ack { .. } => break,
            _ => {}
        }
    }
    let expected: Vec<String> = items.iter().map(|i| i.headline.clone()).collect();
    assert_eq!(replayed, expected, "rewrites survive a restart");
    let _ = std::fs::remove_dir_all(&dir);
}
