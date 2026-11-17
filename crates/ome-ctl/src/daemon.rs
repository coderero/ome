use crate::admit::Admissions;
use crate::launch::{Backend, Launch, Secrets, Settings, plan};
use crate::topology::{Kind, Topology};
use ome_net::{Inbound, Outbox, SessionId, listen_quietly, log};
use ome_protocol::framing::frame;
use ome_protocol::{Health, RegistryMessage, ServerMessage, ServiceInfo, template};
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const START_WAIT: Duration = Duration::from_secs(20);
const SILENT: Duration = Duration::from_secs(5);
const STABLE: Duration = Duration::from_secs(60);
const MAX_BACKOFF: u64 = 30;

struct Proc {
    launch: Launch,
    started: Option<Instant>,
    restarts: u32,
    restart_at: Option<Instant>,
    last_exit: String,
}

pub struct Daemon {
    topology: Topology,
    secrets: Secrets,
    backend: Box<dyn Backend>,
    env: Vec<(String, String)>,
    procs: Vec<Proc>,
    next: usize,
    started_next_at: Instant,
    seen: BTreeMap<String, (ServiceInfo, Instant)>,
    sessions: BTreeMap<SessionId, Box<dyn Outbox>>,
}

fn encode(message: &ServerMessage) -> Arc<[u8]> {
    frame(&message.encode()).into()
}

impl Daemon {
    pub fn new(topology: Topology, settings: Settings, secrets: Secrets, backend: Box<dyn Backend>) -> Self {
        let registry = format!("{}:{}", topology.host(), topology.registry_port());
        let settings = &settings;
        let env = vec![
            ("OME_REGISTRY".to_string(), registry),
            ("OME_REGISTRY_SECRET".to_string(), secrets.registry.clone()),
        ];
        let procs = topology
            .active(settings.record)
            .into_iter()
            .flat_map(|s| plan(&topology, s, settings, &secrets))
            .map(|launch| Proc { launch, started: None, restarts: 0, restart_at: None, last_exit: String::new() })
            .collect();
        Self {
            topology,
            secrets,
            backend,
            env,
            procs,
            next: 0,
            started_next_at: Instant::now(),
            seen: BTreeMap::new(),
            sessions: BTreeMap::new(),
        }
    }

    fn start(&mut self, index: usize) {
        let proc = &mut self.procs[index];
        match self.backend.start(&proc.launch, &self.env) {
            Ok(pid) => {
                log(&format!("started {} (pid {pid})", proc.launch.name));
                proc.started = Some(Instant::now());
                proc.restart_at = None;
            }
            Err(e) => {
                log(&format!("{e}; retrying"));
                proc.restart_at = Some(Instant::now() + Duration::from_secs(5));
                proc.last_exit = e;
            }
        }
    }

    fn announced(&self, name: &str) -> bool {
        self.seen.get(name).is_some_and(|(_, at)| at.elapsed() < SILENT)
    }

    fn tick(&mut self) {
        if self.next < self.procs.len() {
            let previous_up = self.next == 0 || {
                let prev = &self.procs[self.next - 1].launch.name;
                self.announced(prev) || self.started_next_at.elapsed() > START_WAIT
            };
            if previous_up {
                if self.next > 0 && !self.announced(&self.procs[self.next - 1].launch.name.clone()) {
                    log(&format!("{} is not ready after {}s; starting the next service anyway", self.procs[self.next - 1].launch.name, START_WAIT.as_secs()));
                }
                let index = self.next;
                self.start(index);
                self.next += 1;
                self.started_next_at = Instant::now();
            }
        }
        for index in 0..self.next.min(self.procs.len()) {
            let name = self.procs[index].launch.name.clone();
            if let Some(status) = self.backend.exited(&name) {
                let proc = &mut self.procs[index];
                if proc.started.is_some_and(|t| t.elapsed() > STABLE) {
                    proc.restarts = 0;
                }
                let delay = (1u64 << proc.restarts.min(5)).min(MAX_BACKOFF);
                proc.restarts += 1;
                proc.started = None;
                proc.last_exit = status.clone();
                proc.restart_at = Some(Instant::now() + Duration::from_secs(delay));
                log(&format!("{name} exited ({status}); restarting in {delay}s (restart #{})", proc.restarts));
            }
            if self.procs[index].restart_at.is_some_and(|t| Instant::now() >= t) {
                self.start(index);
            }
        }
    }

    fn list(&self, kind: &str) -> Vec<ServiceInfo> {
        let mut list = Vec::new();
        for proc in &self.procs {
            let name = &proc.launch.name;
            let mut info = match self.seen.get(name) {
                Some((info, at)) => {
                    let mut info = info.clone();
                    info.age_ms = at.elapsed().as_millis() as u64;
                    if at.elapsed() > SILENT {
                        info.health = Health::Down;
                        info.detail = format!("silent for {}s", at.elapsed().as_secs());
                    }
                    info
                }
                None => {
                    let mut info = ServiceInfo::new(name, proc.launch.kind.label(), "");
                    info.pid = 0;
                    info.detail = if proc.started.is_some() { "starting".into() } else { "not started yet".into() };
                    info
                }
            };
            if proc.started.is_none() && !proc.last_exit.is_empty() {
                info.health = Health::Down;
                let wait = proc.restart_at.map(|t| t.saturating_duration_since(Instant::now()).as_secs()).unwrap_or(0);
                info.detail = format!("exited ({}); restart #{} in {wait}s", proc.last_exit, proc.restarts);
            }
            if info.kind.is_empty() {
                info.kind = proc.launch.kind.label().into();
            }
            list.push(info);
        }
        for (name, (info, at)) in &self.seen {
            if self.procs.iter().any(|p| &p.launch.name == name) || at.elapsed() > Duration::from_secs(60) {
                continue;
            }
            let mut info = info.clone();
            info.age_ms = at.elapsed().as_millis() as u64;
            info.capabilities = format!("{} (not in the topology)", info.capabilities);
            list.push(info);
        }
        list.retain(|s| kind.is_empty() || s.kind == kind);
        list
    }

    fn reply(&mut self, id: SessionId, message: &ServerMessage) {
        if let Some(outbox) = self.sessions.get(&id)
            && !outbox.send(encode(message))
        {
            self.sessions.remove(&id);
        }
    }

    fn stop_all(&mut self) {
        let names: Vec<String> = self.procs.iter().rev().map(|p| p.launch.name.clone()).collect();
        for name in names {
            log(&format!("stopping {name}"));
            self.backend.stop(&name, Duration::from_secs(5));
        }
    }

    fn handle(&mut self, id: SessionId, bytes: &[u8]) -> bool {
        let message = match RegistryMessage::decode(bytes) {
            Ok(m) => m,
            Err(e) => {
                self.reply(id, &ServerMessage::Reject { ref_template: 0, reason: format!("decode error: {e}") });
                return false;
            }
        };
        let secret = match &message {
            RegistryMessage::Announce { secret, .. } | RegistryMessage::Query { secret, .. } | RegistryMessage::Control { secret, .. } => secret,
        };
        if *secret != self.secrets.registry {
            self.reply(id, &ServerMessage::Reject { ref_template: message.template_id(), reason: "invalid registry secret".into() });
            return false;
        }
        match message {
            RegistryMessage::Announce { service, .. } => {
                if let Some((old, _)) = self.seen.get(&service.name)
                    && old.health != service.health
                {
                    log(&format!("{} is {}{}", service.name, service.health.label(), if service.detail.is_empty() { String::new() } else { format!(": {}", service.detail) }));
                }
                self.seen.insert(service.name.clone(), (service, Instant::now()));
            }
            RegistryMessage::Query { kind, .. } => {
                let list = self.list(&kind);
                self.reply(id, &ServerMessage::Services(list));
            }
            RegistryMessage::Control { action, target, .. } => match action.as_str() {
                "stop" => {
                    log("stop requested");
                    self.stop_all();
                    self.reply(id, &ServerMessage::Ack { ref_template: template::REGISTRY_CONTROL, text: "market stopped".into() });
                    return true;
                }
                "restart" => match self.procs.iter().position(|p| p.launch.name == target) {
                    Some(index) => {
                        self.backend.stop(&target, Duration::from_secs(5));
                        self.procs[index].restarts = 0;
                        self.start(index);
                        self.reply(id, &ServerMessage::Ack { ref_template: template::REGISTRY_CONTROL, text: format!("{target} restarted") });
                    }
                    None => self.reply(id, &ServerMessage::Reject { ref_template: template::REGISTRY_CONTROL, reason: format!("no service named {target}") }),
                },
                other => self.reply(id, &ServerMessage::Reject { ref_template: template::REGISTRY_CONTROL, reason: format!("unknown action {other}") }),
            },
        }
        false
    }

    pub fn run(mut self) -> Result<(), String> {
        let addr = format!("{}:{}", self.topology.host(), self.topology.registry_port());
        let listener = TcpListener::bind(&addr).map_err(|e| format!("cannot listen on {addr}: {e}"))?;
        log(&format!("omectl supervising {} service(s); registry on {addr}", self.procs.len()));
        let (tx, rx) = mpsc::channel::<Inbound>();
        listen_quietly(listener, tx);
        let (topology, secrets) = (self.topology.clone(), self.secrets.clone());
        thread::spawn(move || {
            let mut admissions = Admissions::new();
            loop {
                for line in admissions.pass(&topology, &secrets) {
                    log(&line);
                }
                thread::sleep(Duration::from_secs(2));
            }
        });
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Inbound::Connected(id, outbox)) => {
                    self.sessions.insert(id, Box::new(outbox));
                }
                Ok(Inbound::Frame(id, bytes)) => {
                    if self.handle(id, &bytes) {
                        thread::sleep(Duration::from_millis(200));
                        return Ok(());
                    }
                }
                Ok(Inbound::Disconnected(id)) => {
                    self.sessions.remove(&id);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err("registry listener stopped".into()),
            }
            self.tick();
        }
    }
}

pub fn summary(topology: &Topology, record: bool) -> Vec<(String, String, Kind)> {
    topology
        .active(record)
        .into_iter()
        .filter(|s| s.port != 0)
        .map(|s| (s.name.clone(), topology.addr(s), s.kind))
        .collect()
}
