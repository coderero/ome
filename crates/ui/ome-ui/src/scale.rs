use ome_core::candles::Candle;
use ome_core::price::Price;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceScale {
    pub low: Price,
    pub high: Price,
}

impl PriceScale {
    pub fn fit(candles: &[Candle], extra: &[Price]) -> Option<Self> {
        let traded = candles.iter().filter(|c| c.trades > 0);
        let low = traded
            .clone()
            .map(|c| c.low)
            .chain(extra.iter().copied())
            .min()?;
        let high = traded.map(|c| c.high).chain(extra.iter().copied()).max()?;
        let padding = ((high - low) / 10).max(2);
        Some(Self {
            low: low.saturating_sub(padding),
            high: high + padding,
        })
    }

    pub fn y(&self, price: Price, height: f32) -> f32 {
        let span = (self.high - self.low).max(1) as f32;
        (self.high.saturating_sub(price)) as f32 / span * height
    }

    pub fn price_at(&self, y: f32, height: f32) -> Price {
        let fraction = (y / height.max(1.0)).clamp(0.0, 1.0);
        self.high - ((self.high - self.low) as f32 * fraction).round() as Price
    }

    pub fn labels(&self, count: usize) -> Vec<Price> {
        let count = count.max(2);
        (0..count)
            .map(|i| self.price_at(i as f32, (count - 1) as f32))
            .collect()
    }
}

pub fn visible(candles: &[Candle], width: f32, slot: f32) -> &[Candle] {
    let fit = (width / slot.max(1.0)).floor() as usize;
    &candles[candles.len().saturating_sub(fit)..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(start: u64, low: Price, high: Price) -> Candle {
        let mut c = Candle::new(start, low, 1);
        c.update(high, 1);
        c
    }

    #[test]
    fn the_scale_fits_traded_candles_with_padding_and_maps_both_ways() {
        let candles = [candle(0, 24_00, 25_00), candle(1, 24_50, 26_00)];
        let scale = PriceScale::fit(&candles, &[]).unwrap();
        assert_eq!((scale.low, scale.high), (23_80, 26_20));
        assert_eq!(scale.y(26_20, 100.0), 0.0);
        assert_eq!(scale.y(23_80, 100.0), 100.0);
        assert_eq!(scale.price_at(50.0, 100.0), 25_00);
        assert_eq!(scale.labels(3), vec![26_20, 25_00, 23_80]);
        assert!(PriceScale::fit(&[], &[]).is_none());
        let flat = PriceScale::fit(&[], &[10_00]).unwrap();
        assert!(flat.low < 10_00 && flat.high > 10_00);
        assert_eq!(visible(&candles, 10.0, 8.0).len(), 1);
    }
}
