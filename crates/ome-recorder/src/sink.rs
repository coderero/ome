use crate::observation::Observation;
use std::collections::VecDeque;

pub trait Sink: Send {
    fn name(&self) -> &str;
    fn write(&mut self, batch: &[Observation]) -> Result<(), String>;
}

pub struct Buffered {
    sink: Box<dyn Sink>,
    pending: VecDeque<Observation>,
    capacity: usize,
    dropped: u64,
    healthy: bool,
    written: u64,
}

impl Buffered {
    pub fn new(sink: Box<dyn Sink>, capacity: usize) -> Self {
        Self {
            sink,
            pending: VecDeque::new(),
            capacity,
            dropped: 0,
            healthy: true,
            written: 0,
        }
    }

    pub fn name(&self) -> &str {
        self.sink.name()
    }

    pub fn push(&mut self, batch: &[Observation]) {
        for observation in batch {
            if self.pending.len() == self.capacity {
                self.pending.pop_front();
                self.dropped += 1;
            }
            self.pending.push_back(observation.clone());
        }
    }

    pub fn flush(&mut self) -> Result<usize, String> {
        if self.pending.is_empty() {
            return Ok(0);
        }
        let batch = crate::observation::coalesce(self.pending.iter().cloned().collect());
        match self.sink.write(&batch) {
            Ok(()) => {
                self.pending.clear();
                self.healthy = true;
                self.written += batch.len() as u64;
                Ok(batch.len())
            }
            Err(e) => {
                self.healthy = false;
                Err(e)
            }
        }
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy
    }

    pub fn written(&self) -> u64 {
        self.written
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct MemorySink {
        pub written: Arc<Mutex<Vec<Observation>>>,
        pub down: Arc<Mutex<bool>>,
    }

    impl Sink for MemorySink {
        fn name(&self) -> &str {
            "memory"
        }

        fn write(&mut self, batch: &[Observation]) -> Result<(), String> {
            if *self.down.lock().unwrap() {
                return Err("down".into());
            }
            self.written.lock().unwrap().extend_from_slice(batch);
            Ok(())
        }
    }

    fn status(day: u64) -> Observation {
        Observation::Market(ome_protocol::MarketStatus {
            phase: ome_protocol::MarketPhase::Open,
            day,
            since: 0,
            next_change_at: 0,
        })
    }

    #[test]
    fn outages_keep_a_bounded_backlog_that_is_written_on_recovery() {
        let memory = MemorySink::default();
        let mut sink = Buffered::new(Box::new(memory.clone()), 3);
        *memory.down.lock().unwrap() = true;
        sink.push(&[status(1), status(2)]);
        assert!(sink.flush().is_err());
        assert!(!sink.is_healthy());
        sink.push(&[status(3), status(4)]);
        assert_eq!((sink.pending(), sink.dropped()), (3, 1));
        *memory.down.lock().unwrap() = false;
        assert_eq!(sink.flush(), Ok(3));
        assert_eq!(
            *memory.written.lock().unwrap(),
            vec![status(2), status(3), status(4)]
        );
        assert!(sink.is_healthy());
        assert_eq!(sink.flush(), Ok(0));
    }
}
