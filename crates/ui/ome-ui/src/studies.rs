use ome_core::candles::Candle;

pub fn sma(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    let period = period.max(1);
    let mut out = Vec::with_capacity(candles.len());
    let mut sum = 0.0;
    for (i, c) in candles.iter().enumerate() {
        sum += c.close as f64;
        if i >= period {
            sum -= candles[i - period].close as f64;
        }
        out.push((i + 1 >= period).then(|| sum / period as f64));
    }
    out
}

pub fn ema(candles: &[Candle], period: usize) -> Vec<Option<f64>> {
    let alpha = 2.0 / (period.max(1) as f64 + 1.0);
    let mut value: Option<f64> = None;
    candles
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let close = c.close as f64;
            value = Some(value.map_or(close, |v| v + alpha * (close - v)));
            (i + 1 >= period).then_some(value.unwrap_or(close))
        })
        .collect()
}

pub fn vwap(candles: &[Candle]) -> Vec<Option<f64>> {
    let (mut pv, mut volume) = (0.0, 0.0);
    candles
        .iter()
        .map(|c| {
            if c.trades > 0 {
                let typical = (c.high + c.low + c.close) as f64 / 3.0;
                pv += typical * c.volume as f64;
                volume += c.volume as f64;
            }
            (volume > 0.0).then(|| pv / volume)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closes(values: &[u64]) -> Vec<Candle> {
        values
            .iter()
            .enumerate()
            .map(|(i, v)| Candle::new(i as u64, *v, 10))
            .collect()
    }

    #[test]
    fn studies_follow_their_textbook_definitions() {
        let c = closes(&[10, 20, 30, 40]);
        assert_eq!(sma(&c, 2), vec![None, Some(15.0), Some(25.0), Some(35.0)]);
        let e = ema(&c, 3);
        assert_eq!(e[..2], [None, None]);
        assert!((e[2].unwrap() - 22.5).abs() < 1e-9);
        let v = vwap(&c);
        assert_eq!(v[0], Some(10.0));
        assert_eq!(v[3], Some(25.0));
        assert!(vwap(&[]).is_empty());
    }
}
