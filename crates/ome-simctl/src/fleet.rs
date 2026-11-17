use ome_core::price::parse_amount;
use ome_net::SessionId;
use ome_net::auth::valid_username;
use ome_protocol::Role;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSpec {
    pub name: String,
    pub line: String,
    pub broker: String,
    pub role: Role,
    pub cash: u64,
    pub count: u32,
    pub inflow_bps: u64,
}

pub fn parse_roster(text: &str, scale: f64) -> Result<Vec<GroupSpec>, String> {
    let mut groups: Vec<GroupSpec> = Vec::new();
    for (number, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let at = |why: String| format!("roster line {}: {why}", number + 1);
        let mut words = line.split_whitespace();
        if words.next() != Some("group") {
            return Err(at("lines start with 'group <name>'".into()));
        }
        let name = words.next().ok_or_else(|| at("missing group name".into()))?;
        if !valid_username(&format!("{name}-1")) {
            return Err(at(format!("'{name}' cannot name accounts")));
        }
        let (mut broker, mut role, mut cash, mut count, mut inflow_bps) = (None, None, 0, 1.0, 0);
        for word in words {
            let Some((key, value)) = word.split_once('=') else {
                return Err(at(format!("expected key=value, got '{word}'")));
            };
            match key {
                "broker" => broker = Some(value.to_string()),
                "role" => role = Some(Role::parse(value).ok_or_else(|| at(format!("bad role '{value}'")))?),
                "cash" => cash = parse_amount(value).ok_or_else(|| at(format!("bad amount '{value}'")))?,
                "count" => count = value.parse::<f64>().map_err(|_| at(format!("bad count '{value}'")))?,
                "inflow_bps" => inflow_bps = value.parse().map_err(|_| at(format!("bad inflow_bps '{value}'")))?,
                _ => {}
            }
        }
        if groups.iter().any(|g| g.name == name) {
            return Err(at(format!("group {name} listed twice")));
        }
        groups.push(GroupSpec {
            name: name.to_string(),
            line: line.to_string(),
            broker: broker.ok_or_else(|| at("missing broker=".into()))?,
            role: role.ok_or_else(|| at("missing role=".into()))?,
            cash,
            count: (count * scale).round().max(0.0) as u32,
            inflow_bps,
        });
    }
    Ok(groups)
}

#[derive(Debug, Default)]
pub struct Fleet {
    pub groups: Vec<GroupSpec>,
    assigned: BTreeMap<String, SessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Move {
    Open { group: usize, name: String },
    Assign { group: usize, name: String, runner: SessionId },
    Retire { name: String, runner: SessionId },
}

pub fn agent_key(broker: &str, name: &str) -> String {
    format!("agent:{broker}:{name}")
}

impl Fleet {
    pub fn new(groups: Vec<GroupSpec>) -> Self {
        Self { groups, assigned: BTreeMap::new() }
    }

    pub fn group(&self, name: &str) -> Option<&GroupSpec> {
        self.groups.iter().find(|g| g.name == name)
    }

    pub fn assigned(&self) -> usize {
        self.assigned.len()
    }

    pub fn runner_left(&mut self, runner: SessionId) -> usize {
        let before = self.assigned.len();
        self.assigned.retain(|_, r| *r != runner);
        before - self.assigned.len()
    }

    pub fn confirm(&mut self, mv: &Move) {
        match mv {
            Move::Assign { name, runner, .. } => {
                self.assigned.insert(name.clone(), *runner);
            }
            Move::Retire { name, .. } => {
                self.assigned.remove(name);
            }
            Move::Open { .. } => {}
        }
    }

    pub fn members(&self, group: &str) -> Vec<String> {
        self.assigned
            .keys()
            .filter(|name| group == "*" || name.rsplit_once('-').is_some_and(|(g, _)| g == group))
            .cloned()
            .collect()
    }

    pub fn plan(
        &self,
        target: impl Fn(&GroupSpec) -> u32,
        opened: impl Fn(&GroupSpec, &str) -> bool,
        runners: &[SessionId],
    ) -> Vec<Move> {
        let mut moves = Vec::new();
        let mut load: BTreeMap<SessionId, usize> = runners.iter().map(|r| (*r, 0)).collect();
        for runner in self.assigned.values() {
            if let Some(n) = load.get_mut(runner) {
                *n += 1;
            }
        }
        for (index, group) in self.groups.iter().enumerate() {
            let wanted = target(group);
            for n in 1..=wanted {
                let name = format!("{}-{n}", group.name);
                if self.assigned.contains_key(&name) {
                    continue;
                }
                if !opened(group, &name) {
                    moves.push(Move::Open { group: index, name });
                    continue;
                }
                if let Some((&runner, count)) = load.iter_mut().min_by_key(|(_, n)| **n) {
                    *count += 1;
                    moves.push(Move::Assign { group: index, name, runner });
                }
            }
            for (name, runner) in &self.assigned {
                let Some((g, n)) = name.rsplit_once('-') else { continue };
                if g == group.name && n.parse::<u32>().is_ok_and(|n| n > wanted) {
                    moves.push(Move::Retire { name: name.clone(), runner: *runner });
                }
            }
        }
        moves
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rosters_parse_and_the_fleet_opens_assigns_balances_and_retires() {
        let text = "
            group noise count=2 broker=retail-co role=retail strategy=noise cash=20000 seed=4000 inflow_bps=15
            group hft count=1 broker=dma-co role=hft strategy=market-maker cash=600000 # tier 1
        ";
        let groups = parse_roster(text, 1.0).unwrap();
        assert_eq!((groups[0].count, groups[0].cash, groups[1].role), (2, 20_000_00, Role::Hft));
        assert!(groups[0].line.contains("seed=4000"));
        assert_eq!((groups[0].inflow_bps, groups[1].inflow_bps), (15, 0));
        assert_eq!(parse_roster(text, 2.5).unwrap()[0].count, 5);
        assert!(parse_roster("group x role=retail", 1.0).is_err());
        assert!(parse_roster("agent x", 1.0).is_err());

        let mut fleet = Fleet::new(groups);
        let none_opened = fleet.plan(|g| g.count, |_, _| false, &[1]);
        assert_eq!(none_opened.len(), 3);
        assert!(none_opened.iter().all(|m| matches!(m, Move::Open { .. })));
        let moves = fleet.plan(|g| g.count, |_, _| true, &[1, 2]);
        let runners: Vec<SessionId> = moves
            .iter()
            .filter_map(|m| match m {
                Move::Assign { runner, .. } => Some(*runner),
                _ => None,
            })
            .collect();
        assert_eq!(runners, vec![1, 2, 1], "agents spread over the runners");
        for m in &moves {
            fleet.confirm(m);
        }
        assert!(fleet.plan(|g| g.count, |_, _| true, &[1, 2]).is_empty());
        let shrink = fleet.plan(|g| if g.name == "noise" { 1 } else { g.count }, |_, _| true, &[1, 2]);
        assert_eq!(shrink, vec![Move::Retire { name: "noise-2".into(), runner: 2 }]);
        assert_eq!(fleet.members("noise").len(), 2);
        assert_eq!(fleet.runner_left(1), 2);
        assert_eq!(fleet.assigned(), 1);
    }
}
