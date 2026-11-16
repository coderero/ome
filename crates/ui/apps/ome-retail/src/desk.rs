use crate::app::Core;
use crate::view::{Gadget, Kind, Link};
use egui::{RichText, Ui, WidgetText};
use egui_dock::NodePath;
use ome_ui::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Add(NodePath, Kind),
    Detach(u32),
    Link(u32, Link),
}

pub struct Desk<'a> {
    pub core: &'a mut Core,
    pub actions: Vec<Action>,
}

pub fn link_menu(ui: &mut Ui, current: Link) -> Option<Link> {
    let mut chosen = None;
    for link in Link::ALL {
        let text = RichText::new(format!("■  {}", link.label())).color(link.color());
        if ui.selectable_label(current == link, text).clicked() {
            chosen = Some(link);
            ui.close();
        }
    }
    chosen
}

impl egui_dock::TabViewer for Desk<'_> {
    type Tab = Gadget;

    fn title(&mut self, g: &mut Gadget) -> WidgetText {
        let symbol = g.kind.follows_symbol().then(|| self.core.symbol_for(g)).flatten();
        let title = match symbol {
            Some(s) => format!("{} · {s}", g.kind.title()),
            None => g.kind.title().to_string(),
        };
        RichText::new(title).font(Theme::sans(13.0)).into()
    }

    fn id(&mut self, g: &mut Gadget) -> egui::Id {
        egui::Id::new(("gadget", g.id))
    }

    fn ui(&mut self, ui: &mut Ui, g: &mut Gadget) {
        let visible = ui.max_rect().intersect(ui.clip_rect());
        ui.scope_builder(egui::UiBuilder::new().max_rect(visible), |ui| {
            ui.set_max_size(visible.size());
            self.core.gadget(ui, g);
        });
    }

    fn on_tab_button(&mut self, g: &mut Gadget, response: &egui::Response) {
        let title = self.title(g).text().to_string();
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title.clone()));
    }

    fn context_menu(&mut self, ui: &mut Ui, g: &mut Gadget, _path: NodePath) {
        ui.label(RichText::new("Symbol link").color(self.core.theme.muted));
        if let Some(link) = link_menu(ui, g.link) {
            self.actions.push(Action::Link(g.id, link));
        }
        ui.separator();
        if ui.button("Detach to a window").clicked() {
            self.actions.push(Action::Detach(g.id));
            ui.close();
        }
    }

    fn add_popup(&mut self, ui: &mut Ui, path: NodePath) {
        ui.set_min_width(170.0);
        ui.label(RichText::new("Add gadget").color(self.core.theme.muted));
        for kind in Kind::ALL {
            if ui.button(kind.title()).clicked() {
                self.actions.push(Action::Add(path, kind));
                ui.close();
            }
        }
    }

    fn scroll_bars(&self, _g: &Gadget) -> [bool; 2] {
        [false, false]
    }

    fn allowed_in_windows(&self, _g: &mut Gadget) -> bool {
        false
    }
}
