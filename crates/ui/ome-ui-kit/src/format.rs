use ome_core::price::{Price, format_price};

pub fn price(value: Price) -> String {
    format_price(value)
}

pub fn price_or_dash(value: Option<Price>) -> String {
    value.map_or_else(|| "-".to_string(), format_price)
}

pub fn qty(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn money(cents: u64) -> String {
    let whole = qty(cents / 100);
    format!("{whole}.{:02}", cents % 100)
}

pub fn signed_money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "+" };
    format!("{sign}{}", money(cents.unsigned_abs()))
}

pub fn change_pct(from: Price, to: Price) -> Option<f64> {
    (from > 0).then(|| (to as f64 / from as f64 - 1.0) * 100.0)
}

pub fn pct(value: f64) -> String {
    format!("{}{:.2}%", if value >= 0.0 { "+" } else { "" }, value)
}

pub fn clock(ms: u64) -> String {
    let secs = ms / 1_000;
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3_600 % 24,
        secs / 60 % 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_like_a_trading_screen() {
        assert_eq!(price(25_10), "25.10");
        assert_eq!(price_or_dash(None), "-");
        assert_eq!(qty(1_234_567), "1,234,567");
        assert_eq!(qty(999), "999");
        assert_eq!(money(1_234_567_89), "1,234,567.89");
        assert_eq!(signed_money(-1_50), "-1.50");
        assert_eq!(signed_money(0), "+0.00");
        assert_eq!(pct(change_pct(25_00, 25_50).unwrap()), "+2.00%");
        assert_eq!(pct(-0.5), "-0.50%");
        assert_eq!(change_pct(0, 1), None);
        assert_eq!(clock(3_725_000), "01:02:05");
    }
}
