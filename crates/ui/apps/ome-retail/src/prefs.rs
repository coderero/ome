use ome_core::types::TimeInForce;
use ome_ui::theme::Mode;
use std::path::{Path, PathBuf};

pub const SCALES: [u32; 4] = [90, 100, 110, 125];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefs {
    pub broker: String,
    pub user: String,
    pub remember: bool,
    pub mode: Mode,
    pub scale: u32,
    pub confirm: bool,
    pub qty: u64,
    pub tif: TimeInForce,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            broker: "127.0.0.1:7101".into(),
            user: String::new(),
            remember: true,
            mode: Mode::Dark,
            scale: 100,
            confirm: true,
            qty: 10,
            tif: TimeInForce::Day,
        }
    }
}

pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("ome").join("retail.conf"))
}

impl Prefs {
    pub fn parse(text: &str) -> Self {
        let mut prefs = Prefs::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "broker" if !value.is_empty() => prefs.broker = value.into(),
                "user" => prefs.user = value.into(),
                "remember" => prefs.remember = value != "off",
                "theme" => prefs.mode = Mode::parse(value).unwrap_or(prefs.mode),
                "scale" => {
                    if let Some(scale) = value.parse().ok().filter(|s| SCALES.contains(s)) {
                        prefs.scale = scale;
                    }
                }
                "confirm" => prefs.confirm = value != "off",
                "qty" => {
                    if let Some(qty) = value.parse().ok().filter(|q| *q > 0) {
                        prefs.qty = qty;
                    }
                }
                "tif" => prefs.tif = if value == "gtc" { TimeInForce::Gtc } else { TimeInForce::Day },
                _ => {}
            }
        }
        prefs
    }

    pub fn render(&self) -> String {
        format!(
            "broker={}\nuser={}\nremember={}\ntheme={}\nscale={}\nconfirm={}\nqty={}\ntif={}\n",
            self.broker,
            self.user,
            if self.remember { "on" } else { "off" },
            self.mode.label(),
            self.scale,
            if self.confirm { "on" } else { "off" },
            self.qty,
            if self.tif == TimeInForce::Gtc { "gtc" } else { "day" },
        )
    }

    pub fn load(path: Option<&Path>) -> Self {
        path.and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    pub fn save(&self, path: Option<&Path>) {
        if let Some(path) = path {
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
    fn preferences_round_trip_settings_but_never_a_password() {
        let prefs = Prefs {
            broker: "10.0.0.5:7101".into(),
            user: "alice".into(),
            remember: false,
            mode: Mode::Light,
            scale: 125,
            confirm: false,
            qty: 250,
            tif: TimeInForce::Gtc,
        };
        let text = prefs.render();
        assert!(!text.contains("secret") && !text.contains("password"));
        assert_eq!(Prefs::parse(&text), prefs);
        assert_eq!(Prefs::parse("junk\nbroker=\nscale=300\nqty=0\ntheme=neon\n"), Prefs::default());
    }

    #[test]
    fn preferences_are_written_only_where_asked() {
        let dir = std::env::temp_dir().join(format!("ome-retail-prefs-{}", std::process::id()));
        let file = dir.join("retail.conf");
        let prefs = Prefs { user: "bob".into(), ..Prefs::default() };
        prefs.save(Some(&file));
        assert_eq!(Prefs::load(Some(&file)), prefs);
        prefs.save(None);
        assert_eq!(Prefs::load(None), Prefs::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
