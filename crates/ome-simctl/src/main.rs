use ome_simctl::{SimConfig, start};

const USAGE: &str = "usage: ome-simctl [--listen ADDR] [--journal PATH] [--secret OPERATOR_SECRET]
                  [--runner-secret SECRET] [--exchange ADDR] [--exchange-secret SECRET]
                  [--newsroom ADDR] [--news-secret SECRET]   (news shocks become newsroom stories)
operators log in as 'operator', agent runners as 'runner'";

fn parse_args() -> Result<SimConfig, String> {
    let env = |name: &str, fallback: &str| std::env::var(name).unwrap_or_else(|_| fallback.into());
    let mut config = SimConfig {
        listen: "127.0.0.1:7400".into(),
        journal: None,
        operator: ("operator".into(), env("OME_SIM_SECRET", "sim")),
        runner: ("runner".into(), env("OME_SIM_RUNNER_SECRET", "runner")),
        exchange_addr: env("OME_EXCHANGE", "127.0.0.1:7000"),
        exchange_name: "ops".into(),
        exchange_secret: env("OME_EXCHANGE_SECRET", "ops"),
        newsroom: None,
    };
    let mut news_secret = env("OME_NEWS_SECRET", "ops");
    let mut newsroom = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => config.listen = value()?,
            "--journal" => config.journal = Some(value()?.into()),
            "--secret" => config.operator.1 = value()?,
            "--runner-secret" => config.runner.1 = value()?,
            "--exchange" => config.exchange_addr = value()?,
            "--exchange-secret" => config.exchange_secret = value()?,
            "--newsroom" => newsroom = Some(value()?),
            "--news-secret" => news_secret = value()?,
            _ => return Err(USAGE.into()),
        }
    }
    config.newsroom = newsroom.map(|addr| (addr, news_secret));
    Ok(config)
}

fn main() {
    let result = parse_args().and_then(start).and_then(|(_, handle)| {
        handle
            .join()
            .map_err(|_| "engine thread panicked".to_string())?
    });
    if let Err(e) = result {
        eprintln!("ome-simctl: {e}");
        std::process::exit(1);
    }
}
