use ome_core::candles::Candle;
use ome_core::price::{Price, format_price};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Widget;

const AXIS_WIDTH: u16 = 9;
const VOLUME_ROWS: u16 = 3;
const VOLUME_BLOCKS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

pub struct CandleChart<'a> {
    candles: &'a [Candle],
    last_price: Option<Price>,
}

impl<'a> CandleChart<'a> {
    pub fn new(candles: &'a [Candle], last_price: Option<Price>) -> Self {
        Self {
            candles,
            last_price,
        }
    }

    pub fn capacity(width: u16) -> usize {
        (width.saturating_sub(AXIS_WIDTH) / 2) as usize
    }
}

struct PriceScale {
    low: Price,
    high: Price,
    top: u16,
    rows: u16,
}

impl PriceScale {
    fn row(&self, price: Price) -> u16 {
        let span = (self.high - self.low).max(1) as f64;
        let fraction = (self.high.saturating_sub(price)) as f64 / span;
        self.top + (fraction * (self.rows - 1) as f64).round() as u16
    }

    fn price_at(&self, row: u16) -> Price {
        let fraction = (row - self.top) as f64 / (self.rows - 1) as f64;
        self.high - ((self.high - self.low) as f64 * fraction).round() as Price
    }
}

impl Widget for CandleChart<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width <= AXIS_WIDTH + 2 || area.height < 6 || self.candles.is_empty() {
            return;
        }
        let volume_rows = if area.height >= 14 { VOLUME_ROWS } else { 0 };
        let price_rows = area.height - volume_rows;
        let plot_left = area.x + AXIS_WIDTH;
        let visible = &self.candles[self
            .candles
            .len()
            .saturating_sub(Self::capacity(area.width))..];

        let low = visible.iter().map(|c| c.low).min().unwrap_or(0);
        let high = visible.iter().map(|c| c.high).max().unwrap_or(1);
        let padding = ((high - low) / 10).max(2);
        let scale = PriceScale {
            low: low.saturating_sub(padding),
            high: high + padding,
            top: area.y,
            rows: price_rows,
        };

        let axis_style = Style::default().fg(Color::DarkGray);
        let labels = (price_rows / 4).max(2);
        for i in 0..labels {
            let row = area.y + i * (price_rows - 1) / (labels - 1);
            let label = format!("{:>8} ", format_price(scale.price_at(row)));
            buf.set_string(area.x, row, label, axis_style);
            for x in plot_left..area.right() {
                buf.set_string(x, row, "·", Style::default().fg(Color::Rgb(60, 60, 60)));
            }
        }

        if let Some(last) = self
            .last_price
            .filter(|p| (scale.low..=scale.high).contains(p))
        {
            let row = scale.row(last);
            for x in plot_left..area.right() {
                buf.set_string(x, row, "╌", Style::default().fg(Color::Yellow));
            }
            buf.set_string(
                area.x,
                row,
                format!("{:>8} ", format_price(last)),
                Style::default().fg(Color::Black).bg(Color::Yellow),
            );
        }

        let max_volume = visible.iter().map(|c| c.volume).max().unwrap_or(0).max(1);
        for (i, candle) in visible.iter().enumerate() {
            let x = plot_left + i as u16 * 2;
            let color = match (candle.trades, candle.is_bullish()) {
                (0, _) => Color::DarkGray,
                (_, true) => Color::Green,
                (_, false) => Color::Red,
            };
            let body_top = scale.row(candle.open.max(candle.close));
            let body_bottom = scale.row(candle.open.min(candle.close));
            for y in scale.row(candle.high)..=scale.row(candle.low) {
                let symbol = match (candle.trades, (body_top..=body_bottom).contains(&y)) {
                    (0, _) => "─",
                    (_, true) => "█",
                    (_, false) => "│",
                };
                buf.set_string(x, y, symbol, Style::default().fg(color));
            }

            if volume_rows > 0 {
                let eighths = (candle.volume * (volume_rows as u64 * 8) / max_volume) as u16;
                for r in 0..volume_rows {
                    let fill = eighths.saturating_sub(r * 8).min(8) as usize;
                    let y = area.bottom() - 1 - r;
                    buf.set_string(x, y, VOLUME_BLOCKS[fill], Style::default().fg(color));
                }
            }
        }

        if volume_rows > 0 {
            buf.set_string(area.x, area.bottom() - 1, "   volume", axis_style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_maps_high_to_top_and_low_to_bottom() {
        let scale = PriceScale {
            low: 9_000,
            high: 11_000,
            top: 2,
            rows: 21,
        };
        assert_eq!(scale.row(11_000), 2);
        assert_eq!(scale.row(9_000), 22);
        assert_eq!(scale.row(10_000), 12);
        assert_eq!(scale.price_at(12), 10_000);
    }

    #[test]
    fn renders_without_panicking_on_small_areas() {
        let candles = [Candle {
            start: 0,
            open: 100,
            high: 110,
            low: 90,
            close: 105,
            volume: 10,
            trades: 3,
        }];
        for (w, h) in [(1, 1), (10, 5), (12, 6), (40, 20)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            CandleChart::new(&candles, Some(105)).render(area, &mut buf);
        }
    }
}
