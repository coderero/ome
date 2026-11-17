use ome_client::{Client, ClientError};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_net::log;
use ome_net::registry::Directory;
use ome_protocol::{Asset, ClientMessage, MemberMessage, Role, ServerMessage};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

const DISCOVERY_EVERY: Duration = Duration::from_secs(3);
const WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    FileProspectus {
        key: String,
        symbol: Symbol,
        reference: u64,
        name: String,
    },
    OpenAccount {
        key: String,
        broker: String,
        username: String,
        secret: String,
        role: Role,
        cash: u64,
    },
    ListIpo {
        key: String,
        symbol: Symbol,
        price: Price,
        shares: u64,
        underwriter: String,
        issuer: String,
        name: String,
    },
    SetDmm {
        key: String,
        symbol: Symbol,
        member: String,
    },
    Fund {
        broker: String,
        usernames: Vec<String>,
        amount: u64,
    },
}

impl Task {
    pub fn key(&self) -> Option<&str> {
        match self {
            Task::FileProspectus { key, .. }
            | Task::OpenAccount { key, .. }
            | Task::ListIpo { key, .. }
            | Task::SetDmm { key, .. } => Some(key),
            Task::Fund { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done { key: String, text: String },
    Failed { key: String, reason: String },
    Funded { text: String },
    Brokers(BTreeMap<String, String>),
}

#[derive(Clone)]
pub struct Access {
    pub exchange: (String, String, String),
    pub broker_admin: (String, String),
    pub directory: Arc<dyn Directory>,
}

pub fn spawn<T: Send + 'static>(
    access: Access,
    tasks: Receiver<Task>,
    out: Sender<T>,
    wrap: fn(Outcome) -> T,
) {
    thread::spawn(move || {
        let mut next_discovery = Instant::now();
        let mut known: BTreeMap<String, String> = BTreeMap::new();
        loop {
            if Instant::now() >= next_discovery {
                next_discovery = Instant::now() + DISCOVERY_EVERY;
                let brokers: BTreeMap<String, String> = access.directory.members("broker").into_iter().collect();
                if brokers != known {
                    known = brokers.clone();
                    if out.send(wrap(Outcome::Brokers(brokers))).is_err() {
                        return;
                    }
                }
            }
            let task = match tasks.recv_timeout(Duration::from_millis(500)) {
                Ok(task) => task,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let outcome = match (&task, run(&access, &known, &task)) {
                (Task::Fund { .. }, Ok(text)) => Outcome::Funded { text },
                (Task::Fund { .. }, Err(reason)) => Outcome::Funded { text: format!("funding failed: {reason}") },
                (task, Ok(text)) => Outcome::Done { key: task.key().unwrap_or_default().to_string(), text },
                (task, Err(reason)) => Outcome::Failed { key: task.key().unwrap_or_default().to_string(), reason },
            };
            if out.send(wrap(outcome)).is_err() {
                return;
            }
        }
    });
}

fn exchange(access: &Access) -> Result<Client, String> {
    let (addr, name, secret) = &access.exchange;
    Client::connect_member(addr, name, secret)
        .map(|(client, _)| client)
        .map_err(|e| format!("exchange: {e}"))
}

fn broker_admin(access: &Access, known: &BTreeMap<String, String>, broker: &str) -> Result<Client, String> {
    let addr = known
        .get(broker)
        .cloned()
        .or_else(|| access.directory.resolve(broker))
        .ok_or_else(|| format!("broker {broker} is not registered yet"))?;
    let (name, secret) = &access.broker_admin;
    Client::connect_and_logon(&addr, name, secret)
        .map(|(client, _)| client)
        .map_err(|e| format!("{broker}: {e}"))
}

fn member_id(ops: &mut Client, name: &str) -> Result<u64, String> {
    ops.send_member(&MemberMessage::QueryMembers).map_err(|e| e.to_string())?;
    match ops
        .wait_for(WAIT, |m| matches!(m, ServerMessage::MemberList(_)))
        .map_err(|e| e.to_string())?
    {
        ServerMessage::MemberList(list) => list
            .into_iter()
            .find(|m| m.name == name)
            .map(|m| m.member_id)
            .ok_or_else(|| format!("{name} is not an exchange member yet")),
        _ => unreachable!(),
    }
}

fn accounts(admin: &mut Client) -> Result<Vec<ome_protocol::AccountInfo>, String> {
    admin.send(&ClientMessage::QueryAccounts).map_err(|e| e.to_string())?;
    match admin
        .wait_for(WAIT, |m| matches!(m, ServerMessage::AccountList(_)))
        .map_err(|e| e.to_string())?
    {
        ServerMessage::AccountList(list) => Ok(list),
        _ => unreachable!(),
    }
}

fn already(result: Result<String, ClientError>, words: &[&str], fallback: String) -> Result<String, String> {
    match result {
        Ok(text) => Ok(text),
        Err(ClientError::Rejected(reason)) if words.iter().any(|w| reason.contains(w)) => Ok(fallback),
        Err(e) => Err(e.to_string()),
    }
}

fn run(access: &Access, known: &BTreeMap<String, String>, task: &Task) -> Result<String, String> {
    match task {
        Task::FileProspectus { symbol, reference, name, .. } => {
            let mut ops = exchange(access)?;
            already(
                ops.request_member(&MemberMessage::FileProspectus { symbol: *symbol, reference: *reference, name: name.clone() }),
                &["already"],
                format!("prospectus for {symbol} already on file"),
            )
        }
        Task::OpenAccount { broker, username, secret, role, cash, .. } => {
            let mut admin = broker_admin(access, known, broker)?;
            already(
                admin.request(&ClientMessage::CreateAccount {
                    role: *role,
                    cash: *cash,
                    username: username.clone(),
                    secret: secret.clone(),
                }),
                &["taken"],
                format!("{username} already has an account at {broker}"),
            )
        }
        Task::ListIpo { symbol, price, shares, underwriter, issuer, name, .. } => {
            let mut admin = broker_admin(access, known, underwriter)?;
            let issuer_account = accounts(&mut admin)?
                .into_iter()
                .find(|a| a.username == *issuer)
                .map(|a| a.account_id)
                .ok_or_else(|| format!("issuer account {issuer} not found at {underwriter}"))?;
            let mut ops = exchange(access)?;
            let underwriter_id = member_id(&mut ops, underwriter)?;
            let listed = ops.request_member(&MemberMessage::ListInstrument {
                symbol: *symbol,
                ipo_price: *price,
                shares: *shares,
                underwriter: underwriter_id,
                issuer_account,
                name: name.clone(),
            });
            already(listed, &["already listed", "already exists"], format!("{symbol} is already listed"))
        }
        Task::SetDmm { symbol, member, .. } => {
            let mut ops = exchange(access)?;
            let id = member_id(&mut ops, member)?;
            ops.request_member(&MemberMessage::SetDmm { symbol: *symbol, member: id })
                .map_err(|e| e.to_string())
        }
        Task::Fund { broker, usernames, amount } => {
            let mut admin = broker_admin(access, known, broker)?;
            let mut funded = 0;
            for account in accounts(&mut admin)?.iter().filter(|a| usernames.contains(&a.username)) {
                admin
                    .request(&ClientMessage::SetBalance {
                        account_id: account.account_id,
                        asset: Asset::Cash,
                        amount: account.cash.available + amount,
                    })
                    .map_err(|e| format!("cannot fund {}: {e}", account.username))?;
                funded += 1;
            }
            let text = format!("funded {funded} account(s) at {broker}");
            log(&text);
            Ok(text)
        }
    }
}
