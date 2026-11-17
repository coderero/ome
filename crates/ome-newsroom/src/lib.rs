pub mod gateway;
pub mod journal;
pub mod links;
pub mod llm;
pub mod newsroom;
pub mod writer;

use gateway::{Gateway, OpsCredentials};
use journal::Record;
use newsroom::Newsroom;
use ome_llm::{LlmConfig, Ollama};
use ome_net::auth::hash_secret;
use ome_net::registry::{RegistryAccess, Reporter};
use ome_net::{Inbound, listen, log, now_ms};
use ome_protocol::ServiceInfo;
use ome_store::Journal;
use rand::SeedableRng;
use rand::rngs::StdRng;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use writer::TemplateWriter;

const TICK: Duration = Duration::from_millis(250);
const REPORT_EVERY: Duration = Duration::from_secs(1);

pub struct NewsConfig {
    pub listen: String,
    pub journal: Option<PathBuf>,
    pub ops_name: String,
    pub ops_secret: String,
    pub sources: Vec<(String, String)>,
    pub seed: Option<u64>,
    pub llm: Option<LlmConfig>,
    pub registry: Option<RegistryAccess>,
}

pub enum NewsInbound {
    Client(Inbound),
    Rewrite(links::Done),
}

impl From<Inbound> for NewsInbound {
    fn from(inbound: Inbound) -> Self {
        NewsInbound::Client(inbound)
    }
}

fn rewrite(done: links::Done) -> NewsInbound {
    NewsInbound::Rewrite(done)
}

pub fn start(config: NewsConfig) -> Result<(SocketAddr, JoinHandle<Result<(), String>>), String> {
    let mut room = Newsroom::new();
    let mut last_ts = 0;
    let journal = match &config.journal {
        Some(path) => {
            let (journal, records) = Journal::<Record>::open(path)
                .map_err(|e| format!("cannot open journal {}: {e}", path.display()))?;
            for record in &records {
                room.apply(&record.fact);
                last_ts = record.timestamp;
            }
            log(&format!(
                "replayed {} newsroom fact(s), {} item(s) published",
                records.len(),
                room.next_seq() - 1
            ));
            Some(ome_store::boxed(journal))
        }
        None => None,
    };
    let rng = match config.seed {
        Some(seed) => StdRng::seed_from_u64(seed),
        None => StdRng::from_os_rng(),
    };
    let ops = OpsCredentials {
        secret: hash_secret(&config.ops_name, &config.ops_secret),
        name: config.ops_name.clone(),
    };
    let sources = config
        .sources
        .iter()
        .map(|(name, secret)| (name.clone(), hash_secret(name, secret)))
        .collect();
    let mut gateway = Gateway::new(room, journal, ops, sources, Box::new(TemplateWriter), rng, last_ts);
    let (tx, rx) = mpsc::channel::<NewsInbound>();
    if let Some(llm_config) = &config.llm {
        let (jobs, queue) = mpsc::channel();
        gateway.use_rewriter(jobs, llm_config.model.clone(), llm_config.timeout.as_millis() as u64 + 500);
        links::spawn_writer(Box::new(Ollama::new(llm_config)?), queue, tx.clone(), rewrite);
    }
    gateway.flush().map_err(|e| e.to_string())?;

    let listener = TcpListener::bind(&config.listen)
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    log(&format!(
        "newsroom listening on {addr}; contributors: {}",
        config.sources.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
    ));
    let mut info = ServiceInfo::new("newsroom", "newsroom", &addr.to_string());
    info.capabilities = format!(
        "writer={}",
        config.llm.as_ref().map(|l| l.model.as_str()).unwrap_or("templates")
    );
    let reporter = Reporter::start(config.registry.clone(), info);
    listen(listener, tx);

    let handle = thread::spawn(move || {
        let mut next_tick = Instant::now();
        let mut next_report = Instant::now();
        loop {
            match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
                Ok(first) => {
                    handle(&mut gateway, first);
                    for inbound in rx.try_iter().take(1_024) {
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
            if Instant::now() >= next_report {
                next_report = Instant::now() + REPORT_EVERY;
                reporter.report(gateway.health());
            }
            gateway
                .flush()
                .map_err(|e| format!("journal flush failed: {e}"))?;
        }
    });
    Ok((addr, handle))
}

fn handle(gateway: &mut Gateway, inbound: NewsInbound) {
    match inbound {
        NewsInbound::Client(Inbound::Connected(id, outbox)) => gateway.connect(id, Box::new(outbox)),
        NewsInbound::Client(Inbound::Frame(id, bytes)) => gateway.handle(id, &bytes, now_ms()),
        NewsInbound::Client(Inbound::Disconnected(id)) => gateway.disconnect(id),
        NewsInbound::Rewrite(done) => gateway.on_rewrite(done, now_ms()),
    }
}
