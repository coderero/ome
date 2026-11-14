use crate::journal::Mark;
use ome_core::price::format_price;
use ome_core::types::Timestamp;
use ome_protocol::{RecordingInfo, SimCommand};
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Command(SimCommand),
    Accelerate([u64; 4]),
}

impl Step {
    pub fn describe(&self) -> String {
        match self {
            Step::Command(command) => command.describe(),
            Step::Accelerate([a, b, c, d]) => {
                format!("session speed {a}s pre-open / {b}s open / {c}s closing / {d}s closed")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    pub name: String,
    pub started_at: Timestamp,
    pub stopped_at: Option<Timestamp>,
    pub baseline: Vec<SimCommand>,
    pub steps: Vec<(Timestamp, Step)>,
    pub start_marks: Vec<Mark>,
    pub stop_marks: Vec<Mark>,
}

pub fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("a recording name is 1 - 64 characters".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("a recording name uses letters, digits, '-' and '_'".into());
    }
    if name == "stop" {
        return Err("'stop' is reserved".into());
    }
    Ok(())
}

impl Recording {
    pub fn tape(&self) -> String {
        self.start_marks
            .iter()
            .map(|start| {
                let end = self.stop_marks.iter().find(|m| m.symbol == start.symbol);
                match end {
                    Some(end) => format!(
                        "{} {} -> {} (volume {} -> {})",
                        start.symbol,
                        format_price(start.last),
                        format_price(end.last),
                        start.volume,
                        end.volume
                    ),
                    None => format!(
                        "{} {} (volume {})",
                        start.symbol,
                        format_price(start.last),
                        start.volume
                    ),
                }
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn timeline(&self) -> String {
        let baseline = self
            .baseline
            .iter()
            .map(|c| format!("  +0.0s baseline: {}", c.describe()));
        let steps = self.steps.iter().map(|(offset, step)| {
            format!(
                "  +{}.{}s {}",
                offset / 1_000,
                offset % 1_000 / 100,
                step.describe()
            )
        });
        baseline.chain(steps).collect::<Vec<_>>().join("\n")
    }

    pub fn info(&self, replay: Option<&Replay>, detailed: bool) -> RecordingInfo {
        let replaying = replay.filter(|r| r.name == self.name);
        RecordingInfo {
            name: self.name.clone(),
            started_at: self.started_at,
            stopped_at: self.stopped_at.unwrap_or(0),
            steps: self.steps.len() as u32,
            replaying: replaying.is_some(),
            issued: replaying.map_or(0, |r| r.issued),
            tape: self.tape(),
            timeline: if detailed {
                self.timeline()
            } else {
                String::new()
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    pub name: String,
    pub total: u32,
    pub issued: u32,
    queue: VecDeque<(Timestamp, Step)>,
}

impl Replay {
    pub fn new(recording: &Recording, now: Timestamp, speed_pct: u32) -> Self {
        let speed = u64::from(speed_pct.max(1));
        let queue: VecDeque<(Timestamp, Step)> = recording
            .baseline
            .iter()
            .map(|c| (now, Step::Command(c.clone())))
            .chain(
                recording
                    .steps
                    .iter()
                    .map(|(offset, step)| (now + offset * 100 / speed, step.clone())),
            )
            .collect();
        Self {
            name: recording.name.clone(),
            total: queue.len() as u32,
            issued: 0,
            queue,
        }
    }

    pub fn due(&mut self, now: Timestamp) -> Vec<Step> {
        let mut steps = Vec::new();
        while self.queue.front().is_some_and(|(at, _)| *at <= now) {
            if let Some((_, step)) = self.queue.pop_front() {
                steps.push(step);
            }
        }
        self.issued += steps.len() as u32;
        steps
    }

    pub fn finished(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ome_core::symbol::Symbol;
    use ome_protocol::Scenario;

    fn acme() -> Symbol {
        Symbol::parse("ACME").unwrap()
    }

    fn recording() -> Recording {
        Recording {
            name: "drill".into(),
            started_at: 1_000,
            stopped_at: Some(61_000),
            baseline: vec![SimCommand::Pause {
                group: "noise".into(),
            }],
            steps: vec![
                (
                    10_000,
                    Step::Command(SimCommand::Scenario(Scenario::FlashCrash {
                        symbol: acme(),
                        seconds: 20,
                    })),
                ),
                (40_000, Step::Accelerate([3, 30, 3, 3])),
            ],
            start_marks: vec![Mark {
                symbol: acme(),
                last: 25_00,
                volume: 100,
            }],
            stop_marks: vec![Mark {
                symbol: acme(),
                last: 23_10,
                volume: 900,
            }],
        }
    }

    #[test]
    fn replay_keeps_the_original_spacing_scaled_by_speed() {
        let rec = recording();
        let mut replay = Replay::new(&rec, 500_000, 200);
        assert_eq!(replay.total, 3);
        assert_eq!(
            replay.due(500_000),
            vec![Step::Command(SimCommand::Pause {
                group: "noise".into()
            })],
            "the baseline settings come first"
        );
        assert!(replay.due(504_999).is_empty());
        assert!(matches!(
            replay.due(505_000)[..],
            [Step::Command(SimCommand::Scenario(_))]
        ));
        assert!(!replay.finished());
        assert_eq!(replay.due(600_000), vec![Step::Accelerate([3, 30, 3, 3])]);
        assert!(replay.finished());
        assert_eq!(replay.issued, 3);
    }

    #[test]
    fn recordings_describe_their_tape_and_timeline() {
        let rec = recording();
        assert_eq!(rec.tape(), "ACME 25.00 -> 23.10 (volume 100 -> 900)");
        let timeline = rec.timeline();
        assert!(timeline.contains("+0.0s baseline: paused noise"));
        assert!(timeline.contains("+10.0s flash crash in ACME for 20s"));
        assert!(timeline.contains("+40.0s session speed 3s pre-open"));
        let info = rec.info(None, false);
        assert_eq!(
            (info.steps, info.replaying, info.timeline.as_str()),
            (2, false, "")
        );
        assert!(valid_name("crash-drill_2").is_ok());
        assert!(
            valid_name("stop").is_err() && valid_name("a b").is_err() && valid_name("").is_err()
        );
    }
}
