use ome_recorder::sources::Endpoint;
use ome_recorder::{RecorderConfig, start};

const USAGE: &str = "usage: ome-recorder [--postgres URL] [--redis URL] [--exchange ADDR] [--exchange-secret S]
                    [--clearing ADDR] [--clearing-secret S] [--broker NAME=ADDR ...] [--broker-secret S]
                    [--venue NAME=ADDR ...] [--venue-secret S] [--news ADDR] [--news-secret S] [--poll-ms MS]
                    [--registry ADDR]   (brokers and venues are discovered from the registry as they join)
  e.g. --postgres postgres://ome:ome@127.0.0.1/ome --redis redis://127.0.0.1/";

fn endpoint(name: &str, addr: &str, user: &str, secret: &str) -> Endpoint {
    Endpoint {
        name: name.into(),
        addr: addr.into(),
        user: user.into(),
        secret: secret.into(),
    }
}

fn parse_args() -> Result<RecorderConfig, String> {
    let env = |name: &str, fallback: &str| std::env::var(name).unwrap_or_else(|_| fallback.into());
    let mut exchange = (
        env("OME_EXCHANGE", "127.0.0.1:7000"),
        env("OME_EXCHANGE_SECRET", "ops"),
    );
    let mut clearing: Option<String> = None;
    let mut clearing_secret = env("OME_CLEARING_SECRET", "ops");
    let mut brokers: Vec<(String, String)> = Vec::new();
    let mut broker_secret = env("OME_BROKER_SECRET", "admin");
    let mut venues: Vec<(String, String)> = Vec::new();
    let mut venue_secret = env("OME_VENUE_SECRET", "ops");
    let mut news: Option<String> = None;
    let mut news_secret = env("OME_NEWS_SECRET", "ops");
    let mut postgres = None;
    let mut redis = None;
    let mut poll_ms = 1_000;
    let mut registry = ome_net::registry::RegistryAccess::from_env();
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = || iter.next().ok_or(format!("{flag} needs a value"));
        let named = |text: String| {
            text.split_once('=')
                .map(|(n, a)| (n.to_string(), a.to_string()))
                .ok_or(format!("{flag} needs NAME=ADDR"))
        };
        match flag.as_str() {
            "--postgres" => postgres = Some(value()?),
            "--redis" => redis = Some(value()?),
            "--exchange" => exchange.0 = value()?,
            "--exchange-secret" => exchange.1 = value()?,
            "--clearing" => clearing = Some(value()?),
            "--clearing-secret" => clearing_secret = value()?,
            "--broker" => brokers.push(named(value()?)?),
            "--broker-secret" => broker_secret = value()?,
            "--venue" => venues.push(named(value()?)?),
            "--venue-secret" => venue_secret = value()?,
            "--news" => news = Some(value()?),
            "--news-secret" => news_secret = value()?,
            "--registry" => {
                let addr = value()?;
                let secret = registry.as_ref().map(|r| r.secret.clone()).unwrap_or_else(|| "cluster".into());
                registry = Some(ome_net::registry::RegistryAccess { addr, secret });
            }
            "--poll-ms" => {
                poll_ms = value()?
                    .parse()
                    .map_err(|_| "--poll-ms needs a number".to_string())?
            }
            _ => return Err(USAGE.into()),
        }
    }
    Ok(RecorderConfig {
        exchange: endpoint("exchange", &exchange.0, "ops", &exchange.1),
        clearing: clearing.map(|addr| endpoint("clearing", &addr, "ops", &clearing_secret)),
        brokers: brokers
            .iter()
            .map(|(name, addr)| endpoint(name, addr, "admin", &broker_secret))
            .collect(),
        venues: venues
            .iter()
            .map(|(name, addr)| endpoint(name, addr, "ops", &venue_secret))
            .collect(),
        news: news.map(|addr| endpoint("newsroom", &addr, "ops", &news_secret)),
        postgres,
        redis,
        poll_ms,
        registry,
        broker_secret,
        venue_secret,
    })
}

fn main() {
    let result = parse_args().and_then(start).and_then(|handle| {
        handle
            .join()
            .map_err(|_| "writer thread panicked".to_string())
    });
    if let Err(e) = result {
        eprintln!("ome-recorder: {e}");
        std::process::exit(1);
    }
}
