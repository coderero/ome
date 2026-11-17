use ome_client::DEFAULT_EXCHANGE_ADDR;
use ome_protocol::Venue;
use ome_venue::wholesaler::WholesalerConfig;
use ome_venue::{VenueConfig, start};

const USAGE: &str = "usage: ome-venue --kind wholesaler|ats --member NAME --member-secret SECRET
                 [--listen ADDR] [--exchange ADDR] [--journal PATH] [--ops-name NAME] [--ops-secret SECRET]
                 [--member-kind broker|lp] [--registry ADDR]
                 wholesaler: [--improvement-bps BPS] [--pfof PER-SHARE (hundredths of a tick)]
                             [--max-order QTY] [--inventory-limit QTY] [--hedge-threshold QTY] [--accept-pct PCT]";

fn parse_args() -> Result<VenueConfig, String> {
    let mut config = VenueConfig {
        kind: Venue::Wholesaler,
        listen: "127.0.0.1:7300".into(),
        journal: None,
        ops_name: "ops".into(),
        ops_secret: std::env::var("OME_VENUE_SECRET").unwrap_or_else(|_| "ops".into()),
        exchange_addr: std::env::var("OME_EXCHANGE")
            .unwrap_or_else(|_| DEFAULT_EXCHANGE_ADDR.into()),
        member_name: String::new(),
        member_secret: String::new(),
        wholesaler: WholesalerConfig::default(),
        member_kind: None,
        registry: ome_net::registry::RegistryAccess::from_env(),
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = || iter.next().ok_or(format!("{flag} needs a value"));
        let number = |text: String| {
            text.parse::<u64>()
                .map_err(|_| format!("{flag} needs a number"))
        };
        match flag.as_str() {
            "--kind" => {
                config.kind = match value()?.as_str() {
                    "wholesaler" => Venue::Wholesaler,
                    "ats" => Venue::Ats,
                    _ => return Err("kind must be wholesaler or ats".into()),
                }
            }
            "--listen" => config.listen = value()?,
            "--member-kind" => {
                config.member_kind = Some(ome_protocol::MemberKind::parse(&value()?).ok_or("member kind must be broker or lp")?)
            }
            "--registry" => {
                let addr = value()?;
                let secret = config.registry.as_ref().map(|r| r.secret.clone()).unwrap_or_else(|| "cluster".into());
                config.registry = Some(ome_net::registry::RegistryAccess { addr, secret });
            }
            "--exchange" => config.exchange_addr = value()?,
            "--journal" => config.journal = Some(value()?.into()),
            "--ops-name" => config.ops_name = value()?,
            "--ops-secret" => config.ops_secret = value()?,
            "--member" => config.member_name = value()?,
            "--member-secret" => config.member_secret = value()?,
            "--improvement-bps" => {
                config.wholesaler.improvement_bps = number(value()?)?.min(10_000)
            }
            "--pfof" => config.wholesaler.pfof_per_share = number(value()?)?,
            "--max-order" => config.wholesaler.max_order = number(value()?)?,
            "--inventory-limit" => config.wholesaler.inventory_limit = number(value()?)?,
            "--hedge-threshold" => config.wholesaler.hedge_threshold = number(value()?)?,
            "--accept-pct" => config.wholesaler.accept_pct = number(value()?)?.min(100),
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
        eprintln!("ome-venue: {e}");
        std::process::exit(1);
    }
}
