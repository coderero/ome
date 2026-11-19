use ome_client::{Client, ClientError};
use ome_core::symbol::Symbol;
use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start as start_exchange};
use ome_protocol::{
    GroupInfo, RecordingInfo, Scenario, ServerMessage, SimCommand, SimMessage, SimRole,
};
use ome_simctl::{SimConfig, start};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use ome_broker::profile::{ProfileKind, Regime};
use ome_broker::{BrokerConfig, start as start_broker};
use ome_net::registry::StaticDirectory;
use ome_newsroom::{NewsConfig, start as start_newsroom};
use ome_protocol::{ClientMessage, MemberKind, MemberMessage, NewsItem, NewsKind, WorldStatus};
use ome_simctl::fleet::parse_roster;
use ome_simctl::world::company::parse_world;

fn simctl(exchange: SocketAddr, journal: Option<PathBuf>) -> SocketAddr {
    let mut config = SimConfig::new("127.0.0.1:0", &exchange.to_string(), Arc::new(StaticDirectory::default()));
    config.journal = journal;
    start(config).unwrap().0
}

fn events(client: &Client) -> Vec<SimCommand> {
    let mut out = Vec::new();
    while let Ok(Some(message)) = client.recv_timeout(Duration::from_millis(300)) {
        if let ServerMessage::SimEvent(command) = message {
            out.push(command);
        }
    }
    out
}

#[test]
fn operators_steer_runners_and_settings_survive_reconnects_and_restarts() {
    let exchange = start_exchange(ServerConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        admin_name: "ops".into(),
        admin_secret: "ops".into(),
        exchange: ExchangeConfig::default(),
        schedule: None,
        registry: None,
    })
    .unwrap()
    .0;
    let dir = std::env::temp_dir().join(format!("ome-simctl-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let journal = dir.join("simctl.journal");
    let _ = std::fs::remove_file(&journal);
    let addr = simctl(exchange, Some(journal.clone()));

    assert!(matches!(
        Client::connect_sim(&addr.to_string(), SimRole::Operator, "operator", "wrong"),
        Err(ClientError::Rejected(_))
    ));
    let mut operator =
        Client::connect_sim(&addr.to_string(), SimRole::Operator, "operator", "sim").unwrap();
    let mut runner =
        Client::connect_sim(&addr.to_string(), SimRole::Runner, "runner", "runner").unwrap();

    let acme = Symbol::parse("ACME").unwrap();
    for command in [
        SimCommand::Pause {
            group: "noise".into(),
        },
        SimCommand::SetFair {
            symbol: acme,
            price: 30_00,
            drift_bps: 0,
        },
        SimCommand::Scenario(Scenario::LiquidityDrought { seconds: 600 }),
    ] {
        let text = operator.request_sim(&SimMessage::Command(command)).unwrap();
        assert!(text.contains("1 runner"), "{text}");
    }
    assert!(
        operator
            .request_sim(&SimMessage::Command(SimCommand::SetCount {
                group: "*".into(),
                count: 2
            }))
            .is_err()
    );
    let live = events(&runner);
    assert_eq!(live.len(), 3);
    assert_eq!(
        live[0],
        SimCommand::Pause {
            group: "noise".into()
        }
    );

    runner
        .send_sim(&SimMessage::Groups(vec![GroupInfo {
            name: "noise".into(),
            strategy: "noise".into(),
            broker: "retail-co".into(),
            target: 15,
            running: 15,
            paused: true,
            fills: 3,
            equity: 300_000_00,
            traded: 5_000,
            day_fills: 2,
            day_traded: 900,
            params: String::new(),
        }]))
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    operator.send_sim(&SimMessage::QueryStatus).unwrap();
    let ServerMessage::SimStatus(status) = operator
        .wait_for(Duration::from_secs(3), |m| {
            matches!(m, ServerMessage::SimStatus(_))
        })
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!((status.runners, status.groups.len()), (1, 1));
    assert_eq!(status.fair, vec![(acme, 30_00, 0)]);
    assert_eq!(status.scenarios.len(), 1);

    let again =
        Client::connect_sim(&addr.to_string(), SimRole::Runner, "runner", "runner").unwrap();
    let replay = events(&again);
    assert!(replay.contains(&SimCommand::Pause {
        group: "noise".into()
    }));
    assert!(
        replay
            .iter()
            .any(|c| matches!(c, SimCommand::Scenario(Scenario::LiquidityDrought { .. })))
    );

    let restarted = simctl(exchange, Some(journal.clone()));
    let fresh =
        Client::connect_sim(&restarted.to_string(), SimRole::Runner, "runner", "runner").unwrap();
    assert!(events(&fresh).contains(&SimCommand::SetFair {
        symbol: acme,
        price: 30_00,
        drift_bps: 0
    }));

    let text = operator
        .request_sim(&SimMessage::Accelerate {
            pre_open_s: 2,
            open_s: 20,
            closing_s: 2,
            closed_s: 2,
        })
        .unwrap();
    assert!(text.contains("open 20s"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn recordings(operator: &mut Client, name: &str) -> Vec<RecordingInfo> {
    operator
        .send_sim(&SimMessage::QueryRecordings { name: name.into() })
        .unwrap();
    match operator
        .wait_for(Duration::from_secs(3), |m| {
            matches!(m, ServerMessage::SimRecordings(_))
        })
        .unwrap()
    {
        ServerMessage::SimRecordings(list) => list,
        _ => unreachable!(),
    }
}

#[test]
fn sessions_are_recorded_and_replayed_with_their_original_spacing() {
    let exchange = start_exchange(ServerConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        admin_name: "ops".into(),
        admin_secret: "ops".into(),
        exchange: ExchangeConfig::default(),
        schedule: None,
        registry: None,
    })
    .unwrap()
    .0;
    let dir = std::env::temp_dir().join(format!("ome-simctl-replay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let journal = dir.join("simctl.journal");
    let _ = std::fs::remove_file(&journal);
    let addr = simctl(exchange, Some(journal.clone()));
    let mut operator =
        Client::connect_sim(&addr.to_string(), SimRole::Operator, "operator", "sim").unwrap();
    let runner =
        Client::connect_sim(&addr.to_string(), SimRole::Runner, "runner", "runner").unwrap();
    let record = |start: bool| SimMessage::Record {
        name: "drill".into(),
        start,
    };
    let burst = SimCommand::Scenario(Scenario::VolatilityBurst { seconds: 30 });
    let pause = SimCommand::Pause {
        group: "noise".into(),
    };

    operator
        .request_sim(&SimMessage::Command(SimCommand::SetParam {
            group: "swing".into(),
            key: "activity".into(),
            value: "0.2".into(),
        }))
        .unwrap();
    let text = operator.request_sim(&record(true)).unwrap();
    assert!(text.contains("1 baseline setting"), "{text}");
    assert!(operator.request_sim(&record(true)).is_err());
    operator
        .request_sim(&SimMessage::Command(pause.clone()))
        .unwrap();
    std::thread::sleep(Duration::from_millis(600));
    operator
        .request_sim(&SimMessage::Command(burst.clone()))
        .unwrap();
    assert!(
        operator
            .request_sim(&SimMessage::Replay {
                name: "drill".into(),
                speed_pct: 100
            })
            .is_err(),
        "a live recording cannot be replayed"
    );
    let text = operator.request_sim(&record(false)).unwrap();
    assert!(text.contains("2 step(s)"), "{text}");

    let list = recordings(&mut operator, "");
    assert_eq!((list.len(), list[0].steps), (1, 2));
    assert!(list[0].stopped_at > list[0].started_at);
    let detail = recordings(&mut operator, "drill");
    assert!(
        detail[0]
            .timeline
            .contains("baseline: swing: activity = 0.2")
    );
    assert!(detail[0].timeline.contains("volatility burst for 30s"));
    operator
        .send_sim(&SimMessage::QueryRecordings {
            name: "nope".into(),
        })
        .unwrap();
    assert!(matches!(
        operator.wait_for(Duration::from_secs(3), |m| matches!(
            m,
            ServerMessage::Reject { .. }
        )),
        Ok(ServerMessage::Reject { .. })
    ));
    events(&runner);

    let text = operator
        .request_sim(&SimMessage::Replay {
            name: "drill".into(),
            speed_pct: 100,
        })
        .unwrap();
    assert!(text.contains("3 step(s)"), "{text}");
    let started = std::time::Instant::now();
    let mut seen = Vec::new();
    while seen.len() < 3 && started.elapsed() < Duration::from_secs(5) {
        if let Ok(Some(ServerMessage::SimEvent(command))) =
            runner.recv_timeout(Duration::from_millis(100))
        {
            seen.push((command, started.elapsed()));
        }
    }
    let commands: Vec<_> = seen.iter().map(|(c, _)| c.clone()).collect();
    assert_eq!(
        commands,
        vec![
            SimCommand::SetParam {
                group: "swing".into(),
                key: "activity".into(),
                value: "0.2".into()
            },
            pause,
            burst
        ]
    );
    let gap = seen[2].1 - seen[1].1;
    assert!(
        gap >= Duration::from_millis(300) && gap < Duration::from_millis(3_000),
        "spacing kept: {gap:?}"
    );

    let restarted = simctl(exchange, Some(journal.clone()));
    let mut again =
        Client::connect_sim(&restarted.to_string(), SimRole::Operator, "operator", "sim").unwrap();
    let list = recordings(&mut again, "");
    assert_eq!(
        (list.len(), list[0].steps),
        (1, 2),
        "recordings survive a restart"
    );
    assert!(
        again
            .request_sim(&SimMessage::Record {
                name: "drill".into(),
                start: true
            })
            .is_err(),
        "names stay unique after a restart"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn broker(exchange: SocketAddr, profile: ProfileKind, member: &str, news: Option<SocketAddr>) -> SocketAddr {
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
        news_addr: news.map(|n| n.to_string()),
        postgres: None,
        collar_bps: ome_broker::profile::DEFAULT_COLLAR_BPS,
        exchange_rate: 4_500,
        registry: None,
    })
    .unwrap()
    .0
}

fn world_status(operator: &mut Client) -> WorldStatus {
    operator.send_sim(&SimMessage::QueryWorld).unwrap();
    match operator
        .wait_for(Duration::from_secs(3), |m| matches!(m, ServerMessage::WorldStatus(_)))
        .unwrap()
    {
        ServerMessage::WorldStatus(w) => w,
        _ => unreachable!(),
    }
}

fn eventually<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = f() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

const WORLD: &str = "company ACME sector=robotics price=25.00 shares=20000 eps=1.25 margin=12 growth=9 beta=1.2 underwriter=prime-co dmm=lp-co name=Acme Robotics
company NOVA sector=energy price=12.50 shares=40000 eps=0.80 margin=8 growth=4 beta=0.9 underwriter=prime-co name=Nova Energy
account alice broker=retail-co role=retail cash=25000 secret=alice-pw";

#[test]
fn the_world_seeds_the_market_through_real_interfaces_and_its_events_reach_broker_clients() {
    let exchange = start_exchange(ServerConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        admin_name: "ops".into(),
        admin_secret: "ops".into(),
        exchange: ExchangeConfig { require_prospectus: true, ..ExchangeConfig::default() },
        schedule: Some(ome_exchange::session::Schedule { pre_open_ms: 300, continuous_ms: 1_500, closing_ms: 200, closed_ms: 300 }),
        registry: None,
    })
    .unwrap()
    .0;
    let (mut ops, _) = Client::connect_member(&exchange.to_string(), "ops", "ops").unwrap();
    for (name, kind) in [("retail-co", MemberKind::Broker), ("prime-co", MemberKind::Broker), ("lp-co", MemberKind::LiquidityProvider)] {
        ops.request_member(&MemberMessage::CreateMember { kind, name: name.into(), secret: "pw".into() }).unwrap();
    }
    let news = start_newsroom(NewsConfig {
        listen: "127.0.0.1:0".into(),
        journal: None,
        ops_name: "ops".into(),
        ops_secret: "ops".into(),
        sources: vec![("world".into(), "wire".into())],
        seed: Some(3),
        llm: None,
        registry: None,
    })
    .unwrap()
    .0;
    Client::connect_member(&news.to_string(), "ops", "ops")
        .unwrap()
        .0
        .request_member(&MemberMessage::CreateMember { kind: MemberKind::Broker, name: "retail-co".into(), secret: "pw".into() })
        .unwrap();
    let retail = broker(exchange, ProfileKind::Retail, "retail-co", Some(news));
    let prime = broker(exchange, ProfileKind::Prime, "prime-co", None);
    let lp = broker(exchange, ProfileKind::Lp, "lp-co", None);

    let dir = std::env::temp_dir().join(format!("ome-simctl-world-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let journal = dir.join("simctl.journal");
    let _ = std::fs::remove_file(&journal);
    let directory = StaticDirectory::default()
        .with("retail-co", "broker", &retail.to_string())
        .with("prime-co", "broker", &prime.to_string())
        .with("lp-co", "broker", &lp.to_string());
    let config = |journal: PathBuf| {
        let mut config = SimConfig::new("127.0.0.1:0", &exchange.to_string(), Arc::new(directory.clone()));
        config.journal = Some(journal);
        config.world = parse_world(WORLD).unwrap();
        config.roster = parse_roster("group noise count=2 broker=retail-co role=retail strategy=noise cash=20000 activity=0", 1.0).unwrap();
        config.calendar = true;
        config.newsroom = Some((news.to_string(), "world".into(), "wire".into()));
        config.provision = Some(("admin".into(), "admin".into()));
        config.news_per_day = 20.0;
        config.seed = Some(11);
        config
    };
    let addr = start(config(journal.clone())).unwrap().0;
    let runner = Client::connect_sim(&addr.to_string(), SimRole::Runner, "runner", "runner").unwrap();
    let mut assigned = Vec::new();
    eventually("two agents assigned to the runner", || {
        while let Ok(Some(message)) = runner.recv_timeout(Duration::from_millis(50)) {
            if let ServerMessage::SimAssign(a) = message {
                assigned.push(a);
            }
        }
        (assigned.len() >= 2).then_some(())
    });
    assert_eq!(assigned[0].addr, retail.to_string(), "simctl resolves the broker by name");
    assert!(assigned.iter().any(|a| a.name == "noise-2") && assigned[0].spec.contains("strategy=noise"));
    Client::connect_and_logon(&retail.to_string(), "noise-1", "agent-pw").expect("simctl opened the agent's account");

    ops.send_member(&MemberMessage::QueryInstruments).unwrap();
    let listed = match ops.wait_for(Duration::from_secs(3), |m| matches!(m, ServerMessage::InstrumentList(_))).unwrap() {
        ServerMessage::InstrumentList(list) => list,
        _ => unreachable!(),
    };
    assert_eq!(listed.len(), 2, "both companies listed after their prospectus was filed");
    ops.send_member(&MemberMessage::QueryObligations).unwrap();
    let dmms = match ops.wait_for(Duration::from_secs(3), |m| matches!(m, ServerMessage::Obligations(_))).unwrap() {
        ServerMessage::Obligations(list) => list,
        _ => unreachable!(),
    };
    assert_eq!((dmms.len(), dmms[0].name.as_str()), (1, "lp-co"));

    let (mut alice, _) = Client::connect_and_logon(&retail.to_string(), "alice", "alice-pw").unwrap();
    let mut operator = Client::connect_sim(&addr.to_string(), SimRole::Operator, "operator", "sim").unwrap();
    let text = operator.request_sim(&SimMessage::Inject { event: "lawsuit".into(), symbol: Symbol::parse("ACME") }).unwrap();
    assert!(text.starts_with('#') && text.contains("lawsuit"), "{text}");
    let shock = operator
        .request_sim(&SimMessage::Command(SimCommand::Scenario(Scenario::NewsShock { symbol: Symbol::parse("NOVA").unwrap(), move_bps: -800, seconds: 30 })))
        .unwrap();
    assert!(shock.contains("world event"), "{shock}");
    let mut items: Vec<NewsItem> = Vec::new();
    eventually("prospectuses, a lawsuit, a shock and calendar news reach alice", || {
        alice.send(&ClientMessage::SubscribeNews { recent: 100 }).ok();
        while let Ok(Some(message)) = alice.recv_timeout(Duration::from_millis(100)) {
            if let ServerMessage::News(item) = message
                && !items.iter().any(|i| i.seq == item.seq)
            {
                items.push(item);
            }
        }
        let has = |f: &dyn Fn(&NewsItem) -> bool| items.iter().any(f);
        (has(&|i| i.kind == NewsKind::Prospectus)
            && has(&|i| i.value("event") == Some("lawsuit"))
            && has(&|i| i.value("event") == Some("shock") && i.impact_bps < -500 && i.sentiment == -80)
            && has(&|i| i.kind == NewsKind::Macro || i.day >= 1))
        .then_some(())
    });
    assert!(items.iter().any(|i| i.data.contains("fund=ACME:")), "stories carry the world's fundamentals");

    let before = world_status(&mut operator);
    assert!(before.provisioning.contains("listings 2/2") && before.provisioning.contains("accounts 1/1"), "{}", before.provisioning);
    assert_eq!(before.companies.len(), 2);
    drop(operator);
    let again = start(config(journal.clone())).unwrap().0;
    let mut operator = Client::connect_sim(&again.to_string(), SimRole::Operator, "operator", "sim").unwrap();
    let after = world_status(&mut operator);
    assert!(after.events >= before.events, "the world comes back from the journal");
    assert_eq!(after.companies.iter().map(|c| c.symbol).collect::<Vec<_>>(), before.companies.iter().map(|c| c.symbol).collect::<Vec<_>>());
    assert!(after.provisioning.contains("listings 2/2"), "nothing is seeded twice: {}", after.provisioning);
    let _ = std::fs::remove_dir_all(&dir);
}
