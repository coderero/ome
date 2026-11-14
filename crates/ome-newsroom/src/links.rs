use crate::llm::Llm;
use crate::newsroom::Filing;
use crate::writer::Copy;
use ome_client::Client;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_net::log;
use ome_protocol::NewsItem;
use ome_protocol::{MarketPhase, MemberMessage, ServerMessage, SimCommand, SimMessage, SimRole};
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;
use std::time::Instant;

const RETRY: Duration = Duration::from_secs(2);

pub fn watch_exchange<T: Send + 'static>(
    addr: String,
    name: String,
    secret: String,
    events: Sender<T>,
    wrap: fn(MarketPhase, u64) -> T,
    filings: Receiver<Filing>,
) {
    thread::spawn(move || {
        let mut warned = false;
        let mut filed: BTreeMap<Symbol, Filing> = BTreeMap::new();
        let file = |client: &mut Client, f: &Filing| {
            client.send_member(&MemberMessage::FileProspectus {
                symbol: f.symbol,
                reference: f.reference,
                name: f.name.clone(),
            })
        };
        loop {
            filed.extend(filings.try_iter().map(|f| (f.symbol, f)));
            match Client::connect_member(&addr, &name, &secret) {
                Ok((mut client, _)) => {
                    log(&format!(
                        "following the trading calendar of exchange {addr}; filing {} prospectus(es)",
                        filed.len()
                    ));
                    warned = false;
                    let mut healthy = filed.values().all(|f| file(&mut client, f).is_ok());
                    while healthy {
                        for f in filings.try_iter() {
                            healthy &= file(&mut client, &f).is_ok();
                            filed.insert(f.symbol, f);
                        }
                        match client.recv_timeout(Duration::from_millis(250)) {
                            Ok(Some(ServerMessage::MarketStatus(s))) => {
                                if events.send(wrap(s.phase, s.day)).is_err() {
                                    return;
                                }
                            }
                            Ok(Some(ServerMessage::Reject { reason, .. })) => {
                                log(&format!("exchange refused a request: {reason}"))
                            }
                            Ok(_) => {}
                            Err(_) => healthy = false,
                        }
                    }
                    log("exchange link lost; reconnecting");
                }
                Err(e) if !warned => {
                    log(&format!("exchange {addr} unreachable ({e}); retrying"));
                    warned = true;
                }
                Err(_) => {}
            }
            thread::sleep(RETRY);
        }
    });
}

fn push(
    client: &mut Option<Client>,
    addr: &str,
    secret: &str,
    pending: &mut BTreeMap<Symbol, Price>,
) {
    if client.is_none() {
        *client = Client::connect_sim(addr, SimRole::Operator, "operator", secret).ok();
    }
    let Some(sim) = client.as_mut() else {
        return;
    };
    while let Some((symbol, price)) = pending.first_key_value().map(|(s, p)| (*s, *p)) {
        let command = SimCommand::SetFair {
            symbol,
            price,
            drift_bps: 0,
        };
        match sim.request_sim(&SimMessage::Command(command)) {
            Ok(_) => {
                pending.remove(&symbol);
            }
            Err(e) => {
                log(&format!("simctl refused fair value for {symbol}: {e}"));
                *client = None;
                return;
            }
        }
    }
}

pub fn sync_fair(addr: String, secret: String) -> Sender<Vec<(Symbol, Price)>> {
    let (tx, rx): (_, Receiver<Vec<(Symbol, Price)>>) = mpsc::channel();
    thread::spawn(move || {
        let mut client = None;
        let mut pending = BTreeMap::new();
        loop {
            match rx.recv_timeout(RETRY) {
                Ok(batch) => pending.extend(batch),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            for batch in rx.try_iter() {
                pending.extend(batch);
            }
            if !pending.is_empty() {
                push(&mut client, &addr, &secret, &mut pending);
            }
        }
    });
    tx
}

pub struct Job {
    pub item: NewsItem,
    pub company: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub seq: u64,
    pub result: Result<Copy, String>,
    pub elapsed_ms: u64,
}

pub fn spawn_writer<T: Send + 'static>(
    llm: Llm,
    jobs: Receiver<Job>,
    events: Sender<T>,
    wrap: fn(Done) -> T,
) {
    thread::spawn(move || {
        let started = Instant::now();
        match llm.warm_up() {
            Ok(()) => log(&format!(
                "LLM writer ready: {} loaded in {:?}",
                llm.model(),
                started.elapsed()
            )),
            Err(e) => log(&format!(
                "LLM writer offline ({e}); stories keep their template text"
            )),
        }
        let mut offline_until: Option<Instant> = None;
        for job in jobs {
            let begun = Instant::now();
            let result = if offline_until.is_some_and(|t| t > begun) {
                Err("LLM offline".to_string())
            } else {
                let result = llm.rewrite(&job.item, &job.company);
                if let Err(e) = &result
                    && (e.contains("refused") || e.contains("resolve") || e.contains("no answer"))
                {
                    offline_until = Some(Instant::now() + Duration::from_secs(10));
                }
                result
            };
            let done = Done {
                seq: job.item.seq,
                result,
                elapsed_ms: begun.elapsed().as_millis() as u64,
            };
            if events.send(wrap(done)).is_err() {
                return;
            }
        }
    });
}
