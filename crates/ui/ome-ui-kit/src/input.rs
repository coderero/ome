use crate::theme::{SANS, Theme};
use gpui::{App, AppContext, Entity, Focusable, Styled, Window, px};
use gpui_elements::editable_text::UnicodeTextStorage;
use gpui_elements::editable_text::{
    EditableTextElement, EditableTextState, StringStorage, text_input,
};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

pub use gpui_elements::editable_text::TextChanged;
pub use gpui_elements::editable_text::actions::{DEFAULT_INPUT_CONTEXT, default_bindings};

fn mask(c: char) -> char {
    match c.len_utf8() {
        1 => '*',
        2 => '·',
        3 => '•',
        _ => '🔒',
    }
}

pub struct MaskedStorage {
    secret: Rc<RefCell<String>>,
    shown: String,
    version: u16,
}

impl MaskedStorage {
    pub fn new(secret: Rc<RefCell<String>>) -> Self {
        let shown = secret.borrow().chars().map(mask).collect();
        Self {
            secret,
            shown,
            version: 0,
        }
    }
}

impl UnicodeTextStorage for MaskedStorage {
    fn version(&self) -> u16 {
        self.version
    }

    fn content_utf8(&self) -> &str {
        &self.shown
    }

    fn len_utf16(&self) -> usize {
        self.shown.encode_utf16().count()
    }

    fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let mut secret = self.secret.borrow_mut();
        let end = range.end.min(secret.len());
        let start = range.start.min(end);
        secret.replace_range(start..end, text);
        self.shown = secret.chars().map(mask).collect();
        self.version = self.version.wrapping_add(1);
    }
}

pub fn plain_state(text: &str, cx: &mut App) -> Entity<EditableTextState> {
    let text = text.to_string();
    cx.new(|cx| {
        let mut state = EditableTextState::new(StringStorage::default(), cx);
        state.emplace(&text, cx);
        state
    })
}

pub fn secret_state(secret: Rc<RefCell<String>>, cx: &mut App) -> Entity<EditableTextState> {
    cx.new(|cx| EditableTextState::new(MaskedStorage::new(secret), cx))
}

pub fn field(
    theme: &Theme,
    id: &'static str,
    state: &Entity<EditableTextState>,
    placeholder: &'static str,
) -> EditableTextElement {
    text_input(id)
        .state(state.downgrade())
        .placeholder(placeholder)
        .placeholder_color(theme.faint)
        .caret_color(gpui::rgb_to_hsla(theme.strong))
        .caret_blink_interval_500ms()
        .w_full()
        .min_h_auto()
        .max_h_auto()
        .whitespace_nowrap()
        .px_2()
        .py_1()
        .font_family(SANS)
        .text_size(px(13.))
        .text_color(theme.strong)
        .rounded(px(3.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.field)
}

pub fn focus(state: &Entity<EditableTextState>, window: &mut Window, cx: &mut App) {
    let handle = state.read(cx).focus_handle(cx);
    window.focus(&handle, cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{EntityInputHandler, TestAppContext};

    #[test]
    fn masks_keep_the_byte_and_utf16_length_of_every_character() {
        for c in ['a', 'ä', '€', '🔑'] {
            assert_eq!(mask(c).len_utf8(), c.len_utf8(), "{c}");
            assert_eq!(mask(c).len_utf16(), c.len_utf16(), "{c}");
        }
    }

    struct Host {
        state: Entity<EditableTextState>,
    }

    impl gpui::Render for Host {
        fn render(
            &mut self,
            _: &mut Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
        }
    }

    #[gpui::test]
    fn typing_into_a_password_field_edits_the_secret_and_shows_only_masks(cx: &mut TestAppContext) {
        let secret = Rc::new(RefCell::new(String::new()));
        let shared = secret.clone();
        let window = cx.add_window(move |_, cx| Host {
            state: secret_state(shared, cx),
        });
        window
            .update(cx, |host, window, cx| {
                host.state.update(cx, |state, cx| {
                    for typed in ["p", "ä", "s", "€", "🔑", "w"] {
                        state.replace_text_in_range(None, typed, window, cx);
                    }
                    assert_eq!(state.as_str(), "*·*•🔒*");
                    let end = state.as_str().encode_utf16().count();
                    state.replace_text_in_range(Some(end - 1..end), "", window, cx);
                    state.replace_text_in_range(Some(1..2), "a", window, cx);
                });
            })
            .unwrap();
        assert_eq!(*secret.borrow(), "pas€🔑");
    }
}
