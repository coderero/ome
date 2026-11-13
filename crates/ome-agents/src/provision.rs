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
