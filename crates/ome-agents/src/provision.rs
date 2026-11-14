use crate::roster::AgentSpec;
use ome_client::{Client, ClientError};
use ome_protocol::ClientMessage;
use std::collections::BTreeMap;

pub fn provision(
    agents: &[AgentSpec],
    brokers: &BTreeMap<String, String>,
    admin_secret: &str,
    agent_secret: &str,
) -> Result<usize, String> {
    let mut created = 0;
    let mut by_broker: BTreeMap<&str, Vec<&AgentSpec>> = BTreeMap::new();
    for agent in agents {
        by_broker.entry(&agent.broker).or_default().push(agent);
    }
    for (broker, group) in by_broker {
        let addr = brokers.get(broker).ok_or_else(|| {
            format!("no address for broker '{broker}' (pass --broker {broker}=ADDR)")
        })?;
        let (mut admin, _) = Client::connect_and_logon(addr, "admin", admin_secret)
            .map_err(|e| format!("cannot log in to {broker} as admin: {e}"))?;
        for agent in group {
            match admin.request(&ClientMessage::CreateAccount {
                role: agent.role,
                cash: agent.cash,
                username: agent.name.clone(),
                secret: agent_secret.to_string(),
            }) {
                Ok(_) => created += 1,
                Err(ClientError::Rejected(reason)) if reason.contains("taken") => {}
                Err(e) => return Err(format!("{broker}: cannot open {}: {e}", agent.name)),
            }
        }
    }
    Ok(created)
}

pub fn fund(
    names: &[String],
    broker_addr: &str,
    admin_secret: &str,
    amount: u64,
) -> Result<usize, String> {
    let (mut admin, _) = Client::connect_and_logon(broker_addr, "admin", admin_secret)
        .map_err(|e| format!("cannot log in as broker admin: {e}"))?;
    admin
        .send(&ClientMessage::QueryAccounts)
        .map_err(|e| e.to_string())?;
    let list = match admin
        .wait_for(std::time::Duration::from_secs(5), |m| {
            matches!(m, ome_protocol::ServerMessage::AccountList(_))
        })
        .map_err(|e| e.to_string())?
    {
        ome_protocol::ServerMessage::AccountList(list) => list,
        _ => unreachable!(),
    };
    let mut funded = 0;
    for account in list.iter().filter(|a| names.contains(&a.username)) {
        admin
            .request(&ClientMessage::SetBalance {
                account_id: account.account_id,
                asset: ome_protocol::Asset::Cash,
                amount: account.cash.available + amount,
            })
            .map_err(|e| format!("cannot fund {}: {e}", account.username))?;
        funded += 1;
    }
    Ok(funded)
}
