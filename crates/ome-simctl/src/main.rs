use ome_llm::LlmConfig;
use ome_net::registry::{RegistryAccess, RegistryDirectory, StaticDirectory};
use ome_simctl::fleet::parse_roster;
use ome_simctl::world::company::parse_world;
use ome_simctl::{SimConfig, start};
use std::sync::Arc;
use std::time::Duration;

const USAGE: &str = "usage: ome-simctl [--listen ADDR] [--journal PATH] [--secret OPERATOR_SECRET] [--runner-secret SECRET]
                  [--exchange ADDR] [--exchange-secret SECRET]
                  [--world FILE] [--roster FILE] [--scale F] [--agent-secret S] [--broker-secret S] [--no-provision]
                  [--broker NAME=ADDR ...] [--registry ADDR] [--registry-secret S]
                  [--newsroom ADDR|NAME] [--source NAME] [--source-secret S]
                  [--quarter-days N] [--news-per-day N] [--seed N]
                  [--llm http://HOST:PORT] [--llm-model MODEL] [--llm-timeout-ms MS]
operators log in as 'operator', agent runners as 'runner'. The world file lists companies (with
underwriter= and dmm=) and demo accounts; the roster lists the agent groups that simctl opens accounts
for and hands to runners.";

fn parse_args() -> Result<SimConfig, String> {
    let env = |name: &str, fallback: &str| std::env::var(name).unwrap_or_else(|_| fallback.into());
    let mut config = SimConfig::new("127.0.0.1:7400", &env("OME_EXCHANGE", "127.0.0.1:7000"), Arc::new(StaticDirectory::default()));
    config.operator.1 = env("OME_SIM_SECRET", "sim");
    config.runner.1 = env("OME_SIM_RUNNER_SECRET", "runner");
    config.exchange_secret = env("OME_EXCHANGE_SECRET", "ops");
    config.calendar = true;
    config.news_per_day = 1.0;
    let mut world_path: Option<String> = None;
    let mut roster_path: Option<String> = None;
    let mut scale = 1.0;
    let mut broker_secret = Some(env("OME_BROKER_SECRET", "admin"));
    let mut brokers = StaticDirectory::default();
    let mut registry = RegistryAccess::from_env();
    let mut newsroom: Option<String> = None;
    let mut source = ("world".to_string(), env("OME_NEWS_SOURCE_SECRET", "wire"));
    let mut llm_url: Option<String> = None;
    let mut llm_model = env("OME_LLM_MODEL", "qwen2.5:7b");
    let mut llm_timeout_ms: u64 = 8_000;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        let number = |text: String, what: &str| text.parse::<f64>().map_err(|_| format!("{what} needs a number"));
        match flag.as_str() {
            "--listen" => config.listen = value()?,
            "--journal" => config.journal = Some(value()?.into()),
            "--secret" => config.operator.1 = value()?,
            "--runner-secret" => config.runner.1 = value()?,
            "--exchange" => config.exchange_addr = value()?,
            "--exchange-secret" => config.exchange_secret = value()?,
            "--world" => world_path = Some(value()?),
            "--roster" => roster_path = Some(value()?),
            "--scale" => scale = number(value()?, "--scale")?,
            "--agent-secret" => config.agent_secret = value()?,
            "--broker-secret" => broker_secret = Some(value()?),
            "--no-provision" => broker_secret = None,
            "--broker" => {
                let text = value()?;
                let (name, addr) = text.split_once('=').ok_or("--broker needs NAME=ADDR")?;
                brokers.insert(name, "broker", addr);
            }
            "--registry" => {
                let addr = value()?;
                let secret = registry.as_ref().map(|r| r.secret.clone()).unwrap_or_else(|| "cluster".into());
                registry = Some(RegistryAccess { addr, secret });
            }
            "--registry-secret" => {
                let secret = value()?;
                if let Some(r) = registry.as_mut() {
                    r.secret = secret;
                }
            }
            "--newsroom" => newsroom = Some(value()?),
            "--source" => source.0 = value()?,
            "--source-secret" => source.1 = value()?,
            "--quarter-days" => config.quarter_days = number(value()?, "--quarter-days")? as u64,
            "--news-per-day" => config.news_per_day = number(value()?, "--news-per-day")?,
            "--seed" => config.seed = Some(number(value()?, "--seed")? as u64),
            "--llm" => llm_url = Some(value()?),
            "--llm-model" => llm_model = value()?,
            "--llm-timeout-ms" => llm_timeout_ms = number(value()?, "--llm-timeout-ms")? as u64,
            _ => return Err(USAGE.into()),
        }
    }
    if let Some(path) = world_path {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
        config.world = parse_world(&text).map_err(|e| format!("{path}: {e}"))?;
    }
    if let Some(path) = roster_path {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
        config.roster = parse_roster(&text, scale).map_err(|e| format!("{path}: {e}"))?;
    }
    config.provision = broker_secret.map(|secret| ("admin".to_string(), secret));
    config.newsroom = newsroom.map(|addr| (addr, source.0, source.1));
    config.llm = llm_url.map(|url| LlmConfig { url, model: llm_model, timeout: Duration::from_millis(llm_timeout_ms) });
    config.directory = match &registry {
        Some(access) => Arc::new(RegistryDirectory { access: access.clone(), fallback: brokers }),
        None => Arc::new(brokers),
    };
    config.registry = registry;
    Ok(config)
}

fn main() {
    let result = parse_args().and_then(start).and_then(|(_, handle)| {
        handle.join().map_err(|_| "engine thread panicked".to_string())?
    });
    if let Err(e) = result {
        eprintln!("ome-simctl: {e}");
        std::process::exit(1);
    }
}
