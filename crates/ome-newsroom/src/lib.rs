pub mod company;
pub mod economy;
pub mod events;
pub mod gateway;
pub mod journal;
pub mod links;
pub mod llm;
pub mod newsroom;
pub mod writer;

use gateway::{Gateway, OpsCredentials};
use journal::Record;
use newsroom::Newsroom;
use ome_net::auth::hash_secret;
use ome_net::{Inbound, listen, log, now_ms};
use ome_protocol::MarketPhase;
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

pub struct NewsConfig {
    pub listen: String,
    pub journal: Option<PathBuf>,
    pub companies: Vec<company::CompanySpec>,
    pub ops_name: String,
    pub ops_secret: String,
    pub exchange: Option<(String, String, String)>,
    pub quarter_days: u64,
    pub news_per_day: f64,
    pub seed: Option<u64>,
    pub llm: Option<llm::LlmConfig>,
}

pub enum NewsInbound {
    Client(Inbound),
    Market { phase: MarketPhase, day: u64 },
    Rewrite(links::Done),
    Idea(links::IdeaDone),
}

impl From<Inbound> for NewsInbound {
    fn from(inbound: Inbound) -> Self {
        NewsInbound::Client(inbound)
    }
}

fn idea(done: links::IdeaDone) -> NewsInbound {
    NewsInbound::Idea(done)
}

fn rewrite(done: links::Done) -> NewsInbound {
    NewsInbound::Rewrite(done)
}

fn market(phase: MarketPhase, day: u64) -> NewsInbound {
    NewsInbound::Market { phase, day }
}

pub fn start(config: NewsConfig) -> Result<(SocketAddr, JoinHandle<Result<(), String>>), String> {
    let mut room = Newsroom::new(config.companies.clone(), config.quarter_days);
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
                "replayed {} newsroom fact(s), day {}, {} item(s) published",
                records.len(),
                room.day(),
                room.next_seq() - 1
            ));
            Some(journal)
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
    let mut gateway = Gateway::new(
        room,
        journal,
        ops,
        Box::new(TemplateWriter),
        rng,
        config.news_per_day,
        last_ts,
    );
    let (tx, rx) = mpsc::channel::<NewsInbound>();
    if let Some((addr, name, secret)) = config.exchange.clone() {
        let (filings, filed) = mpsc::channel();
        gateway.file_with(filings);
        links::watch_exchange(addr, name, secret, tx.clone(), market, filed);
    }
    if let Some(llm_config) = &config.llm {
        let llm = llm::Llm::new(llm_config)?;
        let (jobs, queue) = mpsc::channel();
        gateway.use_rewriter(
            jobs,
            llm.model().to_string(),
            llm_config.timeout.as_millis() as u64 + 500,
        );
        links::spawn_writer(llm, queue, tx.clone(), rewrite);
        let (ideas, requests) = mpsc::channel();
        gateway.use_ideas(ideas);
        let patient = llm::LlmConfig { url: llm_config.url.clone(), model: llm_config.model.clone(), timeout: llm_config.timeout * 3 };
        links::spawn_ideas(llm::Llm::new(&patient)?, requests, tx.clone(), idea);
    }
    gateway.start_up(now_ms());
    gateway.flush().map_err(|e| e.to_string())?;

    let listener = TcpListener::bind(&config.listen)
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    log(&format!(
        "newsroom listening on {addr}, covering {} companies",
        config.companies.len()
    ));
    listen(listener, tx);

    let handle = thread::spawn(move || {
        let mut next_tick = Instant::now();
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
            gateway
                .flush()
                .map_err(|e| format!("journal flush failed: {e}"))?;
        }
    });
    Ok((addr, handle))
}

fn handle(gateway: &mut Gateway, inbound: NewsInbound) {
    match inbound {
        NewsInbound::Client(Inbound::Connected(id, outbox)) => {
            gateway.connect(id, Box::new(outbox))
        }
        NewsInbound::Client(Inbound::Frame(id, bytes)) => gateway.handle(id, &bytes, now_ms()),
        NewsInbound::Client(Inbound::Disconnected(id)) => gateway.disconnect(id),
        NewsInbound::Market { phase, day } => gateway.on_market(phase, day, now_ms()),
        NewsInbound::Rewrite(done) => gateway.on_rewrite(done, now_ms()),
        NewsInbound::Idea(done) => gateway.on_idea(done),
    }
}
