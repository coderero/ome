pub mod exchange;
pub mod gateway;
pub mod instrument;
pub mod journal;
pub mod members;

use exchange::{Exchange, ExchangeConfig};
use gateway::Gateway;
use journal::Record;
use ome_net::{Inbound, listen, log, now_ms};
use ome_store::Journal;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(50);
const MAX_BATCH: usize = 4_096;

pub struct ServerConfig {
    pub listen: String,
    pub journal: Option<PathBuf>,
    pub admin_name: String,
    pub admin_secret: String,
    pub exchange: ExchangeConfig,
}

pub fn start(config: ServerConfig) -> Result<(SocketAddr, JoinHandle<Result<(), String>>), String> {
    let mut exchange = Exchange::new(config.exchange);
    let mut last_ts = 0;
    let journal = match &config.journal {
        Some(path) => {
            let started = Instant::now();
            let (journal, records) = Journal::<Record>::open(path)
                .map_err(|e| format!("cannot open journal {}: {e}", path.display()))?;
            for record in &records {
                let _ = exchange.execute(&record.command, record.timestamp);
                last_ts = record.timestamp;
            }
            log(&format!(
                "replayed {} journal entries from {} in {:?}",
                records.len(),
                path.display(),
                started.elapsed()
            ));
            Some(journal)
        }
        None => None,
    };

    let mut gateway = Gateway::new(exchange, journal, last_ts);
    gateway.ensure_admin(&config.admin_name, &config.admin_secret, now_ms())?;
    gateway.flush().map_err(|e| e.to_string())?;

    let listener = TcpListener::bind(&config.listen)
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    log(&format!(
        "exchange listening on {addr} (SBE over TCP) · {} members · {} instruments",
        gateway.exchange().members().iter().count(),
        gateway.exchange().instrument_list().len()
    ));

    let (tx, rx) = mpsc::channel::<Inbound>();
    listen(listener, tx);
    let handle = thread::spawn(move || {
        let mut next_tick = Instant::now() + TICK;
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

fn handle(gateway: &mut Gateway, inbound: Inbound) {
    match inbound {
        Inbound::Connected(id, outbox) => gateway.connect(id, Box::new(outbox)),
        Inbound::Frame(id, bytes) => gateway.handle(id, &bytes, now_ms()),
        Inbound::Disconnected(id) => gateway.disconnect(id),
    }
}
