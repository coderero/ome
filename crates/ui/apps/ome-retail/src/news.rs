use crate::app::RetailApp;
use gpui::{Context, FontWeight, IntoElement, div, prelude::*, px};
use ome_protocol::NewsKind;
use ome_ui_data::NewsFeed;
use ome_ui_kit::format;
use ome_ui_kit::theme::MONO;
use ome_ui_kit::widgets::{Align, badge, body, gadget, table_header, table_row, td, td_text, th};

impl RetailApp {
    pub fn render_news(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let items: Vec<_> = self.store.news().into_iter().cloned().collect();
        let selected = self
            .news_selected
            .and_then(|seq| items.iter().find(|i| i.seq == seq))
            .or(items.first())
            .cloned();
        let mut list = body().child(
            table_header(&t)
                .child(th(&t, "Time", 62., Align::Left))
                .child(th(&t, "Subject", 76., Align::Left))
                .child(th(&t, "Tone", 40., Align::Right))
                .child(th(&t, "Fair", 58., Align::Right))
                .child(th(&t, "Headline", 0., Align::Left)),
        );
        for (i, item) in items.iter().take(150).enumerate() {
            let seq = item.seq;
            let tag = match item.kind {
                NewsKind::Macro => "ECONOMY".to_string(),
                NewsKind::Sector => item.sector.to_uppercase(),
                _ => item.symbol.map(|s| s.to_string()).unwrap_or_default(),
            };
            let chosen = selected.as_ref().is_some_and(|s| s.seq == seq);
            list = list.child(
                table_row(&t, i % 2 == 1)
                    .id(("news", i))
                    .h(px(22.))
                    .cursor_pointer()
                    .when(chosen, |d| d.bg(t.selection))
                    .hover(|d| d.bg(t.raised))
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.news_selected = Some(seq);
                        cx.notify();
                    }))
                    .child(td(format::clock(item.time), 62., Align::Left).text_color(t.muted))
                    .child(
                        td_text(tag, 76., Align::Left)
                            .font_weight(FontWeight::BOLD)
                            .text_color(if chosen { t.strong } else { t.accent }),
                    )
                    .child(
                        td(format!("{:+}", item.sentiment), 40., Align::Right)
                            .text_color(t.tone(item.sentiment as f64)),
                    )
                    .child(
                        td(format!("{:+}", item.impact_bps), 58., Align::Right)
                            .text_color(t.tone(item.impact_bps as f64)),
                    )
                    .child(td_text(item.headline.clone(), 0., Align::Left).text_color(t.strong)),
            );
        }
        let reader = match selected {
            Some(item) => div()
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(badge(
                            item.kind.label().to_uppercase(),
                            t.background,
                            t.accent,
                        ))
                        .children(
                            item.symbol
                                .map(|s| badge(s.to_string(), t.strong, t.raised)),
                        )
                        .child(badge(item.sector.to_uppercase(), t.muted, t.raised))
                        .child(
                            div()
                                .font_family(MONO)
                                .text_size(px(11.))
                                .text_color(t.muted)
                                .child(format!(
                                    "day {} · {} UTC",
                                    item.day,
                                    format::clock(item.time)
                                )),
                        ),
                )
                .child(
                    div()
                        .text_size(px(18.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(t.strong)
                        .child(item.headline.clone()),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .line_height(px(20.))
                        .text_color(t.text)
                        .child(item.body.clone()),
                )
                .child(
                    div()
                        .flex()
                        .gap_6()
                        .pt_2()
                        .border_t_1()
                        .border_color(t.border)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(t.muted)
                                        .child("SENTIMENT"),
                                )
                                .child(
                                    div()
                                        .font_family(MONO)
                                        .text_size(px(16.))
                                        .text_color(t.tone(item.sentiment as f64))
                                        .child(format!("{:+}", item.sentiment)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(t.muted)
                                        .child("FAIR VALUE IMPACT"),
                                )
                                .child(
                                    div()
                                        .font_family(MONO)
                                        .text_size(px(16.))
                                        .text_color(t.tone(item.impact_bps as f64))
                                        .child(format!("{:+} bps", item.impact_bps)),
                                ),
                        ),
                )
                .children((!item.data.is_empty()).then(|| {
                    let mut chips = div().flex().flex_wrap().gap_1();
                    for pair in item.data.split_whitespace() {
                        chips = chips.child(badge(pair.replace('=', " "), t.text, t.raised));
                    }
                    chips
                })),
            None => div().p_4().text_color(t.muted).child("No stories yet."),
        };
        div()
            .flex()
            .size_full()
            .gap_1()
            .child(
                gadget(&t, "News", Some(t.links[1]))
                    .flex_1()
                    .min_w_0()
                    .child(list),
            )
            .child(
                gadget(&t, "Story", Some(t.links[1]))
                    .w(px(520.))
                    .flex_none()
                    .child(reader),
            )
    }
}
