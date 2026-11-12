mod auth;
mod exchange;
mod gateway;
mod instrument;
mod journal;
mod ledger;
mod net;

use exchange::{Exchange, ExchangeConfig};
use gateway::{Gateway, log};
use journal::Journal;
use net::Inbound;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TICK: Duration = Duration::from_millis(50);
const MAX_BATCH: usize = 4_096;

struct Args {
    listen: String,
    journal: PathBuf,
    admin_user: String,
    admin_secret: String,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        listen: std::env::var("OME_LISTEN").unwrap_or_else(|_| "127.0.0.1:7000".into()),
        journal: std::env::var("OME_JOURNAL")
            .unwrap_or_else(|_| "data/ome.journal".into())
            .into(),
        admin_user: "admin".into(),
        admin_secret: std::env::var("OME_ADMIN_SECRET").unwrap_or_else(|_| "admin".into()),
    };
    let mut iter = std::env::args().skip(1);
    while let Some(flag) = iter.next() {
        let mut value = || iter.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--listen" => args.listen = value()?,
            "--journal" => args.journal = value()?.into(),
            "--admin-user" => args.admin_user = value()?,
            "--admin-secret" => args.admin_secret = value()?,
            "-h" | "--help" => {
                return Err("usage: ome-server [--listen ADDR] [--journal PATH] [--admin-user NAME] [--admin-secret SECRET]".into());
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(args)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("ome-server: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    let (journal, entries) = Journal::open(&args.journal)
        .map_err(|e| format!("cannot open journal {}: {e}", args.journal.display()))?;
    let mut exchange = Exchange::new(ExchangeConfig::default());
    let started = Instant::now();
    let mut last_ts = 0;
    for (ts, command) in &entries {
        let _ = exchange.execute(command, *ts);
        last_ts = *ts;
    }
    log(&format!(
        "replayed {} journal entries from {} in {:?}",
        entries.len(),
        args.journal.display(),
        started.elapsed()
    ));

    let mut gateway = Gateway::new(exchange, Some(journal), last_ts);
    gateway.ensure_admin(&args.admin_user, &args.admin_secret, now_ms())?;
    gateway.flush().map_err(|e| e.to_string())?;

    let listener = TcpListener::bind(&args.listen)
        .map_err(|e| format!("cannot listen on {}: {e}", args.listen))?;
    let ex = gateway.exchange();
    log(&format!(
        "listening on {} (SBE over TCP) · {} accounts · {} instruments",
        args.listen,
        ex.account_list().len(),
        ex.instrument_list().len()
    ));

    let (tx, rx) = mpsc::channel();
    net::listen(listener, tx);

    let mut next_tick = Instant::now() + TICK;
    loop {
        let timeout = next_tick.saturating_duration_since(Instant::now());
        match rx.recv_timeout(timeout) {
            Ok(first) => {
                handle(&mut gateway, first);
                for inbound in rx.try_iter().take(MAX_BATCH) {
                    handle(&mut gateway, inbound);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("listener stopped".into()),
        }
        if Instant::now() >= next_tick {
            gateway.tick(now_ms());
            next_tick = Instant::now() + TICK;
        }
        gateway
            .flush()
            .map_err(|e| format!("journal flush failed: {e}"))?;
    }
}

fn handle(gateway: &mut Gateway, inbound: Inbound) {
    match inbound {
        Inbound::Connected(id, outbox) => gateway.connect(id, Box::new(outbox)),
        Inbound::Frame(id, bytes) => gateway.handle(id, &bytes, now_ms()),
        Inbound::Disconnected(id) => gateway.disconnect(id),
    }
}
