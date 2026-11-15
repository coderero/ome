use crate::scale::{PriceScale, visible};
use crate::theme::Theme;
use gpui::{Bounds, Canvas, PaintQuad, Pixels, Window, canvas, point, px, size};
use ome_core::candles::Candle;
use ome_core::price::Price;

const SLOT: f32 = 7.0;
const VOLUME_SHARE: f32 = 0.18;

pub struct ChartModel {
    pub candles: Vec<Candle>,
    pub last: Option<Price>,
    pub fair: Option<Price>,
}

impl ChartModel {
    pub fn scale(&self, width: f32) -> Option<PriceScale> {
        let shown = visible(&self.candles, width, SLOT);
        let extra: Vec<Price> = self.last.into_iter().collect();
        PriceScale::fit(shown, &extra)
    }
}

fn quad(window: &mut Window, x: Pixels, y: Pixels, w: Pixels, h: Pixels, color: gpui::Rgba) {
    window.paint_quad(PaintQuad::new(Bounds::new(point(x, y), size(w, h))).background(color));
}

pub fn candle_chart(model: ChartModel, theme: Theme) -> Canvas<()> {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let price_height = height * (1.0 - VOLUME_SHARE);
            let Some(scale) = model.scale(width) else {
                return;
            };
            for i in 0..5 {
                let y = bounds.origin.y + px(price_height * i as f32 / 4.0);
                quad(
                    window,
                    bounds.origin.x,
                    y,
                    bounds.size.width,
                    px(1.),
                    theme.border,
                );
            }
            let shown = visible(&model.candles, width, SLOT);
            let max_volume = shown.iter().map(|c| c.volume).max().unwrap_or(1).max(1);
            let right = bounds.origin.x + bounds.size.width;
            for (i, candle) in shown.iter().rev().enumerate() {
                let x = right - px(SLOT * (i as f32 + 1.0));
                let color = match (candle.trades, candle.is_bullish()) {
                    (0, _) => theme.faint,
                    (_, true) => theme.buy,
                    (_, false) => theme.sell,
                };
                let y = |p: Price| bounds.origin.y + px(scale.y(p, price_height));
                let wick_x = x + px(SLOT / 2.0 - 0.5);
                quad(
                    window,
                    wick_x,
                    y(candle.high),
                    px(1.),
                    y(candle.low) - y(candle.high) + px(1.),
                    color,
                );
                let top = y(candle.open.max(candle.close));
                let bottom = y(candle.open.min(candle.close));
                quad(
                    window,
                    x + px(1.),
                    top,
                    px(SLOT - 2.0),
                    (bottom - top).max(px(1.)),
                    color,
                );
                let vh = height * VOLUME_SHARE * candle.volume as f32 / max_volume as f32;
                let base = bounds.origin.y + bounds.size.height;
                let soft = if candle.is_bullish() {
                    theme.buy_soft
                } else {
                    theme.sell_soft
                };
                quad(
                    window,
                    x + px(1.),
                    base - px(vh),
                    px(SLOT - 2.0),
                    px(vh),
                    soft,
                );
            }
            for (level, color) in [(model.fair, theme.links[0]), (model.last, theme.accent)] {
                let Some(level) = level.filter(|p| (scale.low..=scale.high).contains(p)) else {
                    continue;
                };
                let y = bounds.origin.y + px(scale.y(level, price_height));
                let mut x = bounds.origin.x;
                while x < right {
                    quad(window, x, y, px(4.), px(1.), color);
                    x += px(8.);
                }
            }
        },
    )
}
