use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefs {
    pub broker: String,
    pub user: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            broker: "127.0.0.1:7101".into(),
            user: String::new(),
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ome").join("retail.conf"))
}

impl Prefs {
    pub fn parse(text: &str) -> Self {
        let mut prefs = Prefs::default();
        for line in text.lines() {
            match line.split_once('=') {
                Some(("broker", v)) if !v.trim().is_empty() => prefs.broker = v.trim().into(),
                Some(("user", v)) => prefs.user = v.trim().into(),
                _ => {}
            }
        }
        prefs
    }

    pub fn render(&self) -> String {
        format!("broker={}\nuser={}\n", self.broker, self.user)
    }

    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(path) = path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, self.render());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_remember_broker_and_user_but_never_a_password() {
        let prefs = Prefs {
            broker: "10.0.0.5:7101".into(),
            user: "alice".into(),
        };
        let text = prefs.render();
        assert!(!text.contains("secret") && !text.contains("password"));
        assert_eq!(Prefs::parse(&text), prefs);
        assert_eq!(Prefs::parse("junk\nbroker=\n"), Prefs::default());
    }
}
