use crate::{log, now_ms};
use ome_protocol::framing::{frame, read_frame};
use ome_protocol::{Dependency, Health, RegistryMessage, ServerMessage, ServiceInfo};
use std::collections::BTreeMap;
use std::io::{BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub const DEFAULT_REGISTRY: &str = "127.0.0.1:7600";
const ANNOUNCE_EVERY: Duration = Duration::from_secs(1);
const RETRY: Duration = Duration::from_secs(2);
const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryAccess {
    pub addr: String,
    pub secret: String,
}

impl RegistryAccess {
    pub fn from_env() -> Option<Self> {
        let addr = std::env::var("OME_REGISTRY").ok().filter(|a| !a.is_empty())?;
        Some(Self {
            addr,
            secret: std::env::var("OME_REGISTRY_SECRET").unwrap_or_else(|_| "cluster".into()),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub health: Option<Health>,
    pub detail: String,
    pub deps: Vec<Dependency>,
    pub metrics: Vec<(String, i64)>,
}

impl Report {
    pub fn dep(&mut self, name: &str, up: bool, detail: impl Into<String>) -> &mut Self {
        self.deps.push(Dependency { name: name.into(), up, detail: detail.into() });
        self
    }

    pub fn metric(&mut self, key: &str, value: i64) -> &mut Self {
        self.metrics.push((key.into(), value));
        self
    }

    pub fn ready_unless(mut self, required: &[&str]) -> Self {
        let missing: Vec<&str> = self
            .deps
            .iter()
            .filter(|d| !d.up)
            .map(|d| d.name.as_str())
            .collect();
        let blocking: Vec<&str> = missing.iter().copied().filter(|m| required.contains(m)).collect();
        self.health = Some(if !blocking.is_empty() {
            Health::Starting
        } else if !missing.is_empty() {
            Health::Degraded
        } else {
            Health::Ready
        });
        if self.detail.is_empty() && !missing.is_empty() {
            self.detail = format!("waiting for {}", missing.join(", "));
        }
        self
    }
}

#[derive(Clone)]
pub struct Reporter {
    info: Arc<Mutex<ServiceInfo>>,
}

impl Reporter {
    pub fn start(access: Option<RegistryAccess>, mut info: ServiceInfo) -> Self {
        info.started_at = now_ms();
        let info = Arc::new(Mutex::new(info));
        if let Some(access) = access {
            let shared = info.clone();
            thread::spawn(move || announce(access, shared));
        }
        Self { info }
    }

    pub fn disabled(name: &str, kind: &str) -> Self {
        Self::start(None, ServiceInfo::new(name, kind, ""))
    }

    pub fn set_address(&self, address: &str) {
        self.info.lock().expect("reporter lock").address = address.to_string();
    }

    pub fn set_capabilities(&self, capabilities: &str) {
        self.info.lock().expect("reporter lock").capabilities = capabilities.to_string();
    }

    pub fn report(&self, report: Report) {
        let mut info = self.info.lock().expect("reporter lock");
        info.health = report.health.unwrap_or(Health::Ready);
        info.detail = report.detail;
        info.deps = report.deps;
        info.metrics = report.metrics;
    }

    pub fn stopping(&self) {
        self.info.lock().expect("reporter lock").health = Health::Stopping;
    }

    pub fn snapshot(&self) -> ServiceInfo {
        self.info.lock().expect("reporter lock").clone()
    }
}

fn announce(access: RegistryAccess, info: Arc<Mutex<ServiceInfo>>) {
    let mut warned = false;
    loop {
        match connect(&access.addr) {
            Ok(mut stream) => {
                warned = false;
                loop {
                    let service = info.lock().expect("reporter lock").clone();
                    let message = RegistryMessage::Announce { secret: access.secret.clone(), service };
                    if stream.write_all(&frame(&message.encode())).is_err() {
                        break;
                    }
                    thread::sleep(ANNOUNCE_EVERY);
                }
            }
            Err(e) if !warned => {
                log(&format!("registry {} unreachable ({e}); retrying", access.addr));
                warned = true;
            }
            Err(_) => {}
        }
        thread::sleep(RETRY);
    }
}

fn connect(addr: &str) -> std::io::Result<TcpStream> {
    let target = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no address"))?;
    let stream = TcpStream::connect_timeout(&target, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

pub fn query(access: &RegistryAccess, kind: &str) -> Result<Vec<ServiceInfo>, String> {
    let mut stream = connect(&access.addr).map_err(|e| format!("registry {}: {e}", access.addr))?;
    let message = RegistryMessage::Query { secret: access.secret.clone(), kind: kind.to_string() };
    stream
        .write_all(&frame(&message.encode()))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    loop {
        match read_frame(&mut reader) {
            Ok(Some(bytes)) => match ServerMessage::decode(&bytes) {
                Ok(ServerMessage::Services(list)) => return Ok(list),
                Ok(ServerMessage::Reject { reason, .. }) => return Err(reason),
                _ => {}
            },
            Ok(None) => return Err("registry closed the connection".into()),
            Err(e) => return Err(e.to_string()),
        }
    }
}

pub fn control(access: &RegistryAccess, action: &str, target: &str) -> Result<String, String> {
    let mut stream = connect(&access.addr).map_err(|e| format!("registry {}: {e}", access.addr))?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
    let message = RegistryMessage::Control {
        secret: access.secret.clone(),
        action: action.to_string(),
        target: target.to_string(),
    };
    stream
        .write_all(&frame(&message.encode()))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    loop {
        match read_frame(&mut reader) {
            Ok(Some(bytes)) => match ServerMessage::decode(&bytes) {
                Ok(ServerMessage::Ack { text, .. }) => return Ok(text),
                Ok(ServerMessage::Reject { reason, .. }) => return Err(reason),
                _ => {}
            },
            Ok(None) => return Err("registry closed the connection".into()),
            Err(e) => return Err(e.to_string()),
        }
    }
}

pub trait Directory: Send + Sync {
    fn resolve(&self, name: &str) -> Option<String>;
    fn members(&self, kind: &str) -> Vec<(String, String)>;
}

#[derive(Debug, Clone, Default)]
pub struct StaticDirectory {
    entries: BTreeMap<String, (String, String)>,
}

impl StaticDirectory {
    pub fn with(mut self, name: &str, kind: &str, addr: &str) -> Self {
        self.entries.insert(name.into(), (kind.into(), addr.into()));
        self
    }

    pub fn insert(&mut self, name: &str, kind: &str, addr: &str) {
        self.entries.insert(name.into(), (kind.into(), addr.into()));
    }
}

impl Directory for StaticDirectory {
    fn resolve(&self, name: &str) -> Option<String> {
        self.entries.get(name).map(|(_, addr)| addr.clone())
    }

    fn members(&self, kind: &str) -> Vec<(String, String)> {
        self.entries
            .iter()
            .filter(|(_, (k, _))| k == kind)
            .map(|(n, (_, a))| (n.clone(), a.clone()))
            .collect()
    }
}

pub struct RegistryDirectory {
    pub access: RegistryAccess,
    pub fallback: StaticDirectory,
}

impl Directory for RegistryDirectory {
    fn resolve(&self, name: &str) -> Option<String> {
        query(&self.access, "")
            .ok()
            .and_then(|list| list.into_iter().find(|s| s.name == name).map(|s| s.address))
            .filter(|a| !a.is_empty())
            .or_else(|| self.fallback.resolve(name))
    }

    fn members(&self, kind: &str) -> Vec<(String, String)> {
        let mut found: BTreeMap<String, String> = self.fallback.members(kind).into_iter().collect();
        if let Ok(list) = query(&self.access, kind) {
            for s in list.into_iter().filter(|s| !s.address.is_empty() && s.health != Health::Down) {
                found.insert(s.name, s.address);
            }
        }
        found.into_iter().collect()
    }
}

pub fn resolve_addr(directory: &dyn Directory, target: &str) -> Option<String> {
    if target.contains(':') {
        Some(target.to_string())
    } else {
        directory.resolve(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_turn_dependency_state_into_health() {
        let mut r = Report::default();
        r.dep("exchange", true, "").dep("news", false, "refused");
        let r = r.ready_unless(&["exchange"]);
        assert_eq!(r.health, Some(Health::Degraded));
        assert!(r.detail.contains("news"));
        let mut r = Report::default();
        r.dep("exchange", false, "");
        assert_eq!(r.ready_unless(&["exchange"]).health, Some(Health::Starting));
        assert_eq!(Report::default().ready_unless(&[]).health, Some(Health::Ready));
    }

    #[test]
    fn static_directories_resolve_names_and_list_kinds() {
        let d = StaticDirectory::default()
            .with("retail-co", "broker", "127.0.0.1:7101")
            .with("ats-co", "venue", "127.0.0.1:7302");
        assert_eq!(resolve_addr(&d, "retail-co").as_deref(), Some("127.0.0.1:7101"));
        assert_eq!(resolve_addr(&d, "10.0.0.1:9").as_deref(), Some("10.0.0.1:9"));
        assert_eq!(d.members("broker").len(), 1);
        assert!(d.resolve("nope").is_none());
    }
}
