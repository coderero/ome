use crate::llm::Llm;
use crate::newsroom::Filing;
use crate::writer::Copy;
use ome_client::Client;
use ome_core::symbol::Symbol;
use ome_net::log;
use ome_protocol::NewsItem;
use ome_protocol::{MarketPhase, MemberMessage, ServerMessage};
use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, Sender};
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

pub struct IdeaJob {
    pub symbol: Symbol,
    pub context: crate::llm::IdeaContext,
    pub seed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdeaDone {
    pub symbol: Symbol,
    pub result: Result<crate::llm::Idea, String>,
    pub elapsed_ms: u64,
}

pub fn spawn_ideas<T: Send + 'static>(llm: Llm, jobs: Receiver<IdeaJob>, events: Sender<T>, wrap: fn(IdeaDone) -> T) {
    thread::spawn(move || {
        let mut offline_until: Option<Instant> = None;
        for job in jobs {
            if let Some(until) = offline_until.take() {
                thread::sleep(until.saturating_duration_since(Instant::now()));
            }
            let begun = Instant::now();
            let result = {
                let result = llm.idea(&job.context, job.seed);
                if let Err(e) = &result
                    && (e.contains("refused") || e.contains("resolve") || e.contains("no answer"))
                {
                    offline_until = Some(Instant::now() + Duration::from_secs(10));
                }
                result
            };
            let done = IdeaDone { symbol: job.symbol, result, elapsed_ms: begun.elapsed().as_millis() as u64 };
            if events.send(wrap(done)).is_err() {
                return;
            }
        }
    });
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
