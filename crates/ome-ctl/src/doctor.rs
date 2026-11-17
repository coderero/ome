use crate::launch::{Secrets, Settings};
use crate::topology::{Kind, Topology};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub level: Level,
    pub what: String,
    pub detail: String,
}

fn check(level: Level, what: &str, detail: impl Into<String>) -> Check {
    Check { level, what: what.into(), detail: detail.into() }
}

pub fn reachable(addr: &str) -> bool {
    addr.to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .is_some_and(|a| TcpStream::connect_timeout(&a, Duration::from_millis(500)).is_ok())
}

pub fn host_port(url: &str, default_port: u16) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let rest = rest.rsplit('@').next().unwrap_or(rest);
    let host = rest.split(['/', '?']).next().unwrap_or(rest);
    if host.contains(':') { host.to_string() } else { format!("{host}:{default_port}") }
}

fn free_kb(dir: &Path) -> Option<u64> {
    let out = Command::new("df").arg("-Pk").arg(dir).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()
}

pub fn run(topology: &Topology, settings: &Settings, secrets: &Secrets, writer: &str, running: bool) -> Vec<Check> {
    let mut checks = Vec::new();
    let services = topology.active(settings.record);
    let mut missing: Vec<String> = Vec::new();
    for kind in services.iter().map(|s| s.kind).collect::<std::collections::BTreeSet<_>>() {
        if !settings.bin.join(kind.binary()).exists() {
            missing.push(kind.binary().to_string());
        }
    }
    checks.push(if missing.is_empty() {
        check(Level::Ok, "binaries", format!("all in {}", settings.bin.display()))
    } else {
        check(Level::Fail, "binaries", format!("missing {} in {} (build with cargo build --release --workspace)", missing.join(", "), settings.bin.display()))
    });
    if running {
        checks.push(check(Level::Ok, "ports", "omectl is already supervising this market"));
    } else {
        let mut busy = Vec::new();
        let ports = services.iter().map(|s| s.port).chain([topology.registry_port()]).filter(|p| *p != 0);
        for port in ports {
            if TcpListener::bind((topology.host().as_str(), port)).is_err() {
                busy.push(port.to_string());
            }
        }
        checks.push(if busy.is_empty() {
            check(Level::Ok, "ports", "all free")
        } else {
            check(Level::Fail, "ports", format!("in use: {} (another market running? `omectl down` or scripts/stop.sh)", busy.join(", ")))
        });
    }
    let probe = settings.data.join(".omectl-probe");
    let writable = std::fs::create_dir_all(&settings.data).is_ok() && std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    checks.push(if writable {
        check(Level::Ok, "data directory", settings.data.display().to_string())
    } else {
        check(Level::Fail, "data directory", format!("cannot write to {}", settings.data.display()))
    });
    match free_kb(&settings.data) {
        Some(kb) if kb < 200 * 1024 => checks.push(check(Level::Fail, "disk", format!("only {} MB free", kb / 1024))),
        Some(kb) if kb < 2 * 1024 * 1024 => checks.push(check(Level::Warn, "disk", format!("{} MB free; journals grow with every order", kb / 1024))),
        Some(kb) => checks.push(check(Level::Ok, "disk", format!("{} GB free", kb / 1024 / 1024))),
        None => checks.push(check(Level::Warn, "disk", "could not measure free space")),
    }
    for service in services.iter().filter(|s| s.kind == Kind::Simctl) {
        for (key, default) in [("world", "config/world.conf"), ("roster", "config/roster.conf")] {
            let path = service.get(key).unwrap_or(default);
            checks.push(if Path::new(path).exists() {
                check(Level::Ok, key, path)
            } else {
                check(Level::Fail, key, format!("{path} not found"))
            });
        }
    }
    if writer != "templates" && topology.first(Kind::Newsroom).is_some() {
        let url = std::env::var("OME_LLM").unwrap_or_else(|_| "http://127.0.0.1:11434".into());
        let model = std::env::var("OME_LLM_MODEL").unwrap_or_else(|_| "qwen2.5:7b".into());
        checks.push(match ome_llm::probe(&url, &model) {
            Ok(true) => check(Level::Ok, "llm", format!("{model} at {url}")),
            Ok(false) => check(Level::Warn, "llm", format!("Ollama answers at {url} but {model} is not pulled (scripts/llm.sh up pulls it); stories use templates")),
            Err(e) if writer == "llm" => check(Level::Fail, "llm", format!("Ollama at {url}: {e}")),
            Err(e) => check(Level::Warn, "llm", format!("Ollama at {url}: {e}; stories and events use templates (scripts/llm.sh up, or set writer=templates)")),
        });
    }
    if settings.record {
        for (what, url, port) in [("postgres", &settings.postgres, 5432), ("redis", &settings.redis, 6379)] {
            let addr = host_port(url, port);
            let docker = Command::new("docker").arg("info").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
            checks.push(if reachable(&addr) {
                check(Level::Ok, what, addr)
            } else if docker {
                check(Level::Ok, what, format!("{addr} not running; omectl up starts it in Docker"))
            } else {
                check(Level::Warn, what, format!("{addr} unreachable and no Docker; the market runs without recording"))
            });
        }
    }
    let docker = Command::new("docker").arg("--version").output().is_ok_and(|o| o.status.success());
    checks.push(check(Level::Ok, "docker", if docker { "available" } else { "not installed (only needed for databases and Ollama)" }));
    let host = topology.host();
    let defaults = secrets.defaults_in_use();
    if !defaults.is_empty() {
        let loopback = host.starts_with("127.") || host == "localhost";
        checks.push(check(
            if loopback { Level::Ok } else { Level::Warn },
            "secrets",
            if loopback {
                format!("default secrets on loopback ({} defaults)", defaults.len())
            } else {
                format!("default secrets while listening on {host}: set {}", defaults.join(", "))
            },
        ));
    }
    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_urls_turn_into_addresses() {
        assert_eq!(host_port("postgres://ome:ome@127.0.0.1/ome", 5432), "127.0.0.1:5432");
        assert_eq!(host_port("redis://127.0.0.1/", 6379), "127.0.0.1:6379");
        assert_eq!(host_port("postgres://u:p@db:6000/x", 5432), "db:6000");
        assert!(!reachable("127.0.0.1:1"));
    }
}
