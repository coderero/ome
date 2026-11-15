#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command<A> {
    pub label: String,
    pub keys: String,
    pub action: A,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMenu<A> {
    pub commands: Vec<Command<A>>,
    pub open: bool,
    pub selected: usize,
}

impl<A: Clone> CommandMenu<A> {
    pub fn new(commands: Vec<Command<A>>) -> Self {
        Self {
            commands,
            open: false,
            selected: 0,
        }
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn step(&mut self, delta: i64) {
        let n = self.commands.len() as i64;
        if n > 0 {
            self.selected = (self.selected as i64 + delta).rem_euclid(n) as usize;
        }
    }

    pub fn choose(&mut self) -> Option<A> {
        if !self.open {
            return None;
        }
        self.open = false;
        self.commands.get(self.selected).map(|c| c.action.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_menu_is_driven_by_keys_alone() {
        let command = |label: &str, action| Command {
            label: label.into(),
            keys: String::new(),
            action,
        };
        let mut menu = CommandMenu::new(vec![
            command("buy", 1),
            command("sell", 2),
            command("cancel", 3),
        ]);
        assert_eq!(menu.choose(), None, "closed menus choose nothing");
        menu.toggle();
        menu.step(-1);
        assert_eq!(menu.selected, 2, "wraps around");
        menu.step(2);
        assert_eq!(menu.choose(), Some(2));
        assert!(!menu.open);
    }
}
