use gpui::App;
use std::borrow::Cow;

pub const SANS: &str = "Inter";
pub const MONO: &str = "JetBrains Mono";

const FILES: [&[u8]; 7] = [
    include_bytes!("../fonts/Inter-Regular.ttf"),
    include_bytes!("../fonts/Inter-Medium.ttf"),
    include_bytes!("../fonts/Inter-SemiBold.ttf"),
    include_bytes!("../fonts/Inter-Bold.ttf"),
    include_bytes!("../fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../fonts/JetBrainsMono-Medium.ttf"),
    include_bytes!("../fonts/JetBrainsMono-Bold.ttf"),
];

pub fn register(cx: &mut App) {
    let fonts = FILES.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        eprintln!("could not load the bundled fonts: {e}");
    }
}
