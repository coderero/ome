use crate::app::RetailApp;
use gpui::{Context, FontWeight, IntoElement, div, prelude::*, px};
use ome_ui_kit::input::field;
use ome_ui_kit::widgets::{Tone, button};

impl RetailApp {
    pub fn render_login(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let label = |text: &'static str| {
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.muted)
                .child(text)
        };
        let status = if self.login.connecting {
            Some(
                div()
                    .text_color(t.accent)
                    .child("Connecting to the broker…"),
            )
        } else {
            self.login
                .error
                .clone()
                .map(|e| div().text_color(t.down).child(e))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .bg(t.background)
            .child(
                div()
                    .w(px(380.))
                    .flex()
                    .flex_col()
                    .bg(t.panel)
                    .border_1()
                    .border_color(t.border)
                    .rounded(px(4.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_5()
                            .h(px(64.))
                            .border_b_1()
                            .border_color(t.border)
                            .child(
                                div()
                                    .w(px(30.))
                                    .h(px(30.))
                                    .rounded(px(3.))
                                    .bg(t.selection)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(t.strong)
                                    .child("O"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_size(px(16.))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(t.strong)
                                            .child("OME Trader"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(t.muted)
                                            .child("Retail brokerage account"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_5()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(label("BROKER"))
                                    .child(field(&t, "login-broker", &self.login.broker, "host:port")),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(label("LOGIN ID"))
                                    .child(field(&t, "login-user", &self.login.user, "username")),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(label("PASSWORD"))
                                    .child(field(
                                        &t,
                                        "login-secret",
                                        &self.login.secret_field,
                                        "password",
                                    )),
                            )
                            .child(
                                button(
                                    &t,
                                    "login",
                                    if self.login.connecting { "Logging in…" } else { "Log In" },
                                    Tone::Primary,
                                    cx.listener(|app, _, window, cx| app.log_in(window, cx)),
                                )
                                .h(px(32.))
                                .w_full(),
                            )
                            .child(
                                div()
                                    .min_h(px(18.))
                                    .text_size(px(12.))
                                    .children(status),
                            ),
                    )
                    .child(
                        div()
                            .px_5()
                            .py_3()
                            .border_t_1()
                            .border_color(t.border)
                            .text_size(px(11.))
                            .text_color(t.faint)
                            .child("Tab moves between fields · Enter logs in · your password is never stored"),
                    ),
            )
    }
}
