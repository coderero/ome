use crate::format;
use crate::scale::PriceScale;
use crate::studies::{ema, sma, vwap};
use crate::theme::Theme;
use egui::{Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, pos2};
use ome_core::candles::Candle;
use ome_core::price::Price;

const AXIS: f32 = 64.0;
const TIME_AXIS: f32 = 22.0;
const VOLUME_SHARE: f32 = 0.16;
pub const RIGHT_MARGIN: f32 = 4.0;
pub const MIN_SPACING: f32 = 2.0;
pub const MAX_SPACING: f32 = 48.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Studies {
    pub volume: bool,
    pub vwap: bool,
    pub sma: Option<usize>,
    pub ema: Option<usize>,
}

impl Default for Studies {
    fn default() -> Self {
        Self {
            volume: true,
            vwap: true,
            sma: Some(20),
            ema: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChartView {
    pub spacing: f32,
    pub offset: f32,
    pub scale: Option<(f64, f64)>,
}

impl Default for ChartView {
    fn default() -> Self {
        Self { spacing: 8.0, offset: 0.0, scale: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Plot,
    PriceAxis,
    TimeAxis,
}

impl ChartView {
    pub fn window(&self, bars: usize, width: f32) -> (usize, usize, f32) {
        if bars == 0 {
            return (0, 0, 0.0);
        }
        let end = (bars - 1) as f32 + RIGHT_MARGIN - self.offset;
        let fit = width / self.spacing;
        let start = (end - fit).floor().max(0.0) as usize;
        let stop = (end.ceil().max(0.0) as usize).min(bars - 1);
        (start.min(stop), stop, end)
    }

    pub fn drag(&mut self, zone: Zone, delta: Vec2, bars: usize, auto: (f64, f64), price_h: f32) {
        match zone {
            Zone::TimeAxis => {
                self.spacing = (self.spacing * (1.0 + delta.x * 0.01)).clamp(MIN_SPACING, MAX_SPACING);
            }
            Zone::PriceAxis => {
                let (low, high) = self.scale.unwrap_or(auto);
                let mid = (low + high) / 2.0;
                let half = ((high - low) / 2.0 * (delta.y as f64 * 0.01).exp()).max(1.0);
                self.scale = Some((mid - half, mid + half));
            }
            Zone::Plot => {
                let limit = bars.saturating_sub(1) as f32 + RIGHT_MARGIN - 1.0;
                self.offset = (self.offset + delta.x / self.spacing).clamp(0.0, limit.max(0.0));
                if let Some((low, high)) = self.scale {
                    let shift = delta.y as f64 * (high - low) / price_h.max(1.0) as f64;
                    self.scale = Some((low + shift, high + shift));
                }
            }
        }
    }

    pub fn zoom(&mut self, scroll: f32) {
        self.spacing = (self.spacing * if scroll > 0.0 { 1.1 } else { 0.9 }).clamp(MIN_SPACING, MAX_SPACING);
    }
}

pub struct Chart<'a> {
    pub candles: &'a [Candle],
    pub last: Option<Price>,
    pub reference: Option<Price>,
    pub interval: u64,
    pub studies: Studies,
    pub view: &'a mut ChartView,
}

fn time_label(ms: u64, interval: u64) -> String {
    let clock = format::clock(ms);
    if interval >= 60_000 { clock[..5].to_string() } else { clock }
}

fn dashed(painter: &egui::Painter, from: Pos2, to: Pos2, color: Color32) {
    painter.extend(Shape::dashed_line(&[from, to], Stroke::new(1.0, color), 4.0, 4.0));
}

impl Chart<'_> {
    pub fn show(self, ui: &mut Ui, t: &Theme) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let plot = Rect::from_min_max(rect.min, pos2(rect.max.x - AXIS, rect.max.y - TIME_AXIS));
        let price_axis = Rect::from_min_max(pos2(plot.right(), rect.top()), pos2(rect.right(), plot.bottom()));
        let time_axis = Rect::from_min_max(pos2(rect.left(), plot.bottom()), pos2(plot.right(), rect.bottom()));
        painter.line_segment([pos2(plot.right(), rect.top()), pos2(plot.right(), plot.bottom())], Stroke::new(1.0, t.divider));
        painter.line_segment([pos2(rect.left(), plot.bottom()), pos2(plot.right(), plot.bottom())], Stroke::new(1.0, t.divider));
        if plot.width() < 60.0 || plot.height() < 60.0 || self.candles.is_empty() {
            painter.text(plot.center(), Align2::CENTER_CENTER, "Waiting for trades", Theme::sans(13.0), t.muted);
            return;
        }
        let zone_at = |p: Pos2| {
            if price_axis.contains(p) {
                Zone::PriceAxis
            } else if time_axis.contains(p) {
                Zone::TimeAxis
            } else {
                Zone::Plot
            }
        };
        if let Some(p) = response.hover_pos() {
            ui.ctx().set_cursor_icon(match zone_at(p) {
                Zone::PriceAxis => CursorIcon::ResizeVertical,
                Zone::TimeAxis => CursorIcon::ResizeHorizontal,
                Zone::Plot if response.dragged() => CursorIcon::Grabbing,
                Zone::Plot => CursorIcon::Crosshair,
            });
        }
        let volume_h = if self.studies.volume { plot.height() * VOLUME_SHARE } else { 0.0 };
        let price_h = plot.height() - volume_h - 6.0;
        let bars = self.candles.len();
        let (start, stop, end) = self.view.window(bars, plot.width());
        let shown = &self.candles[start..=stop];
        let all_sma = self.studies.sma.map(|n| sma(self.candles, n));
        let all_ema = self.studies.ema.map(|n| ema(self.candles, n));
        let all_vwap = self.studies.vwap.then(|| vwap(self.candles));
        let mut extra: Vec<Price> = self.last.filter(|_| self.view.offset < RIGHT_MARGIN + 1.0).into_iter().collect();
        for series in [&all_sma, &all_ema, &all_vwap].into_iter().flatten() {
            extra.extend(series[start..=stop].iter().flatten().map(|v| v.round() as Price));
        }
        let Some(fit) = PriceScale::fit(shown, &extra) else { return };
        let auto = (fit.low as f64, fit.high as f64);
        if response.dragged()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            self.view.drag(zone_at(origin), response.drag_delta(), bars, auto, price_h);
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.view.zoom(scroll);
            }
        }
        if response.double_clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            match zone_at(p) {
                Zone::PriceAxis => self.view.scale = None,
                _ => *self.view = ChartView::default(),
            }
        }
        let (low, high) = self.view.scale.unwrap_or(auto);
        let span = (high - low).max(1.0);
        let y = |p: f64| plot.top() + ((high - p) / span) as f32 * price_h;
        let price_at = |py: f32| high - ((py - plot.top()) / price_h) as f64 * span;
        let slot = self.view.spacing;
        let x = |i: usize| plot.right() - slot * (end - i as f32) - slot / 2.0;
        let plot_painter = ui.painter_at(plot);

        for i in 0..6 {
            let py = plot.top() + price_h * i as f32 / 5.0;
            plot_painter.line_segment([pos2(plot.left(), py), pos2(plot.right(), py)], Stroke::new(1.0, t.grid));
            let label_y = py.clamp(plot.top() + 7.0, plot.bottom() - 7.0);
            painter.text(pos2(plot.right() + 6.0, label_y), Align2::LEFT_CENTER, format::price(price_at(py).max(0.0).round() as Price), Theme::sans(11.5), t.muted);
        }
        let every = ((110.0 / slot).ceil() as usize).max(1);
        for (k, c) in shown.iter().enumerate() {
            let i = start + k;
            if i.is_multiple_of(every) {
                let cx = x(i);
                plot_painter.line_segment([pos2(cx, plot.top()), pos2(cx, plot.bottom())], Stroke::new(1.0, t.grid));
                if cx > rect.left() + 30.0 && cx < plot.right() - 30.0 {
                    painter.text(pos2(cx, plot.bottom() + 4.0), Align2::CENTER_TOP, time_label(c.start, self.interval), Theme::sans(11.0), t.muted);
                }
            }
        }
        let max_volume = shown.iter().map(|c| c.volume).max().unwrap_or(1).max(1) as f32;
        let body = (slot * 0.7).max(1.0);
        for (k, c) in shown.iter().enumerate() {
            let cx = x(start + k);
            let color = match (c.trades, c.close >= c.open) {
                (0, _) => t.faint,
                (_, true) => t.up,
                (_, false) => t.down,
            };
            plot_painter.line_segment([pos2(cx, y(c.high as f64)), pos2(cx, y(c.low as f64))], Stroke::new(1.0, color));
            let top = y(c.open.max(c.close) as f64);
            let bottom = y(c.open.min(c.close) as f64);
            plot_painter.rect_filled(
                Rect::from_min_max(pos2(cx - body / 2.0, top), pos2(cx + body / 2.0, bottom.max(top + 1.0))),
                CornerRadius::ZERO,
                color,
            );
            if volume_h > 0.0 && c.volume > 0 {
                let h = volume_h * c.volume as f32 / max_volume;
                let soft = if c.close >= c.open { t.up_soft } else { t.down_soft };
                plot_painter.rect_filled(
                    Rect::from_min_max(pos2(cx - body / 2.0, plot.bottom() - h), pos2(cx + body / 2.0, plot.bottom())),
                    CornerRadius::ZERO,
                    soft,
                );
            }
        }
        for (series, color) in [(&all_vwap, t.studies[2]), (&all_sma, t.studies[0]), (&all_ema, t.studies[1])] {
            let Some(series) = series else { continue };
            let points: Vec<Pos2> = series[start..=stop]
                .iter()
                .enumerate()
                .filter_map(|(k, v)| v.map(|v| pos2(x(start + k), y(v))))
                .collect();
            if points.len() > 1 {
                plot_painter.add(Shape::line(points, Stroke::new(1.5, color)));
            }
        }
        let visible = |p: Price| (low..=high).contains(&(p as f64));
        if let Some(reference) = self.reference.filter(|p| visible(*p)) {
            let ry = y(reference as f64);
            dashed(&plot_painter, pos2(plot.left(), ry), pos2(plot.right(), ry), t.faint);
        }
        if let Some(last) = self.last.filter(|p| visible(*p)) {
            let ly = y(last as f64);
            let tag = if self.reference.is_none_or(|r| last >= r) { t.up_fill } else { t.down_fill };
            dashed(&plot_painter, pos2(plot.left(), ly), pos2(plot.right(), ly), tag);
            let label = Rect::from_min_size(pos2(plot.right() + 1.0, ly - 9.0), Vec2::new(AXIS - 2.0, 18.0));
            painter.rect_filled(label, CornerRadius::ZERO, tag);
            painter.text(label.left_center() + Vec2::new(5.0, 0.0), Align2::LEFT_CENTER, format::price(last), Theme::bold(11.5), t.on_fill);
        }
        if self.view.scale.is_some() {
            painter.text(price_axis.left_bottom() + Vec2::new(6.0, -4.0), Align2::LEFT_BOTTOM, "manual", Theme::sans(10.5), t.warning);
        }
        if let Some(hover) = response.hover_pos().filter(|p| plot.contains(*p) && p.y < plot.top() + price_h) {
            dashed(&plot_painter, pos2(hover.x, plot.top()), pos2(hover.x, plot.bottom()), t.muted);
            dashed(&plot_painter, pos2(plot.left(), hover.y), pos2(plot.right(), hover.y), t.muted);
            let label = Rect::from_min_size(pos2(plot.right() + 1.0, hover.y - 9.0), Vec2::new(AXIS - 2.0, 18.0));
            painter.rect_filled(label, CornerRadius::ZERO, t.button);
            painter.text(label.left_center() + Vec2::new(5.0, 0.0), Align2::LEFT_CENTER, format::price(price_at(hover.y).max(0.0).round() as Price), Theme::sans(11.5), t.strong);
            let index = (end - (plot.right() - hover.x - slot / 2.0) / slot).round();
            if index >= 0.0 && (index as usize) < bars {
                let c = self.candles[index as usize];
                let text = format!(
                    "{}   O {}   H {}   L {}   C {}   V {}",
                    time_label(c.start, self.interval),
                    format::price(c.open),
                    format::price(c.high),
                    format::price(c.low),
                    format::price(c.close),
                    format::qty(c.volume)
                );
                painter.text(plot.left_top() + Vec2::new(8.0, 8.0), Align2::LEFT_TOP, text, Theme::sans(12.0), t.strong);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_view_keeps_bar_width_fixed_and_follows_axis_drags() {
        let mut view = ChartView::default();
        let (start, stop, end) = view.window(500, 400.0);
        assert_eq!(stop, 499);
        assert_eq!(end, 499.0 + RIGHT_MARGIN, "a right margin follows the last bar");
        assert_eq!(start, (end - 50.0).floor() as usize, "400 px at 8 px a bar shows 50 bars");
        let (start, stop, _) = view.window(5, 400.0);
        assert_eq!((start, stop), (0, 4), "few bars stay narrow instead of stretching");

        view.drag(Zone::TimeAxis, Vec2::new(50.0, 0.0), 500, (100.0, 200.0), 300.0);
        assert!(view.spacing > 8.0, "dragging the time axis right widens bars");
        view.drag(Zone::TimeAxis, Vec2::new(-10_000.0, 0.0), 500, (100.0, 200.0), 300.0);
        assert_eq!(view.spacing, MIN_SPACING);

        view.drag(Zone::PriceAxis, Vec2::new(0.0, 69.3), 500, (100.0, 200.0), 300.0);
        let (low, high) = view.scale.unwrap();
        assert!((high - low - 200.0).abs() < 1.0, "dragging the price axis down doubles the range around its middle");
        assert!(((low + high) / 2.0 - 150.0).abs() < 1e-6);

        view.drag(Zone::Plot, Vec2::new(16.0, 30.0), 500, (100.0, 200.0), 300.0);
        assert_eq!(view.offset, 16.0 / MIN_SPACING, "dragging the plot right scrolls back in time");
        assert!((view.scale.unwrap().0 - (low + 20.0)).abs() < 0.1, "and pans a manual scale");
        view.drag(Zone::Plot, Vec2::new(-1e6, 0.0), 500, (100.0, 200.0), 300.0);
        assert_eq!(view.offset, 0.0);
    }
}
