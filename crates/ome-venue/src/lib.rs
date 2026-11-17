pub mod dark;
pub mod gateway;
pub mod journal;
pub mod quotes;
pub mod venue;
pub mod wholesaler;

use gateway::{Gateway, OpsCredentials};
use journal::Record;
use ome_client::LinkEvent;
use ome_client::upstream::{MemberCredentials, TcpConnector, Upstream};
use ome_net::auth::hash_secret;
use ome_net::{Inbound, listen, log, now_ms};
use ome_protocol::Venue;
use ome_store::Journal;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use venue::VenueState;
use wholesaler::WholesalerConfig;

const TICK: Duration = Duration::from_millis(50);
const MAX_BATCH: usize = 4_096;

pub struct VenueConfig {
    pub kind: Venue,
    pub listen: String,
    pub journal: Option<PathBuf>,
    pub ops_name: String,
    pub ops_secret: String,
    pub exchange_addr: String,
    pub member_name: String,
    pub member_secret: String,
    pub wholesaler: WholesalerConfig,
    pub member_kind: Option<ome_protocol::MemberKind>,
    pub registry: Option<ome_net::registry::RegistryAccess>,
}

pub enum VenueInbound {
    Client(Inbound),
    Exchange { generation: u64, event: LinkEvent },
}

impl From<Inbound> for VenueInbound {
    fn from(inbound: Inbound) -> Self {
        VenueInbound::Client(inbound)
    }
}

fn wrap(generation: u64, event: LinkEvent) -> VenueInbound {
    VenueInbound::Exchange { generation, event }
}

pub fn start(config: VenueConfig) -> Result<(SocketAddr, JoinHandle<Result<(), String>>), String> {
    let mut venue = VenueState::new(config.kind, config.wholesaler);
    let mut last_ts = 0;
    let journal = match &config.journal {
        Some(path) => {
            let started = Instant::now();
            let (journal, records) = Journal::<Record>::open(path)
                .map_err(|e| format!("cannot open journal {}: {e}", path.display()))?;
            for record in &records {
                venue.apply(&record.fact);
                last_ts = record.timestamp;
            }
            log(&format!(
                "replayed {} journal facts from {} in {:?}",
                records.len(),
                path.display(),
                started.elapsed()
            ));
            Some(ome_store::boxed(journal))
        }
        None => None,
    };

    let (tx, rx) = mpsc::channel::<VenueInbound>();
    let connector = TcpConnector::new(config.exchange_addr.clone(), tx.clone(), wrap);
    let credentials = MemberCredentials {
        name: config.member_name.clone(),
        secret: config.member_secret.clone(),
    };
    let member_kind = config.member_kind.unwrap_or(match config.kind {
        Venue::Wholesaler => ome_protocol::MemberKind::LiquidityProvider,
        _ => ome_protocol::MemberKind::Broker,
    });
    let upstream = Upstream::new("exchange", Box::new(connector), credentials)
        .applying_as(member_kind, config.kind.label());
    let ops = OpsCredentials {
        secret: hash_secret(&config.ops_name, &config.ops_secret),
        name: config.ops_name.clone(),
    };
    let mut gateway = Gateway::new(venue, journal, upstream, ops, last_ts);

    let listener = TcpListener::bind(&config.listen)
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    log(&format!(
        "{} listening on {addr}, member '{}' of exchange {}",
        config.kind.label(),
        config.member_name,
        config.exchange_addr
    ));
    let mut info = ome_protocol::ServiceInfo::new(&config.member_name, "venue", &addr.to_string());
    info.capabilities = format!("venue={}", config.kind.label());
    let reporter = ome_net::registry::Reporter::start(config.registry.clone(), info);
    listen(listener, tx);

    let handle = thread::spawn(move || {
        let mut next_tick = Instant::now();
        let mut next_report = Instant::now();
        loop {
            if Instant::now() >= next_report {
                next_report = Instant::now() + Duration::from_secs(1);
                reporter.report(gateway.health());
            }
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

fn handle(gateway: &mut Gateway, inbound: VenueInbound) {
    match inbound {
        VenueInbound::Client(Inbound::Connected(id, outbox)) => {
            gateway.connect(id, Box::new(outbox))
        }
        VenueInbound::Client(Inbound::Frame(id, bytes)) => gateway.handle(id, &bytes, now_ms()),
        VenueInbound::Client(Inbound::Disconnected(id)) => gateway.disconnect(id),
        VenueInbound::Exchange { generation, event } => {
            gateway.on_exchange(generation, event, now_ms())
        }
    }
}
