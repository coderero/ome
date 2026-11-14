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
use std::time::Duration;

fn simctl(exchange: SocketAddr, journal: Option<PathBuf>) -> SocketAddr {
    start(SimConfig {
        listen: "127.0.0.1:0".into(),
        journal,
        operator: ("operator".into(), "sim".into()),
        runner: ("runner".into(), "runner".into()),
        exchange_addr: exchange.to_string(),
        exchange_name: "ops".into(),
        exchange_secret: "ops".into(),
    })
    .unwrap()
    .0
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
        gap >= Duration::from_millis(450) && gap < Duration::from_millis(1_500),
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
