use ome_newsroom::company::parse_companies;
use ome_newsroom::{NewsConfig, start};

const USAGE: &str = "usage: ome-newsroom [--listen ADDR] [--journal PATH] [--companies FILE]
                    [--ops-name NAME] [--ops-secret SECRET]
                    [--exchange ADDR] [--exchange-secret SECRET] [--no-exchange]
                    [--simctl ADDR] [--sim-secret SECRET] [--no-simctl]
                    [--quarter-days N] [--news-per-minute RATE] [--seed N]";

fn parse_args() -> Result<NewsConfig, String> {
    let env = |name: &str, fallback: &str| std::env::var(name).unwrap_or_else(|_| fallback.into());
    let mut companies_path = "config/companies.conf".to_string();
    let mut exchange = Some((
        env("OME_EXCHANGE", "127.0.0.1:7000"),
        "ops".to_string(),
        env("OME_EXCHANGE_SECRET", "ops"),
    ));
    let mut simctl = Some((
        env("OME_SIMCTL", "127.0.0.1:7400"),
        env("OME_SIM_SECRET", "sim"),
    ));
    let mut config = NewsConfig {
        listen: "127.0.0.1:7500".into(),
        journal: None,
        companies: Vec::new(),
        ops_name: "ops".into(),
        ops_secret: env("OME_NEWS_SECRET", "ops"),
        exchange: None,
        simctl: None,
        quarter_days: 5,
        news_per_minute: 3.0,
        seed: None,
    };
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
            "--simctl" => {
                if let Some(s) = simctl.as_mut() {
                    s.0 = value()?;
                }
            }
            "--sim-secret" => {
                if let Some(s) = simctl.as_mut() {
                    s.1 = value()?;
                }
            }
            "--no-simctl" => simctl = None,
            "--quarter-days" => {
                config.quarter_days = value()?
                    .parse()
                    .map_err(|_| "--quarter-days needs a number")?
            }
            "--news-per-minute" => {
                config.news_per_minute = value()?
                    .parse()
                    .map_err(|_| "--news-per-minute needs a number")?
            }
            "--seed" => config.seed = Some(value()?.parse().map_err(|_| "--seed needs a number")?),
            _ => return Err(USAGE.into()),
        }
    }
    let text = std::fs::read_to_string(&companies_path)
        .map_err(|e| format!("cannot read {companies_path}: {e}"))?;
    config.companies = parse_companies(&text).map_err(|e| format!("{companies_path}: {e}"))?;
    config.exchange = exchange;
    config.simctl = simctl;
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
