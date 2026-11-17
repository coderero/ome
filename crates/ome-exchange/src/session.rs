use ome_core::types::Timestamp;
use ome_protocol::{MarketPhase, MarketStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub pre_open_ms: u64,
    pub continuous_ms: u64,
    pub closing_ms: u64,
    pub closed_ms: u64,
}

impl Schedule {
    pub fn duration(&self, phase: MarketPhase) -> u64 {
        match phase {
            MarketPhase::PreOpen => self.pre_open_ms,
            MarketPhase::Open => self.continuous_ms,
            MarketPhase::ClosingAuction => self.closing_ms,
            MarketPhase::Closed => self.closed_ms,
        }
    }

    pub fn next(&self, phase: MarketPhase) -> MarketPhase {
        let mut next = following(phase);
        for _ in 0..3 {
            if self.duration(next) > 0 {
                break;
            }
            next = following(next);
        }
        next
    }
}

fn following(phase: MarketPhase) -> MarketPhase {
    match phase {
        MarketPhase::Closed => MarketPhase::PreOpen,
        MarketPhase::PreOpen => MarketPhase::Open,
        MarketPhase::Open => MarketPhase::ClosingAuction,
        MarketPhase::ClosingAuction => MarketPhase::Closed,
    }
}

pub struct Session {
    pub phase: MarketPhase,
    pub day: u64,
    pub since: Timestamp,
    pub schedule: Option<Schedule>,
    pub next_change_at: Option<Timestamp>,
    pub planned: Option<Schedule>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            phase: MarketPhase::Open,
            day: 0,
            since: 0,
            schedule: None,
            next_change_at: None,
            planned: None,
        }
    }
}

impl Session {
    pub fn status(&self) -> MarketStatus {
        MarketStatus {
            phase: self.phase,
            day: self.day,
            since: self.since,
            next_change_at: self.next_change_at.unwrap_or(0),
        }
    }

    pub fn enter(&mut self, phase: MarketPhase, now: Timestamp) {
        if phase == MarketPhase::PreOpen
            || (phase == MarketPhase::Open && self.phase == MarketPhase::Closed)
        {
            self.day += 1;
            if let Some(planned) = self.planned.take() {
                self.schedule = Some(planned);
            }
        }
        self.phase = phase;
        self.since = now;
        self.next_change_at = self.schedule.map(|s| now + s.duration(phase).max(1));
    }

    pub fn set_schedule(&mut self, schedule: Option<Schedule>, now: Timestamp) {
        if self.since == 0 {
            self.since = now;
        }
        self.schedule = schedule;
        self.next_change_at = schedule.map(|s| now + s.duration(self.phase).max(1));
    }

    pub fn plan(&mut self, schedule: Schedule, now: Timestamp) {
        if self.schedule.is_none() {
            self.set_schedule(Some(schedule), now);
        } else {
            self.planned = Some(schedule);
        }
    }

    pub fn due(&self, now: Timestamp) -> Option<MarketPhase> {
        let at = self.next_change_at?;
        (now >= at).then(|| self.schedule.map_or(self.phase, |s| s.next(self.phase)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Schedule = Schedule {
        pre_open_ms: 5,
        continuous_ms: 40,
        closing_ms: 5,
        closed_ms: 10,
    };

    #[test]
    fn schedule_cycles_through_a_trading_day() {
        let mut s = Session::default();
        s.set_schedule(Some(DAY), 0);
        assert_eq!(s.due(39), None);
        assert_eq!(s.due(40), Some(MarketPhase::ClosingAuction));
        s.enter(MarketPhase::ClosingAuction, 40);
        assert_eq!(s.due(45), Some(MarketPhase::Closed));
        s.enter(MarketPhase::Closed, 45);
        s.enter(MarketPhase::PreOpen, 55);
        assert_eq!(s.day, 1);
        assert_eq!(s.status().next_change_at, 60);
        let longer = Schedule { continuous_ms: 400, ..DAY };
        s.plan(longer, 60);
        s.enter(MarketPhase::Open, 60);
        assert_eq!(s.status().next_change_at, 100, "today keeps its length");
        s.enter(MarketPhase::ClosingAuction, 100);
        s.enter(MarketPhase::Closed, 105);
        s.enter(MarketPhase::PreOpen, 115);
        s.enter(MarketPhase::Open, 120);
        assert_eq!(s.status().next_change_at, 520, "the planned day starts at the next pre-open");
        let mut manual = Session::default();
        manual.plan(DAY, 0);
        assert_eq!(manual.schedule, Some(DAY), "with no calendar yet it applies at once");
    }

    #[test]
    fn zero_length_phases_are_skipped_and_manual_mode_never_fires() {
        let no_auctions = Schedule {
            pre_open_ms: 0,
            closing_ms: 0,
            ..DAY
        };
        assert_eq!(no_auctions.next(MarketPhase::Closed), MarketPhase::Open);
        assert_eq!(no_auctions.next(MarketPhase::Open), MarketPhase::Closed);
        let mut manual = Session::default();
        manual.enter(MarketPhase::Closed, 0);
        assert_eq!(manual.due(u64::MAX), None);
    }
}
