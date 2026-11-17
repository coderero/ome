use crate::strategies::Params;
use crate::world::World;
use ome_protocol::{GroupInfo, SimCommand};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
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
    groups: RwLock<BTreeMap<String, Arc<GroupControl>>>,
    history: Mutex<Vec<SimCommand>>,
    pub world: Mutex<World>,
}

const HISTORY: usize = 2_000;

impl Shared {
    pub fn new(groups: BTreeMap<String, Arc<GroupControl>>) -> Self {
        Self {
            groups: RwLock::new(groups),
            history: Mutex::new(Vec::new()),
            world: Mutex::new(World::default()),
        }
    }

    pub fn group(&self, name: &str) -> Option<Arc<GroupControl>> {
        self.groups.read().expect("groups lock").get(name).cloned()
    }

    pub fn ensure(&self, name: &str, strategy: &str, broker: &str, params: Params) -> Arc<GroupControl> {
        if let Some(group) = self.group(name) {
            return group;
        }
        let group = Arc::new(GroupControl::new(name, strategy, broker, 0, params));
        for command in self.history.lock().expect("history lock").iter() {
            Self::apply_to(&group, command);
        }
        self.groups
            .write()
            .expect("groups lock")
            .entry(name.to_string())
            .or_insert(group)
            .clone()
    }

    fn matching(&self, group: &str) -> Vec<Arc<GroupControl>> {
        self.groups
            .read()
            .expect("groups lock")
            .values()
            .filter(|g| group == ALL || g.name == group)
            .cloned()
            .collect()
    }

    fn apply_to(g: &GroupControl, command: &SimCommand) {
        match command {
            SimCommand::Pause { group } | SimCommand::Resume { group } if group == ALL || *group == g.name => {
                g.paused.store(matches!(command, SimCommand::Pause { .. }), Ordering::Relaxed);
            }
            SimCommand::SetParam { group, key, value } if group == ALL || *group == g.name => g.set_param(key, value),
            _ => {}
        }
    }

    pub fn apply(&self, command: &SimCommand, now: Instant) {
        if matches!(command, SimCommand::Pause { .. } | SimCommand::Resume { .. } | SimCommand::SetParam { .. }) {
            let mut history = self.history.lock().expect("history lock");
            if history.len() >= HISTORY {
                history.remove(0);
            }
            history.push(command.clone());
        }
        match command {
            SimCommand::Pause { group } | SimCommand::Resume { group } => {
                let paused = matches!(command, SimCommand::Pause { .. });
                for g in self.matching(group) {
                    g.paused.store(paused, Ordering::Relaxed);
                }
            }
            SimCommand::SetCount { .. } | SimCommand::Fund { .. } => {}
            SimCommand::SetParam { group, key, value } => {
                for g in self.matching(group) {
                    g.set_param(key, value);
                }
            }
            SimCommand::SetFair { .. } | SimCommand::Scenario(_) => {
                self.world.lock().expect("world lock").apply(command, now)
            }
        }
    }

    pub fn report(&self, stats: &BTreeMap<String, Vec<Arc<AgentStats>>>) -> Vec<GroupInfo> {
        self.groups
            .read()
            .expect("groups lock")
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
    fn commands_reach_the_matching_groups_and_groups_created_later() {
        let shared = shared();
        let now = Instant::now();
        shared.apply(&SimCommand::Pause { group: ALL.into() }, now);
        assert!(shared.matching(ALL).iter().all(|g| g.is_paused()));
        shared.apply(&SimCommand::Resume { group: "swing".into() }, now);
        assert!(!shared.group("swing").unwrap().is_paused() && shared.group("noise").unwrap().is_paused());
        shared.apply(
            &SimCommand::SetParam { group: "noise".into(), key: "activity".into(), value: "0.9".into() },
            now,
        );
        assert_eq!(shared.group("noise").unwrap().generation(), 1);
        assert_eq!(shared.group("noise").unwrap().params().f64("activity", 0.0), 0.9);
        shared.apply(&SimCommand::SetParam { group: "late".into(), key: "gamma".into(), value: "0.4".into() }, now);
        let late = shared.ensure("late", "market-maker", "dma-co", Params::default());
        assert!(late.is_paused(), "settings sent before the group existed still apply");
        assert_eq!(late.params().f64("gamma", 0.0), 0.4);
        assert!(Arc::ptr_eq(&late, &shared.ensure("late", "market-maker", "dma-co", Params::default())));
        let report = shared.report(&BTreeMap::new());
        assert_eq!(report.len(), 3);
        assert_eq!(report[1].params, "activity=0.9");
    }
}
