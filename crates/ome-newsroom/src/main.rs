use ome_llm::LlmConfig;
use ome_net::registry::RegistryAccess;
use ome_newsroom::{NewsConfig, start};

const USAGE: &str = "usage: ome-newsroom [--listen ADDR] [--journal PATH] [--ops-name NAME] [--ops-secret SECRET]
                    [--source NAME=SECRET ...] [--seed N] [--registry ADDR] [--registry-secret S]
                    [--llm http://HOST:PORT] [--llm-model MODEL] [--llm-timeout-ms MS]
A news vendor: contributors (the simulated world's wire) send events, the desk writes the stories
(templates, or the local LLM with --llm) and publishes them to subscribers.";

fn parse_args() -> Result<NewsConfig, String> {
    let env = |name: &str, fallback: &str| std::env::var(name).unwrap_or_else(|_| fallback.into());
    let mut config = NewsConfig {
        listen: "127.0.0.1:7500".into(),
        journal: None,
        ops_name: "ops".into(),
        ops_secret: env("OME_NEWS_SECRET", "ops"),
        sources: Vec::new(),
        seed: None,
        llm: None,
        registry: RegistryAccess::from_env(),
    };
    let mut llm_url: Option<String> = None;
    let mut llm_model = env("OME_LLM_MODEL", "qwen2.5:7b");
    let mut llm_timeout_ms: u64 = 8_000;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => config.listen = value()?,
            "--journal" => config.journal = Some(value()?.into()),
            "--ops-name" => config.ops_name = value()?,
            "--ops-secret" => config.ops_secret = value()?,
            "--source" => {
                let text = value()?;
                let (name, secret) = text.split_once('=').ok_or("--source needs NAME=SECRET")?;
                config.sources.push((name.to_string(), secret.to_string()));
            }
            "--registry" => {
                let addr = value()?;
                let secret = config.registry.as_ref().map(|r| r.secret.clone()).unwrap_or_else(|| "cluster".into());
                config.registry = Some(RegistryAccess { addr, secret });
            }
            "--registry-secret" => {
                let secret = value()?;
                if let Some(r) = config.registry.as_mut() {
                    r.secret = secret;
                }
            }
            "--llm" => llm_url = Some(value()?),
            "--llm-model" => llm_model = value()?,
            "--llm-timeout-ms" => {
                llm_timeout_ms = value()?.parse().map_err(|_| "--llm-timeout-ms needs a number")?
            }
            "--seed" => config.seed = Some(value()?.parse().map_err(|_| "--seed needs a number")?),
            _ => return Err(USAGE.into()),
        }
    }
    if config.sources.is_empty() {
        config.sources.push(("world".into(), env("OME_NEWS_SOURCE_SECRET", "wire")));
    }
    config.llm = llm_url.map(|url| LlmConfig {
        url,
        model: llm_model,
        timeout: std::time::Duration::from_millis(llm_timeout_ms),
    });
    Ok(config)
}

fn main() {
    let result = parse_args().and_then(start).and_then(|(_, handle)| {
        handle.join().map_err(|_| "engine thread panicked".to_string())?
    });
    if let Err(e) = result {
        eprintln!("ome-newsroom: {e}");
        std::process::exit(1);
    }
}
