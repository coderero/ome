use crate::strategies::Params;
use crate::world::World;
use ome_protocol::{GroupInfo, SimCommand};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const ALL: &str = "*";

pub struct GroupControl {
    pub name: String,
    pub strategy: String,
    pub broker: String,
    pub paused: AtomicBool,
    pub target: AtomicU32,
    generation: AtomicU64,
    params: Mutex<Params>,
}

impl GroupControl {
    pub fn new(name: &str, strategy: &str, broker: &str, target: u32, params: Params) -> Self {
        Self {
            name: name.into(),
            strategy: strategy.into(),
            broker: broker.into(),
            paused: AtomicBool::new(false),
            target: AtomicU32::new(target),
            generation: AtomicU64::new(0),
            params: Mutex::new(params),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    pub fn params(&self) -> Params {
        self.params.lock().expect("params lock").clone()
    }

    pub fn set_param(&self, key: &str, value: &str) {
        self.params
            .lock()
            .expect("params lock")
            .0
            .insert(key.into(), value.into());
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
pub struct AgentStats {
    pub fills: AtomicU64,
    pub equity: AtomicU64,
}

pub struct Shared {
    pub groups: BTreeMap<String, Arc<GroupControl>>,
    pub world: Mutex<World>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Followup {
    Fund { group: String, amount: u64 },
}

impl Shared {
    pub fn new(groups: BTreeMap<String, Arc<GroupControl>>) -> Self {
        Self {
            groups,
            world: Mutex::new(World::default()),
        }
    }

    fn matching(&self, group: &str) -> Vec<&Arc<GroupControl>> {
        self.groups
            .values()
            .filter(|g| group == ALL || g.name == group)
            .collect()
    }

    pub fn apply(&self, command: &SimCommand, now: Instant) -> Option<Followup> {
        match command {
            SimCommand::Pause { group } | SimCommand::Resume { group } => {
                let paused = matches!(command, SimCommand::Pause { .. });
                for g in self.matching(group) {
                    g.paused.store(paused, Ordering::Relaxed);
                }
            }
            SimCommand::SetCount { group, count } => {
                for g in self.matching(group) {
                    g.target.store(*count, Ordering::Relaxed);
                }
            }
            SimCommand::SetParam { group, key, value } => {
                for g in self.matching(group) {
                    g.set_param(key, value);
                }
            }
            SimCommand::Fund { group, amount } => {
                return Some(Followup::Fund {
                    group: group.clone(),
                    amount: *amount,
                });
            }
            SimCommand::SetFair { .. } | SimCommand::Scenario(_) => {
                self.world.lock().expect("world lock").apply(command, now)
            }
        }
        None
    }

    pub fn report(&self, stats: &BTreeMap<String, Vec<Arc<AgentStats>>>) -> Vec<GroupInfo> {
        self.groups
            .values()
            .map(|g| {
                let agents = stats.get(&g.name).map(Vec::as_slice).unwrap_or_default();
                GroupInfo {
                    name: g.name.clone(),
                    strategy: g.strategy.clone(),
                    broker: g.broker.clone(),
                    target: g.target.load(Ordering::Relaxed),
                    running: agents.len() as u32,
                    paused: g.is_paused(),
                    fills: agents.iter().map(|a| a.fills.load(Ordering::Relaxed)).sum(),
                    equity: agents
                        .iter()
                        .map(|a| a.equity.load(Ordering::Relaxed))
                        .sum(),
                    params: g
                        .params()
                        .0
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(" "),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Shared {
        let group = |name: &str| {
            (
                name.to_string(),
                Arc::new(GroupControl::new(
                    name,
                    "noise",
                    "retail-co",
                    3,
                    Params::default(),
                )),
            )
        };
        Shared::new([group("noise"), group("swing")].into())
    }

    #[test]
    fn commands_reach_the_matching_groups() {
        let shared = shared();
        let now = Instant::now();
        shared.apply(&SimCommand::Pause { group: ALL.into() }, now);
        assert!(shared.groups.values().all(|g| g.is_paused()));
        shared.apply(
            &SimCommand::Resume {
                group: "swing".into(),
            },
            now,
        );
        assert!(!shared.groups["swing"].is_paused() && shared.groups["noise"].is_paused());
        shared.apply(
            &SimCommand::SetParam {
                group: "noise".into(),
                key: "activity".into(),
                value: "0.9".into(),
            },
            now,
        );
        assert_eq!(shared.groups["noise"].generation(), 1);
        assert_eq!(shared.groups["noise"].params().f64("activity", 0.0), 0.9);
        shared.apply(
            &SimCommand::SetCount {
                group: "swing".into(),
                count: 7,
            },
            now,
        );
        assert_eq!(shared.groups["swing"].target.load(Ordering::Relaxed), 7);
        assert_eq!(
            shared.apply(
                &SimCommand::Fund {
                    group: "noise".into(),
                    amount: 100
                },
                now
            ),
            Some(Followup::Fund {
                group: "noise".into(),
                amount: 100
            })
        );
        let report = shared.report(&BTreeMap::new());
        assert_eq!(report.len(), 2);
        assert_eq!(report[0].params, "activity=0.9");
    }
}
