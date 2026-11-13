use ome_broker::profile::{ProfileKind, Regime};
use ome_broker::{BrokerConfig, start};
use ome_client::DEFAULT_EXCHANGE_ADDR;

const USAGE: &str = "usage: ome-broker --profile retail|prime|dma|lp --member NAME --member-secret SECRET
                  [--regime us|india] [--listen ADDR] [--exchange ADDR] [--clearing ADDR] [--journal PATH] [--admin-name NAME] [--admin-secret SECRET]";

fn parse_args() -> Result<BrokerConfig, String> {
    let mut config = BrokerConfig {
        listen: "127.0.0.1:7101".into(),
        journal: None,
        profile: ProfileKind::Retail,
        admin_name: "admin".into(),
        admin_secret: std::env::var("OME_BROKER_SECRET").unwrap_or_else(|_| "admin".into()),
        exchange_addr: std::env::var("OME_EXCHANGE")
            .unwrap_or_else(|_| DEFAULT_EXCHANGE_ADDR.into()),
        member_name: String::new(),
        member_secret: String::new(),
        clearing_addr: std::env::var("OME_CLEARING").ok(),
        regime: Regime::Us,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = || iter.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--profile" => {
                config.profile = ProfileKind::parse(&value()?)
                    .ok_or("profile must be retail, prime, dma or lp")?
            }
            "--regime" => {
                config.regime = Regime::parse(&value()?).ok_or("regime must be us or india")?
            }
            "--listen" => config.listen = value()?,
            "--exchange" => config.exchange_addr = value()?,
            "--journal" => config.journal = Some(value()?.into()),
            "--admin-name" => config.admin_name = value()?,
            "--admin-secret" => config.admin_secret = value()?,
            "--member" => config.member_name = value()?,
            "--member-secret" => config.member_secret = value()?,
            "--clearing" => config.clearing_addr = Some(value()?),
            _ => return Err(USAGE.into()),
        }
    }
    if config.member_name.is_empty() || config.member_secret.is_empty() {
        return Err(USAGE.into());
    }
    if config.journal.is_none() {
        config.journal = Some(format!("data/{}.journal", config.member_name).into());
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
        eprintln!("ome-broker: {e}");
        std::process::exit(1);
    }
}
