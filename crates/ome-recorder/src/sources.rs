use crate::observation::Observation;
use ome_client::{Client, ClientError};
use ome_net::log;
use ome_protocol::{ClientMessage, MemberMessage, ServerMessage};
use std::collections::BTreeSet;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub name: String,
    pub addr: String,
    pub user: String,
    pub secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Exchange,
    Clearing,
    Broker,
    Venue,
}

const RECONNECT: Duration = Duration::from_secs(1);
const READ: Duration = Duration::from_millis(100);

fn connect(source: Source, endpoint: &Endpoint) -> Result<Client, ClientError> {
    match source {
        Source::Broker => {
            Client::connect_and_logon(&endpoint.addr, &endpoint.user, &endpoint.secret)
                .map(|(c, _)| c)
        }
        _ => {
            Client::connect_member(&endpoint.addr, &endpoint.user, &endpoint.secret).map(|(c, _)| c)
        }
    }
}

fn poll(source: Source, client: &mut Client) -> Result<(), ClientError> {
    match source {
        Source::Exchange => {
            client.send_member(&MemberMessage::QueryInstruments)?;
            client.send_member(&MemberMessage::QueryMembers)
        }
        Source::Clearing => client.send_member(&MemberMessage::QueryClearing),
        Source::Venue => client.send_member(&MemberMessage::QueryVenue),
        Source::Broker => {
            client.send(&ClientMessage::QueryAccounts)?;
            client.send(&ClientMessage::QueryFees)
        }
    }
}

fn translate(source: Source, endpoint: &Endpoint, message: ServerMessage) -> Vec<Observation> {
    match (source, message) {
        (Source::Broker, ServerMessage::AccountList(list)) => vec![Observation::Accounts {
            broker: endpoint.name.clone(),
            list,
        }],
        (Source::Broker, ServerMessage::FeeSummary(fees)) => vec![Observation::Routing {
            broker: endpoint.name.clone(),
            fees,
        }],
        (Source::Broker, _) => vec![],
        (_, message) => Observation::from_market(message),
    }
}

fn session(
    source: Source,
    endpoint: &Endpoint,
    client: &mut Client,
    every: Duration,
    tx: &Sender<Observation>,
) -> Result<(), ClientError> {
    let mut subscribed = BTreeSet::new();
    let mut next_poll = Instant::now();
    loop {
        if Instant::now() >= next_poll {
            poll(source, client)?;
            next_poll = Instant::now() + every;
        }
        let Some(message) = client.recv_timeout(READ)? else {
            continue;
        };
        if source == Source::Exchange {
            let symbols: Vec<_> = match &message {
                ServerMessage::InstrumentList(list) => list.iter().map(|i| i.symbol).collect(),
                ServerMessage::Instrument(info) => vec![info.symbol],
                _ => vec![],
            };
            for symbol in symbols {
                if subscribed.insert(symbol) {
                    client.send_member(&MemberMessage::Subscribe { symbol })?;
                }
            }
        }
        for observation in translate(source, endpoint, message) {
            if tx.send(observation).is_err() {
                return Ok(());
            }
        }
    }
}

pub fn spawn(source: Source, endpoint: Endpoint, every: Duration, tx: Sender<Observation>) {
    thread::spawn(move || {
        let mut reported = false;
        loop {
            match connect(source, &endpoint) {
                Ok(mut client) => {
                    log(&format!("recording {} at {}", endpoint.name, endpoint.addr));
                    reported = false;
                    match session(source, &endpoint, &mut client, every, &tx) {
                        Ok(()) => return,
                        Err(e) => log(&format!("lost {}: {e}; reconnecting", endpoint.name)),
                    }
                }
                Err(e) if !reported => {
                    log(&format!(
                        "cannot reach {} at {}: {e}",
                        endpoint.name, endpoint.addr
                    ));
                    reported = true;
                }
                Err(_) => {}
            }
            thread::sleep(RECONNECT);
        }
    });
}
