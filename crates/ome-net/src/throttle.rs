pub struct TokenBucket {
    per_second: f64,
    burst: f64,
    tokens: f64,
    refilled_at: u64,
    dropped: u64,
}

impl TokenBucket {
    pub fn new(per_second: f64, burst: f64, now: u64) -> Self {
        Self {
            per_second,
            burst,
            tokens: burst,
            refilled_at: now,
            dropped: 0,
        }
    }

    pub fn allow(&mut self, now: u64) -> bool {
        let elapsed = now.saturating_sub(self.refilled_at) as f64 / 1_000.0;
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.burst);
        self.refilled_at = self.refilled_at.max(now);
        if self.tokens < 1.0 {
            self.dropped += 1;
            return false;
        }
        self.tokens -= 1.0;
        true
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bursts_are_allowed_then_refill_at_the_rate() {
        let mut bucket = TokenBucket::new(10.0, 3.0, 0);
        assert!((0..3).all(|_| bucket.allow(0)));
        assert!(!bucket.allow(0));
        assert!(!bucket.allow(50));
        assert!(bucket.allow(100));
        assert!(!bucket.allow(100));
        assert_eq!(bucket.dropped(), 3);
        assert!((0..3).all(|_| bucket.allow(10_000)));
        assert!(!bucket.allow(10_000));
    }
}
