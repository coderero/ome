use ome_protocol::{MemberId, ServerMessage};
use std::collections::{HashMap, VecDeque};

pub struct Replay {
    pub messages: Vec<ServerMessage>,
    pub next_seq: u64,
    pub gap: bool,
}

struct Feed {
    next_seq: u64,
    retained: VecDeque<ServerMessage>,
}

pub struct Feeds {
    capacity: usize,
    feeds: HashMap<MemberId, Feed>,
}

fn stamp(message: &mut ServerMessage, seq: u64) {
    match message {
        ServerMessage::Execution(report) => report.seq = seq,
        ServerMessage::Allocation(allocation) => allocation.seq = seq,
        ServerMessage::ClearedTrade(trade) => trade.seq = seq,
        ServerMessage::SettlementReport(report) => report.seq = seq,
        _ => {}
    }
}

impl Feeds {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            feeds: HashMap::new(),
        }
    }

    pub fn publish(&mut self, member: MemberId, mut message: ServerMessage) -> ServerMessage {
        let feed = self.feeds.entry(member).or_insert_with(|| Feed {
            next_seq: 1,
            retained: VecDeque::new(),
        });
        stamp(&mut message, feed.next_seq);
        feed.next_seq += 1;
        if feed.retained.len() == self.capacity {
            feed.retained.pop_front();
        }
        feed.retained.push_back(message.clone());
        message
    }

    pub fn replay(&self, member: MemberId, after_seq: u64) -> Replay {
        let Some(feed) = self.feeds.get(&member) else {
            return Replay {
                messages: Vec::new(),
                next_seq: 1,
                gap: false,
            };
        };
        let oldest = feed.next_seq - feed.retained.len() as u64;
        let skip = after_seq.saturating_add(1).saturating_sub(oldest);
        Replay {
            messages: feed
                .retained
                .iter()
                .skip(skip.min(usize::MAX as u64) as usize)
                .cloned()
                .collect(),
            next_seq: feed.next_seq,
            gap: after_seq.saturating_add(1) < oldest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heartbeat() -> ServerMessage {
        ServerMessage::ReplayComplete { next_seq: 0 }
    }

    #[test]
    fn replays_after_a_sequence_and_detects_gaps() {
        let mut feeds = Feeds::new(2);
        for _ in 0..5 {
            feeds.publish(7, heartbeat());
        }
        let replay = feeds.replay(7, 0);
        assert!(replay.gap);
        assert_eq!(replay.messages.len(), 2);
        assert_eq!(replay.next_seq, 6);
        assert!(!feeds.replay(7, 3).gap);
        assert_eq!(feeds.replay(7, 4).messages.len(), 1);
        assert!(feeds.replay(7, u64::MAX).messages.is_empty());
        assert_eq!(feeds.replay(8, 0).next_seq, 1);
    }
}
