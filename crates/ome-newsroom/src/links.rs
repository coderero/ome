use ome_client::Client;
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_net::log;
use ome_protocol::{MarketPhase, ServerMessage, SimCommand, SimMessage, SimRole};
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

const RETRY: Duration = Duration::from_secs(2);

pub fn watch_exchange<T: Send + 'static>(
    addr: String,
    name: String,
    secret: String,
    events: Sender<T>,
    wrap: fn(MarketPhase, u64) -> T,
) {
    thread::spawn(move || {
        let mut warned = false;
        loop {
            match Client::connect_member(&addr, &name, &secret) {
                Ok((client, _)) => {
                    log(&format!(
                        "following the trading calendar of exchange {addr}"
                    ));
                    warned = false;
                    loop {
                        match client.recv_timeout(Duration::from_secs(1)) {
                            Ok(Some(ServerMessage::MarketStatus(s))) => {
                                if events.send(wrap(s.phase, s.day)).is_err() {
                                    return;
                                }
                            }
                            Ok(_) => {}
                            Err(_) => break,
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
