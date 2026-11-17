use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Exchange,
    Clearing,
    Newsroom,
    Venue,
    Broker,
    Simctl,
    Agents,
    Recorder,
    Postgres,
    Redis,
    Ollama,
}

impl Kind {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "exchange" => Kind::Exchange,
            "clearing" => Kind::Clearing,
            "newsroom" => Kind::Newsroom,
            "venue" => Kind::Venue,
            "broker" => Kind::Broker,
            "simctl" => Kind::Simctl,
            "agents" => Kind::Agents,
            "recorder" => Kind::Recorder,
            "postgres" => Kind::Postgres,
            "redis" => Kind::Redis,
            "ollama" => Kind::Ollama,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Exchange => "exchange",
            Kind::Clearing => "clearing",
            Kind::Newsroom => "newsroom",
            Kind::Venue => "venue",
            Kind::Broker => "broker",
            Kind::Simctl => "simctl",
            Kind::Agents => "agents",
            Kind::Recorder => "recorder",
            Kind::Postgres => "postgres",
            Kind::Redis => "redis",
            Kind::Ollama => "ollama",
        }
    }

    pub fn is_backing(self) -> bool {
        matches!(self, Kind::Postgres | Kind::Redis | Kind::Ollama)
    }

    pub fn binary(self) -> &'static str {
        match self {
            Kind::Exchange => "ome-exchange",
            Kind::Clearing => "ome-clearing",
            Kind::Newsroom => "ome-newsroom",
            Kind::Venue => "ome-venue",
            Kind::Broker => "ome-broker",
            Kind::Simctl => "ome-simctl",
            Kind::Agents => "ome-agents",
            Kind::Recorder => "ome-recorder",
            Kind::Postgres | Kind::Redis | Kind::Ollama => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub kind: Kind,
    pub name: String,
    pub port: u16,
    pub options: BTreeMap<String, String>,
}

impl Service {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.options.get(key).map(String::as_str)
    }

    pub fn number(&self, key: &str, default: u64) -> u64 {
        self.get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    pub settings: BTreeMap<String, String>,
    pub services: Vec<Service>,
}

impl Topology {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut topology = Topology { settings: BTreeMap::new(), services: Vec::new() };
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let at = |why: String| format!("topology line {}: {why}", n + 1);
            let mut words = line.split_whitespace();
            let head = words.next().unwrap_or_default();
            let pairs = |words: std::str::SplitWhitespace| -> Result<BTreeMap<String, String>, String> {
                words
                    .map(|w| {
                        w.split_once('=')
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .ok_or_else(|| at(format!("expected key=value, got '{w}'")))
                    })
                    .collect()
            };
            if head == "set" {
                topology.settings.extend(pairs(words)?);
                continue;
            }
            let kind = Kind::parse(head).ok_or_else(|| at(format!("unknown service kind '{head}'")))?;
            let name = words.next().ok_or_else(|| at("missing service name".into()))?.to_string();
            if !ome_net::auth::valid_username(&name) {
                return Err(at(format!("'{name}' is not a valid service name")));
            }
            if topology.services.iter().any(|s| s.name == name) {
                return Err(at(format!("{name} declared twice")));
            }
            let options = pairs(words)?;
            let port = match options.get("port") {
                Some(p) => p.parse().map_err(|_| at(format!("bad port '{p}'")))?,
                None if matches!(kind, Kind::Agents | Kind::Recorder) => 0,
                None => return Err(at(format!("{name} needs port="))),
            };
            if port != 0 && topology.services.iter().any(|s| s.port == port) {
                return Err(at(format!("port {port} used twice")));
            }
            topology.services.push(Service { kind, name, port, options });
        }
        if !topology.services.iter().any(|s| s.kind == Kind::Exchange) {
            return Err("the topology needs an exchange service".into());
        }
        Ok(topology)
    }

    pub fn setting(&self, key: &str, default: &str) -> String {
        self.settings.get(key).cloned().unwrap_or_else(|| default.to_string())
    }

    pub fn host(&self) -> String {
        self.setting("host", "127.0.0.1")
    }

    pub fn addr(&self, service: &Service) -> String {
        format!("{}:{}", self.host(), service.port)
    }

    pub fn first(&self, kind: Kind) -> Option<&Service> {
        self.services.iter().find(|s| s.kind == kind)
    }

    pub fn find(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.name == name)
    }

    pub fn registry_port(&self) -> u16 {
        self.setting("registry", "7600").parse().unwrap_or(7600)
    }

    pub fn active(&self, record: bool) -> Vec<&Service> {
        let mut services: Vec<&Service> = self
            .services
            .iter()
            .filter(|s| !s.kind.is_backing())
            .filter(|s| s.get("when") != Some("record") || record)
            .collect();
        services.sort_by_key(|s| s.kind);
        services
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "
        set host=127.0.0.1 data=data regime=us   # comment
        exchange exchange port=7000 schedule=5,45,5,5
        broker retail-co port=7101 profile=retail collateral=2000000 wholesaler=wholesale-co
        venue wholesale-co port=7301 kind=wholesaler member=lp
        recorder recorder when=record
    ";

    #[test]
    fn topologies_parse_with_settings_ports_and_start_order() {
        let t = Topology::parse(SAMPLE).unwrap();
        assert_eq!(t.setting("regime", "india"), "us");
        assert_eq!(t.addr(t.find("retail-co").unwrap()), "127.0.0.1:7101");
        assert_eq!(t.find("retail-co").unwrap().number("collateral", 0), 2_000_000);
        let order: Vec<&str> = t.active(false).iter().map(|s| s.name.as_str()).collect();
        assert_eq!(order, vec!["exchange", "wholesale-co", "retail-co"], "venues before brokers, recorder only when recording");
        assert_eq!(t.active(true).len(), 4);
        assert!(Topology::parse("broker x port=1").unwrap_err().contains("exchange"));
        assert!(Topology::parse("exchange e port=1\nbroker b port=1").unwrap_err().contains("twice"));
        assert!(Topology::parse("exchange e port=1\nplanet p port=2").is_err());
        assert!(Topology::parse("exchange e").is_err());
    }
}
