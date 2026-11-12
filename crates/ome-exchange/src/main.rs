use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start};

const USAGE: &str = "usage: ome-exchange [--listen ADDR] [--journal PATH] [--admin-name NAME] [--admin-secret SECRET]";

fn parse_args() -> Result<ServerConfig, String> {
    let mut config = ServerConfig {
        listen: std::env::var("OME_EXCHANGE_LISTEN").unwrap_or_else(|_| "127.0.0.1:7000".into()),
        journal: Some(
            std::env::var("OME_EXCHANGE_JOURNAL")
                .unwrap_or_else(|_| "data/exchange.journal".into())
                .into(),
        ),
        admin_name: "ops".into(),
        admin_secret: std::env::var("OME_EXCHANGE_SECRET").unwrap_or_else(|_| "ops".into()),
        exchange: ExchangeConfig::default(),
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = || iter.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => config.listen = value()?,
            "--journal" => config.journal = Some(value()?.into()),
            "--admin-name" => config.admin_name = value()?,
            "--admin-secret" => config.admin_secret = value()?,
            _ => return Err(USAGE.into()),
        }
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
        eprintln!("ome-exchange: {e}");
        std::process::exit(1);
    }
}
