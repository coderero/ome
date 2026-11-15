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

const BULLET: char = '•';

pub struct MaskedStorage {
    secret: Rc<RefCell<String>>,
    shown: String,
    version: u16,
}

impl MaskedStorage {
    pub fn new(secret: Rc<RefCell<String>>) -> Self {
        let shown = BULLET.to_string().repeat(secret.borrow().chars().count());
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
        self.shown.chars().count()
    }

    fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let width = BULLET.len_utf8();
        let (from, to) = (range.start / width, range.end / width);
        let mut secret = self.secret.borrow_mut();
        let chars: Vec<char> = secret.chars().collect();
        let mut next: String = chars[..from.min(chars.len())].iter().collect();
        next.push_str(text);
        next.extend(chars[to.min(chars.len())..].iter());
        *secret = next;
        self.shown = BULLET.to_string().repeat(secret.chars().count());
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

    #[test]
    fn masked_storage_edits_the_secret_and_shows_only_bullets() {
        let secret = Rc::new(RefCell::new(String::new()));
        let mut storage = MaskedStorage::new(secret.clone());
        storage.replace_range(0..0, "pässwd");
        assert_eq!(storage.content_utf8(), "••••••");
        assert_eq!(*secret.borrow(), "pässwd");
        let w = BULLET.len_utf8();
        storage.replace_range(w..2 * w, "a");
        assert_eq!(*secret.borrow(), "passwd");
        storage.replace_range(5 * w..6 * w, "");
        assert_eq!(*secret.borrow(), "passw");
        assert_eq!(storage.len_utf16(), 5);
        assert_eq!(storage.version(), 3);
    }
}
