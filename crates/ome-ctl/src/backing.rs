use crate::doctor::reachable;
use crate::topology::{Kind, Topology};
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backing {
    pub kind: Kind,
    pub name: String,
    pub addr: String,
}

pub fn declared(topology: &Topology) -> Vec<Backing> {
    topology
        .services
        .iter()
        .filter(|s| s.kind.is_backing())
        .map(|s| Backing { kind: s.kind, name: s.name.clone(), addr: topology.addr(s) })
        .collect()
}

pub fn find(topology: &Topology, kind: Kind) -> Option<Backing> {
    declared(topology).into_iter().find(|b| b.kind == kind)
}

pub fn docker() -> bool {
    Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn compose() -> Command {
    let mut command = Command::new("docker");
    command.args(["compose", "-f", "docker-compose.yml"]);
    let gpu = Command::new("docker")
        .args(["info", "--format", "{{json .Runtimes}}"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("nvidia"));
    if gpu && Path::new("docker-compose.gpu.yml").exists() {
        command.args(["-f", "docker-compose.gpu.yml"]);
    }
    command.args(["--profile", "llm"]);
    command
}

fn started_file(data: &Path) -> std::path::PathBuf {
    data.join("omectl-backing.txt")
}

pub fn ensure(backing: &Backing, data: &Path) -> Result<bool, String> {
    if reachable(&backing.addr) {
        return Ok(false);
    }
    if !docker() || !Path::new("docker-compose.yml").exists() {
        return Err(format!("{} is not running at {} and Docker is not available to start it", backing.name, backing.addr));
    }
    println!("starting {} in Docker", backing.name);
    let ok = compose()
        .args(["up", "-d", "--wait", backing.kind.label()])
        .status()
        .is_ok_and(|s| s.success());
    if !ok || !reachable(&backing.addr) {
        return Err(format!("docker compose could not start {}", backing.name));
    }
    let path = started_file(data);
    let mut names = std::fs::read_to_string(&path).unwrap_or_default();
    if !names.lines().any(|l| l == backing.kind.label()) {
        names.push_str(backing.kind.label());
        names.push('\n');
        let _ = std::fs::write(&path, names);
    }
    Ok(true)
}

pub fn stop_started(data: &Path) {
    let path = started_file(data);
    let Ok(names) = std::fs::read_to_string(&path) else { return };
    let names: Vec<&str> = names.lines().filter(|l| !l.is_empty()).collect();
    if names.is_empty() {
        return;
    }
    println!("stopping {} (started by omectl)", names.join(", "));
    let _ = compose().arg("stop").args(&names).stdout(Stdio::null()).status();
    let _ = std::fs::remove_file(path);
}

pub fn pull_model(url: &str, model: &str, in_docker: bool) -> bool {
    if ome_llm::probe(url, model).unwrap_or(false) {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        println!("the model {model} is not pulled; stories and events use templates (pull it with scripts/llm.sh up)");
        return false;
    }
    print!("the local model {model} is not downloaded yet (several GB). Pull it now? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    if !answer.trim().eq_ignore_ascii_case("y") {
        println!("skipping the model; stories and events use templates");
        return false;
    }
    let ok = if in_docker {
        compose().args(["exec", "ollama", "ollama", "pull", model]).status()
    } else {
        Command::new("ollama").args(["pull", model]).status()
    }
    .is_ok_and(|s| s.success());
    ok && ome_llm::probe(url, model).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backing_services_come_from_the_topology() {
        let t = Topology::parse("exchange e port=7000\npostgres postgres port=5432\nredis redis port=6379\nollama ollama port=11434").unwrap();
        let all = declared(&t);
        assert_eq!(all.len(), 3);
        assert_eq!(find(&t, Kind::Redis).unwrap().addr, "127.0.0.1:6379");
        assert!(t.active(true).iter().all(|s| !s.kind.is_backing()), "the supervisor never launches them as processes");
    }
}
