use crate::theme::Theme;
use egui::{Color32, CornerRadius, Layout, Rect, RichText, Sense, Stroke, Ui, UiBuilder, Vec2, pos2};
use std::collections::BTreeSet;
use egui_extras::{Column, TableBuilder};

pub const ROW: f32 = 21.0;
pub const HEAD: f32 = 21.0;
const PAD: f32 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Col<'a> {
    pub title: &'a str,
    pub width: Option<f32>,
    pub align: Align,
}

impl<'a> Col<'a> {
    pub fn left(title: &'a str, width: f32) -> Self {
        Self { title, width: Some(width), align: Align::Left }
    }

    pub fn right(title: &'a str, width: f32) -> Self {
        Self { title, width: Some(width), align: Align::Right }
    }

    pub fn center(title: &'a str, width: f32) -> Self {
        Self { title, width: Some(width), align: Align::Center }
    }

    pub fn fill(title: &'a str, align: Align) -> Self {
        Self { title, width: None, align }
    }

    pub fn column(&self) -> Column {
        match self.width {
            Some(w) => Column::exact(w).clip(true),
            None => Column::remainder().at_least(40.0).clip(true),
        }
    }
}

fn layout(align: Align) -> Layout {
    match align {
        Align::Left => Layout::left_to_right(egui::Align::Center),
        Align::Right => Layout::right_to_left(egui::Align::Center),
        Align::Center => Layout::centered_and_justified(egui::Direction::LeftToRight),
    }
}

fn inner<R>(ui: &mut Ui, align: Align, add: impl FnOnce(&mut Ui) -> R) -> R {
    let rect = ui.max_rect().shrink2(Vec2::new(PAD, 0.0));
    ui.scope_builder(UiBuilder::new().max_rect(rect).layout(layout(align)), |ui| {
        ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
        add(ui)
    })
    .inner
}

fn edges(ui: &Ui, rect: Rect, side: Color32, bottom: Option<Color32>) {
    let painter = ui.painter();
    painter.line_segment([pos2(rect.right() - 0.5, rect.top()), pos2(rect.right() - 0.5, rect.bottom())], Stroke::new(1.0, side));
    if let Some(color) = bottom {
        painter.line_segment([pos2(rect.left(), rect.bottom() - 0.5), pos2(rect.right(), rect.bottom() - 0.5)], Stroke::new(1.0, color));
    }
}

pub fn lines(ui: &Ui, t: &Theme) {
    edges(ui, ui.max_rect(), t.row_line, Some(t.row_line));
}

pub fn fill(ui: &Ui, color: Color32) {
    let rect = ui.max_rect().expand2(Vec2::new(PAD, 0.0)).shrink2(Vec2::new(0.5, 0.5));
    ui.painter().rect_filled(rect, CornerRadius::ZERO, color);
}

pub fn head(ui: &mut Ui, t: &Theme, title: &str, align: Align) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, CornerRadius::ZERO, t.head);
    edges(ui, rect, t.divider, Some(t.divider));
    inner(ui, align, |ui| {
        ui.add(egui::Label::new(RichText::new(title).color(t.text)).selectable(false).truncate());
    });
}

pub fn cell<R>(ui: &mut Ui, t: &Theme, align: Align, add: impl FnOnce(&mut Ui) -> R) -> R {
    lines(ui, t);
    inner(ui, align, add)
}

pub fn text(ui: &mut Ui, t: &Theme, align: Align, value: RichText) {
    cell(ui, t, align, |ui| {
        ui.add(egui::Label::new(value).selectable(false).truncate());
    });
}

pub fn builder<'a>(ui: &'a mut Ui, id: &str, cols: &[Col<'_>]) -> TableBuilder<'a> {
    let mut b = TableBuilder::new(ui).id_salt(id).striped(false).resizable(false);
    for c in cols {
        b = b.column(c.column());
    }
    b
}

pub struct Grid<'a> {
    pub id: &'a str,
    pub cols: &'a [Col<'a>],
    pub rows: usize,
    pub row_height: f32,
    pub selected: Option<usize>,
    pub clickable: bool,
    pub scroll: bool,
    pub tints: Option<&'a [Option<Color32>]>,
    pub hidden: Option<&'a BTreeSet<String>>,
}

pub fn key(table: &str, column: &str) -> String {
    format!("{table}:{column}")
}

pub fn customize(ui: &mut Ui, t: &Theme, table: &str, cols: &[Col<'_>], hidden: &mut BTreeSet<String>) {
    let gear = ui.add(egui::Button::new(RichText::new("⚙").color(t.text)).frame(false)).on_hover_text("Customize columns");
    gear.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Customize {table}")));
    egui::Popup::menu(&gear).show(|ui| {
        ui.label(RichText::new("Show columns").color(t.muted));
        for c in cols.iter().skip(1).filter(|c| !c.title.is_empty()) {
            let k = key(table, c.title);
            let mut shown = !hidden.contains(&k);
            if ui.checkbox(&mut shown, c.title).changed() {
                if shown {
                    hidden.remove(&k);
                } else {
                    hidden.insert(k);
                }
            }
        }
        if ui.button("Reset").clicked() {
            hidden.retain(|k| !k.starts_with(&format!("{table}:")));
        }
    });
}

impl<'a> Grid<'a> {
    pub fn new(id: &'a str, cols: &'a [Col<'a>], rows: usize) -> Self {
        Self { id, cols, rows, row_height: ROW, selected: None, clickable: false, scroll: false, tints: None, hidden: None }
    }

    pub fn selected(mut self, row: Option<usize>) -> Self {
        self.selected = row;
        self.clickable = true;
        self
    }

    pub fn tints(mut self, tints: &'a [Option<Color32>]) -> Self {
        self.tints = Some(tints);
        self
    }

    pub fn hidden(mut self, hidden: &'a BTreeSet<String>) -> Self {
        self.hidden = Some(hidden);
        self
    }

    pub fn scroll(mut self) -> Self {
        self.scroll = true;
        self
    }

    pub fn show(self, ui: &mut Ui, t: &Theme, mut add: impl FnMut(&mut Ui, usize, usize)) -> Option<usize> {
        let mut clicked = None;
        let visible: Vec<usize> = (0..self.cols.len())
            .filter(|&i| i == 0 || self.cols[i].title.is_empty() || !self.hidden.is_some_and(|h| h.contains(&key(self.id, self.cols[i].title))))
            .collect();
        let cols: Vec<Col<'_>> = visible.iter().map(|&i| self.cols[i]).collect();
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let mut b = builder(ui, self.id, &cols).vscroll(self.scroll);
            if self.scroll {
                b = b.auto_shrink([false, false]);
            }
            if self.clickable {
                b = b.sense(Sense::click());
            }
            b.header(HEAD, |mut h| {
                for c in &cols {
                    h.col(|ui| head(ui, t, c.title, c.align));
                }
            })
            .body(|body| {
                body.rows(self.row_height, self.rows, |mut row| {
                    let index = row.index();
                    let selected = self.selected == Some(index);
                    row.set_selected(selected);
                    let tint = self.tints.and_then(|tints| tints.get(index).copied().flatten()).filter(|_| !selected);
                    for &i in &visible {
                        let c = self.cols[i];
                        row.col(|ui| {
                            if let Some(color) = tint {
                                ui.painter().rect_filled(ui.max_rect(), CornerRadius::ZERO, color);
                            }
                            cell(ui, t, c.align, |ui| add(ui, index, i))
                        });
                    }
                    if self.clickable && row.response().clicked() {
                        clicked = Some(index);
                    }
                });
            });
        });
        clicked
    }
}
