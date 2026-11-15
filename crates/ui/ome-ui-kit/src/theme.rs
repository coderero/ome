use gpui::{Rgba, rgb, rgba};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub background: Rgba,
    pub panel: Rgba,
    pub header: Rgba,
    pub raised: Rgba,
    pub field: Rgba,
    pub border: Rgba,
    pub grid: Rgba,
    pub text: Rgba,
    pub strong: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    pub up: Rgba,
    pub down: Rgba,
    pub up_flash: Rgba,
    pub down_flash: Rgba,
    pub bid: Rgba,
    pub bid_cell: Rgba,
    pub ask: Rgba,
    pub ask_cell: Rgba,
    pub selection: Rgba,
    pub buy_button: Rgba,
    pub sell_button: Rgba,
    pub accent: Rgba,
    pub last: Rgba,
    pub warning: Rgba,
    pub overlay: Rgba,
    pub links: [Rgba; 4],
    pub studies: [Rgba; 3],
}

pub use crate::fonts::{MONO, SANS};

impl Theme {
    pub fn dark() -> Self {
        Self {
            background: rgb(0x050506),
            panel: rgb(0x0e0f11),
            header: rgb(0x17191c),
            raised: rgb(0x1f2226),
            field: rgb(0x0a0b0d),
            border: rgb(0x2a2d32),
            grid: rgb(0x1a1c20),
            text: rgb(0xc9ccd1),
            strong: rgb(0xf2f3f5),
            muted: rgb(0x7d828a),
            faint: rgb(0x3e4249),
            up: rgb(0x2fbf71),
            down: rgb(0xef4444),
            up_flash: rgba(0x2fbf7140),
            down_flash: rgba(0xef444440),
            bid: rgb(0x4d9fff),
            bid_cell: rgb(0x0f2340),
            ask: rgb(0xff5c5c),
            ask_cell: rgb(0x3a1414),
            selection: rgb(0x1f4fd1),
            buy_button: rgb(0x1d8a50),
            sell_button: rgb(0xb3262b),
            accent: rgb(0x3d8bfd),
            last: rgb(0xf5c542),
            warning: rgb(0xff9f1c),
            overlay: rgba(0x000000b0),
            links: [rgb(0x3d8bfd), rgb(0xf5c542), rgb(0xa77bff), rgb(0x2ec4b6)],
            studies: [rgb(0xf5c542), rgb(0xa77bff), rgb(0x2ec4b6)],
        }
    }

    pub fn tone(&self, value: f64) -> Rgba {
        if value > 0.0 {
            self.up
        } else if value < 0.0 {
            self.down
        } else {
            self.text
        }
    }
}
