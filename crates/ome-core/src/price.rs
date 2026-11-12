pub type Price = u64;

pub const PRICE_DECIMALS: usize = 2;
pub const TICKS_PER_UNIT: Price = 10u64.pow(PRICE_DECIMALS as u32);

pub fn format_price(price: Price) -> String {
    format!(
        "{}.{:0width$}",
        price / TICKS_PER_UNIT,
        price % TICKS_PER_UNIT,
        width = PRICE_DECIMALS
    )
}

pub fn to_f64(price: Price) -> f64 {
    price as f64 / TICKS_PER_UNIT as f64
}

pub fn parse_price(text: &str) -> Option<Price> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && frac.is_empty())
        || frac.len() > PRICE_DECIMALS
        || !all_digits(whole)
        || !all_digits(frac)
    {
        return None;
    }

    let whole: Price = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let frac: Price = format!("{:0<width$}", frac, width = PRICE_DECIMALS)
        .parse()
        .ok()?;
    let ticks = whole.checked_mul(TICKS_PER_UNIT)?.checked_add(frac)?;
    (ticks > 0).then_some(ticks)
}

pub fn parse_amount(text: &str) -> Option<Price> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    let zero = |s: &str| s.bytes().all(|b| b == b'0');
    if !(whole.is_empty() && frac.is_empty())
        && zero(whole)
        && zero(frac)
        && frac.len() <= PRICE_DECIMALS
    {
        return Some(0);
    }
    parse_price(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prices_into_ticks() {
        assert_eq!(parse_price("100"), Some(10000));
        assert_eq!(parse_price("100.5"), Some(10050));
        assert_eq!(parse_price("100.25"), Some(10025));
        assert_eq!(parse_price("0.29"), Some(29));
        assert_eq!(parse_price(".5"), Some(50));
    }

    #[test]
    fn rejects_bad_prices() {
        for bad in ["", ".", "-1", "1.234", "abc", "1e3", "NaN", "0", "0.00"] {
            assert_eq!(parse_price(bad), None, "{bad} should be rejected");
        }
    }

    #[test]
    fn amounts_allow_zero() {
        assert_eq!(parse_amount("0"), Some(0));
        assert_eq!(parse_amount("0.00"), Some(0));
        assert_eq!(parse_amount("12.5"), Some(1250));
        assert_eq!(parse_amount(""), None);
        assert_eq!(parse_amount("."), None);
        assert_eq!(parse_amount("-1"), None);
    }

    #[test]
    fn formats_ticks_back_to_text() {
        assert_eq!(format_price(10025), "100.25");
        assert_eq!(format_price(5), "0.05");
        assert_eq!(format_price(10000), "100.00");
    }
}
