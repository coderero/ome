use ome_client::{Client, ClientError};
use ome_core::symbol::Symbol;
use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start as start_exchange};
use ome_protocol::{
    GroupInfo, Scenario, ServerMessage, SimCommand, SimMessage, SimRole,
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
