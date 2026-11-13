use ome_clearing::clearing::ClearingConfig;
use ome_clearing::{ServiceConfig, start};
use ome_client::DEFAULT_EXCHANGE_ADDR;

const USAGE: &str =
    "usage: ome-clearing --member NAME --member-secret SECRET [--listen ADDR] [--exchange ADDR]
                    [--journal PATH] [--ops-name NAME] [--ops-secret SECRET]
                    [--cycle-ms MS] [--settlement-lag CYCLES] [--margin-bps BPS]";

fn parse_args() -> Result<ServiceConfig, String> {
    let mut config = ServiceConfig {
        listen: "127.0.0.1:7200".into(),
        journal: Some("data/clearing.journal".into()),
        ops_name: "ops".into(),
        ops_secret: std::env::var("OME_CLEARING_SECRET").unwrap_or_else(|_| "ops".into()),
        exchange_addr: std::env::var("OME_EXCHANGE")
            .unwrap_or_else(|_| DEFAULT_EXCHANGE_ADDR.into()),
        member_name: String::new(),
        member_secret: String::new(),
        clearing: ClearingConfig::default(),
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = || iter.next().ok_or(format!("{flag} needs a value"));
        let number = |text: String| {
            text.parse::<u64>()
                .map_err(|_| format!("{flag} needs a number"))
        };
        match flag.as_str() {
            "--listen" => config.listen = value()?,
            "--exchange" => config.exchange_addr = value()?,
            "--journal" => config.journal = Some(value()?.into()),
            "--ops-name" => config.ops_name = value()?,
            "--ops-secret" => config.ops_secret = value()?,
            "--member" => config.member_name = value()?,
            "--member-secret" => config.member_secret = value()?,
            "--cycle-ms" => config.clearing.cycle_ms = number(value()?)?.max(1_000),
            "--settlement-lag" => config.clearing.settlement_lag = number(value()?)?,
            "--margin-bps" => config.clearing.margin_bps = number(value()?)?.max(1),
            _ => return Err(USAGE.into()),
        }
    }
    if config.member_name.is_empty() || config.member_secret.is_empty() {
        return Err(USAGE.into());
    }
    Ok(config)
}

fn main() {
    let result = parse_args().and_then(start).and_then(|(_, handle)| {
        handle
            .join()
            .map_err(|_| "engine thread panicked".to_string())?
    });
    if let Err(e) = result {
        eprintln!("ome-clearing: {e}");
        std::process::exit(1);
    }
}
