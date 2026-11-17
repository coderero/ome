use crate::codec::{DecodeError, Decoder, Encoder};
use crate::template::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Health {
    Starting = 1,
    Ready = 2,
    Degraded = 3,
    Stopping = 4,
    Down = 5,
}

impl Health {
    pub fn from_u8(code: u8) -> Option<Self> {
        Some(match code {
            1 => Health::Starting,
            2 => Health::Ready,
            3 => Health::Degraded,
            4 => Health::Stopping,
            5 => Health::Down,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Health::Starting => "starting",
            Health::Ready => "ready",
            Health::Degraded => "degraded",
            Health::Stopping => "stopping",
            Health::Down => "down",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub up: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    pub name: String,
    pub kind: String,
    pub address: String,
    pub pid: u32,
    pub started_at: u64,
    pub age_ms: u64,
    pub health: Health,
    pub detail: String,
    pub capabilities: String,
    pub deps: Vec<Dependency>,
    pub metrics: Vec<(String, i64)>,
}

impl ServiceInfo {
    pub fn new(name: &str, kind: &str, address: &str) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            address: address.into(),
            pid: std::process::id(),
            started_at: 0,
            age_ms: 0,
            health: Health::Starting,
            detail: String::new(),
            capabilities: String::new(),
            deps: Vec::new(),
            metrics: Vec::new(),
        }
    }

    pub fn metric(&self, key: &str) -> Option<i64> {
        self.metrics.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
    }
}

pub fn put_services(e: &mut Encoder, list: &[ServiceInfo]) {
    e.end_block();
    let mut g = e.begin_group(list.len());
    for s in list {
        e.begin_entry(&mut g);
        e.u32(s.pid).u64(s.started_at).u64(s.age_ms).u8(s.health as u8);
        e.end_entry_block(&mut g);
        let deps: Vec<String> = s
            .deps
            .iter()
            .map(|d| format!("{}\t{}\t{}", d.name, if d.up { 1 } else { 0 }, d.detail))
            .collect();
        let metrics: Vec<String> = s.metrics.iter().map(|(k, v)| format!("{k}\t{v}")).collect();
        e.var(&s.name)
            .var(&s.kind)
            .var(&s.address)
            .var(&s.detail)
            .var(&s.capabilities)
            .var(&deps.join("\n"))
            .var(&metrics.join("\n"));
    }
}

pub fn get_services(d: &mut Decoder) -> Result<Vec<ServiceInfo>, DecodeError> {
    d.end_block()?;
    let g = d.group()?;
    let mut list = Vec::with_capacity(g.count);
    for _ in 0..g.count {
        let start = d.entry_start();
        let pid = d.u32()?;
        let started_at = d.u64()?;
        let age_ms = d.u64()?;
        let health = Health::from_u8(d.u8()?).ok_or(DecodeError::Invalid("health"))?;
        d.end_entry_block(start, &g)?;
        let name = d.var()?;
        let kind = d.var()?;
        let address = d.var()?;
        let detail = d.var()?;
        let capabilities = d.var()?;
        let deps = d
            .var()?
            .lines()
            .filter_map(|l| {
                let mut parts = l.splitn(3, '\t');
                Some(Dependency {
                    name: parts.next()?.to_string(),
                    up: parts.next()? == "1",
                    detail: parts.next().unwrap_or("").to_string(),
                })
            })
            .collect();
        let metrics = d
            .var()?
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once('\t')?;
                Some((k.to_string(), v.parse().ok()?))
            })
            .collect();
        list.push(ServiceInfo {
            name,
            kind,
            address,
            pid,
            started_at,
            age_ms,
            health,
            detail,
            capabilities,
            deps,
            metrics,
        });
    }
    Ok(list)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryMessage {
    Announce { secret: String, service: ServiceInfo },
    Query { secret: String, kind: String },
    Control { secret: String, action: String, target: String },
}

impl RegistryMessage {
    pub fn template_id(&self) -> u16 {
        match self {
            RegistryMessage::Announce { .. } => REGISTRY_ANNOUNCE,
            RegistryMessage::Query { .. } => REGISTRY_QUERY,
            RegistryMessage::Control { .. } => REGISTRY_CONTROL,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(self.template_id());
        match self {
            RegistryMessage::Announce { secret, service } => {
                put_services(&mut e, std::slice::from_ref(service));
                e.var(secret);
            }
            RegistryMessage::Query { secret, kind } => {
                e.end_block().var(secret).var(kind);
            }
            RegistryMessage::Control { secret, action, target } => {
                e.end_block().var(secret).var(action).var(target);
            }
        }
        e.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes)?;
        Ok(match d.template_id {
            REGISTRY_ANNOUNCE => {
                let service = get_services(&mut d)?
                    .pop()
                    .ok_or(DecodeError::Invalid("announce without a service"))?;
                RegistryMessage::Announce { secret: d.var()?, service }
            }
            REGISTRY_QUERY => {
                d.end_block()?;
                RegistryMessage::Query { secret: d.var()?, kind: d.var()? }
            }
            REGISTRY_CONTROL => {
                d.end_block()?;
                RegistryMessage::Control { secret: d.var()?, action: d.var()?, target: d.var()? }
            }
            other => return Err(DecodeError::UnknownTemplate(other)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_messages_round_trip() {
        let mut service = ServiceInfo::new("retail-co", "broker", "127.0.0.1:7101");
        service.health = Health::Degraded;
        service.detail = "news feed down".into();
        service.capabilities = "profile=retail routes=wholesaler".into();
        service.deps = vec![
            Dependency { name: "exchange".into(), up: true, detail: String::new() },
            Dependency { name: "news".into(), up: false, detail: "connection refused".into() },
        ];
        service.metrics = vec![("accounts".into(), 42), ("lag_ms".into(), -1)];
        let messages = [
            RegistryMessage::Announce { secret: "s".into(), service: service.clone() },
            RegistryMessage::Query { secret: "s".into(), kind: "broker".into() },
            RegistryMessage::Control { secret: "s".into(), action: "stop".into(), target: String::new() },
        ];
        for m in messages {
            assert_eq!(RegistryMessage::decode(&m.encode()), Ok(m));
        }
        assert_eq!(service.metric("accounts"), Some(42));
        assert!(RegistryMessage::decode(&crate::MemberMessage::QueryMembers.encode()).is_err());
    }
}
