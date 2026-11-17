use ome_agents::control::{AgentStats, Shared};
use ome_agents::roster::{AgentSpec, parse};
use ome_agents::runner::{AgentContext, supervise};
use ome_client::Client;
use ome_net::registry::{RegistryAccess, Report, Reporter};
use ome_protocol::{Assignment, ServerMessage, ServiceInfo, SimMessage, SimRole};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: ome-agents [--simctl ADDR] [--sim-secret S] [--name NAME] [--registry ADDR]

An agent runner. The simulation controller (ome-simctl) owns the roster: it opens every agent's
account at its broker and assigns agents to the connected runners, which only run the strategies.
Several runners can share the load.";

struct Options {
    simctl: String,
    sim_secret: String,
    name: String,
    registry: Option<RegistryAccess>,
}

fn parse_args() -> Result<Options, String> {
    let mut options = Options {
        simctl: std::env::var("OME_SIMCTL").unwrap_or_else(|_| "127.0.0.1:7400".into()),
        sim_secret: std::env::var("OME_SIM_RUNNER_SECRET").unwrap_or_else(|_| "runner".into()),
        name: "agents".into(),
        registry: RegistryAccess::from_env(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--simctl" => options.simctl = value()?,
            "--sim-secret" => options.sim_secret = value()?,
            "--name" => options.name = value()?,
            "--registry" => {
                let addr = value()?;
                let secret = options.registry.as_ref().map(|r| r.secret.clone()).unwrap_or_else(|| "cluster".into());
                options.registry = Some(RegistryAccess { addr, secret });
            }
            _ => return Err(USAGE.into()),
        }
    }
    Ok(options)
}

struct Running {
    group: String,
    stop: Arc<AtomicBool>,
    stats: Arc<AgentStats>,
    handle: JoinHandle<()>,
}

type Agents = Arc<Mutex<BTreeMap<String, Running>>>;

fn spec_of(assignment: &Assignment) -> Result<AgentSpec, String> {
    let spec = parse(&assignment.spec, 1.0)?
        .into_iter()
        .next()
        .ok_or("the roster line yields no agent")?;
    Ok(AgentSpec { name: assignment.name.clone(), ..spec })
}

fn start(assignment: Assignment, shared: &Arc<Shared>, agents: &Agents) {
    if agents.lock().expect("agents lock").contains_key(&assignment.name) {
        return;
    }
    let spec = match spec_of(&assignment) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!("cannot run {}: {e}", assignment.name);
            return;
        }
    };
    let group = shared.ensure(&spec.group, &spec.strategy, &spec.broker, spec.params.clone());
    let ctx = AgentContext {
        group: group.clone(),
        shared: shared.clone(),
        stats: Arc::new(AgentStats::default()),
        stop: Arc::new(AtomicBool::new(false)),
    };
    let (stop, stats) = (ctx.stop.clone(), ctx.stats.clone());
    let name = spec.name.clone();
    let group_name = spec.group.clone();
    let handle = thread::spawn(move || supervise(spec, assignment.addr, assignment.secret, ctx));
    let mut all = agents.lock().expect("agents lock");
    all.insert(name, Running { group: group_name.clone(), stop, stats, handle });
    let count = all.values().filter(|a| a.group == group_name).count() as u32;
    group.target.store(count, Ordering::Relaxed);
}

fn retire(name: &str, shared: &Arc<Shared>, agents: &Agents) {
    let mut all = agents.lock().expect("agents lock");
    if let Some(agent) = all.remove(name) {
        agent.stop.store(true, Ordering::Relaxed);
        eprintln!("retiring {name}");
        let count = all.values().filter(|a| a.group == agent.group).count() as u32;
        if let Some(group) = shared.group(&agent.group) {
            group.target.store(count, Ordering::Relaxed);
        }
    }
}

fn stop_all(agents: &Agents) {
    let mut all = agents.lock().expect("agents lock");
    for (_, agent) in std::mem::take(&mut *all) {
        agent.stop.store(true, Ordering::Relaxed);
    }
}

fn report(shared: &Shared, agents: &Agents) -> Vec<ome_protocol::GroupInfo> {
    let mut stats: BTreeMap<String, Vec<Arc<AgentStats>>> = BTreeMap::new();
    let mut all = agents.lock().expect("agents lock");
    all.retain(|name, a| {
        let alive = !a.handle.is_finished();
        if !alive {
            eprintln!("{name} stopped");
        }
        alive
    });
    for agent in all.values() {
        stats.entry(agent.group.clone()).or_default().push(agent.stats.clone());
    }
    shared.report(&stats)
}

fn main() {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let shared = Arc::new(Shared::new(BTreeMap::new()));
    let agents: Agents = Arc::new(Mutex::new(BTreeMap::new()));
    let reporter = Reporter::start(options.registry.clone(), ServiceInfo::new(&options.name, "agents", ""));
    let health = |connected: bool, problem: &str, agents: &Agents| {
        let mut r = Report::default();
        r.dep("simctl", connected, problem);
        r.metric("agents", agents.lock().expect("agents lock").len() as i64);
        r.ready_unless(&["simctl"])
    };
    loop {
        match Client::connect_sim(&options.simctl, SimRole::Runner, "runner", &options.sim_secret) {
            Ok(mut client) => {
                eprintln!("following the simulation controller at {}", options.simctl);
                let mut next_report = Instant::now();
                loop {
                    if Instant::now() >= next_report {
                        next_report = Instant::now() + Duration::from_secs(2);
                        reporter.report(health(true, "", &agents));
                        if client.send_sim(&SimMessage::Groups(report(&shared, &agents))).is_err() {
                            break;
                        }
                    }
                    match client.recv_timeout(Duration::from_millis(300)) {
                        Ok(Some(ServerMessage::SimEvent(command))) => {
                            eprintln!("simctl: {}", command.describe());
                            shared.apply(&command, Instant::now());
                        }
                        Ok(Some(ServerMessage::SimAssign(assignment))) => start(assignment, &shared, &agents),
                        Ok(Some(ServerMessage::SimRetire { name })) => retire(&name, &shared, &agents),
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                eprintln!("lost the simulation controller; stopping agents until it reassigns them");
                stop_all(&agents);
            }
            Err(e) => eprintln!("cannot reach ome-simctl at {}: {e}", options.simctl),
        }
        reporter.report(health(false, "unreachable", &agents));
        thread::sleep(Duration::from_secs(2));
    }
}
