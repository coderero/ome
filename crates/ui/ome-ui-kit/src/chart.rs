use crate::format;
use crate::scale::PriceScale;
use crate::studies::{ema, sma, vwap};
use crate::theme::{MONO, Theme};
use gpui::{
    App, Bounds, Canvas, PaintQuad, PathBuilder, Pixels, Point, Rgba, SharedString, TextAlign,
    TextRun, Window, canvas, font, point, px, size,
};
use ome_core::candles::Candle;
use ome_core::price::Price;

const AXIS: f32 = 66.0;
const TIME_AXIS: f32 = 18.0;
const VOLUME_SHARE: f32 = 0.2;
const LABEL: f32 = 11.0;

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

pub struct ChartModel {
    pub candles: Vec<Candle>,
    pub last: Option<Price>,
    pub reference: Option<Price>,
    pub interval: u64,
    pub studies: Studies,
    pub hover: Option<Point<Pixels>>,
    pub slot: f32,
}

fn quad(window: &mut Window, x: Pixels, y: Pixels, w: Pixels, h: Pixels, color: Rgba) {
    window.paint_quad(PaintQuad::new(Bounds::new(point(x, y), size(w, h))).background(color));
}

fn label(
    window: &mut Window,
    cx: &mut App,
    text: String,
    origin: Point<Pixels>,
    color: Rgba,
    align: TextAlign,
    width: Option<Pixels>,
) {
    let text: SharedString = text.into();
    let run = TextRun {
        len: text.len(),
        font: font(MONO),
        color: gpui::rgb_to_hsla(color),
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing: None,
    };
    let line = window.text_system().shape_line(text, px(LABEL), &[run]);
    let _ = line.paint(origin, px(LABEL + 3.0), align, width, window, cx);
}

fn dashed_h(window: &mut Window, from: Pixels, to: Pixels, y: Pixels, color: Rgba) {
    let mut x = from;
    while x < to {
        quad(window, x, y, px(3.), px(1.), color);
        x += px(6.);
    }
}

fn dashed_v(window: &mut Window, x: Pixels, from: Pixels, to: Pixels, color: Rgba) {
    let mut y = from;
    while y < to {
        quad(window, x, y, px(1.), px(3.), color);
        y += px(6.);
    }
}

fn time_label(ms: u64, interval: u64) -> String {
    let clock = format::clock(ms);
    if interval >= 60_000 {
        clock[..5].to_string()
    } else {
        clock
    }
}

pub fn visible_count(width: f32, slot: f32) -> usize {
    ((width - AXIS).max(0.0) / slot.max(2.0)).floor() as usize
}

pub fn candle_chart(model: ChartModel, theme: Theme) -> Canvas<()> {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, cx| {
            let plot_w = f32::from(bounds.size.width) - AXIS;
            let roomy = plot_w / model.candles.len().max(1) as f32;
            let slot = model.slot.max(roomy.min(18.0)).max(3.0);
            let full_h = f32::from(bounds.size.height) - TIME_AXIS;
            if plot_w < 40.0 || full_h < 60.0 {
                return;
            }
            let volume_h = if model.studies.volume {
                full_h * VOLUME_SHARE
            } else {
                0.0
            };
            let price_h = full_h - volume_h - 4.0;
            let origin = bounds.origin;
            let right = origin.x + px(plot_w);
            let fit = visible_count(f32::from(bounds.size.width), slot);
            let start = model.candles.len().saturating_sub(fit);
            let shown = &model.candles[start..];
            let all_sma = model.studies.sma.map(|n| sma(&model.candles, n));
            let all_ema = model.studies.ema.map(|n| ema(&model.candles, n));
            let all_vwap = model.studies.vwap.then(|| vwap(&model.candles));
            let mut extra: Vec<Price> = model.last.into_iter().collect();
            for series in [&all_sma, &all_ema, &all_vwap].into_iter().flatten() {
                extra.extend(series[start..].iter().flatten().map(|v| v.round() as Price));
            }
            let Some(scale) = PriceScale::fit(shown, &extra) else {
                return;
            };
            let y = |p: f64| {
                origin.y
                    + px(
                        ((scale.high as f64 - p) / (scale.high - scale.low).max(1) as f64) as f32
                            * price_h,
                    )
            };
            let x_of = |i: usize| right - px(slot * (shown.len() - i) as f32);

            for (i, price) in scale.labels(7).into_iter().enumerate() {
                let py = origin.y + px(price_h * i as f32 / 6.0);
                quad(window, origin.x, py, px(plot_w), px(1.), theme.grid);
                label(
                    window,
                    cx,
                    format::price(price),
                    point(right + px(6.), py - px(7.)),
                    theme.muted,
                    TextAlign::Start,
                    None,
                );
            }
            quad(window, right, origin.y, px(1.), px(full_h), theme.border);
            quad(
                window,
                origin.x,
                origin.y + px(full_h),
                px(plot_w),
                px(1.),
                theme.border,
            );

            let every = ((90.0 / slot).ceil() as usize).max(1);
            for (i, c) in shown.iter().enumerate() {
                if (start + i).is_multiple_of(every) {
                    let x = x_of(i);
                    quad(window, x, origin.y, px(1.), px(full_h), theme.grid);
                    label(
                        window,
                        cx,
                        time_label(c.start, model.interval),
                        point(x - px(30.), origin.y + px(full_h + 3.)),
                        theme.muted,
                        TextAlign::Center,
                        Some(px(60.)),
                    );
                }
            }

            let max_volume = shown.iter().map(|c| c.volume).max().unwrap_or(1).max(1);
            let body_w = (slot - 2.0).max(1.0);
            for (i, c) in shown.iter().enumerate() {
                let x = x_of(i);
                let color = match (c.trades, c.close >= c.open) {
                    (0, _) => theme.faint,
                    (_, true) => theme.up,
                    (_, false) => theme.down,
                };
                let high = y(c.high as f64);
                let low = y(c.low as f64);
                quad(
                    window,
                    x + px(slot / 2.0 - 0.5),
                    high,
                    px(1.),
                    (low - high).max(px(1.)),
                    color,
                );
                let top = y(c.open.max(c.close) as f64);
                let bottom = y(c.open.min(c.close) as f64);
                quad(
                    window,
                    x + px(1.),
                    top,
                    px(body_w),
                    (bottom - top).max(px(1.)),
                    color,
                );
                if volume_h > 0.0 && c.volume > 0 {
                    let h = volume_h * c.volume as f32 / max_volume as f32;
                    let base = origin.y + px(full_h);
                    let soft = if c.close >= c.open {
                        theme.up_flash
                    } else {
                        theme.down_flash
                    };
                    quad(window, x + px(1.), base - px(h), px(body_w), px(h), soft);
                }
            }

            let studies = [
                (all_vwap.as_ref(), theme.studies[2]),
                (all_sma.as_ref(), theme.studies[0]),
                (all_ema.as_ref(), theme.studies[1]),
            ];
            for (series, color) in studies {
                let Some(series) = series else { continue };
                let mut path = PathBuilder::stroke(px(1.5));
                let mut started = false;
                for (i, value) in series[start..].iter().enumerate() {
                    let Some(value) = value else { continue };
                    let p = point(x_of(i) + px(slot / 2.0), y(*value));
                    if started {
                        path.line_to(p);
                    } else {
                        path.move_to(p);
                        started = true;
                    }
                }
                if started && let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            }

            if let Some(reference) = model
                .reference
                .filter(|p| (scale.low..=scale.high).contains(p))
            {
                dashed_h(window, origin.x, right, y(reference as f64), theme.faint);
            }
            if let Some(last) = model.last.filter(|p| (scale.low..=scale.high).contains(p)) {
                let ly = y(last as f64);
                let up = model.reference.is_none_or(|r| last >= r);
                let tag = if up { theme.up } else { theme.down };
                dashed_h(window, origin.x, right, ly, tag);
                quad(
                    window,
                    right + px(1.),
                    ly - px(8.),
                    px(AXIS - 2.0),
                    px(16.),
                    tag,
                );
                label(
                    window,
                    cx,
                    format::price(last),
                    point(right + px(6.), ly - px(7.)),
                    theme.background,
                    TextAlign::Start,
                    None,
                );
            }

            if let Some(hover) = model.hover
                && hover.x >= origin.x
                && hover.x < right
                && hover.y >= origin.y
                && hover.y < origin.y + px(price_h)
            {
                dashed_v(
                    window,
                    hover.x,
                    origin.y,
                    origin.y + px(full_h),
                    theme.muted,
                );
                dashed_h(window, origin.x, right, hover.y, theme.muted);
                let price = scale.price_at(f32::from(hover.y - origin.y), price_h);
                quad(
                    window,
                    right + px(1.),
                    hover.y - px(8.),
                    px(AXIS - 2.0),
                    px(16.),
                    theme.raised,
                );
                label(
                    window,
                    cx,
                    format::price(price),
                    point(right + px(6.), hover.y - px(7.)),
                    theme.strong,
                    TextAlign::Start,
                    None,
                );
                let index = ((f32::from(right - hover.x)) / slot).floor() as usize;
                if index < shown.len() {
                    let c = shown[shown.len() - 1 - index];
                    let text = format!(
                        "{}  O {}  H {}  L {}  C {}  V {}",
                        time_label(c.start, model.interval),
                        format::price(c.open),
                        format::price(c.high),
                        format::price(c.low),
                        format::price(c.close),
                        format::qty(c.volume)
                    );
                    label(
                        window,
                        cx,
                        text,
                        point(origin.x + px(6.), origin.y + px(4.)),
                        theme.strong,
                        TextAlign::Start,
                        None,
                    );
                }
            }
        },
    )
}
