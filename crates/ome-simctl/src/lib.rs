pub mod gateway;
pub mod journal;
pub mod recording;
pub mod state;

use gateway::{Credentials, ExchangeAccess, Gateway};
use journal::Record;
use ome_net::auth::hash_secret;
use ome_net::{Inbound, listen, log, now_ms};
use ome_store::Journal;
use state::SimState;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct SimConfig {
    pub listen: String,
    pub journal: Option<PathBuf>,
    pub operator: (String, String),
    pub runner: (String, String),
    pub exchange_addr: String,
    pub exchange_name: String,
    pub exchange_secret: String,
    pub newsroom: Option<(String, String)>,
}

pub fn start(config: SimConfig) -> Result<(SocketAddr, JoinHandle<Result<(), String>>), String> {
    let mut state = SimState::default();
    let mut clock = 0;
    let journal = match &config.journal {
        Some(path) => {
            let (journal, records) = Journal::<Record>::open(path)
                .map_err(|e| format!("cannot open journal {}: {e}", path.display()))?;
            for record in &records {
                state.apply_fact(&record.fact, record.timestamp);
                clock = record.timestamp;
            }
            log(&format!("replayed {} simulation fact(s)", records.len()));
            Some(journal)
        }
        None => None,
    };
    let credentials = Credentials {
        operator: (
            config.operator.0.clone(),
            hash_secret(&config.operator.0, &config.operator.1),
        ),
        runner: (
            config.runner.0.clone(),
            hash_secret(&config.runner.0, &config.runner.1),
        ),
    };
    let exchange = ExchangeAccess {
        addr: config.exchange_addr.clone(),
        name: config.exchange_name.clone(),
        secret: config.exchange_secret.clone(),
    };
    let mut gateway = Gateway::new(state, journal, credentials, exchange, clock)
        .with_newsroom(config.newsroom.clone());
    let listener = TcpListener::bind(&config.listen)
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    log(&format!("simulation control plane listening on {addr}"));
    let (tx, rx) = mpsc::channel::<Inbound>();
    listen(listener, tx);
    let handle = thread::spawn(move || {
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Inbound::Connected(id, outbox)) => gateway.connect(id, Box::new(outbox)),
                Ok(Inbound::Frame(id, bytes)) => gateway.handle(id, &bytes, now_ms()),
                Ok(Inbound::Disconnected(id)) => gateway.disconnect(id),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("listener stopped".to_string());
                }
            }
            gateway.tick(now_ms());
            gateway
                .flush()
                .map_err(|e| format!("journal flush failed: {e}"))?;
        }
    });
    Ok((addr, handle))
}
