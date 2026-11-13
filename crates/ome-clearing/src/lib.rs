pub mod clearing;
pub mod gateway;
pub mod journal;

use clearing::{Clearing, ClearingConfig};
use gateway::{Gateway, OpsCredentials};
use journal::Record;
use ome_client::LinkEvent;
use ome_client::upstream::{MemberCredentials, TcpConnector, Upstream};
use ome_net::auth::hash_secret;
use ome_net::{Inbound, listen, log, now_ms};
use ome_store::Journal;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(50);
const MAX_BATCH: usize = 4_096;

pub struct ServiceConfig {
    pub listen: String,
    pub journal: Option<PathBuf>,
    pub ops_name: String,
    pub ops_secret: String,
    pub exchange_addr: String,
    pub member_name: String,
    pub member_secret: String,
    pub clearing: ClearingConfig,
}

pub enum ClearingInbound {
    Client(Inbound),
    Exchange { generation: u64, event: LinkEvent },
}

impl From<Inbound> for ClearingInbound {
    fn from(inbound: Inbound) -> Self {
        ClearingInbound::Client(inbound)
    }
}

fn wrap(generation: u64, event: LinkEvent) -> ClearingInbound {
    ClearingInbound::Exchange { generation, event }
}

pub fn start(
    config: ServiceConfig,
) -> Result<(SocketAddr, JoinHandle<Result<(), String>>), String> {
    let mut clearing = Clearing::new(config.clearing.clone());
    let mut last_ts = 0;
    let journal = match &config.journal {
        Some(path) => {
            let started = Instant::now();
            let (journal, records) = Journal::<Record>::open(path)
                .map_err(|e| format!("cannot open journal {}: {e}", path.display()))?;
            for record in &records {
                clearing.apply(&record.fact);
                last_ts = record.timestamp;
            }
            log(&format!(
                "replayed {} journal facts from {} in {:?}",
                records.len(),
                path.display(),
                started.elapsed()
            ));
            Some(journal)
        }
        None => None,
    };

    let (tx, rx) = mpsc::channel::<ClearingInbound>();
    let connector = TcpConnector::new(config.exchange_addr.clone(), tx.clone(), wrap);
    let credentials = MemberCredentials {
        name: config.member_name.clone(),
        secret: config.member_secret.clone(),
    };
    let upstream = Upstream::new("exchange", Box::new(connector), credentials);
    let ops = OpsCredentials {
        secret: hash_secret(&config.ops_name, &config.ops_secret),
        name: config.ops_name.clone(),
    };
    let mut gateway = Gateway::new(clearing, journal, upstream, ops, last_ts);

    let listener = TcpListener::bind(&config.listen)
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    log(&format!(
        "clearing house listening on {addr}, member '{}' of exchange {} · cycle {} ms · T+{} · margin {} bps",
        config.member_name,
        config.exchange_addr,
        config.clearing.cycle_ms,
        config.clearing.settlement_lag,
        config.clearing.margin_bps
    ));
    listen(listener, tx);

    let handle = thread::spawn(move || {
        let mut next_tick = Instant::now();
        loop {
            match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
                Ok(first) => {
                    handle(&mut gateway, first);
                    for inbound in rx.try_iter().take(MAX_BATCH) {
                        handle(&mut gateway, inbound);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err("listener stopped".to_string()),
            }
            if Instant::now() >= next_tick {
                gateway.tick(now_ms());
                next_tick = Instant::now() + TICK;
            }
            gateway
                .flush()
                .map_err(|e| format!("journal flush failed: {e}"))?;
        }
    });
    Ok((addr, handle))
}

fn handle(gateway: &mut Gateway, inbound: ClearingInbound) {
    match inbound {
        ClearingInbound::Client(Inbound::Connected(id, outbox)) => {
            gateway.connect(id, Box::new(outbox))
        }
        ClearingInbound::Client(Inbound::Frame(id, bytes)) => gateway.handle(id, &bytes, now_ms()),
        ClearingInbound::Client(Inbound::Disconnected(id)) => gateway.disconnect(id),
        ClearingInbound::Exchange { generation, event } => {
            gateway.on_exchange(generation, event, now_ms())
        }
    }
}
