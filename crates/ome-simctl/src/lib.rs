pub mod fleet;
pub mod gateway;
pub mod journal;
pub mod provision;
pub mod recording;
pub mod state;
pub mod world;

use fleet::{Fleet, GroupSpec};
use gateway::{Credentials, ExchangeAccess, Gateway, IdeaDone, IdeaJob};
use journal::Record;
use ome_client::LinkEvent;
use ome_client::upstream::{MemberCredentials, TcpConnector, Upstream};
use ome_llm::{LlmConfig, Model, Ollama, offline};
use ome_net::auth::hash_secret;
use ome_net::registry::{Directory, RegistryAccess, Reporter};
use ome_net::{Inbound, listen, log, now_ms};
use ome_protocol::ServiceInfo;
use ome_store::Journal;
use provision::{Access, Outcome};
use state::SimState;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use world::World;
use world::company::WorldSpec;

const REPORT_EVERY: Duration = Duration::from_secs(1);

pub struct SimConfig {
    pub listen: String,
    pub journal: Option<PathBuf>,
    pub operator: (String, String),
    pub runner: (String, String),
    pub exchange_addr: String,
    pub exchange_name: String,
    pub exchange_secret: String,
    pub world: WorldSpec,
    pub roster: Vec<GroupSpec>,
    pub quarter_days: u64,
    pub news_per_day: f64,
    pub newsroom: Option<(String, String, String)>,
    pub calendar: bool,
    pub provision: Option<(String, String)>,
    pub directory: Arc<dyn Directory>,
    pub agent_secret: String,
    pub llm: Option<LlmConfig>,
    pub seed: Option<u64>,
    pub registry: Option<RegistryAccess>,
}

impl SimConfig {
    pub fn new(listen: &str, exchange_addr: &str, directory: Arc<dyn Directory>) -> Self {
        Self {
            listen: listen.into(),
            journal: None,
            operator: ("operator".into(), "sim".into()),
            runner: ("runner".into(), "runner".into()),
            exchange_addr: exchange_addr.into(),
            exchange_name: "ops".into(),
            exchange_secret: "ops".into(),
            world: WorldSpec::default(),
            roster: Vec::new(),
            quarter_days: 20,
            news_per_day: 0.0,
            newsroom: None,
            calendar: false,
            provision: None,
            directory,
            agent_secret: "agent-pw".into(),
            llm: None,
            seed: None,
            registry: None,
        }
    }
}

pub enum SimInbound {
    Client(Inbound),
    Calendar { generation: u64, event: LinkEvent },
    Wire { generation: u64, event: LinkEvent },
    Idea(IdeaDone),
    Outcome(Outcome),
}

impl From<Inbound> for SimInbound {
    fn from(inbound: Inbound) -> Self {
        SimInbound::Client(inbound)
    }
}

fn calendar_event(generation: u64, event: LinkEvent) -> SimInbound {
    SimInbound::Calendar { generation, event }
}

fn wire_event(generation: u64, event: LinkEvent) -> SimInbound {
    SimInbound::Wire { generation, event }
}

fn outcome(outcome: Outcome) -> SimInbound {
    SimInbound::Outcome(outcome)
}

fn spawn_ideas(model: Box<dyn Model>, jobs: Receiver<IdeaJob>, events: Sender<SimInbound>) {
    thread::spawn(move || {
        let mut offline_until: Option<Instant> = None;
        for job in jobs {
            if let Some(until) = offline_until.take() {
                thread::sleep(until.saturating_duration_since(Instant::now()));
            }
            let begun = Instant::now();
            let result = world::ideas::invent(model.as_ref(), &job.context, job.seed);
            if let Err(e) = &result
                && offline(e)
            {
                offline_until = Some(Instant::now() + Duration::from_secs(10));
            }
            let done = IdeaDone { symbol: job.symbol, result, elapsed_ms: begun.elapsed().as_millis() as u64 };
            if events.send(SimInbound::Idea(done)).is_err() {
                return;
            }
        }
    });
}

pub fn start(config: SimConfig) -> Result<(SocketAddr, JoinHandle<Result<(), String>>), String> {
    let mut state = SimState::new(World::new(config.world.companies.clone(), config.quarter_days));
    let mut clock = 0;
    let journal = match &config.journal {
        Some(path) => {
            let (journal, records) = Journal::<Record>::open(path)
                .map_err(|e| format!("cannot open journal {}: {e}", path.display()))?;
            for record in &records {
                state.apply_fact(&record.fact, record.timestamp);
                clock = record.timestamp;
            }
            log(&format!(
                "replayed {} simulation fact(s): world day {}, {} event(s)",
                records.len(),
                state.world().day(),
                state.world().next_seq() - 1
            ));
            Some(ome_store::boxed(journal))
        }
        None => None,
    };
    let credentials = Credentials {
        operator: (config.operator.0.clone(), hash_secret(&config.operator.0, &config.operator.1)),
        runner: (config.runner.0.clone(), hash_secret(&config.runner.0, &config.runner.1)),
    };
    let exchange = ExchangeAccess {
        addr: config.exchange_addr.clone(),
        name: config.exchange_name.clone(),
        secret: config.exchange_secret.clone(),
    };
    let (tx, rx) = mpsc::channel::<SimInbound>();
    let mut gateway = Gateway::new(state, journal, credentials, exchange, clock)
        .with_world(config.world.accounts.clone(), config.news_per_day, config.seed)
        .with_fleet(Fleet::new(config.roster.clone()), &config.agent_secret);
    if config.calendar {
        let credentials = MemberCredentials { name: config.exchange_name.clone(), secret: config.exchange_secret.clone() };
        let connector = TcpConnector::new(config.exchange_addr.clone(), tx.clone(), calendar_event);
        gateway = gateway.with_calendar(Upstream::new("exchange calendar", Box::new(connector), credentials));
    }
    if let Some((addr, name, secret)) = config.newsroom.clone() {
        let connector = TcpConnector::new(addr, tx.clone(), wire_event).resolving(Some(config.directory.clone()));
        gateway = gateway.with_wire(Upstream::new("newsroom", Box::new(connector), MemberCredentials { name, secret }));
    }
    if let Some(llm) = &config.llm {
        let (jobs, queue) = mpsc::channel();
        let patient = LlmConfig { timeout: llm.timeout * 3, ..llm.clone() };
        spawn_ideas(Box::new(Ollama::new(&patient)?), queue, tx.clone());
        gateway = gateway.with_ideas(jobs, &llm.model);
    }
    if let Some(broker_admin) = config.provision.clone() {
        let (tasks, queue) = mpsc::channel();
        let access = Access {
            exchange: (config.exchange_addr.clone(), config.exchange_name.clone(), config.exchange_secret.clone()),
            broker_admin,
            directory: config.directory.clone(),
        };
        provision::spawn(access, queue, tx.clone(), outcome);
        gateway = gateway.with_provisioner(tasks);
    }
    gateway.start_up(now_ms());
    gateway.flush().map_err(|e| e.to_string())?;

    let listener = TcpListener::bind(&config.listen).map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    log(&format!(
        "simulation controller listening on {addr}: {} companies, {} account(s), {} agent group(s)",
        config.world.companies.len(),
        config.world.accounts.len(),
        config.roster.len()
    ));
    let mut info = ServiceInfo::new("simctl", "simctl", &addr.to_string());
    info.capabilities = format!("companies={} groups={}", config.world.companies.len(), config.roster.len());
    let reporter = Reporter::start(config.registry.clone(), info);
    listen(listener, tx);
    let handle = thread::spawn(move || {
        let mut next_report = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(inbound) => handle(&mut gateway, inbound),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("listener stopped".to_string()),
            }
            gateway.tick(now_ms());
            gateway.flush().map_err(|e| format!("journal flush failed: {e}"))?;
            if Instant::now() >= next_report {
                next_report = Instant::now() + REPORT_EVERY;
                reporter.report(gateway.health());
            }
        }
    });
    Ok((addr, handle))
}

fn handle(gateway: &mut Gateway, inbound: SimInbound) {
    match inbound {
        SimInbound::Client(Inbound::Connected(id, outbox)) => gateway.connect(id, Box::new(outbox)),
        SimInbound::Client(Inbound::Frame(id, bytes)) => gateway.handle(id, &bytes, now_ms()),
        SimInbound::Client(Inbound::Disconnected(id)) => gateway.disconnect(id),
        SimInbound::Calendar { generation, event } => gateway.on_calendar(generation, event, now_ms()),
        SimInbound::Wire { generation, event } => gateway.on_wire(generation, event, now_ms()),
        SimInbound::Idea(done) => gateway.on_idea(done),
        SimInbound::Outcome(outcome) => gateway.on_outcome(outcome, now_ms()),
    }
}
