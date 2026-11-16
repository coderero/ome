use ome_newsroom::company::parse_companies;
use ome_newsroom::{NewsConfig, start};

const USAGE: &str = "usage: ome-newsroom [--listen ADDR] [--journal PATH] [--companies FILE]
                    [--ops-name NAME] [--ops-secret SECRET]
                    [--exchange ADDR] [--exchange-secret SECRET] [--no-exchange]
                    [--quarter-days N] [--news-per-day STORIES_PER_COMPANY] [--seed N]
                    [--llm http://HOST:PORT] [--llm-model MODEL] [--llm-timeout-ms MS]";

fn parse_args() -> Result<NewsConfig, String> {
    let env = |name: &str, fallback: &str| std::env::var(name).unwrap_or_else(|_| fallback.into());
    let mut companies_path = "config/companies.conf".to_string();
    let mut exchange = Some((
        env("OME_EXCHANGE", "127.0.0.1:7000"),
        "ops".to_string(),
        env("OME_EXCHANGE_SECRET", "ops"),
    ));
    let mut config = NewsConfig {
        listen: "127.0.0.1:7500".into(),
        journal: None,
        companies: Vec::new(),
        ops_name: "ops".into(),
        ops_secret: env("OME_NEWS_SECRET", "ops"),
        exchange: None,
        quarter_days: 20,
        news_per_day: 1.0,
        seed: None,
        llm: None,
    };
    let mut llm_url: Option<String> = None;
    let mut llm_model = env("OME_LLM_MODEL", "qwen2.5:1.5b");
    let mut llm_timeout_ms: u64 = 8_000;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => config.listen = value()?,
            "--journal" => config.journal = Some(value()?.into()),
            "--companies" => companies_path = value()?,
            "--ops-name" => config.ops_name = value()?,
            "--ops-secret" => config.ops_secret = value()?,
            "--exchange" => {
                if let Some(e) = exchange.as_mut() {
                    e.0 = value()?;
                }
            }
            "--exchange-secret" => {
                if let Some(e) = exchange.as_mut() {
                    e.2 = value()?;
                }
            }
            "--no-exchange" => exchange = None,
            "--quarter-days" => {
                config.quarter_days = value()?
                    .parse()
                    .map_err(|_| "--quarter-days needs a number")?
            }
            "--news-per-day" => {
                config.news_per_day = value()?
                    .parse()
                    .map_err(|_| "--news-per-day needs a number")?
            }
            "--llm" => llm_url = Some(value()?),
            "--llm-model" => llm_model = value()?,
            "--llm-timeout-ms" => {
                llm_timeout_ms = value()?
                    .parse()
                    .map_err(|_| "--llm-timeout-ms needs a number")?
            }
            "--seed" => config.seed = Some(value()?.parse().map_err(|_| "--seed needs a number")?),
            _ => return Err(USAGE.into()),
        }
    }
    let text = std::fs::read_to_string(&companies_path)
        .map_err(|e| format!("cannot read {companies_path}: {e}"))?;
    config.companies = parse_companies(&text).map_err(|e| format!("{companies_path}: {e}"))?;
    config.llm = llm_url.map(|url| ome_newsroom::llm::LlmConfig {
        url,
        model: llm_model,
        timeout: std::time::Duration::from_millis(llm_timeout_ms),
    });
    config.exchange = exchange;
    Ok(config)
}

fn main() {
    let result = parse_args().and_then(start).and_then(|(_, handle)| {
        handle
            .join()
            .map_err(|_| "engine thread panicked".to_string())?
    });
    if let Err(e) = result {
        eprintln!("ome-newsroom: {e}");
        std::process::exit(1);
    }
}
