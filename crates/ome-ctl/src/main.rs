mod admit;
mod daemon;
mod doctor;
mod launch;
mod topology;

use doctor::Level;
use launch::{LocalBackend, Secrets, Settings, signal};
use ome_net::registry::{RegistryAccess, control, query};
use ome_protocol::{Health, ServiceInfo};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use topology::Topology;

const USAGE: &str = "usage: omectl [--topology FILE] <command>
  up [--fresh] [--record] [--no-build] [--writer auto|llm|templates]
                    preflight checks, build, start the market under supervision and wait until it is ready
  down              stop every service in reverse start order
  status            live cluster view: health, dependencies and metrics reported by each service
  logs <service> [-f]
  restart <service>
  doctor            preflight checks only
  admit             approve pending applications of participants declared in the topology now
  serve             run the supervisor in the foreground (what `up` starts in the background)
The topology (config/topology.conf by default) lists every service: kind, name, port and options.";

struct Cli {
    topology: PathBuf,
    command: Vec<String>,
}

fn parse() -> Result<Cli, String> {
    let mut cli = Cli { topology: "config/topology.conf".into(), command: Vec::new() };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--topology" => cli.topology = args.next().ok_or("--topology needs a file")?.into(),
            "-h" | "--help" => return Err(USAGE.into()),
            _ => cli.command.push(arg),
        }
    }
    if cli.command.is_empty() {
        return Err(USAGE.into());
    }
    Ok(cli)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn settings(topology: &Topology, record: bool, llm: bool) -> Settings {
    Settings {
        bin: env("OME_BIN").unwrap_or_else(|| topology.setting("bin", "target/release")).into(),
        data: env("OME_DATA").unwrap_or_else(|| topology.setting("data", "data")).into(),
        record,
        llm: llm.then(|| {
            (
                env("OME_LLM").unwrap_or_else(|| "http://127.0.0.1:11434".into()),
                env("OME_LLM_MODEL").unwrap_or_else(|| "qwen2.5:7b".into()),
            )
        }),
        postgres: env("OME_POSTGRES").unwrap_or_else(|| "postgres://ome:ome@127.0.0.1/ome".into()),
        redis: env("OME_REDIS").unwrap_or_else(|| "redis://127.0.0.1/".into()),
    }
}

fn access(topology: &Topology, secrets: &Secrets) -> RegistryAccess {
    RegistryAccess { addr: format!("{}:{}", topology.host(), topology.registry_port()), secret: secrets.registry.clone() }
}

fn running(topology: &Topology, secrets: &Secrets) -> bool {
    query(&access(topology, secrets), "").is_ok()
}

fn print_checks(checks: &[doctor::Check]) -> bool {
    for c in checks {
        let tag = match c.level {
            Level::Ok => "ok  ",
            Level::Warn => "WARN",
            Level::Fail => "FAIL",
        };
        println!("  {tag} {:<15} {}", c.what, c.detail);
    }
    !checks.iter().any(|c| c.level == Level::Fail)
}

fn health_tag(h: Health) -> &'static str {
    match h {
        Health::Ready => "ready",
        Health::Degraded => "DEGRADED",
        Health::Starting => "starting",
        Health::Stopping => "stopping",
        Health::Down => "DOWN",
    }
}

fn print_status(list: &[ServiceInfo]) {
    println!("{:<14} {:<9} {:<9} {:>7}  {:<16} detail", "SERVICE", "KIND", "HEALTH", "PID", "ADDRESS");
    for s in list {
        let down: Vec<String> = s.deps.iter().filter(|d| !d.up).map(|d| if d.detail.is_empty() { d.name.clone() } else { format!("{} ({})", d.name, d.detail) }).collect();
        let metrics: Vec<String> = s.metrics.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let mut detail = s.detail.clone();
        if !down.is_empty() && !detail.contains("waiting") {
            detail = format!("{detail}{}down: {}", if detail.is_empty() { "" } else { " · " }, down.join(", "));
        }
        println!(
            "{:<14} {:<9} {:<9} {:>7}  {:<16} {}{}",
            s.name,
            s.kind,
            health_tag(s.health),
            if s.pid == 0 { "-".to_string() } else { s.pid.to_string() },
            if s.address.is_empty() { "-" } else { &s.address },
            detail,
            if metrics.is_empty() { String::new() } else { format!("  [{}]", metrics.join(" ")) }
        );
    }
}

fn resolve_writer(requested: &str, docker_start: bool) -> bool {
    let url = env("OME_LLM").unwrap_or_else(|| "http://127.0.0.1:11434".into());
    let model = env("OME_LLM_MODEL").unwrap_or_else(|| "qwen2.5:7b".into());
    match requested {
        "templates" => false,
        "llm" => true,
        _ => {
            if ome_llm::probe(&url, &model).unwrap_or(false) {
                return true;
            }
            let docker = Command::new("docker").arg("info").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
            if docker_start && docker && std::path::Path::new("scripts/llm.sh").exists() {
                println!("starting Ollama for the newsroom and the world's event ideas (writer=templates skips this)");
                let _ = Command::new("scripts/llm.sh").arg("up").status();
                return ome_llm::probe(&url, &model).unwrap_or(false);
            }
            false
        }
    }
}

fn stop_from_pid_files(data: &std::path::Path) -> usize {
    let mut stopped = 0;
    if let Ok(entries) = std::fs::read_dir(data) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "pid")
                && let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                if signal(pid, "TERM") {
                    stopped += 1;
                }
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    stopped
}

fn down(topology: &Topology, secrets: &Secrets, data: &std::path::Path) {
    match control(&access(topology, secrets), "stop", "") {
        Ok(text) => println!("{text}"),
        Err(_) => {
            let n = stop_from_pid_files(data);
            println!("no supervisor answered; stopped {n} process(es) from pid files");
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while running(topology, secrets) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::fs::remove_file(data.join("omectl.pid"));
}

fn up(cli: &Cli, topology: &Topology, secrets: &Secrets, flags: &[String]) -> Result<(), String> {
    let has = |f: &str| flags.iter().any(|a| a == f);
    let record = has("--record") || topology.setting("record", "0") == "1" || env("OME_RECORD").as_deref() == Some("1");
    let requested = flags
        .iter()
        .position(|a| a == "--writer")
        .and_then(|i| flags.get(i + 1).cloned())
        .or_else(|| env("OME_WRITER"))
        .unwrap_or_else(|| topology.setting("writer", "auto"));
    let base = settings(topology, record, false);
    if has("--fresh") {
        if running(topology, secrets) {
            down(topology, secrets, &base.data);
        } else {
            stop_from_pid_files(&base.data);
        }
        if let Ok(entries) = std::fs::read_dir(&base.data) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "journal") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        println!("fresh start: journals removed from {}", base.data.display());
    }
    if running(topology, secrets) {
        println!("the market is already running under omectl:");
        print_status(&query(&access(topology, secrets), "")?);
        return Ok(());
    }
    if !has("--no-build") {
        println!("building (cargo build --release --workspace)");
        let ok = Command::new("cargo").args(["build", "--release", "--workspace", "--quiet"]).status().is_ok_and(|s| s.success());
        if !ok {
            return Err("build failed".into());
        }
    }
    let llm = resolve_writer(&requested, true);
    let settings = settings(topology, record, llm);
    println!("preflight:");
    let checks = doctor::run(topology, &settings, secrets, &requested, false);
    if !print_checks(&checks) {
        return Err("preflight failed; fix the FAIL lines above (omectl doctor re-checks)".into());
    }
    std::fs::create_dir_all(&settings.data).map_err(|e| e.to_string())?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(settings.data.join("omectl.log"))
        .map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut command = Command::new(exe);
    command
        .arg("--topology")
        .arg(&cli.topology)
        .arg("serve")
        .args(if record { vec!["--record"] } else { vec![] })
        .args(["--writer", if llm { "llm" } else { "templates" }])
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().map_err(|e| format!("cannot start the supervisor: {e}"))?;
    println!("supervisor started (pid {}), log: {}", child.id(), settings.data.join("omectl.log").display());
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut last = String::new();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let Ok(list) = query(&access(topology, secrets), "") else {
            if Instant::now() > deadline {
                return Err("the supervisor never answered; see the log".into());
            }
            continue;
        };
        let pending: Vec<String> = list
            .iter()
            .filter(|s| !matches!(s.health, Health::Ready | Health::Degraded))
            .map(|s| format!("{} ({})", s.name, if s.detail.is_empty() { health_tag(s.health).to_string() } else { s.detail.clone() }))
            .collect();
        let line = if pending.is_empty() { String::new() } else { format!("waiting for {}", pending.join(", ")) };
        if line != last && !line.is_empty() {
            println!("  {line}");
            last = line;
        }
        if pending.is_empty() {
            println!();
            print_status(&list);
            break;
        }
        if Instant::now() > deadline {
            println!();
            print_status(&list);
            println!("\nnot every service is ready after 3 minutes; `omectl status` and `omectl logs <service>` show why");
            break;
        }
    }
    let bin = settings.bin.display();
    println!(
        "\nmarket is live (writer: {}{})",
        if llm { "local LLM" } else { "templates" },
        if record { ", recording into PostgreSQL and Redis" } else { "" }
    );
    for (name, addr, kind) in daemon::summary(topology, record) {
        println!("  {:<13} {:<16} {}", name, addr, kind.label());
    }
    println!(
        "examples:\n  {bin}/ome broker --addr 127.0.0.1:7101 --user alice --secret alice-pw balance\n  {bin}/ome sim world | status | event lawsuit ACME | news NOVA -8\n  {bin}/ome exchange members | applications\n  omectl status | logs <service> | restart <service> | down"
    );
    Ok(())
}

fn main() {
    let cli = match parse() {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let result = std::fs::read_to_string(&cli.topology)
        .map_err(|e| format!("cannot read {}: {e}", cli.topology.display()))
        .and_then(|text| Topology::parse(&text).map_err(|e| format!("{}: {e}", cli.topology.display())))
        .and_then(|mut topology| {
            for (var, key) in [("OME_HOST", "host"), ("OME_DATA", "data"), ("OME_REGIME", "regime"), ("OME_TRADING_DAY", "trading-day")] {
                if let Some(value) = env(var) {
                    topology.settings.insert(key.into(), value.replace(' ', ","));
                }
            }
            let secrets = Secrets::from_env();
            let args: Vec<&str> = cli.command.iter().map(String::as_str).collect();
            match args.as_slice() {
                ["up", ..] => up(&cli, &topology, &secrets, &cli.command[1..]),
                ["down"] => {
                    down(&topology, &secrets, &settings(&topology, false, false).data);
                    Ok(())
                }
                ["status"] => {
                    let list = query(&access(&topology, &secrets), "").map_err(|e| format!("the market is not running under omectl ({e})"))?;
                    print_status(&list);
                    Ok(())
                }
                ["logs", name, rest @ ..] => {
                    let path = settings(&topology, false, false).data.join(format!("{name}.log"));
                    let mut tail = Command::new("tail");
                    tail.args(["-n", "100"]);
                    if rest.contains(&"-f") {
                        tail.arg("-f");
                    }
                    tail.arg(&path).status().map(|_| ()).map_err(|e| e.to_string())
                }
                ["restart", name] => control(&access(&topology, &secrets), "restart", name).map(|t| println!("{t}")),
                ["doctor"] => {
                    let requested = env("OME_WRITER").unwrap_or_else(|| topology.setting("writer", "auto"));
                    let llm = resolve_writer(&requested, false);
                    let record = topology.setting("record", "0") == "1" || env("OME_RECORD").as_deref() == Some("1");
                    let settings = settings(&topology, record, llm);
                    let ok = print_checks(&doctor::run(&topology, &settings, &secrets, &requested, running(&topology, &secrets)));
                    if ok { Ok(()) } else { Err("preflight failed".into()) }
                }
                ["admit"] => {
                    let lines = admit::Admissions::new().pass(&topology, &secrets);
                    if lines.is_empty() {
                        println!("no pending applications from declared participants");
                    }
                    for line in lines {
                        println!("{line}");
                    }
                    Ok(())
                }
                ["serve", rest @ ..] => {
                    let record = rest.contains(&"--record");
                    let llm = rest.windows(2).any(|w| w == ["--writer", "llm"]);
                    let settings = settings(&topology, record, llm);
                    std::fs::create_dir_all(&settings.data).map_err(|e| e.to_string())?;
                    let _ = std::fs::write(settings.data.join("omectl.pid"), std::process::id().to_string());
                    let backend = Box::new(LocalBackend::new(&settings.data));
                    let result = daemon::Daemon::new(topology.clone(), settings.clone(), secrets, backend).run();
                    let _ = std::fs::remove_file(settings.data.join("omectl.pid"));
                    result
                }
                _ => Err(USAGE.into()),
            }
        });
    if let Err(e) = result {
        eprintln!("omectl: {e}");
        std::process::exit(1);
    }
}
