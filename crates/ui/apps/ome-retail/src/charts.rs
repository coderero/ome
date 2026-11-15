use crate::app::RetailApp;
use crate::trade::interval_label;
use gpui::{Context, FontWeight, IntoElement, div, prelude::*, px};
use ome_ui_data::MarketData;
use ome_ui_kit::chart::ChartModel;
use ome_ui_kit::widgets::{Tone, button, gadget, segmented};

const INTERVALS: [(u64, &str); 6] = [
    (1_000, "1s"),
    (5_000, "5s"),
    (15_000, "15s"),
    (60_000, "1m"),
    (300_000, "5m"),
    (900_000, "15m"),
];

impl RetailApp {
    pub fn render_charts(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let Some(symbol) = self.selected else {
            return gadget(&t, "Charts", Some(t.links[0])).size_full();
        };
        let entity = cx.entity().downgrade();
        let intervals = {
            let entity = entity.clone();
            segmented(
                &t,
                "interval",
                &INTERVALS,
                self.interval,
                move |value, _, cx| {
                    entity
                        .update(cx, |app, cx| app.set_interval(value, cx))
                        .ok();
                },
            )
        };
        let toggle = |id: &'static str, label: &'static str, on: bool, flip: fn(&mut RetailApp)| {
            button(
                &t,
                id,
                label,
                if on { Tone::Primary } else { Tone::Plain },
                cx.listener(move |app, _, _, cx| {
                    flip(app);
                    cx.notify();
                }),
            )
        };
        let s = self.studies;
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_2()
            .h(px(32.))
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(14.))
                    .text_color(t.strong)
                    .child(symbol.to_string()),
            )
            .child(intervals)
            .child(div().w(px(12.)))
            .child(toggle("study-vol", "Volume", s.volume, |a| {
                a.studies.volume = !a.studies.volume
            }))
            .child(toggle("study-vwap", "VWAP", s.vwap, |a| {
                a.studies.vwap = !a.studies.vwap
            }))
            .child(toggle("study-sma", "SMA 20", s.sma.is_some(), |a| {
                a.studies.sma = if a.studies.sma.is_some() {
                    None
                } else {
                    Some(20)
                }
            }))
            .child(toggle("study-ema", "EMA 50", s.ema.is_some(), |a| {
                a.studies.ema = if a.studies.ema.is_some() {
                    None
                } else {
                    Some(50)
                }
            }))
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(t.faint)
                    .child("scroll to zoom · hover for the crosshair"),
            );
        let model = ChartModel {
            candles: self.store.candles(symbol, self.interval),
            last: self.store.last(symbol),
            reference: self.store.instrument(symbol).map(|i| i.reference_price),
            interval: self.interval,
            studies: self.studies,
            hover: self.hover,
            slot: self.slot,
        };
        let title = format!(
            "{symbol} · {} · {}",
            self.store
                .instrument(symbol)
                .map(|i| i.name.clone())
                .unwrap_or_default(),
            interval_label(self.interval)
        );
        gadget(&t, title, Some(t.links[0]))
            .size_full()
            .child(toolbar)
            .child(self.chart_area(model, cx))
    }
}
