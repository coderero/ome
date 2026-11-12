mod state;
mod strategy;

use ome_client::{Client, ClientError, DEFAULT_ADDR};
use ome_core::price::format_price;
use ome_core::symbol::Symbol;
use ome_protocol::ClientMessage;
use rand::SeedableRng;
use rand::rngs::StdRng;
use state::BotState;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::thread;
use std::time::{Duration, Instant};
use strategy::{Action, Strategy};

const USAGE: &str = "usage: ome-bots [--addr HOST:PORT] [--file BOTS_FILE] [SPEC...]

SPEC is strategy:SYMBOL:username:secret, one per bot; a bots file holds one SPEC per line (# comments)
strategies: mm (market maker), momentum, meanrev (mean reversion), noise (retail flow)";

const STATUS_EVERY: Duration = Duration::from_secs(30);

struct BotSpec {
    strategy: String,
    symbol: Symbol,
    username: String,
    secret: String,
}

fn parse_spec(text: &str) -> Result<BotSpec, String> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    let [strategy, symbol, username, secret] = parts.as_slice() else {
        return Err(format!(
            "bad bot spec '{text}', expected strategy:SYMBOL:username:secret"
        ));
    };
    if strategy::create(strategy).is_none() {
        return Err(format!("unknown strategy '{strategy}'"));
    }
    Ok(BotSpec {
        strategy: strategy.to_string(),
        symbol: Symbol::parse(symbol).ok_or(format!("bad symbol '{symbol}'"))?,
        username: username.to_string(),
        secret: secret.to_string(),
    })
}

fn parse_args() -> Result<(String, Vec<BotSpec>), String> {
    let mut addr = std::env::var("OME_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.into());
    let mut specs = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--addr" => addr = args.next().ok_or("--addr needs a value")?,
            "--file" => {
                let path = args.next().ok_or("--file needs a value")?;
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read {path}: {e}"))?;
                for line in text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                {
                    specs.push(parse_spec(line)?);
                }
            }
            "-h" | "--help" => return Err(USAGE.into()),
            spec => specs.push(parse_spec(spec)?),
        }
    }
    if specs.is_empty() {
        return Err(USAGE.into());
    }
    Ok((addr, specs))
}

fn main() {
    let (addr, specs) = match parse_args() {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    eprintln!("starting {} bots against {addr}", specs.len());
    let handles: Vec<_> = specs
        .into_iter()
        .map(|spec| {
            let addr = addr.clone();
            thread::spawn(move || supervise(&addr, spec))
        })
        .collect();
    for handle in handles {
        let _ = handle.join();
    }
}

fn supervise(addr: &str, spec: BotSpec) {
    let mut hasher = DefaultHasher::new();
    spec.username.hash(&mut hasher);
    std::time::SystemTime::now().hash(&mut hasher);
    let mut rng = StdRng::seed_from_u64(hasher.finish());

    loop {
        let strategy = strategy::create(&spec.strategy).expect("validated in parse_spec");
        match run(addr, &spec, strategy, &mut rng) {
            Err(ClientError::Rejected(reason)) => {
                eprintln!("[{}] logon rejected: {reason}; giving up", spec.username);
                return;
            }
            Err(e) => eprintln!("[{}] {e}; reconnecting in 2s", spec.username),
            Ok(()) => return,
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn run(
    addr: &str,
    spec: &BotSpec,
    mut strategy: Box<dyn Strategy>,
    rng: &mut StdRng,
) -> Result<(), ClientError> {
    let (mut client, session) = Client::connect_and_logon(addr, &spec.username, &spec.secret)?;
    client.send(&ClientMessage::Subscribe {
        symbol: spec.symbol,
    })?;
    eprintln!(
        "[{}] {} on {} (account {})",
        spec.username,
        strategy.name(),
        spec.symbol,
        session.account_id
    );

    let mut state = BotState::new(spec.symbol);
    let mut next_tick = Instant::now() + strategy.interval();
    let mut next_status = Instant::now() + STATUS_EVERY;
    loop {
        let wait = next_tick.saturating_duration_since(Instant::now());
        if let Some(message) = client.recv_timeout(wait)? {
            state.apply(&message);
            while let Some(message) = client.try_recv()? {
                state.apply(&message);
            }
        }
        if Instant::now() < next_tick {
            continue;
        }
        next_tick += strategy.interval();

        for action in strategy.on_tick(&state, rng) {
            match action {
                Action::Place { side, kind, qty } => {
                    let id = client.send_order(spec.symbol, side, kind, qty)?;
                    state.pending.insert(id);
                }
                Action::Cancel(order_id) => client.send(&ClientMessage::Cancel { order_id })?,
                Action::CancelAll => client.send(&ClientMessage::CancelAll {
                    symbol: Some(spec.symbol),
                })?,
            }
        }

        if Instant::now() >= next_status {
            next_status += STATUS_EVERY;
            eprintln!(
                "[{}] cash {} shares {} open {} fills {}",
                spec.username,
                format_price(state.cash + state.cash_reserved),
                state.total_shares(),
                state.open_orders.len(),
                state.fills
            );
        }
    }
}
