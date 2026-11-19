use crate::journal::Fact;
use crate::recording::{Recording, Step, valid_name};
use ome_core::price::Price;
use ome_core::symbol::Symbol;
use ome_core::types::Timestamp;
use ome_protocol::{GroupInfo, Scenario, SimCommand, SimStatus};
use crate::world::World;
use std::collections::{BTreeMap, BTreeSet};

pub const ALL: &str = "*";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupSettings {
    pub paused: Option<bool>,
    pub count: Option<u32>,
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fair {
    pub price: Price,
    pub drift_bps: i64,
    pub set_at: Timestamp,
}

impl Fair {
    pub fn at(&self, now: Timestamp) -> Price {
        let minutes = now.saturating_sub(self.set_at) as f64 / 60_000.0;
        let factor = (1.0 + self.drift_bps as f64 / 10_000.0).powf(minutes);
        (self.price as f64 * factor).round().max(1.0) as Price
    }
}

pub struct SimState {
    groups: BTreeMap<String, GroupSettings>,
    fair: BTreeMap<Symbol, Fair>,
    scenarios: Vec<(Scenario, Timestamp)>,
    recordings: BTreeMap<String, Recording>,
    recording: Option<String>,
    world: World,
    provisioned: BTreeSet<String>,
}

impl Default for SimState {
    fn default() -> Self {
        Self::new(World::new(Vec::new(), 20))
    }
}

pub fn validate(command: &SimCommand) -> Result<(), String> {
    let group_ok = |g: &str| {
        if g.is_empty() {
            Err("a group name is required".to_string())
        } else {
            Ok(())
        }
    };
    match command {
        SimCommand::Pause { group } | SimCommand::Resume { group } => group_ok(group),
        SimCommand::SetCount { group, count } => {
            group_ok(group)?;
            if group == ALL {
                return Err("scale one group at a time".into());
            }
            if *count > 1_000 {
                return Err("at most 1000 agents per group".into());
            }
            Ok(())
        }
        SimCommand::SetParam { group, key, .. } => {
            group_ok(group)?;
            if key.is_empty() {
                return Err("a parameter name is required".into());
            }
            Ok(())
        }
        SimCommand::Fund { group, amount } => {
            group_ok(group)?;
            if *amount == 0 {
                return Err("fund a positive amount".into());
            }
            Ok(())
        }
        SimCommand::SetFair { price, .. } if *price == 0 => {
            Err("fair value must be positive".into())
        }
        SimCommand::SetFair { drift_bps, .. } if drift_bps.abs() > 1_000 => {
            Err("drift is limited to 10% per minute".into())
        }
        SimCommand::SetFair { .. } => Ok(()),
        SimCommand::Scenario(s) if s.seconds() == 0 || s.seconds() > 3_600 => {
            Err("scenarios last 1 - 3600 seconds".into())
        }
        SimCommand::Scenario(Scenario::NewsShock { move_bps, .. }) if move_bps.abs() > 5_000 => {
            Err("a news shock moves fair value by at most 50%".into())
        }
        SimCommand::Scenario(_) => Ok(()),
    }
}

impl SimState {
    pub fn new(world: World) -> Self {
        Self {
            groups: BTreeMap::new(),
            fair: BTreeMap::new(),
            scenarios: Vec::new(),
            recordings: BTreeMap::new(),
            recording: None,
            world,
            provisioned: BTreeSet::new(),
        }
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn provisioned(&self, key: &str) -> bool {
        self.provisioned.contains(key)
    }

    pub fn provisioned_count(&self, prefix: &str) -> usize {
        self.provisioned.iter().filter(|k| k.starts_with(prefix)).count()
    }

    pub fn group_count(&self, group: &str) -> Option<u32> {
        self.groups.get(group).and_then(|g| g.count)
    }

    pub fn check(&self, fact: &Fact) -> Result<(), String> {
        match fact {
            Fact::World(_) | Fact::Provisioned { .. } => Ok(()),
            Fact::Command(command) | Fact::Forwarded(command) => validate(command),
            Fact::Accelerated(_) => Ok(()),
            Fact::RecordingStarted { name, .. } => {
                valid_name(name)?;
                if let Some(active) = &self.recording {
                    return Err(format!("already recording '{active}'"));
                }
                if self.recordings.contains_key(name) {
                    return Err(format!("a recording named '{name}' exists"));
                }
                Ok(())
            }
            Fact::RecordingStopped { .. } => match &self.recording {
                Some(_) => Ok(()),
                None => Err("nothing is being recorded".into()),
            },
        }
    }

    pub fn apply_fact(&mut self, fact: &Fact, at: Timestamp) {
        let step = match fact {
            Fact::Command(command) => {
                self.apply(command, at);
                Some(Step::Command(command.clone()))
            }
            Fact::Accelerated(seconds) => Some(Step::Accelerate(*seconds)),
            Fact::Forwarded(command) => Some(Step::Command(command.clone())),
            Fact::RecordingStarted { name, marks } => {
                let recording = Recording {
                    name: name.clone(),
                    started_at: at,
                    stopped_at: None,
                    baseline: self.replay(at),
                    steps: Vec::new(),
                    start_marks: marks.clone(),
                    stop_marks: Vec::new(),
                };
                self.recordings.insert(name.clone(), recording);
                self.recording = Some(name.clone());
                None
            }
            Fact::World(fact) => {
                self.world.apply(fact);
                None
            }
            Fact::Provisioned { key } => {
                self.provisioned.insert(key.clone());
                None
            }
            Fact::RecordingStopped { marks } => {
                if let Some(name) = self.recording.take()
                    && let Some(recording) = self.recordings.get_mut(&name)
                {
                    recording.stopped_at = Some(at);
                    recording.stop_marks = marks.clone();
                }
                None
            }
        };
        if let Some(step) = step
            && let Some(name) = &self.recording
            && let Some(recording) = self.recordings.get_mut(name)
        {
            recording
                .steps
                .push((at.saturating_sub(recording.started_at), step));
        }
    }

    pub fn recording(&self) -> Option<&str> {
        self.recording.as_deref()
    }

    pub fn recordings(&self) -> &BTreeMap<String, Recording> {
        &self.recordings
    }

    pub fn apply(&mut self, command: &SimCommand, at: Timestamp) {
        match command {
            SimCommand::Pause { group } | SimCommand::Resume { group } => {
                let paused = matches!(command, SimCommand::Pause { .. });
                if group == ALL {
                    for settings in self.groups.values_mut() {
                        settings.paused = None;
                    }
                }
                self.groups.entry(group.clone()).or_default().paused = Some(paused);
            }
            SimCommand::SetCount { group, count } => {
                self.groups.entry(group.clone()).or_default().count = Some(*count);
            }
            SimCommand::SetParam { group, key, value } => {
                self.groups
                    .entry(group.clone())
                    .or_default()
                    .params
                    .insert(key.clone(), value.clone());
            }
            SimCommand::Fund { .. } => {}
            SimCommand::SetFair {
                symbol,
                price,
                drift_bps,
            } => {
                self.fair.insert(
                    *symbol,
                    Fair {
                        price: *price,
                        drift_bps: *drift_bps,
                        set_at: at,
                    },
                );
            }
            SimCommand::Scenario(scenario) => {
                if let Scenario::NewsShock {
                    symbol, move_bps, ..
                } = scenario
                    && let Some(fair) = self.fair.get_mut(symbol)
                {
                    let moved = fair.at(at) as f64 * (1.0 + *move_bps as f64 / 10_000.0);
                    *fair = Fair {
                        price: moved.round().max(1.0) as Price,
                        drift_bps: fair.drift_bps,
                        set_at: at,
                    };
                }
                self.scenarios
                    .push((*scenario, at + scenario.seconds() * 1_000));
            }
        }
    }

    pub fn active(&self, now: Timestamp) -> Vec<(Scenario, Timestamp)> {
        self.scenarios
            .iter()
            .filter(|(_, until)| *until > now)
            .copied()
            .collect()
    }

    pub fn replay(&self, now: Timestamp) -> Vec<SimCommand> {
        let mut commands = Vec::new();
        let ordered = self
            .groups
            .iter()
            .filter(|(g, _)| *g == ALL)
            .chain(self.groups.iter().filter(|(g, _)| *g != ALL));
        for (group, settings) in ordered {
            if let Some(paused) = settings.paused {
                commands.push(if paused {
                    SimCommand::Pause {
                        group: group.clone(),
                    }
                } else {
                    SimCommand::Resume {
                        group: group.clone(),
                    }
                });
            }
            if let Some(count) = settings.count {
                commands.push(SimCommand::SetCount {
                    group: group.clone(),
                    count,
                });
            }
            for (key, value) in &settings.params {
                commands.push(SimCommand::SetParam {
                    group: group.clone(),
                    key: key.clone(),
                    value: value.clone(),
                });
            }
        }
        for (symbol, fair) in &self.fair {
            commands.push(SimCommand::SetFair {
                symbol: *symbol,
                price: fair.at(now),
                drift_bps: fair.drift_bps,
            });
        }
        for (scenario, until) in self.active(now) {
            if let Scenario::NewsShock {
                symbol, seconds, ..
            } = scenario
                && self.fair.contains_key(&symbol)
            {
                let remaining = (until - now).div_ceil(1_000).min(seconds);
                commands.push(SimCommand::Scenario(Scenario::NewsShock {
                    symbol,
                    move_bps: 0,
                    seconds: remaining,
                }));
                continue;
            }
            commands.push(SimCommand::Scenario(
                scenario.with_seconds((until - now).div_ceil(1_000)),
            ));
        }
        commands
    }

    pub fn status(&self, reports: &[Vec<GroupInfo>], now: Timestamp) -> SimStatus {
        let mut merged: BTreeMap<String, GroupInfo> = BTreeMap::new();
        for info in reports.iter().flatten() {
            merged
                .entry(info.name.clone())
                .and_modify(|g| {
                    g.target += info.target;
                    g.running += info.running;
                    g.fills += info.fills;
                    g.equity += info.equity;
                    g.traded += info.traded;
                    g.day_fills += info.day_fills;
                    g.day_traded += info.day_traded;
                    g.paused |= info.paused;
                })
                .or_insert_with(|| info.clone());
        }
        SimStatus {
            runners: reports.len() as u32,
            groups: merged.into_values().collect(),
            fair: self
                .fair
                .iter()
                .map(|(s, f)| (*s, f.at(now), f.drift_bps))
                .collect(),
            scenarios: self
                .active(now)
                .into_iter()
                .map(|(s, until)| (s.describe(), until))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    #[test]
    fn replay_compacts_settings_with_the_wildcard_first() {
        let mut state = SimState::default();
        state.apply(
            &SimCommand::Pause {
                group: "noise".into(),
            },
            0,
        );
        state.apply(&SimCommand::Pause { group: ALL.into() }, 1);
        state.apply(
            &SimCommand::Resume {
                group: "dmm-desk".into(),
            },
            2,
        );
        state.apply(
            &SimCommand::SetCount {
                group: "swing".into(),
                count: 30,
            },
            3,
        );
        state.apply(
            &SimCommand::SetParam {
                group: "swing".into(),
                key: "activity".into(),
                value: "0.3".into(),
            },
            4,
        );
        let replay = state.replay(10);
        assert_eq!(replay[0], SimCommand::Pause { group: ALL.into() });
        assert!(replay.contains(&SimCommand::Resume {
            group: "dmm-desk".into()
        }));
        assert!(
            !replay.contains(&SimCommand::Pause {
                group: "noise".into()
            }),
            "superseded by *"
        );
        assert!(replay.contains(&SimCommand::SetCount {
            group: "swing".into(),
            count: 30
        }));
    }

    #[test]
    fn fair_values_drift_and_news_moves_them_while_scenarios_expire() {
        let mut state = SimState::default();
        state.apply(
            &SimCommand::SetFair {
                symbol: acme(),
                price: 100_00,
                drift_bps: 100,
            },
            0,
        );
        assert_eq!(state.fair[&acme()].at(60_000), 101_00);
        state.apply(
            &SimCommand::Scenario(Scenario::NewsShock {
                symbol: acme(),
                move_bps: -1_000,
                seconds: 30,
            }),
            60_000,
        );
        assert_eq!(state.fair[&acme()].at(60_000), 90_90);
        state.apply(
            &SimCommand::Scenario(Scenario::LiquidityDrought { seconds: 10 }),
            60_000,
        );
        let replay = state.replay(65_000);
        assert!(
            replay.contains(&SimCommand::Scenario(Scenario::LiquidityDrought {
                seconds: 5
            }))
        );
        assert!(
            replay.contains(&SimCommand::Scenario(Scenario::NewsShock {
                symbol: acme(),
                move_bps: 0,
                seconds: 25
            })),
            "the fair value already carries the move, so replay only restores sentiment"
        );
        assert_eq!(state.active(200_000), vec![]);
        let status = state.status(&[], 60_000);
        assert_eq!(status.fair, vec![(acme(), 90_90, 100)]);
    }

    #[test]
    fn commands_are_validated_before_they_are_recorded() {
        assert!(
            validate(&SimCommand::SetCount {
                group: ALL.into(),
                count: 3
            })
            .is_err()
        );
        assert!(
            validate(&SimCommand::SetCount {
                group: "x".into(),
                count: 5_000
            })
            .is_err()
        );
        assert!(
            validate(&SimCommand::Fund {
                group: "x".into(),
                amount: 0
            })
            .is_err()
        );
        assert!(
            validate(&SimCommand::Scenario(Scenario::VolatilityBurst {
                seconds: 0
            }))
            .is_err()
        );
        assert!(
            validate(&SimCommand::Pause {
                group: String::new()
            })
            .is_err()
        );
        assert!(
            validate(&SimCommand::Pause {
                group: "noise".into()
            })
            .is_ok()
        );
    }

    #[test]
    fn recordings_capture_a_baseline_and_the_timeline_between_start_and_stop() {
        let mut state = SimState::default();
        let pause = SimCommand::Pause {
            group: "noise".into(),
        };
        state.apply_fact(&Fact::Command(pause.clone()), 1_000);
        let start = Fact::RecordingStarted {
            name: "drill".into(),
            marks: vec![],
        };
        assert!(
            state
                .check(&Fact::RecordingStopped { marks: vec![] })
                .is_err()
        );
        assert!(state.check(&start).is_ok());
        state.apply_fact(&start, 5_000);
        assert!(state.check(&start).is_err(), "one recording at a time");
        let crash = SimCommand::Scenario(Scenario::FlashCrash {
            symbol: acme(),
            seconds: 20,
        });
        state.apply_fact(&Fact::Command(crash.clone()), 17_500);
        state.apply_fact(&Fact::Accelerated([1, 20, 1, 1]), 20_000);
        state.apply_fact(&Fact::RecordingStopped { marks: vec![] }, 30_000);
        state.apply_fact(&Fact::Command(pause.clone()), 40_000);
        assert_eq!(state.recording(), None);
        let drill = &state.recordings()["drill"];
        assert_eq!(drill.baseline, vec![pause]);
        assert_eq!(
            drill.steps,
            vec![
                (12_500, Step::Command(crash)),
                (15_000, Step::Accelerate([1, 20, 1, 1]))
            ]
        );
        assert_eq!(drill.stopped_at, Some(30_000));
        assert!(state.check(&start).is_err(), "names are unique");
    }

    #[test]
    fn status_merges_reports_from_every_runner() {
        let info = |running| GroupInfo {
            name: "noise".into(),
            strategy: "noise".into(),
            broker: "retail-co".into(),
            target: 5,
            running,
            paused: false,
            fills: 10,
            equity: 100,
            traded: 5_000,
            day_fills: 2,
            day_traded: 900,
            params: String::new(),
        };
        let status = SimState::default().status(&[vec![info(5)], vec![info(4)]], 0);
        assert_eq!(status.runners, 2);
        assert_eq!((status.groups[0].target, status.groups[0].running), (10, 9));
    }
}
