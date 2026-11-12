use std::fmt;

pub const SYMBOL_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Symbol([u8; SYMBOL_LEN]);

impl Symbol {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty()
            || text.len() > SYMBOL_LEN
            || !text.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return None;
        }
        let mut bytes = [0u8; SYMBOL_LEN];
        for (slot, b) in bytes.iter_mut().zip(text.bytes()) {
            *slot = b.to_ascii_uppercase();
        }
        Some(Self(bytes))
    }

    pub fn from_bytes(bytes: [u8; SYMBOL_LEN]) -> Option<Self> {
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(SYMBOL_LEN);
        let text = std::str::from_utf8(&bytes[..len]).ok()?;
        let symbol = Self::parse(text)?;
        (symbol.0 == bytes).then_some(symbol)
    }

    pub fn as_bytes(&self) -> [u8; SYMBOL_LEN] {
        self.0
    }

    pub fn as_str(&self) -> &str {
        let len = self.0.iter().position(|&b| b == 0).unwrap_or(SYMBOL_LEN);
        std::str::from_utf8(&self.0[..len]).unwrap_or("?")
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes() {
        let s = Symbol::parse("acme").unwrap();
        assert_eq!(s.as_str(), "ACME");
        assert_eq!(s.to_string(), "ACME");
        assert_eq!(Symbol::from_bytes(s.as_bytes()), Some(s));
    }

    #[test]
    fn rejects_invalid_symbols() {
        for bad in ["", "TOOLONGSYM", "AC-ME", "A B", "ÄCME"] {
            assert_eq!(Symbol::parse(bad), None, "{bad}");
        }
        assert_eq!(Symbol::from_bytes([0; SYMBOL_LEN]), None);
        assert_eq!(Symbol::from_bytes(*b"AB\0CDEFG"), None);
    }
}
