use ome_broker::profile::{DEFAULT_COLLAR_BPS, ProfileKind, Regime};
use ome_broker::{BrokerConfig, start as start_broker};
use ome_client::Client;
use ome_core::symbol::Symbol;
use ome_core::types::{OrderKind, OrderOptions, Side, TimeInForce};
use ome_exchange::exchange::ExchangeConfig;
use ome_exchange::{ServerConfig, start as start_exchange};
use ome_protocol::{Asset, ClientMessage, MemberKind, MemberMessage, OrderStatus, Role, ServerMessage};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: ome-load exchange|broker [--sessions N] [--seconds S] [--window N] [--subscribers N] [--no-journal]
  exchange: N member sessions send crossing orders straight to an in-process exchange (journal on disk)
  broker:   N HFT accounts send crossing orders through an in-process DMA broker to the exchange, while
            --subscribers passive clients follow the symbol's market data (the fan-out)
Reports orders per second, trades per second, the latency to the first report and what the
subscribers received.";

struct Options {
    mode: String,
    sessions: usize,
    seconds: u64,
    window: usize,
    subscribers: usize,
    journal: bool,
}

#[derive(Default)]
struct Totals {
    sent: AtomicU64,
    acked: AtomicU64,
    fills: AtomicU64,
    rejected: AtomicU64,
    fanned: AtomicU64,
    latencies: Mutex<Vec<u64>>,
}

fn parse() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or(USAGE)?;
    let mut o = Options { mode, sessions: 4, seconds: 10, window: 64, subscribers: 0, journal: true };
    while let Some(flag) = args.next() {
        let mut number = || args.next().and_then(|v| v.parse::<u64>().ok()).ok_or(format!("{flag} needs a number"));
        match flag.as_str() {
            "--sessions" => o.sessions = number()? as usize,
            "--seconds" => o.seconds = number()?,
            "--window" => o.window = number()? as usize,
            "--subscribers" => o.subscribers = number()? as usize,
            "--no-journal" => o.journal = false,
            _ => return Err(USAGE.into()),
        }
    }
    if !matches!(o.mode.as_str(), "exchange" | "broker") {
        return Err(USAGE.into());
    }
    Ok(o)
}

fn symbol() -> Symbol {
    Symbol::parse("LOAD").unwrap()
}

fn exchange(journal: bool) -> SocketAddr {
    let dir = std::env::temp_dir().join(format!("ome-load-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    start_exchange(ServerConfig {
        listen: "127.0.0.1:0".into(),
        journal: journal.then(|| dir.join("exchange.journal")),
        admin_name: "ops".into(),
        admin_secret: "ops".into(),
        exchange: ExchangeConfig::default(),
        schedule: None,
        registry: None,
    })
    .expect("exchange")
    .0
}

fn list(ops: &mut Client, underwriter: u64) {
    ops.request_member(&MemberMessage::ListInstrument {
        symbol: symbol(),
        ipo_price: 100_00,
        shares: 1,
        underwriter,
        issuer_account: 1,
        name: "Load test".into(),
    })
    .expect("listing");
}

fn record(totals: &Totals, sent_at: &mut HashMap<u64, Instant>, report: &ome_protocol::ExecutionReport) {
    if let Some(at) = sent_at.remove(&report.client_order_id) {
        totals.acked.fetch_add(1, Ordering::Relaxed);
        totals.latencies.lock().unwrap().push(at.elapsed().as_micros() as u64);
    }
    match report.status {
        OrderStatus::Filled => {
            totals.fills.fetch_add(1, Ordering::Relaxed);
        }
        OrderStatus::Rejected => {
            totals.rejected.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
}

fn pump(mut client: Client, member: bool, deadline: Instant, window: usize, totals: Arc<Totals>, accounts: (u64, u64)) {
    let mut sent_at: HashMap<u64, Instant> = HashMap::new();
    let mut next = 0u64;
    while Instant::now() < deadline {
        while sent_at.len() < window {
            next += 1;
            let side = if next.is_multiple_of(2) { Side::Buy } else { Side::Sell };
            let options = OrderOptions::tif(TimeInForce::Ioc);
            let sent = if member {
                let account = if side == Side::Buy { accounts.0 } else { accounts.1 };
                let rest = OrderOptions::tif(TimeInForce::Gtc);
                client.send_member(&MemberMessage::NewOrder {
                    member_order_id: next,
                    account,
                    symbol: symbol(),
                    side,
                    kind: OrderKind::Limit(100_00),
                    options: if side == Side::Buy { rest } else { options },
                    qty: 1,
                })
            } else {
                client.send(&ClientMessage::NewOrder {
                    client_order_id: next,
                    symbol: symbol(),
                    side,
                    kind: OrderKind::Limit(100_00),
                    options: if side == Side::Buy { OrderOptions::tif(TimeInForce::Gtc) } else { options },
                    qty: 1,
                })
            };
            if sent.is_err() {
                return;
            }
            sent_at.insert(next, Instant::now());
            totals.sent.fetch_add(1, Ordering::Relaxed);
        }
        match client.recv_timeout(Duration::from_millis(50)) {
            Ok(Some(ServerMessage::Execution(report))) => record(&totals, &mut sent_at, &report),
            Ok(Some(ServerMessage::Reject { .. })) => {
                totals.rejected.fetch_add(1, Ordering::Relaxed);
                sent_at.clear();
            }
            Ok(_) => {}
            Err(_) => return,
        }
        while let Ok(Some(message)) = client.try_recv() {
            match message {
                ServerMessage::Execution(report) => record(&totals, &mut sent_at, &report),
                ServerMessage::Reject { .. } => {
                    totals.rejected.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
        }
        if sent_at.values().any(|t| t.elapsed() > Duration::from_secs(2)) {
            sent_at.retain(|_, t| t.elapsed() <= Duration::from_secs(2));
        }
    }
}

fn subscriber(addr: String, user: String, deadline: Instant, totals: Arc<Totals>) {
    let Ok((mut client, _)) = Client::connect_and_logon(&addr, &user, "pw") else { return };
    if client.send(&ClientMessage::Subscribe { symbol: symbol() }).is_err() {
        return;
    }
    while Instant::now() < deadline {
        match client.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(_)) => {
                totals.fanned.fetch_add(1, Ordering::Relaxed);
            }
            Ok(None) => {}
            Err(_) => return,
        }
    }
}

fn main() {
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let totals = Arc::new(Totals::default());
    let exchange_addr = exchange(o.journal);
    let (mut ops, _) = Client::connect_member(&exchange_addr.to_string(), "ops", "ops").expect("ops");
    let mut threads = Vec::new();
    let started;
    let deadline;
    if o.mode == "exchange" {
        for i in 0..o.sessions {
            ops.request_member(&MemberMessage::CreateMember { kind: MemberKind::Broker, name: format!("load-{i}"), secret: "pw".into() })
                .expect("member");
        }
        list(&mut ops, 2);
        let clients: Vec<Client> = (0..o.sessions)
            .map(|i| Client::connect_member(&exchange_addr.to_string(), &format!("load-{i}"), "pw").expect("logon").0)
            .collect();
        started = Instant::now();
        deadline = started + Duration::from_secs(o.seconds);
        for client in clients {
            let totals = totals.clone();
            let window = o.window;
            threads.push(thread::spawn(move || pump(client, true, deadline, window, totals, (1, 2))));
        }
    } else {
        ops.request_member(&MemberMessage::CreateMember { kind: MemberKind::Broker, name: "dma-co".into(), secret: "pw".into() }).expect("member");
        let broker = start_broker(BrokerConfig {
            listen: "127.0.0.1:0".into(),
            journal: o.journal.then(|| std::env::temp_dir().join(format!("ome-load-{}/dma-co.journal", std::process::id()))),
            profile: ProfileKind::Dma,
            admin_name: "admin".into(),
            admin_secret: "admin".into(),
            exchange_addr: exchange_addr.to_string(),
            member_name: "dma-co".into(),
            member_secret: "pw".into(),
            clearing_addr: None,
            regime: Regime::India,
            wholesaler_addr: None,
            ats_addr: None,
            news_addr: None,
            postgres: None,
            collar_bps: DEFAULT_COLLAR_BPS,
            exchange_rate: 4_500,
            registry: None,
        })
        .expect("broker")
        .0
        .to_string();
        list(&mut ops, 2);
        thread::sleep(Duration::from_millis(500));
        let (mut admin, _) = Client::connect_and_logon(&broker, "admin", "admin").expect("admin");
        let mut users = Vec::new();
        for i in 0..o.sessions + o.subscribers {
            let name = format!("load-{i}");
            let text = admin
                .request(&ClientMessage::CreateAccount { role: Role::Hft, cash: 1_000_000_000_00, username: name.clone(), secret: "pw".into() })
                .expect("account");
            let id: u64 = text.split_whitespace().nth(1).and_then(|t| t.parse().ok()).expect("account id");
            admin.request(&ClientMessage::SetBalance { account_id: id, asset: Asset::Shares(symbol()), amount: 100_000_000 }).expect("shares");
            users.push(name);
        }
        let clients: Vec<Client> = users[..o.sessions]
            .iter()
            .map(|u| Client::connect_and_logon(&broker, u, "pw").expect("logon").0)
            .collect();
        started = Instant::now();
        deadline = started + Duration::from_secs(o.seconds);
        for user in users[o.sessions..].iter().cloned() {
            let (broker, totals) = (broker.clone(), totals.clone());
            threads.push(thread::spawn(move || subscriber(broker, user, deadline, totals)));
        }
        for client in clients {
            let totals = totals.clone();
            let window = o.window;
            threads.push(thread::spawn(move || pump(client, false, deadline, window, totals, (0, 0))));
        }
    }
    for t in threads {
        let _ = t.join();
    }
    let secs = started.elapsed().as_secs_f64();
    let per = |v: &AtomicU64| v.load(Ordering::Relaxed) as f64 / secs;
    let mut lat = totals.latencies.lock().unwrap().clone();
    lat.sort_unstable();
    let q = |p: f64| lat.get(((lat.len() as f64 - 1.0) * p) as usize).copied().unwrap_or(0) as f64 / 1_000.0;
    println!(
        "{} · {} session(s) · window {} · journal {} · {:.1}s",
        o.mode,
        o.sessions,
        o.window,
        if o.journal { "on" } else { "off" },
        secs
    );
    println!("  orders sent    {:>10.0}/s", per(&totals.sent));
    println!("  first reports  {:>10.0}/s   latency p50 {:.2} ms · p99 {:.2} ms · max {:.2} ms", per(&totals.acked), q(0.5), q(0.99), q(1.0));
    println!("  fills          {:>10.0}/s", per(&totals.fills));
    println!("  rejected       {:>10.0}/s", per(&totals.rejected));
    if o.subscribers > 0 {
        println!(
            "  market data    {:>10.0} messages/s to {} subscriber(s) ({:.0} each)",
            per(&totals.fanned),
            o.subscribers,
            per(&totals.fanned) / o.subscribers as f64
        );
    }
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("ome-load-{}", std::process::id())));
}
