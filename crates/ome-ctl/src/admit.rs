use crate::launch::Secrets;
use crate::topology::{Kind, Service, Topology};
use ome_client::{Client, ClientError};
use ome_core::price::parse_amount;
use ome_protocol::{Application, MemberKind, MemberMessage, ServerMessage};
use std::collections::BTreeSet;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(3);

pub fn member_kind(service: &Service) -> Option<MemberKind> {
    match service.kind {
        Kind::Broker if service.get("profile") == Some("lp") => Some(MemberKind::LiquidityProvider),
        Kind::Broker => Some(MemberKind::Broker),
        Kind::Clearing => Some(MemberKind::Clearing),
        Kind::Venue => Some(match service.get("member") {
            Some("broker") => MemberKind::Broker,
            Some("lp") => MemberKind::LiquidityProvider,
            _ if service.get("kind") == Some("ats") => MemberKind::Broker,
            _ => MemberKind::LiquidityProvider,
        }),
        _ => None,
    }
}

fn collateral(topology: &Topology, service: &Service) -> u64 {
    let text = service
        .get("collateral")
        .map(str::to_string)
        .unwrap_or_else(|| topology.setting("collateral", "2000000"));
    parse_amount(&text).unwrap_or(0)
}

fn ops(addr: &str, secret: &str) -> Option<Client> {
    Client::connect_member(addr, "ops", secret).ok().map(|(c, _)| c)
}

fn applications(client: &mut Client) -> Vec<Application> {
    if client.send_member(&MemberMessage::QueryApplications).is_err() {
        return Vec::new();
    }
    match client.wait_for(WAIT, |m| matches!(m, ServerMessage::Applications(_) | ServerMessage::Reject { .. })) {
        Ok(ServerMessage::Applications(list)) => list,
        _ => Vec::new(),
    }
}

fn approve(client: &mut Client, name: &str, amount: u64) -> Result<String, ClientError> {
    client.request_member(&MemberMessage::Decide { name: name.into(), approve: true, amount, note: "declared in the topology".into() })
}

pub struct Admissions {
    done: BTreeSet<String>,
    noted: BTreeSet<String>,
}

impl Default for Admissions {
    fn default() -> Self {
        Self::new()
    }
}

impl Admissions {
    pub fn new() -> Self {
        Self { done: BTreeSet::new(), noted: BTreeSet::new() }
    }

    fn note(&mut self, out: &mut Vec<String>, key: String, text: String) {
        if self.noted.insert(key) {
            out.push(text);
        }
    }

    pub fn pass(&mut self, topology: &Topology, secrets: &Secrets) -> Vec<String> {
        let mut out = Vec::new();
        let declared = |name: &str| topology.find(name);
        if let Some(exchange) = topology.first(Kind::Exchange)
            && let Some(mut client) = ops(&topology.addr(exchange), &secrets.exchange)
        {
            for app in applications(&mut client) {
                match declared(&app.name).and_then(member_kind) {
                    Some(kind) if kind == app.kind => match approve(&mut client, &app.name, 0) {
                        Ok(text) => out.push(format!("exchange: {text}")),
                        Err(e) => self.note(&mut out, format!("x:{}", app.name), format!("exchange: {} not admitted yet: {e}", app.name)),
                    },
                    Some(kind) => self.note(
                        &mut out,
                        format!("xk:{}", app.name),
                        format!("exchange: {} applied as {} but the topology declares {}; left for review", app.name, app.kind.label(), kind.label()),
                    ),
                    None => self.note(
                        &mut out,
                        format!("xu:{}", app.name),
                        format!("exchange: {} is not in the topology; its application waits for `ome exchange approve {}`", app.name, app.name),
                    ),
                }
            }
        }
        if let Some(clearing) = topology.first(Kind::Clearing)
            && let Some(mut client) = ops(&topology.addr(clearing), &secrets.clearing)
        {
            for app in applications(&mut client) {
                let Some(service) = declared(&app.name).filter(|s| s.kind == Kind::Broker) else {
                    self.note(&mut out, format!("cu:{}", app.name), format!("clearing: {} is not a declared broker; left for review", app.name));
                    continue;
                };
                match approve(&mut client, &app.name, collateral(topology, service)) {
                    Ok(text) => out.push(format!("clearing: {text}")),
                    Err(e) => self.note(&mut out, format!("c:{}:{e}", app.name), format!("clearing: {} not admitted yet: {e}", app.name)),
                }
            }
            for venue in topology.services.iter().filter(|s| s.kind == Kind::Venue && s.get("collateral").is_some()) {
                let key = format!("clearing-direct:{}", venue.name);
                if self.done.contains(&key) {
                    continue;
                }
                match client.request_member(&MemberMessage::CreateMember {
                    kind: MemberKind::Broker,
                    name: venue.name.clone(),
                    secret: secrets.member.clone(),
                }) {
                    Ok(text) => {
                        out.push(format!("clearing: {text}"));
                        if let Some(id) = text.split_whitespace().nth(2).and_then(|t| t.parse::<u64>().ok()) {
                            let amount = collateral(topology, venue) as i64;
                            if let Ok(text) = client.request_member(&MemberMessage::Deposit { member: id, amount }) {
                                out.push(format!("clearing: {text}"));
                            }
                        }
                        self.done.insert(key);
                    }
                    Err(ClientError::Rejected(reason)) if reason.contains("already") => {
                        self.done.insert(key);
                    }
                    Err(_) => {}
                }
            }
        }
        for venue in topology.services.iter().filter(|s| s.kind == Kind::Venue) {
            let Some(mut client) = ops(&topology.addr(venue), &secrets.venue) else { continue };
            for app in applications(&mut client) {
                let routes = declared(&app.name).is_some_and(|b| {
                    b.kind == Kind::Broker && [b.get("wholesaler"), b.get("ats")].contains(&Some(venue.name.as_str()))
                });
                if !routes {
                    self.note(&mut out, format!("v:{}:{}", venue.name, app.name), format!("{}: {} does not route here in the topology; left for review", venue.name, app.name));
                    continue;
                }
                if let Ok(text) = approve(&mut client, &app.name, 0) {
                    out.push(format!("{}: {text}", venue.name));
                }
            }
        }
        if let Some(news) = topology.first(Kind::Newsroom)
            && let Some(mut client) = ops(&topology.addr(news), &secrets.news)
        {
            for app in applications(&mut client) {
                let subscribes = declared(&app.name).is_some_and(|b| b.kind == Kind::Broker && b.get("news") != Some("no"));
                if !subscribes {
                    self.note(&mut out, format!("n:{}", app.name), format!("newsroom: {} is not a declared subscriber; left for review", app.name));
                    continue;
                }
                if let Ok(text) = approve(&mut client, &app.name, 0) {
                    out.push(format!("newsroom: {text}"));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_services_map_to_the_member_kind_they_apply_as() {
        let t = Topology::parse(
            "exchange e port=1\nbroker lp-co port=2 profile=lp\nbroker retail-co port=3 profile=retail\nvenue ats-co port=4 kind=ats\nvenue w port=5 kind=wholesaler\nclearing ccp port=6",
        )
        .unwrap();
        let kind = |n: &str| member_kind(t.find(n).unwrap());
        assert_eq!(kind("lp-co"), Some(MemberKind::LiquidityProvider));
        assert_eq!(kind("retail-co"), Some(MemberKind::Broker));
        assert_eq!(kind("ats-co"), Some(MemberKind::Broker));
        assert_eq!(kind("w"), Some(MemberKind::LiquidityProvider));
        assert_eq!(kind("ccp"), Some(MemberKind::Clearing));
        assert_eq!(kind("e"), None);
        assert_eq!(collateral(&t, t.find("retail-co").unwrap()), 2_000_000_00);
    }
}
