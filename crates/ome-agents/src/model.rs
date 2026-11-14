use crate::state::Market;

pub const FEATURES: usize = 7;

pub type Features = [f64; FEATURES];

pub const FEATURE_NAMES: [&str; FEATURES] = [
    "return 5",
    "return 20",
    "return 100",
    "volatility",
    "z-score",
    "sentiment",
    "spread",
];

pub fn features(market: &Market, sentiment: f64) -> Option<Features> {
    let squash = |value: f64, scale: f64| (value / scale).tanh();
    let z = market.z_score(100)?;
    let spread = match (market.bid, market.ask) {
        (Some(bid), Some(ask)) if bid > 0 && ask > bid => {
            (ask - bid) as f64 / ((ask + bid) as f64 / 2.0) * 10_000.0
        }
        _ => 0.0,
    };
    Some([
        squash(market.change_bps(5)?, 50.0),
        squash(market.change_bps(20)?, 100.0),
        squash(market.change_bps(100).unwrap_or(0.0), 200.0),
        squash(market.volatility_bps(50).unwrap_or(0.0), 20.0),
        squash(z, 2.0),
        sentiment.clamp(-1.0, 1.0),
        squash(spread, 50.0),
    ])
}

#[derive(Debug, Clone, PartialEq)]
pub struct OnlineLogit<const N: usize> {
    pub weights: [f64; N],
    pub bias: f64,
    pub learning_rate: f64,
    pub l2: f64,
    pub seen: u64,
    hit_rate: f64,
    base_rate: f64,
}

impl<const N: usize> OnlineLogit<N> {
    pub fn new(learning_rate: f64, l2: f64) -> Self {
        Self {
            weights: [0.0; N],
            bias: 0.0,
            learning_rate,
            l2,
            seen: 0,
            hit_rate: 0.5,
            base_rate: 0.5,
        }
    }

    pub fn predict(&self, x: &[f64; N]) -> f64 {
        let z = self.bias + self.weights.iter().zip(x).map(|(w, v)| w * v).sum::<f64>();
        1.0 / (1.0 + (-z.clamp(-30.0, 30.0)).exp())
    }

    pub fn learn(&mut self, x: &[f64; N], up: bool) {
        let p = self.predict(x);
        let y = if up { 1.0 } else { 0.0 };
        let hit = if (p >= 0.5) == up { 1.0 } else { 0.0 };
        let alpha = (1.0 / (self.seen as f64 + 1.0)).max(0.02);
        self.hit_rate += alpha * (hit - self.hit_rate);
        self.base_rate += (1.0 / (self.seen as f64 + 1.0)).max(0.002) * (y - self.base_rate);
        let error = p - y;
        for (w, v) in self.weights.iter_mut().zip(x) {
            *w -= self.learning_rate * (error * v + self.l2 * *w);
        }
        self.bias -= self.learning_rate * error;
        self.seen += 1;
    }

    pub fn hit_rate(&self) -> f64 {
        self.hit_rate
    }

    pub fn base_rate(&self) -> f64 {
        self.base_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::gaussian;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn logistic_regression_learns_a_noisy_linear_rule_online() {
        let mut rng = StdRng::seed_from_u64(11);
        let mut model = OnlineLogit::<3>::new(0.05, 1e-4);
        let sample = |rng: &mut StdRng| {
            let x = [gaussian(rng), gaussian(rng), gaussian(rng)];
            let up = 2.0 * x[0] - x[1] + 0.3 * gaussian(rng) > 0.0;
            (x, up)
        };
        for _ in 0..3_000 {
            let (x, up) = sample(&mut rng);
            model.learn(&x, up);
        }
        let correct = (0..1_000)
            .filter(|_| {
                let (x, up) = sample(&mut rng);
                (model.predict(&x) >= 0.5) == up
            })
            .count();
        assert!(correct > 900, "held-out accuracy {correct}/1000");
        assert!(model.weights[0] > 0.0 && model.weights[1] < 0.0);
        assert!(model.weights[2].abs() < model.weights[0].abs() / 4.0);
        assert!(model.hit_rate() > 0.85);
        assert_eq!(model.seen, 3_000);
        assert!((model.base_rate() - 0.5).abs() < 0.1);
    }

    #[test]
    fn features_need_enough_history_and_stay_bounded() {
        let mut market = Market {
            trading: true,
            reference: 10_000,
            bid: Some(9_990),
            ask: Some(10_010),
            ..Market::default()
        };
        for p in 0..10 {
            market.record(10_000 + p);
        }
        assert_eq!(features(&market, 0.0), None);
        for p in 0..200 {
            market.record(10_000 + p * 50);
        }
        let x = features(&market, 2.0).unwrap();
        assert!(x.iter().all(|v| (-1.0..=1.0).contains(v)));
        assert!(x[0] > 0.0 && x[1] > 0.0 && x[4] > 0.0);
        assert_eq!(x[5], 1.0);
        assert_eq!(FEATURE_NAMES.len(), FEATURES);
    }
}
