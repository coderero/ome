use ome_agents::control::{AgentStats, Followup, GroupControl, Shared};
use ome_agents::provision::{fund, provision};
use ome_agents::roster::{AgentSpec, parse};
use ome_agents::runner::{AgentContext, supervise};
use ome_client::Client;
use ome_protocol::{ServerMessage, SimMessage, SimRole};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: ome-agents --roster FILE --broker NAME=ADDR [--broker NAME=ADDR ...]
                  [--admin-secret S] [--secret S] [--scale F] [--only GROUP[,GROUP]] [--no-provision]
                  [--simctl ADDR] [--sim-secret S]

The roster lists groups of agents: `group <name> count=N broker=B role=R strategy=S [symbols=A,B|*]
[cash=AMOUNT] [seed=AMOUNT] [param=value ...]`. Agents only connect to their broker. With --simctl
the runner takes orders from the simulation control plane: pause, scale, parameters, funding,
fair values and scenarios.";

#[derive(Clone)]
struct Options {
    roster: String,
    brokers: BTreeMap<String, String>,
    admin_secret: String,
    secret: String,
    scale: f64,
    only: Option<Vec<String>>,
    provision: bool,
    simctl: Option<String>,
    sim_secret: String,
}

fn parse_args() -> Result<Options, String> {
    let mut options = Options {
        roster: String::new(),
        brokers: BTreeMap::new(),
        admin_secret: std::env::var("OME_BROKER_SECRET").unwrap_or_else(|_| "admin".into()),
        secret: "agent-pw".into(),
        scale: 1.0,
        only: None,
        provision: true,
        simctl: None,
        sim_secret: std::env::var("OME_SIM_RUNNER_SECRET").unwrap_or_else(|_| "runner".into()),
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--roster" => options.roster = value()?,
            "--broker" => {
                let text = value()?;
                let (name, addr) = text.split_once('=').ok_or("--broker needs NAME=ADDR")?;
                options.brokers.insert(name.into(), addr.into());
            }
            "--admin-secret" => options.admin_secret = value()?,
            "--secret" => options.secret = value()?,
            "--scale" => options.scale = value()?.parse().map_err(|_| "--scale needs a number")?,
            "--only" => options.only = Some(value()?.split(',').map(String::from).collect()),
            "--no-provision" => options.provision = false,
            "--simctl" => options.simctl = Some(value()?),
            "--sim-secret" => options.sim_secret = value()?,
            _ => return Err(USAGE.into()),
        }
    }
    if options.roster.is_empty() {
        return Err(USAGE.into());
    }
    Ok(options)
}

struct Running {
    index: usize,
    name: String,
    stop: Arc<AtomicBool>,
    stats: Arc<AgentStats>,
    handle: JoinHandle<()>,
}

type Roster = Arc<Mutex<BTreeMap<String, Vec<Running>>>>;

fn spawn(
    spec: AgentSpec,
    index: usize,
    options: &Options,
    shared: &Arc<Shared>,
) -> Option<Running> {
    let addr = options.brokers.get(&spec.broker)?.clone();
    let ctx = AgentContext {
        group: shared.groups.get(&spec.group)?.clone(),
        shared: shared.clone(),
        stats: Arc::new(AgentStats::default()),
        stop: Arc::new(AtomicBool::new(false)),
    };
    let (name, stop, stats) = (spec.name.clone(), ctx.stop.clone(), ctx.stats.clone());
    let secret = options.secret.clone();
    let handle = thread::spawn(move || supervise(spec, addr, secret, ctx));
    Some(Running {
        index,
        name,
        stop,
        stats,
        handle,
    })
}

fn manage(
    templates: BTreeMap<String, AgentSpec>,
    options: Options,
    shared: Arc<Shared>,
    roster: Roster,
) {
    loop {
        thread::sleep(Duration::from_secs(1));
        for (group, template) in &templates {
            let Some(control) = shared.groups.get(group) else {
                continue;
            };
            let target = control.target.load(Ordering::Relaxed) as usize;
            let mut all = roster.lock().expect("roster lock");
            let agents = all.entry(group.clone()).or_default();
            agents.retain(|a| !a.handle.is_finished());
            while agents.len() > target {
                if let Some(agent) = agents.pop() {
                    agent.stop.store(true, Ordering::Relaxed);
                    eprintln!("scaling {group} down: stopping {}", agent.name);
                }
            }
            if agents.len() < target {
                let index = agents.iter().map(|a| a.index).max().unwrap_or(0) + 1;
                let spec = AgentSpec {
                    name: format!("{group}-{index}"),
                    ..template.clone()
                };
                drop(all);
                if options.provision
                    && let Err(e) = provision(
                        std::slice::from_ref(&spec),
                        &options.brokers,
                        &options.admin_secret,
                        &options.secret,
                    )
                {
                    eprintln!("cannot open {}: {e}", spec.name);
                    continue;
                }
                eprintln!("scaling {group} up: starting {}", spec.name);
                if let Some(agent) = spawn(spec, index, &options, &shared) {
                    roster
                        .lock()
                        .expect("roster lock")
                        .entry(group.clone())
                        .or_default()
                        .push(agent);
                }
            }
        }
    }
}

fn follow_simctl(addr: String, options: Options, shared: Arc<Shared>, roster: Roster) {
    loop {
        match Client::connect_sim(&addr, SimRole::Runner, "runner", &options.sim_secret) {
            Ok(mut client) => {
                eprintln!("following the simulation control plane at {addr}");
                let mut next_report = Instant::now();
                loop {
                    if Instant::now() >= next_report {
                        next_report = Instant::now() + Duration::from_secs(2);
                        let stats: BTreeMap<String, Vec<Arc<AgentStats>>> = roster
                            .lock()
                            .expect("roster lock")
                            .iter()
                            .map(|(g, agents)| {
                                (g.clone(), agents.iter().map(|a| a.stats.clone()).collect())
                            })
                            .collect();
                        if client
                            .send_sim(&SimMessage::Groups(shared.report(&stats)))
                            .is_err()
                        {
                            break;
                        }
                    }
                    match client.recv_timeout(Duration::from_millis(300)) {
                        Ok(Some(ServerMessage::SimEvent(command))) => {
                            eprintln!("simctl: {}", command.describe());
                            if let Some(Followup::Fund { group, amount }) =
                                shared.apply(&command, Instant::now())
                            {
                                fund_group(&group, amount, &options, &shared, &roster);
                            }
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                eprintln!("lost the simulation control plane; reconnecting");
            }
            Err(e) => eprintln!("cannot reach ome-simctl at {addr}: {e}"),
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn fund_group(group: &str, amount: u64, options: &Options, shared: &Arc<Shared>, roster: &Roster) {
    let names: BTreeMap<String, Vec<String>> = {
        let all = roster.lock().expect("roster lock");
        let mut by_broker: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, agents) in all.iter().filter(|(g, _)| group == "*" || *g == group) {
            if let Some(control) = shared.groups.get(name) {
                by_broker
                    .entry(control.broker.clone())
                    .or_default()
                    .extend(agents.iter().map(|a| a.name.clone()));
            }
        }
        by_broker
    };
    for (broker, accounts) in names {
        let Some(addr) = options.brokers.get(&broker) else {
            continue;
        };
        match fund(&accounts, addr, &options.admin_secret, amount) {
            Ok(n) => eprintln!("funded {n} account(s) at {broker}"),
            Err(e) => eprintln!("funding at {broker} failed: {e}"),
        }
    }
}

fn main() {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let parsed = std::fs::read_to_string(&options.roster)
        .map_err(|e| format!("cannot read {}: {e}", options.roster))
        .and_then(|text| parse(&text, options.scale));
    let mut agents = match parsed {
        Ok(agents) => agents,
        Err(e) => {
            eprintln!("ome-agents: {e}");
            std::process::exit(2);
        }
    };
    if let Some(only) = &options.only {
        agents.retain(|a| only.contains(&a.group));
    }
    if options.provision {
        match provision(
            &agents,
            &options.brokers,
            &options.admin_secret,
            &options.secret,
        ) {
            Ok(created) => eprintln!("provisioned {created} new account(s)"),
            Err(e) => {
                eprintln!("ome-agents: {e}");
                std::process::exit(1);
            }
        }
    }
    let mut templates: BTreeMap<String, AgentSpec> = BTreeMap::new();
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for agent in &agents {
        templates
            .entry(agent.group.clone())
            .or_insert_with(|| agent.clone());
        *counts.entry(agent.group.clone()).or_default() += 1;
    }
    let groups = templates
        .iter()
        .map(|(name, spec)| {
            (
                name.clone(),
                Arc::new(GroupControl::new(
                    name,
                    &spec.strategy,
                    &spec.broker,
                    counts[name],
                    spec.params.clone(),
                )),
            )
        })
        .collect();
    let shared = Arc::new(Shared::new(groups));
    let roster: Roster = Arc::new(Mutex::new(BTreeMap::new()));
    eprintln!(
        "starting {} agents in {} groups",
        agents.len(),
        templates.len()
    );
    for agent in agents {
        let index = agent
            .name
            .rsplit('-')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(1);
        let group = agent.group.clone();
        if let Some(running) = spawn(agent, index, &options, &shared) {
            roster
                .lock()
                .expect("roster lock")
                .entry(group)
                .or_default()
                .push(running);
        }
    }
    if let Some(addr) = options.simctl.clone() {
        let (options, shared, roster) = (options.clone(), shared.clone(), roster.clone());
        thread::spawn(move || follow_simctl(addr, options, shared, roster));
    }
    manage(templates, options, shared, roster);
}
