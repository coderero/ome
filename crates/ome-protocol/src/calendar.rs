use crate::types::{MarketPhase, MarketStatus};
use ome_core::types::Timestamp;

pub const PRESETS: &[(&str, [u64; 4], &str)] = &[
    ("real", [900, 22_500, 600, 62_400], "a full exchange day: pre-open 15 min, open 6 h 15 min, closing auction 10 min, closed overnight (24 h)"),
    ("compressed", [30, 540, 20, 10], "one trading day in 10 minutes"),
    ("fast", [5, 45, 5, 5], "one trading day a minute"),
];

pub fn preset(name: &str) -> Option<[u64; 4]> {
    PRESETS.iter().find(|(n, _, _)| *n == name).map(|(_, s, _)| *s)
}

const PRE_OPEN: u32 = 9 * 60;
const OPEN: u32 = 9 * 60 + 15;
const CLOSE: u32 = 15 * 60 + 30;
const CLOSED: u32 = 15 * 60 + 40;
const DAY: u32 = 24 * 60;
const WEEKDAYS: [&str; 5] = ["Mon", "Tue", "Wed", "Thu", "Fri"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimClock {
    pub day: u64,
    pub minute: u32,
}

impl SimClock {
    pub fn weekday(&self) -> &'static str {
        WEEKDAYS[(self.day.max(1) - 1) as usize % 5]
    }

    pub fn label(&self) -> String {
        format!("{} {:02}:{:02} · day {}", self.weekday(), self.minute / 60, self.minute % 60, self.day)
    }
}

pub fn sim_clock(status: &MarketStatus, now: Timestamp) -> Option<SimClock> {
    if status.next_change_at <= status.since {
        return None;
    }
    let (from, to) = match status.phase {
        MarketPhase::PreOpen => (PRE_OPEN, OPEN),
        MarketPhase::Open => (OPEN, CLOSE),
        MarketPhase::ClosingAuction => (CLOSE, CLOSED),
        MarketPhase::Closed => (CLOSED, DAY + PRE_OPEN),
    };
    let span = (status.next_change_at - status.since) as f64;
    let done = (now.saturating_sub(status.since) as f64 / span).clamp(0.0, 1.0);
    let minute = from + ((to - from) as f64 * done) as u32;
    Some(SimClock { day: status.day, minute: minute % DAY })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_maps_onto_exchange_hours_and_presets_add_up() {
        let status = |phase, since, next| MarketStatus { phase, day: 3, since, next_change_at: next };
        let open = sim_clock(&status(MarketPhase::Open, 0, 45_000), 22_500).unwrap();
        assert_eq!((open.minute / 60, open.weekday()), (12, "Wed"));
        assert_eq!(sim_clock(&status(MarketPhase::PreOpen, 0, 5_000), 0).unwrap().label(), "Wed 09:00 · day 3");
        assert_eq!(sim_clock(&status(MarketPhase::Closed, 0, 10), 10).unwrap().minute, PRE_OPEN, "the night ends at the next pre-open");
        assert_eq!(sim_clock(&status(MarketPhase::Open, 0, 0), 5), None, "no calendar, no clock");
        assert_eq!(preset("real").unwrap().iter().sum::<u64>(), 86_400);
        assert_eq!(preset("compressed").unwrap().iter().sum::<u64>(), 600);
        assert_eq!(preset("fast").unwrap().iter().sum::<u64>(), 60);
        assert!(preset("slow").is_none());
    }
}
