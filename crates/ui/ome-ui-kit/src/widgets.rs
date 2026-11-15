use crate::theme::{MONO, Theme};
use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, FontWeight, IntoElement, ParentElement, Rgba,
    SharedString, Stateful, Styled, Window, div, prelude::*, px,
};

pub fn panel(theme: &Theme, title: impl Into<SharedString>, link: Option<Rgba>) -> Div {
    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .h(px(24.))
        .border_b_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.muted)
        .child(
            div()
                .w(px(6.))
                .h(px(6.))
                .rounded_full()
                .bg(link.unwrap_or(theme.faint)),
        )
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title.into()));
    div()
        .flex()
        .flex_col()
        .min_h_0()
        .min_w_0()
        .overflow_hidden()
        .bg(theme.panel)
        .border_1()
        .border_color(theme.border)
        .rounded(px(4.))
        .child(header)
}

pub fn body() -> Div {
    div().flex().flex_col().flex_1().min_h_0().overflow_hidden()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Buy,
    Sell,
    Accent,
}

pub fn button(
    theme: &Theme,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    tone: Tone,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (fg, bg) = match (tone, active) {
        (Tone::Buy, _) => (theme.background, theme.buy),
        (Tone::Sell, _) => (theme.background, theme.sell),
        (Tone::Accent, true) | (Tone::Plain, true) => (theme.background, theme.accent),
        (Tone::Accent, false) => (theme.accent, theme.raised),
        (Tone::Plain, false) => (theme.text, theme.raised),
    };
    div()
        .id(id.into())
        .flex()
        .items_center()
        .justify_center()
        .px_2()
        .h(px(24.))
        .rounded(px(3.))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(fg)
        .bg(bg)
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(|s| s.opacity(0.85))
        .on_click(on_click)
        .child(label.into())
}

pub fn mono(text: impl Into<SharedString>) -> Div {
    div().font_family(MONO).child(text.into())
}

pub fn cell(text: impl Into<SharedString>, width: f32, right: bool) -> Div {
    let d = div()
        .w(px(width))
        .flex_none()
        .overflow_hidden()
        .whitespace_nowrap();
    let d = if right { d.flex().justify_end() } else { d };
    d.font_family(MONO).child(text.into())
}

pub fn row() -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .h(px(20.))
        .text_xs()
}

pub fn header_row(theme: &Theme) -> Div {
    row()
        .text_color(theme.muted)
        .border_b_1()
        .border_color(theme.border)
}

pub fn stat(theme: &Theme, label: &str, value: impl Into<SharedString>, color: Rgba) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted)
                .child(label.to_string()),
        )
        .child(
            div()
                .font_family(MONO)
                .text_sm()
                .text_color(color)
                .child(value.into()),
        )
        .into_any_element()
}

pub fn tabs(
    theme: &Theme,
    group: &'static str,
    labels: &[&'static str],
    active: usize,
    on_select: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let mut bar = div()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .h(px(26.))
        .border_b_1()
        .border_color(theme.border);
    for (i, label) in labels.iter().enumerate() {
        let select = on_select.clone();
        let chosen = i == active;
        bar = bar.child(
            div()
                .id((group, i))
                .px_2()
                .h(px(22.))
                .flex()
                .items_center()
                .text_xs()
                .rounded(px(3.))
                .cursor_pointer()
                .text_color(if chosen { theme.text } else { theme.muted })
                .when(chosen, |d| d.bg(theme.raised))
                .on_click(move |_, window, cx| select(i, window, cx))
                .child(format!("{} {}", i + 1, label)),
        );
    }
    bar
}
