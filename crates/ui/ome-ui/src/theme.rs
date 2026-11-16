use crate::fonts;
use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Visuals};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Dark,
    Light,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Dark => "dark",
            Mode::Light => "light",
        }
    }

    pub fn parse(text: &str) -> Option<Mode> {
        match text.trim() {
            "dark" => Some(Mode::Dark),
            "light" => Some(Mode::Light),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub mode: Mode,
    pub base: Color32,
    pub status: Color32,
    pub tab_bar: Color32,
    pub tab_on: Color32,
    pub strip: Color32,
    pub toolbar: Color32,
    pub head: Color32,
    pub buy_row: Color32,
    pub sell_row: Color32,
    pub divider: Color32,
    pub row_line: Color32,
    pub grid: Color32,
    pub text: Color32,
    pub strong: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub selection: Color32,
    pub button: Color32,
    pub button_line: Color32,
    pub button_hover: Color32,
    pub field: Color32,
    pub field_line: Color32,
    pub up: Color32,
    pub down: Color32,
    pub up_fill: Color32,
    pub down_fill: Color32,
    pub tick_down_fill: Color32,
    pub up_soft: Color32,
    pub down_soft: Color32,
    pub bid: Color32,
    pub bid_soft: Color32,
    pub ask: Color32,
    pub ask_soft: Color32,
    pub warning: Color32,
    pub last: Color32,
    pub on_fill: Color32,
    pub studies: [Color32; 3],
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn soft(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

impl Theme {
    pub fn of(mode: Mode) -> Self {
        match mode {
            Mode::Dark => Self::dark(),
            Mode::Light => Self::light(),
        }
    }

    pub fn dark() -> Self {
        let up = rgb(0x3c_cf_3c);
        let down = rgb(0xff_45_45);
        let bid = rgb(0x5a_a0_ff);
        let ask = rgb(0xff_5a_5a);
        Self {
            mode: Mode::Dark,
            base: rgb(0x00_00_00),
            status: rgb(0x00_00_00),
            tab_bar: rgb(0x29_29_29),
            tab_on: rgb(0x4a_4a_4a),
            strip: rgb(0x2e_2e_2e),
            toolbar: rgb(0x33_33_33),
            head: rgb(0x1b_1b_1b),
            buy_row: rgb(0x00_34_0e),
            sell_row: rgb(0x56_0b_0b),
            divider: rgb(0x40_40_40),
            row_line: rgb(0x1e_1e_1e),
            grid: rgb(0x1a_1a_1a),
            text: rgb(0xd8_d8_d8),
            strong: rgb(0xf0_f0_f0),
            muted: rgb(0x9a_9a_9a),
            faint: rgb(0x6e_6e_6e),
            accent: rgb(0x3f_8c_e8),
            selection: rgb(0x00_5e_82),
            button: rgb(0x34_34_34),
            button_line: rgb(0x50_50_50),
            button_hover: rgb(0x46_46_46),
            field: rgb(0x1e_1e_1e),
            field_line: rgb(0x4c_4c_4c),
            up,
            down,
            up_fill: rgb(0x0a_5a_22),
            down_fill: rgb(0x7a_12_12),
            tick_down_fill: rgb(0x7a_12_12),
            up_soft: soft(up, 60),
            down_soft: soft(down, 60),
            bid,
            bid_soft: soft(bid, 70),
            ask,
            ask_soft: soft(ask, 70),
            warning: rgb(0xff_9f_1a),
            last: rgb(0xff_d2_3f),
            on_fill: rgb(0xff_ff_ff),
            studies: [rgb(0xff_d2_3f), rgb(0xc0_8c_ff), rgb(0x3d_d6_c6)],
        }
    }

    pub fn light() -> Self {
        let up = rgb(0x0b_7a_28);
        let down = rgb(0xc8_1e_1e);
        let bid = rgb(0x1f_62_c4);
        Self {
            mode: Mode::Light,
            base: rgb(0xff_ff_ff),
            status: rgb(0xf2_f2_f2),
            tab_bar: rgb(0xe4_e4_e4),
            tab_on: rgb(0xff_ff_ff),
            strip: rgb(0xdc_dc_dc),
            toolbar: rgb(0xec_ec_ec),
            head: rgb(0xf0_f0_f0),
            buy_row: rgb(0xd8_f0_dc),
            sell_row: rgb(0xf6_d8_d8),
            divider: rgb(0xb8_b8_b8),
            row_line: rgb(0xe6_e6_e6),
            grid: rgb(0xf0_f0_f0),
            text: rgb(0x1e_1e_1e),
            strong: rgb(0x00_00_00),
            muted: rgb(0x5a_5a_5a),
            faint: rgb(0x9a_9a_9a),
            accent: rgb(0x1f_62_c4),
            selection: rgb(0xc4_e2_f2),
            button: rgb(0xee_ee_ee),
            button_line: rgb(0xa8_a8_a8),
            button_hover: rgb(0xdd_dd_dd),
            field: rgb(0xff_ff_ff),
            field_line: rgb(0xa0_a0_a0),
            up,
            down,
            up_fill: rgb(0x1e8c32),
            down_fill: rgb(0xc8_1e_1e),
            tick_down_fill: rgb(0xc0_52_00),
            up_soft: soft(up, 48),
            down_soft: soft(down, 48),
            bid,
            bid_soft: soft(bid, 48),
            ask: down,
            ask_soft: soft(down, 48),
            warning: rgb(0xa3_55_00),
            last: rgb(0x8a_66_00),
            on_fill: rgb(0xff_ff_ff),
            studies: [rgb(0xc4_8a_00), rgb(0x7b_3f_d1), rgb(0x00_8f_86)],
        }
    }

    pub fn tone(&self, value: f64) -> Color32 {
        if value > 0.0 {
            self.up
        } else if value < 0.0 {
            self.down
        } else {
            self.text
        }
    }

    pub fn apply(&self, ctx: &egui::Context) {
        ctx.set_fonts(fonts::definitions());
        let mut style = (*ctx.global_style()).clone();
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
            (TextStyle::Heading, FontId::new(15.0, fonts::bold())),
        ]
        .into();
        let spacing = &mut style.spacing;
        spacing.item_spacing = egui::vec2(6.0, 4.0);
        spacing.button_padding = egui::vec2(8.0, 2.0);
        spacing.interact_size = egui::vec2(24.0, 22.0);
        spacing.window_margin = Margin::same(0);
        spacing.menu_margin = Margin::same(2);
        spacing.menu_spacing = 0.0;
        spacing.scroll.bar_width = 8.0;
        spacing.scroll.floating = false;
        let mut v = match self.mode {
            Mode::Dark => Visuals::dark(),
            Mode::Light => Visuals::light(),
        };
        v.panel_fill = self.base;
        v.window_fill = self.base;
        v.extreme_bg_color = self.field;
        v.text_edit_bg_color = Some(self.field);
        v.faint_bg_color = self.grid;
        v.code_bg_color = self.field;
        v.window_stroke = Stroke::new(1.0, self.button_line);
        v.window_corner_radius = CornerRadius::ZERO;
        v.menu_corner_radius = CornerRadius::ZERO;
        v.window_shadow = Shadow::NONE;
        v.popup_shadow = Shadow::NONE;
        v.selection.bg_fill = self.selection;
        v.selection.stroke = Stroke::new(1.0, self.strong);
        v.hyperlink_color = self.accent;
        v.override_text_color = Some(self.text);
        v.warn_fg_color = self.warning;
        v.error_fg_color = self.down;
        v.striped = false;
        v.text_options.subpixel_binning = false;
        v.text_options.font_hinting = true;
        v.indent_has_left_vline = false;
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.base;
        w.noninteractive.weak_bg_fill = self.base;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.divider);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.text);
        w.inactive.bg_fill = self.field;
        w.inactive.weak_bg_fill = self.button;
        w.inactive.bg_stroke = Stroke::new(1.0, self.button_line);
        w.inactive.fg_stroke = Stroke::new(1.0, self.text);
        w.hovered.bg_fill = self.button_hover;
        w.hovered.weak_bg_fill = self.button_hover;
        w.hovered.bg_stroke = Stroke::new(1.0, self.field_line);
        w.hovered.fg_stroke = Stroke::new(1.0, self.strong);
        w.active.bg_fill = self.tab_on;
        w.active.weak_bg_fill = self.tab_on;
        w.active.bg_stroke = Stroke::new(1.0, self.field_line);
        w.active.fg_stroke = Stroke::new(1.0, self.strong);
        for state in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active] {
            state.corner_radius = CornerRadius::same(2);
            state.expansion = 0.0;
        }
        w.noninteractive.corner_radius = CornerRadius::ZERO;
        w.open = w.active;
        style.visuals = v;
        ctx.all_styles_mut(|s| *s = style.clone());
    }

    pub fn dock_style(&self, egui_style: &egui::Style) -> egui_dock::Style {
        let mut s = egui_dock::Style::from_egui(egui_style);
        s.dock_area_padding = Some(Margin::ZERO);
        s.main_surface_border_stroke = Stroke::NONE;
        s.main_surface_border_rounding = CornerRadius::ZERO;
        s.tab_bar.bg_fill = self.toolbar;
        s.tab_bar.height = 26.0;
        s.tab_bar.corner_radius = CornerRadius::ZERO;
        s.tab_bar.hline_color = self.base;
        s.tab_bar.inner_margin = Margin { left: 4, right: 2, top: 1, bottom: 1 };
        s.tab.spacing = 2.0;
        s.tab.hline_below_active_tab_name = false;
        s.tab.minimum_width = None;
        s.tab.tab_body.bg_fill = self.base;
        s.tab.tab_body.inner_margin = Margin::ZERO;
        s.tab.tab_body.stroke = Stroke::new(1.0, self.divider);
        s.tab.tab_body.corner_radius = CornerRadius::ZERO;
        let state = |bg: Color32, outline: Color32, text: Color32| egui_dock::TabInteractionStyle {
            outline_color: outline,
            corner_radius: CornerRadius::same(2),
            bg_fill: bg,
            text_color: text,
        };
        s.tab.active = state(self.base, self.divider, self.strong);
        s.tab.focused = state(self.base, self.accent, self.strong);
        s.tab.active_with_kb_focus = s.tab.active.clone();
        s.tab.focused_with_kb_focus = s.tab.focused.clone();
        s.tab.inactive = state(self.toolbar, self.toolbar, self.text);
        s.tab.inactive_with_kb_focus = s.tab.inactive.clone();
        s.tab.hovered = state(self.button_hover, self.button_hover, self.strong);
        s.buttons.close_tab_color = self.muted;
        s.buttons.close_tab_active_color = self.strong;
        s.buttons.close_tab_bg_fill = self.button_hover;
        s.buttons.add_tab_color = self.text;
        s.buttons.add_tab_active_color = self.strong;
        s.buttons.add_tab_bg_fill = self.button_hover;
        s.buttons.add_tab_border_color = self.toolbar;
        s.separator.width = 4.0;
        s.separator.extra_interact_width = 2.0;
        s.separator.color_idle = self.tab_bar;
        s.separator.color_hovered = self.accent;
        s.separator.color_dragged = self.accent;
        s.overlay.selection_color = self.selection.gamma_multiply(0.6);
        s.overlay.button_color = self.toolbar;
        s.overlay.button_border_stroke = Stroke::new(1.0, self.divider);
        s
    }

    pub fn bold(size: f32) -> FontId {
        FontId::new(size, fonts::bold())
    }

    pub fn sans(size: f32) -> FontId {
        FontId::new(size, FontFamily::Proportional)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(c: Color32) -> f64 {
        let channel = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn both_themes_keep_text_readable_on_every_surface() {
        for t in [Theme::dark(), Theme::light()] {
            for surface in [t.base, t.status, t.tab_bar, t.tab_on, t.strip, t.toolbar, t.head, t.buy_row, t.sell_row, t.button, t.field, t.selection] {
                assert!(contrast(t.text, surface) >= 4.5, "{:?} text on {surface:?}", t.mode);
                if surface != t.selection {
                    assert!(contrast(t.muted, surface) >= 3.0, "{:?} muted on {surface:?}", t.mode);
                }
            }
            for color in [t.up, t.down, t.bid, t.warning] {
                assert!(contrast(color, t.base) >= 4.5, "{:?} {color:?} on base", t.mode);
            }
            for fill in [t.up_fill, t.down_fill, t.tick_down_fill] {
                assert!(contrast(t.on_fill, fill) >= 3.0, "{:?} white on {fill:?}", t.mode);
            }
        }
        assert_eq!(Mode::parse(Mode::Light.label()), Some(Mode::Light));
        assert_eq!(Mode::parse("sepia"), None);
    }
}
