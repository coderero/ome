use ome_agents::provision::provision;
use ome_agents::roster::parse;
use ome_agents::runner::supervise;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;

const USAGE: &str = "usage: ome-agents --roster FILE --broker NAME=ADDR [--broker NAME=ADDR ...]
                  [--admin-secret S] [--secret S] [--scale F] [--only GROUP[,GROUP]] [--no-provision]

The roster lists groups of agents: `group <name> count=N broker=B role=R strategy=S [symbols=A,B|*]
[cash=AMOUNT] [seed=AMOUNT] [param=value ...]`. Agents only connect to their broker.";

struct Options {
    roster: String,
    brokers: BTreeMap<String, String>,
    admin_secret: String,
    secret: String,
    scale: f64,
    only: Option<Vec<String>>,
    provision: bool,
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
            _ => return Err(USAGE.into()),
        }
    }
    if options.roster.is_empty() {
        return Err(USAGE.into());
    }
    Ok(options)
}

fn main() {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let result = std::fs::read_to_string(&options.roster)
        .map_err(|e| format!("cannot read {}: {e}", options.roster))
        .and_then(|text| parse(&text, options.scale));
    let mut agents = match result {
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
    eprintln!("starting {} agents", agents.len());
    let stop = Arc::new(AtomicBool::new(false));
    let handles: Vec<_> = agents
        .into_iter()
        .filter_map(|agent| {
            let addr = options.brokers.get(&agent.broker)?.clone();
            let secret = options.secret.clone();
            let stop = stop.clone();
            Some(thread::spawn(move || supervise(agent, addr, secret, stop)))
        })
        .collect();
    for handle in handles {
        let _ = handle.join();
    }
}
