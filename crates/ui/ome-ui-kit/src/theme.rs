use gpui::{Rgba, rgb};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub background: Rgba,
    pub panel: Rgba,
    pub raised: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    pub buy: Rgba,
    pub buy_soft: Rgba,
    pub sell: Rgba,
    pub sell_soft: Rgba,
    pub accent: Rgba,
    pub warning: Rgba,
    pub links: [Rgba; 4],
}

pub const MONO: &str = "Lilex";

impl Theme {
    pub fn dark() -> Self {
        Self {
            background: rgb(0x0a0d10),
            panel: rgb(0x10141a),
            raised: rgb(0x171c24),
            border: rgb(0x222a35),
            text: rgb(0xd9dee5),
            muted: rgb(0x8590a0),
            faint: rgb(0x3a4452),
            buy: rgb(0x22c58b),
            buy_soft: rgb(0x0f3328),
            sell: rgb(0xf04b55),
            sell_soft: rgb(0x3a161b),
            accent: rgb(0xf5b942),
            warning: rgb(0xff9f43),
            links: [rgb(0x4ea8ff), rgb(0xf5b942), rgb(0xb27cff), rgb(0x2ec4b6)],
        }
    }

    pub fn tone(&self, value: f64) -> Rgba {
        if value > 0.0 {
            self.buy
        } else if value < 0.0 {
            self.sell
        } else {
            self.muted
        }
    }
}
