use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start};

const USAGE: &str = "usage: ome-exchange [--listen ADDR] [--journal PATH] [--admin-name NAME] [--admin-secret SECRET] [--require-prospectus] [--registry ADDR]\n                    [--schedule PRE,OPEN,CLOSING,CLOSED seconds | real | compressed | fast, used when the journal has no calendar yet]\n                    [--session-rate MSGS_PER_S (per member session, default 5000)] [--luld-bps BPS (0 = off)] [--luld-window-ms MS] [--luld-pause-ms MS]";

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
        schedule: None,
        registry: ome_net::registry::RegistryAccess::from_env(),
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = || iter.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => config.listen = value()?,
            "--journal" => config.journal = Some(value()?.into()),
            "--admin-name" => config.admin_name = value()?,
            "--admin-secret" => config.admin_secret = value()?,
            "--registry" => {
                let addr = value()?;
                let secret = config.registry.as_ref().map(|r| r.secret.clone()).unwrap_or_else(|| "cluster".into());
                config.registry = Some(ome_net::registry::RegistryAccess { addr, secret });
            }
            "--schedule" => {
                let text = value()?;
                let text = ome_protocol::preset(&text).map(|p| p.map(|s| s.to_string()).join(",")).unwrap_or(text);
                let parts: Vec<u64> = text
                    .split([',', ' '])
                    .filter(|p| !p.is_empty())
                    .map(|p| p.parse::<u64>().map_err(|_| "--schedule needs four numbers of seconds"))
                    .collect::<Result<_, _>>()?;
                let [pre, open, closing, closed] = parts[..] else {
                    return Err("--schedule needs four numbers of seconds".into());
                };
                config.schedule = (pre + open + closing + closed > 0).then_some(ome_exchange::session::Schedule {
                    pre_open_ms: pre * 1_000,
                    continuous_ms: open * 1_000,
                    closing_ms: closing * 1_000,
                    closed_ms: closed * 1_000,
                });
            }
            "--require-prospectus" => config.exchange.require_prospectus = true,
            "--session-rate" => config.exchange.session_rate = value()?.parse().map_err(|_| "--session-rate needs a number")?,
            "--luld-bps" => config.exchange.luld_bps = value()?.parse().map_err(|_| "--luld-bps needs a number")?,
            "--luld-window-ms" => config.exchange.luld_window_ms = value()?.parse().map_err(|_| "--luld-window-ms needs a number")?,
            "--luld-pause-ms" => config.exchange.luld_pause_ms = value()?.parse().map_err(|_| "--luld-pause-ms needs a number")?,
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
