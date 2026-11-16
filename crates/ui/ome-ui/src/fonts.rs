use egui::{FontData, FontDefinitions, FontFamily};
use std::sync::Arc;

pub const SANS: &str = "Open Sans";
pub const BOLD: &str = "Open Sans Bold";

const FILES: [(&str, &[u8]); 2] = [
    (SANS, include_bytes!("../fonts/OpenSans-Regular.ttf")),
    (BOLD, include_bytes!("../fonts/OpenSans-Bold.ttf")),
];

pub fn definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in FILES {
        fonts
            .font_data
            .insert(name.to_string(), Arc::new(FontData::from_static(bytes)));
    }
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, SANS.to_string());
    let mut chain = vec![BOLD.to_string()];
    chain.extend(fonts.families[&FontFamily::Proportional].iter().skip(1).cloned());
    fonts.families.insert(bold(), chain);
    fonts
}

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_sans_comes_first_and_keeps_egui_fallbacks() {
        let fonts = definitions();
        assert_eq!(fonts.families[&FontFamily::Proportional][0], SANS);
        assert!(fonts.families[&FontFamily::Proportional].len() > 1, "emoji and symbol fallbacks stay");
        assert_eq!(fonts.families[&bold()][0], BOLD);
        assert!(fonts.families[&bold()].len() > 1);
    }
}
