use crate::theme::{MONO, SANS, Theme};
use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, FontWeight, IntoElement, ParentElement, Rgba,
    SharedString, Stateful, Styled, Window, div, prelude::*, px,
};

pub const ROW: f32 = 20.0;

pub fn gadget(theme: &Theme, title: impl Into<SharedString>, link: Option<Rgba>) -> Div {
    div()
        .flex()
        .flex_col()
        .min_h_0()
        .min_w_0()
        .overflow_hidden()
        .bg(theme.panel)
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(22.))
                .flex_none()
                .bg(theme.header)
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .children(link.map(|c| div().w(px(3.)).h(px(12.)).bg(c)))
                .child(title.into()),
        )
}

pub fn body() -> Div {
    div().flex().flex_col().flex_1().min_h_0().overflow_hidden()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Primary,
    Buy,
    Sell,
    Quiet,
}

pub fn button(
    theme: &Theme,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    tone: Tone,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (fg, bg, border) = match tone {
        Tone::Plain => (theme.text, theme.raised, theme.border),
        Tone::Primary => (theme.strong, theme.selection, theme.selection),
        Tone::Buy => (theme.strong, theme.buy_button, theme.up),
        Tone::Sell => (theme.strong, theme.sell_button, theme.down),
        Tone::Quiet => (theme.muted, theme.panel, theme.panel),
    };
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .px_3()
        .h(px(24.))
        .rounded(px(2.))
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(fg)
        .bg(bg)
        .border_1()
        .border_color(border)
        .cursor_pointer()
        .hover(|s| s.opacity(0.85))
        .active(|s| s.opacity(0.7))
        .on_click(on_click)
        .child(label.into())
}

pub fn segmented<T: Copy + PartialEq + 'static>(
    theme: &Theme,
    group: &'static str,
    options: &[(T, &'static str)],
    selected: T,
    on_select: impl Fn(T, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let mut bar = div()
        .flex()
        .flex_none()
        .border_1()
        .border_color(theme.border)
        .rounded(px(2.))
        .overflow_hidden();
    for (i, (value, label)) in options.iter().copied().enumerate() {
        let chosen = value == selected;
        let select = on_select.clone();
        bar = bar.child(
            div()
                .id((group, i))
                .px_2()
                .h(px(22.))
                .flex()
                .items_center()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .when(i > 0, |d| d.border_l_1().border_color(theme.border))
                .text_color(if chosen { theme.strong } else { theme.muted })
                .bg(if chosen { theme.selection } else { theme.field })
                .hover(|d| d.text_color(theme.strong))
                .on_click(move |_, window, cx| select(value, window, cx))
                .child(label),
        );
    }
    bar
}

pub fn main_tabs(
    theme: &Theme,
    labels: &[&'static str],
    active: usize,
    on_select: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let mut bar = div()
        .flex()
        .flex_none()
        .items_end()
        .gap_px()
        .px_2()
        .h(px(30.))
        .bg(theme.background)
        .border_b_1()
        .border_color(theme.border);
    for (i, label) in labels.iter().enumerate() {
        let chosen = i == active;
        let select = on_select.clone();
        bar = bar.child(
            div()
                .id(("main-tab", i))
                .px_4()
                .h(px(26.))
                .flex()
                .items_center()
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .border_t_2()
                .border_color(if chosen {
                    theme.accent
                } else {
                    theme.background
                })
                .bg(if chosen {
                    theme.panel
                } else {
                    theme.background
                })
                .text_color(if chosen { theme.strong } else { theme.muted })
                .hover(|d| d.text_color(theme.strong))
                .on_click(move |_, window, cx| select(i, window, cx))
                .child(*label),
        );
    }
    bar
}

pub fn sub_tabs(
    theme: &Theme,
    group: &'static str,
    labels: &[&'static str],
    active: usize,
    on_select: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let mut bar = div()
        .flex()
        .flex_none()
        .items_center()
        .gap_3()
        .px_2()
        .h(px(24.))
        .border_b_1()
        .border_color(theme.border);
    for (i, label) in labels.iter().enumerate() {
        let chosen = i == active;
        let select = on_select.clone();
        bar = bar.child(
            div()
                .id((group, i))
                .h_full()
                .flex()
                .items_center()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .border_b_2()
                .border_color(if chosen { theme.accent } else { theme.panel })
                .text_color(if chosen { theme.strong } else { theme.muted })
                .on_click(move |_, window, cx| select(i, window, cx))
                .child(*label),
        );
    }
    bar
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    Center,
}

pub fn th(theme: &Theme, text: impl Into<SharedString>, width: f32, align: Align) -> Div {
    cell_box(width, align)
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.muted)
        .child(text.into())
}

pub fn td(text: impl Into<SharedString>, width: f32, align: Align) -> Div {
    cell_box(width, align).font_family(MONO).child(text.into())
}

pub fn td_text(text: impl Into<SharedString>, width: f32, align: Align) -> Div {
    cell_box(width, align).font_family(SANS).child(text.into())
}

fn cell_box(width: f32, align: Align) -> Div {
    let d = div()
        .flex()
        .items_center()
        .h_full()
        .overflow_hidden()
        .whitespace_nowrap();
    let d = if width > 0.0 {
        d.w(px(width)).flex_none()
    } else {
        d.flex_1().min_w_0()
    };
    match align {
        Align::Left => d,
        Align::Right => d.justify_end(),
        Align::Center => d.justify_center(),
    }
}

pub fn table_header(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_3()
        .px_2()
        .h(px(ROW))
        .bg(theme.header)
        .border_b_1()
        .border_color(theme.border)
}

pub fn table_row(theme: &Theme, stripe: bool) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_3()
        .px_2()
        .h(px(ROW))
        .text_size(px(12.))
        .when(stripe, |d| d.bg(theme.field))
        .border_b_1()
        .border_color(theme.grid)
}

pub fn metric(
    theme: &Theme,
    label: &str,
    value: impl Into<SharedString>,
    color: Rgba,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap_px()
        .child(
            div()
                .text_size(px(10.))
                .text_color(theme.muted)
                .child(label.to_uppercase()),
        )
        .child(
            div()
                .font_family(MONO)
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(color)
                .child(value.into()),
        )
        .into_any_element()
}

pub fn badge(text: impl Into<SharedString>, fg: Rgba, bg: Rgba) -> Div {
    div()
        .flex_none()
        .px_2()
        .h(px(18.))
        .flex()
        .items_center()
        .rounded(px(2.))
        .text_size(px(10.5))
        .font_weight(FontWeight::BOLD)
        .text_color(fg)
        .bg(bg)
        .child(text.into())
}

pub fn kv(theme: &Theme, label: &str, value: impl Into<SharedString>, color: Rgba) -> Div {
    div()
        .flex()
        .justify_between()
        .items_center()
        .h(px(20.))
        .text_size(px(12.))
        .child(div().text_color(theme.muted).child(label.to_string()))
        .child(
            div()
                .font_family(MONO)
                .text_color(color)
                .child(value.into()),
        )
}

pub fn dialog(theme: &Theme, title: impl Into<SharedString>, width: f32) -> Div {
    div()
        .w(px(width))
        .flex()
        .flex_col()
        .bg(theme.panel)
        .border_1()
        .border_color(theme.border)
        .rounded(px(4.))
        .shadow_lg()
        .child(
            div()
                .px_3()
                .h(px(30.))
                .flex()
                .items_center()
                .bg(theme.header)
                .border_b_1()
                .border_color(theme.border)
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.strong)
                .child(title.into()),
        )
}

pub fn overlay(theme: &Theme) -> Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.overlay)
}
