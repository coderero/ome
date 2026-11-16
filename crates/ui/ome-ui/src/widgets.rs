use crate::theme::Theme;
use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, Painter, Pos2, Rect, Response, RichText, Sense,
    Shape, Stroke, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType, pos2,
};

pub const STRIP: f32 = 24.0;
pub const KV_ROW: f32 = 21.0;

pub fn chevron(painter: &Painter, center: Pos2, open: bool, color: Color32) {
    let points = if open {
        vec![center + Vec2::new(-4.0, -2.0), center + Vec2::new(4.0, -2.0), center + Vec2::new(0.0, 2.5)]
    } else {
        vec![center + Vec2::new(-2.0, -4.0), center + Vec2::new(2.5, 0.0), center + Vec2::new(-2.0, 4.0)]
    };
    painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
}

pub fn menu_icon(painter: &Painter, center: Pos2, color: Color32) {
    for dy in [-4.0, 0.0, 4.0] {
        painter.line_segment([center + Vec2::new(-5.0, dy), center + Vec2::new(5.0, dy)], Stroke::new(1.5, color));
    }
}

fn hline(painter: &Painter, rect: Rect, y: f32, color: Color32) {
    painter.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0, color));
}

pub struct Section {
    pub header: Response,
    pub menu: Option<Response>,
}

pub fn corner_mark(painter: &Painter, rect: Rect, color: Color32) {
    let p = rect.right_bottom() + Vec2::new(-1.0, -2.0);
    painter.add(Shape::convex_polygon(vec![p, p + Vec2::new(-4.0, 0.0), p + Vec2::new(0.0, -4.0)], color, Stroke::NONE));
}

pub fn menu_label(ui: &mut Ui, t: &Theme, text: &str) -> Response {
    let response = ui.add(egui::Label::new(RichText::new(text).color(t.text)).sense(Sense::click()).selectable(false));
    corner_mark(ui.painter(), response.rect.expand2(Vec2::new(6.0, 0.0)), t.muted);
    response
}

pub fn section(ui: &mut Ui, t: &Theme, title: &str, open: &mut bool, menu: bool, inline: impl FnOnce(&mut Ui)) -> Section {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), STRIP), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::CollapsingHeader, true, title));
    if response.clicked() {
        *open = !*open;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, if response.hovered() { t.button } else { t.strip });
    hline(painter, rect, rect.top() + 0.5, t.divider);
    hline(painter, rect, rect.bottom() - 0.5, t.base);
    chevron(painter, rect.left_center() + Vec2::new(12.0, 0.0), *open, t.text);
    let galley = painter.layout_no_wrap(title.to_string(), Theme::sans(13.0), t.text);
    let width = galley.size().x;
    painter.galley(pos2(rect.left() + 24.0, rect.center().y - galley.size().y / 2.0), galley, t.text);
    let mut right = rect.right() - 6.0;
    let mut menu_response = None;
    if menu {
        let icon = Rect::from_center_size(pos2(rect.right() - 16.0, rect.center().y), Vec2::new(22.0, 20.0));
        let r = ui.interact(icon, ui.id().with((title, "menu")), Sense::click());
        r.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("{title} menu")));
        if r.hovered() {
            ui.painter().rect_filled(icon, CornerRadius::same(2), t.button_hover);
        }
        menu_icon(ui.painter(), icon.center(), t.text);
        corner_mark(ui.painter(), icon, t.muted);
        menu_response = Some(r);
        right -= 28.0;
    }
    let inner = Rect::from_min_max(pos2(rect.left() + 36.0 + width, rect.top()), pos2(right, rect.bottom()));
    if inner.width() > 10.0 {
        let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
        child.spacing_mut().item_spacing.x = 10.0;
        inline(&mut child);
    }
    Section { header: response, menu: menu_response }
}

pub fn header_strip(ui: &mut Ui, t: &Theme, title: &str) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), STRIP), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, title));
    ui.painter().rect_filled(rect, CornerRadius::ZERO, t.strip);
    ui.painter().text(rect.left_center() + Vec2::new(10.0, 0.0), Align2::LEFT_CENTER, title, Theme::sans(13.0), t.text);
}

pub fn toolbar<R>(ui: &mut Ui, t: &Theme, add: impl FnOnce(&mut Ui) -> R) -> R {
    let response = Frame::new()
        .fill(t.base)
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
                add(ui)
            })
            .inner
        });
    let rect = response.response.rect;
    hline(ui.painter(), rect, rect.bottom() - 0.5, t.row_line);
    response.inner
}

pub fn grip(painter: &Painter, t: &Theme, center: Pos2, vertical: bool) {
    for k in -2..=2 {
        let d = k as f32 * 4.0;
        let p = if vertical { center + Vec2::new(0.0, d) } else { center + Vec2::new(d, 0.0) };
        painter.circle_filled(p, 1.0, t.muted);
    }
}

pub fn divider(ui: &mut Ui, t: &Theme) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, ui.available_height().min(18.0)), Sense::hover());
    ui.painter().line_segment([rect.center_top(), rect.center_bottom()], Stroke::new(1.0, t.divider));
}

pub fn main_tab(ui: &mut Ui, t: &Theme, label: &str, on: bool) -> Response {
    let text = RichText::new(label).font(Theme::sans(13.5)).color(if on { t.strong } else { t.text });
    let button = egui::Button::new(text)
        .fill(if on { t.tab_on } else { Color32::TRANSPARENT })
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::ZERO)
        .min_size(Vec2::new(0.0, 26.0))
        .selected(on);
    ui.add(button)
}

pub fn sub_tab(ui: &mut Ui, t: &Theme, label: &str, icon: Color32, on: bool) -> Response {
    let font = Theme::sans(13.0);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font.clone(), t.text);
    let size = Vec2::new(galley.size().x + 36.0, 24.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, on, label));
    let painter = ui.painter();
    let frame = rect.shrink2(Vec2::new(2.0, 1.0));
    if on {
        painter.rect_filled(frame, CornerRadius::same(2), t.base);
        painter.rect_stroke(frame, CornerRadius::same(2), Stroke::new(1.0, t.divider), egui::StrokeKind::Inside);
    } else if response.hovered() {
        painter.rect_filled(frame, CornerRadius::same(2), t.button);
    }
    let icon_rect = Rect::from_center_size(rect.left_center() + Vec2::new(14.0, 0.0), Vec2::new(10.0, 12.0));
    painter.rect_filled(icon_rect, CornerRadius::same(1), icon);
    for dy in [4.0, 7.0] {
        painter.line_segment([icon_rect.left_top() + Vec2::new(2.0, dy), icon_rect.right_top() + Vec2::new(-2.0, dy)], Stroke::new(1.0, t.base));
    }
    painter.text(rect.left_center() + Vec2::new(26.0, 0.0), Align2::LEFT_CENTER, label, font, if on { t.strong } else { t.text });
    response
}

pub fn kv_row(ui: &mut Ui, t: &Theme, label: &str, value: &str, color: Color32) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), KV_ROW), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, label));
    let painter = ui.painter();
    hline(painter, rect, rect.bottom() - 0.5, t.row_line);
    painter.text(rect.left_center() + Vec2::new(10.0, 0.0), Align2::LEFT_CENTER, label, Theme::sans(13.0), t.text);
    painter.text(rect.right_center() - Vec2::new(8.0, 0.0), Align2::RIGHT_CENTER, value, Theme::bold(13.0), color);
}

pub fn kv(ui: &mut Ui, t: &Theme, label: &str, value: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(t.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).font(Theme::bold(13.0)).color(color));
        });
    });
}

pub fn value(ui: &mut Ui, t: &Theme, label: &str, text: &str, color: Color32) {
    let mut job = egui::text::LayoutJob::default();
    job.append(label, 0.0, egui::TextFormat::simple(Theme::sans(13.0), t.text));
    job.append(text, 5.0, egui::TextFormat::simple(Theme::bold(13.0), color));
    job.wrap.max_width = f32::INFINITY;
    ui.add(egui::Label::new(job).wrap_mode(egui::TextWrapMode::Extend).selectable(false));
}

pub fn badge(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) -> Response {
    Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(2))
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(Theme::bold(12.0)).color(fg));
        })
        .response
}

pub fn filled_button(ui: &mut Ui, t: &Theme, text: &str, fill: Color32, width: f32) -> Response {
    let button = egui::Button::new(RichText::new(text).font(Theme::bold(13.0)).color(t.on_fill))
        .fill(fill)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(2))
        .min_size(Vec2::new(width, 24.0));
    ui.add(button)
}

pub fn plain_button(ui: &mut Ui, text: &str, width: f32) -> Response {
    ui.add(egui::Button::new(text).min_size(Vec2::new(width, 24.0)))
}

pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, t: &Theme, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (option, label) in options {
            let chosen = *value == *option;
            let text = RichText::new(*label).color(if chosen { t.strong } else { t.text });
            let button = egui::Button::new(text)
                .fill(if chosen { t.tab_on } else { t.button })
                .stroke(Stroke::new(1.0, t.button_line))
                .corner_radius(CornerRadius::ZERO)
                .min_size(Vec2::new(0.0, 22.0))
                .selected(chosen);
            if ui.add(button).clicked() && !chosen {
                *value = *option;
                changed = true;
            }
        }
    });
    changed
}

pub fn empty_state(ui: &mut Ui, t: &Theme, title: &str, hint: &str) {
    ui.add_space(14.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).color(t.text));
        ui.label(RichText::new(hint).color(t.muted));
    });
    ui.add_space(14.0);
}

pub fn padded<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new().inner_margin(Margin::symmetric(10, 6)).show(ui, add).inner
}

pub fn dialog(t: &Theme) -> Frame {
    Frame::new()
        .fill(t.base)
        .stroke(Stroke::new(1.0, t.button_line))
        .inner_margin(Margin::ZERO)
}

pub fn dialog_body<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new().inner_margin(Margin::same(12)).show(ui, add).inner
}

pub fn shortcuts(ctx: &egui::Context, t: &Theme, open: &mut bool, keys: &[(&str, &str)]) {
    if !*open {
        return;
    }
    let response = egui::Modal::new("shortcuts".into()).frame(dialog(t)).show(ctx, |ui| {
        ui.set_width(440.0);
        header_strip(ui, t, "Keyboard shortcuts");
        let mut close = false;
        ui.add_space(4.0);
        for (key, action) in keys {
            kv_row(ui, t, action, key, t.strong);
        }
        dialog_body(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                close = plain_button(ui, "Close", 80.0).clicked();
            });
        });
        close
    });
    if response.inner || response.should_close() {
        *open = false;
    }
}
