use crate::topology::{Kind, Service, Topology};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Secrets {
    pub exchange: String,
    pub clearing: String,
    pub broker: String,
    pub member: String,
    pub venue: String,
    pub news: String,
    pub source: String,
    pub sim: String,
    pub runner: String,
    pub registry: String,
}

impl Secrets {
    pub fn from_env() -> Self {
        let env = |name: &str, fallback: &str| std::env::var(name).unwrap_or_else(|_| fallback.into());
        Self {
            exchange: env("OME_EXCHANGE_SECRET", "ops"),
            clearing: env("OME_CLEARING_SECRET", "ops"),
            broker: env("OME_BROKER_SECRET", "admin"),
            member: env("OME_MEMBER_SECRET", "member-pw"),
            venue: env("OME_VENUE_SECRET", "ops"),
            news: env("OME_NEWS_SECRET", "ops"),
            source: env("OME_NEWS_SOURCE_SECRET", "wire"),
            sim: env("OME_SIM_SECRET", "sim"),
            runner: env("OME_SIM_RUNNER_SECRET", "runner"),
            registry: env("OME_REGISTRY_SECRET", "cluster"),
        }
    }

    pub fn defaults_in_use(&self) -> Vec<&'static str> {
        let d = Self {
            exchange: "ops".into(),
            clearing: "ops".into(),
            broker: "admin".into(),
            member: "member-pw".into(),
            venue: "ops".into(),
            news: "ops".into(),
            source: "wire".into(),
            sim: "sim".into(),
            runner: "runner".into(),
            registry: "cluster".into(),
        };
        let mut out = Vec::new();
        for (name, a, b) in [
            ("OME_EXCHANGE_SECRET", &self.exchange, &d.exchange),
            ("OME_CLEARING_SECRET", &self.clearing, &d.clearing),
            ("OME_BROKER_SECRET", &self.broker, &d.broker),
            ("OME_MEMBER_SECRET", &self.member, &d.member),
            ("OME_VENUE_SECRET", &self.venue, &d.venue),
            ("OME_NEWS_SECRET", &self.news, &d.news),
            ("OME_SIM_SECRET", &self.sim, &d.sim),
            ("OME_REGISTRY_SECRET", &self.registry, &d.registry),
        ] {
            if a == b {
                out.push(name);
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub bin: PathBuf,
    pub data: PathBuf,
    pub record: bool,
    pub llm: Option<(String, String)>,
    pub postgres: String,
    pub redis: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub name: String,
    pub kind: Kind,
    pub program: PathBuf,
    pub args: Vec<String>,
}

fn s(text: impl Into<String>) -> String {
    text.into()
}

fn option(service: &Service, key: &str, var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| service.get(key).map(str::to_string))
        .unwrap_or_else(|| default.to_string())
}

pub fn plan(topology: &Topology, service: &Service, settings: &Settings, secrets: &Secrets) -> Vec<Launch> {
    let addr = topology.addr(service);
    let journal = |name: &str| settings.data.join(format!("{name}.journal")).display().to_string();
    let exchange = topology.first(Kind::Exchange).map(|e| topology.addr(e)).unwrap_or_default();
    let clearing = topology.first(Kind::Clearing).map(|c| topology.addr(c));
    let newsroom = topology.first(Kind::Newsroom).map(|n| topology.addr(n));
    let simctl = topology.first(Kind::Simctl).map(|n| topology.addr(n));
    let regime = topology.setting("regime", "us");
    let mut args: Vec<String> = Vec::new();
    let mut push = |items: &[String]| args.extend(items.iter().cloned());
    match service.kind {
        Kind::Exchange => {
            push(&[s("--listen"), addr.clone(), s("--journal"), journal(&service.name), s("--admin-secret"), secrets.exchange.clone()]);
            if service.get("require-prospectus") != Some("no") {
                push(&[s("--require-prospectus")]);
            }
            let schedule = service.get("schedule").map(str::to_string).unwrap_or_else(|| topology.setting("trading-day", "5,45,5,5"));
            if schedule != "off" {
                push(&[s("--schedule"), schedule]);
            }
            if let Some(bps) = service.get("luld-bps") {
                push(&[s("--luld-bps"), s(bps)]);
            }
        }
        Kind::Clearing => {
            push(&[
                s("--listen"), addr.clone(), s("--exchange"), exchange.clone(), s("--member"), service.name.clone(),
                s("--member-secret"), secrets.member.clone(), s("--journal"), journal(&service.name),
                s("--ops-secret"), secrets.clearing.clone(), s("--cycle-ms"),
                s(service.get("cycle-ms").unwrap_or(if topology.setting("trading-day", "5,45,5,5") == "off" { "30000" } else { "0" })),
            ]);
        }
        Kind::Newsroom => {
            push(&[
                s("--listen"), addr.clone(), s("--journal"), journal(&service.name), s("--ops-secret"), secrets.news.clone(),
                s("--source"), format!("world={}", secrets.source),
            ]);
            if let Some((url, model)) = &settings.llm {
                push(&[s("--llm"), url.clone(), s("--llm-model"), model.clone()]);
            }
        }
        Kind::Venue => {
            push(&[
                s("--kind"), s(service.get("kind").unwrap_or("wholesaler")), s("--listen"), addr.clone(),
                s("--exchange"), exchange.clone(), s("--member"), service.name.clone(), s("--member-secret"), secrets.member.clone(),
                s("--journal"), journal(&service.name), s("--ops-secret"), secrets.venue.clone(),
            ]);
            if let Some(member) = service.get("member") {
                push(&[s("--member-kind"), s(member)]);
            }
        }
        Kind::Broker => {
            push(&[
                s("--profile"), s(service.get("profile").unwrap_or("retail")), s("--listen"), addr.clone(),
                s("--exchange"), exchange.clone(), s("--member"), service.name.clone(), s("--member-secret"), secrets.member.clone(),
                s("--journal"), journal(&service.name), s("--admin-secret"), secrets.broker.clone(), s("--regime"), regime.clone(),
            ]);
            if let Some(clearing) = &clearing {
                push(&[s("--clearing"), clearing.clone()]);
            }
            if let Some(news) = newsroom.as_ref().filter(|_| service.get("news") != Some("no")) {
                push(&[s("--news"), news.clone()]);
            }
            if regime == "us" {
                for route in ["wholesaler", "ats"] {
                    if let Some(venue) = service.get(route) {
                        push(&[format!("--{route}"), s(venue)]);
                    }
                }
            }
            if settings.record {
                push(&[s("--postgres"), settings.postgres.clone()]);
            }
        }
        Kind::Simctl => {
            push(&[
                s("--listen"), addr.clone(), s("--journal"), journal(&service.name), s("--secret"), secrets.sim.clone(),
                s("--runner-secret"), secrets.runner.clone(), s("--exchange"), exchange.clone(), s("--exchange-secret"), secrets.exchange.clone(),
                s("--broker-secret"), secrets.broker.clone(), s("--world"), s(service.get("world").unwrap_or("config/world.conf")),
                s("--roster"), s(service.get("roster").unwrap_or("config/roster.conf")),
                s("--quarter-days"), option(service, "quarter-days", "OME_QUARTER_DAYS", "20"),
                s("--news-per-day"), option(service, "news-per-day", "OME_NEWS_PER_DAY", "1"),
                s("--scale"), option(service, "scale", "OME_AGENT_SCALE", "1"),
            ]);
            if let Some(news) = &newsroom {
                push(&[s("--newsroom"), news.clone(), s("--source"), s("world"), s("--source-secret"), secrets.source.clone()]);
            }
            if let Some((url, model)) = &settings.llm {
                push(&[s("--llm"), url.clone(), s("--llm-model"), model.clone()]);
            }
        }
        Kind::Agents => {
            let runners = service.number("runners", 1).max(1);
            return (1..=runners)
                .map(|n| {
                    let name = if n == 1 { service.name.clone() } else { format!("{}-{n}", service.name) };
                    Launch {
                        program: settings.bin.join(Kind::Agents.binary()),
                        args: vec![
                            s("--simctl"), simctl.clone().unwrap_or_default(), s("--sim-secret"), secrets.runner.clone(),
                            s("--name"), name.clone(),
                        ],
                        name,
                        kind: Kind::Agents,
                    }
                })
                .collect();
        }
        Kind::Recorder => {
            push(&[
                s("--postgres"), settings.postgres.clone(), s("--redis"), settings.redis.clone(),
                s("--exchange"), exchange.clone(), s("--exchange-secret"), secrets.exchange.clone(),
                s("--broker-secret"), secrets.broker.clone(), s("--venue-secret"), secrets.venue.clone(),
            ]);
            if let Some(clearing) = &clearing {
                push(&[s("--clearing"), clearing.clone(), s("--clearing-secret"), secrets.clearing.clone()]);
            }
            if let Some(news) = &newsroom {
                push(&[s("--news"), news.clone(), s("--news-secret"), secrets.news.clone()]);
            }
        }
    }
    vec![Launch { name: service.name.clone(), kind: service.kind, program: settings.bin.join(service.kind.binary()), args }]
}

pub trait Backend: Send {
    fn start(&mut self, launch: &Launch, env: &[(String, String)]) -> Result<u32, String>;
    fn exited(&mut self, name: &str) -> Option<String>;
    fn stop(&mut self, name: &str, grace: std::time::Duration);
}

pub struct LocalBackend {
    data: PathBuf,
    children: std::collections::BTreeMap<String, Child>,
}

impl LocalBackend {
    pub fn new(data: &Path) -> Self {
        Self { data: data.to_path_buf(), children: Default::default() }
    }

    fn log(&self, name: &str) -> std::io::Result<File> {
        OpenOptions::new().create(true).append(true).open(self.data.join(format!("{name}.log")))
    }
}

pub fn signal(pid: u32, signal: &str) -> bool {
    Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

impl Backend for LocalBackend {
    fn start(&mut self, launch: &Launch, env: &[(String, String)]) -> Result<u32, String> {
        let out = self.log(&launch.name).map_err(|e| e.to_string())?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let child = Command::new(&launch.program)
            .args(&launch.args)
            .envs(env.iter().cloned())
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", launch.program.display()))?;
        let pid = child.id();
        let _ = std::fs::write(self.data.join(format!("{}.pid", launch.name)), pid.to_string());
        self.children.insert(launch.name.clone(), child);
        Ok(pid)
    }

    fn exited(&mut self, name: &str) -> Option<String> {
        let child = self.children.get_mut(name)?;
        match child.try_wait() {
            Ok(Some(status)) => {
                self.children.remove(name);
                let _ = std::fs::remove_file(self.data.join(format!("{name}.pid")));
                Some(status.to_string())
            }
            _ => None,
        }
    }

    fn stop(&mut self, name: &str, grace: std::time::Duration) {
        let Some(mut child) = self.children.remove(name) else { return };
        signal(child.id(), "TERM");
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(self.data.join(format!("{name}.pid")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_plans_follow_the_topology_and_regime() {
        let text = "
            set regime=us
            exchange exchange port=7000 schedule=3,30,3,3
            clearing ccp port=7200
            newsroom newsroom port=7500
            broker retail-co port=7101 profile=retail wholesaler=wholesale-co
            simctl simctl port=7400
            agents agents runners=2
        ";
        let topology = Topology::parse(text).unwrap();
        let settings = Settings {
            bin: "target/release".into(),
            data: "data".into(),
            record: false,
            llm: Some(("http://127.0.0.1:11434".into(), "qwen2.5:7b".into())),
            postgres: "postgres://x".into(),
            redis: "redis://x".into(),
        };
        let secrets = Secrets::from_env();
        let launch = |name: &str| plan(&topology, topology.find(name).unwrap(), &settings, &secrets);
        let exchange = &launch("exchange")[0];
        assert!(exchange.args.windows(2).any(|w| w == ["--schedule", "3,30,3,3"]));
        let broker = &launch("retail-co")[0];
        assert!(broker.args.windows(2).any(|w| w == ["--wholesaler", "wholesale-co"]));
        assert!(broker.args.windows(2).any(|w| w == ["--news", "127.0.0.1:7500"]));
        assert!(!broker.args.contains(&"--postgres".to_string()));
        assert_eq!(broker.program, PathBuf::from("target/release/ome-broker"));
        assert!(launch("newsroom")[0].args.contains(&"--llm".to_string()));
        let runners = launch("agents");
        assert_eq!(runners.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), vec!["agents", "agents-2"]);
        let india = Topology::parse(&text.replace("regime=us", "regime=india")).unwrap();
        let broker = &plan(&india, india.find("retail-co").unwrap(), &settings, &secrets)[0];
        assert!(!broker.args.contains(&"--wholesaler".to_string()), "SEBI: everything goes to the exchange");
    }
}
