use crate::theme::Theme;
use egui::{Align2, CornerRadius, Frame, Margin, RichText, Stroke, Vec2};
use std::time::{Duration, Instant};

const LIFETIME: Duration = Duration::from_secs(6);
const MAX: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Good,
    Bad,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub level: Level,
    pub title: String,
    pub detail: String,
    pub at: Instant,
}

#[derive(Debug, Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, level: Level, title: impl Into<String>, detail: impl Into<String>) {
        self.items.push(Toast {
            level,
            title: title.into(),
            detail: detail.into(),
            at: Instant::now(),
        });
        if self.items.len() > MAX {
            self.items.remove(0);
        }
    }

    pub fn active(&self) -> impl Iterator<Item = &Toast> {
        self.items.iter().filter(|t| t.at.elapsed() < LIFETIME)
    }

    pub fn show(&mut self, ctx: &egui::Context, t: &Theme) {
        self.items.retain(|toast| toast.at.elapsed() < LIFETIME);
        if self.items.is_empty() {
            return;
        }
        egui::Area::new("toasts".into())
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-12.0, -30.0))
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_width(320.0);
                for toast in self.items.iter().rev() {
                    let accent = match toast.level {
                        Level::Info => t.accent,
                        Level::Good => t.up,
                        Level::Bad => t.down,
                    };
                    Frame::new()
                        .fill(t.strip)
                        .stroke(Stroke::new(1.0, t.button_line))
                        .inner_margin(Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(Vec2::new(3.0, 32.0), egui::Sense::hover());
                                ui.painter().rect_filled(rect, CornerRadius::ZERO, accent);
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    ui.label(RichText::new(&toast.title).font(Theme::bold(12.5)).color(t.strong));
                                    if !toast.detail.is_empty() {
                                        ui.label(RichText::new(&toast.detail).color(t.text));
                                    }
                                });
                            });
                        });
                    ui.add_space(6.0);
                }
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_keep_the_latest_few() {
        let mut toasts = Toasts::default();
        for i in 0..8 {
            toasts.push(Level::Info, format!("t{i}"), "");
        }
        let titles: Vec<_> = toasts.active().map(|t| t.title.clone()).collect();
        assert_eq!(titles, ["t3", "t4", "t5", "t6", "t7"]);
    }
}
