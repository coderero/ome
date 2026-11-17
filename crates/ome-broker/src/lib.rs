pub mod algo;
pub mod broker;
pub mod fees;
pub mod gateway;
pub mod history;
pub mod journal;
pub mod ledger;
pub mod market;
pub mod oms;
pub mod profile;
pub mod risk;

use broker::Broker;
use gateway::Gateway;
use journal::Record;
use ome_client::LinkEvent;
use ome_client::upstream::{MemberCredentials, TcpConnector, Upstream};
use ome_net::{Inbound, listen, log, now_ms};
use ome_store::Journal;
use profile::{Profile, ProfileKind, Regime};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(50);
const MAX_BATCH: usize = 4_096;

pub struct BrokerConfig {
    pub listen: String,
    pub journal: Option<PathBuf>,
    pub profile: ProfileKind,
    pub admin_name: String,
    pub admin_secret: String,
    pub exchange_addr: String,
    pub member_name: String,
    pub member_secret: String,
    pub clearing_addr: Option<String>,
    pub regime: Regime,
    pub wholesaler_addr: Option<String>,
    pub ats_addr: Option<String>,
    pub news_addr: Option<String>,
    pub postgres: Option<String>,
    pub collar_bps: u64,
    pub exchange_rate: u64,
    pub registry: Option<ome_net::registry::RegistryAccess>,
}

pub enum BrokerInbound {
    Client(Inbound),
    Exchange { generation: u64, event: LinkEvent },
    Clearing { generation: u64, event: LinkEvent },
    Wholesaler { generation: u64, event: LinkEvent },
    Ats { generation: u64, event: LinkEvent },
    News { generation: u64, event: LinkEvent },
    History(history::HistoryResult),
}

impl From<Inbound> for BrokerInbound {
    fn from(inbound: Inbound) -> Self {
        BrokerInbound::Client(inbound)
    }
}

fn exchange_event(generation: u64, event: LinkEvent) -> BrokerInbound {
    BrokerInbound::Exchange { generation, event }
}

fn clearing_event(generation: u64, event: LinkEvent) -> BrokerInbound {
    BrokerInbound::Clearing { generation, event }
}

fn wholesaler_event(generation: u64, event: LinkEvent) -> BrokerInbound {
    BrokerInbound::Wholesaler { generation, event }
}

fn ats_event(generation: u64, event: LinkEvent) -> BrokerInbound {
    BrokerInbound::Ats { generation, event }
}

fn history_result(result: history::HistoryResult) -> BrokerInbound {
    BrokerInbound::History(result)
}

fn news_event(generation: u64, event: LinkEvent) -> BrokerInbound {
    BrokerInbound::News { generation, event }
}

pub fn start(config: BrokerConfig) -> Result<(SocketAddr, JoinHandle<Result<(), String>>), String> {
    let mut profile = Profile::with_regime(config.profile, config.regime);
    profile.collar_bps = config.collar_bps;
    let mut broker = Broker::new(profile);
    let mut last_ts = 0;
    let journal = match &config.journal {
        Some(path) => {
            let started = Instant::now();
            let (journal, records) = Journal::<Record>::open(path)
                .map_err(|e| format!("cannot open journal {}: {e}", path.display()))?;
            for record in &records {
                broker.apply(&record.fact);
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

    let (tx, rx) = mpsc::channel::<BrokerInbound>();
    let credentials = || MemberCredentials {
        name: config.member_name.clone(),
        secret: config.member_secret.clone(),
    };
    let directory: Option<std::sync::Arc<dyn ome_net::registry::Directory>> = config.registry.clone().map(|access| {
        std::sync::Arc::new(ome_net::registry::RegistryDirectory {
            access,
            fallback: ome_net::registry::StaticDirectory::default(),
        }) as std::sync::Arc<dyn ome_net::registry::Directory>
    });
    let kind = match config.profile {
        ProfileKind::Lp => ome_protocol::MemberKind::LiquidityProvider,
        _ => ome_protocol::MemberKind::Broker,
    };
    let note = format!("{} '{}'", config.profile.label(), config.member_name);
    let exchange = Upstream::new(
        "exchange",
        Box::new(TcpConnector::new(
            config.exchange_addr.clone(),
            tx.clone(),
            exchange_event,
        )),
        credentials(),
    )
    .applying_as(kind, &note);
    let clearing = config.clearing_addr.clone().map(|addr| {
        Upstream::new(
            "clearing",
            Box::new(TcpConnector::new(addr, tx.clone(), clearing_event)),
            credentials(),
        )
        .applying_as(kind, &note)
    });
    let mut venues = std::collections::BTreeMap::new();
    if let Some(addr) = config.wholesaler_addr.clone() {
        venues.insert(
            ome_protocol::Venue::Wholesaler,
            Upstream::new(
                "wholesaler",
                Box::new(TcpConnector::new(addr, tx.clone(), wholesaler_event).resolving(directory.clone())),
                credentials(),
            )
            .applying_as(kind, &note),
        );
    }
    if let Some(addr) = config.ats_addr.clone() {
        venues.insert(
            ome_protocol::Venue::Ats,
            Upstream::new(
                "dark pool",
                Box::new(TcpConnector::new(addr, tx.clone(), ats_event).resolving(directory.clone())),
                credentials(),
            )
            .applying_as(kind, &note),
        );
    }
    let news = config.news_addr.clone().map(|addr| {
        Upstream::new(
            "news feed",
            Box::new(TcpConnector::new(addr, tx.clone(), news_event).resolving(directory.clone())),
            credentials(),
        )
        .applying_as(kind, &note)
    });
    let history = config
        .postgres
        .clone()
        .map(|url| history::spawn(url, tx.clone(), history_result));
    let mut gateway = Gateway::new(broker, journal, exchange, clearing, venues, last_ts)
        .with_news(news)
        .with_history(history)
        .pacing(config.exchange_rate);
    gateway.ensure_admin(&config.admin_name, &config.admin_secret, now_ms())?;
    gateway.flush().map_err(|e| e.to_string())?;

    let listener = TcpListener::bind(&config.listen)
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    log(&format!(
        "{} '{}' listening on {addr}, member of exchange {} · {} accounts",
        config.profile.label(),
        config.member_name,
        config.exchange_addr,
        gateway.broker().ledger().accounts().count()
    ));
    let mut info = ome_protocol::ServiceInfo::new(&config.member_name, "broker", &addr.to_string());
    info.capabilities = format!(
        "profile={} regime={:?} member={}",
        config.profile.label(),
        config.regime,
        kind.label()
    );
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

fn handle(gateway: &mut Gateway, inbound: BrokerInbound) {
    match inbound {
        BrokerInbound::Client(Inbound::Connected(id, outbox)) => {
            gateway.connect(id, Box::new(outbox))
        }
        BrokerInbound::Client(Inbound::Frame(id, bytes)) => gateway.handle(id, &bytes, now_ms()),
        BrokerInbound::Client(Inbound::Disconnected(id)) => gateway.disconnect(id),
        BrokerInbound::Exchange { generation, event } => {
            gateway.on_exchange(generation, event, now_ms())
        }
        BrokerInbound::Clearing { generation, event } => {
            gateway.on_clearing(generation, event, now_ms())
        }
        BrokerInbound::Wholesaler { generation, event } => {
            gateway.on_venue(ome_protocol::Venue::Wholesaler, generation, event, now_ms())
        }
        BrokerInbound::Ats { generation, event } => {
            gateway.on_venue(ome_protocol::Venue::Ats, generation, event, now_ms())
        }
        BrokerInbound::News { generation, event } => gateway.on_news(generation, event, now_ms()),
        BrokerInbound::History(result) => gateway.on_history(result),
    }
}
