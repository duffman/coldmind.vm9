//! Number ↔ string conversions with ECMAScript semantics.

/// Number::toString(10): shortest round-trip digits, JS exponent rules.
pub fn number_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n == 0.0 {
        return "0".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if n < 0.0 {
        return format!("-{}", number_to_string(-n));
    }
    // Rust's `{:e}` prints the shortest digits that round-trip.
    let s = format!("{n:e}");
    let (mant, exp) = s.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let point = exp + 1; // value = 0.d1d2…dk × 10^point
    if k <= point && point <= 21 {
        format!("{digits}{}", "0".repeat((point - k) as usize))
    } else if 0 < point && point <= 21 {
        format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else {
        let e = point - 1;
        let sign = if e < 0 { '-' } else { '+' };
        if k == 1 {
            format!("{digits}e{sign}{}", e.abs())
        } else {
            format!("{}.{}e{sign}{}", &digits[..1], &digits[1..], e.abs())
        }
    }
}

/// Number::toString(radix) for radix 2..=36.
pub fn number_to_radix_string(n: f64, radix: u32) -> String {
    if radix == 10 || !n.is_finite() {
        return number_to_string(n);
    }
    if n == 0.0 {
        return "0".into();
    }
    let neg = n < 0.0;
    let n = n.abs();
    let mut int = n.trunc();
    let mut frac = n - int;
    let digit = |d: u32| std::char::from_digit(d, radix).unwrap();
    let mut int_digits = Vec::new();
    if int == 0.0 {
        int_digits.push('0');
    }
    while int >= 1.0 {
        let d = (int % radix as f64) as u32;
        int_digits.push(digit(d));
        int = ((int - d as f64) / radix as f64).trunc();
    }
    int_digits.reverse();
    let mut out: String = int_digits.into_iter().collect();
    if frac > 0.0 {
        out.push('.');
        // Emit digits until the fraction is exhausted or precision runs out
        // (52 bits of mantissa ≈ 52 / log2(radix) digits).
        let max = (52.0 / (radix as f64).log2()).ceil() as usize + 1;
        let mut count = 0;
        while frac > 0.0 && count < max {
            frac *= radix as f64;
            let d = frac.trunc() as u32;
            out.push(digit(d));
            frac -= d as f64;
            count += 1;
        }
    }
    if neg { format!("-{out}") } else { out }
}

pub fn is_js_whitespace(c: u16) -> bool {
    matches!(
        c,
        0x09 | 0x0b | 0x0c | 0x20 | 0xa0 | 0xfeff | 0x0a | 0x0d | 0x2028 | 0x2029 | 0x1680 | 0x2000..=0x200a | 0x202f | 0x205f | 0x3000
    )
}

pub fn trim_js(units: &[u16]) -> &[u16] {
    let start = units.iter().position(|&c| !is_js_whitespace(c)).unwrap_or(units.len());
    let end = units.iter().rposition(|&c| !is_js_whitespace(c)).map_or(start, |e| e + 1);
    &units[start..end]
}

/// StringToNumber.
pub fn string_to_number(units: &[u16]) -> f64 {
    let t = trim_js(units);
    if t.is_empty() {
        return 0.0;
    }
    let Ok(s) = String::from_utf16(t) else { return f64::NAN };
    let b = s.as_bytes();
    if b.len() > 2 && b[0] == b'0' {
        let radix = match b[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if radix != 0 {
            let mut v = 0.0f64;
            for &c in &b[2..] {
                match (c as char).to_digit(radix) {
                    Some(d) => v = v * radix as f64 + d as f64,
                    None => return f64::NAN,
                }
            }
            return v;
        }
    }
    match s.as_str() {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if !is_decimal_literal(b) {
        return f64::NAN;
    }
    s.parse::<f64>().unwrap_or(f64::NAN)
}

/// `[+-]? (digits ('.' digits?)? | '.' digits) ([eE] [+-]? digits)?`
fn is_decimal_literal(b: &[u8]) -> bool {
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let fs = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        frac_digits = i - fs;
    }
    if int_digits == 0 && frac_digits == 0 {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let es = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == es {
            return false;
        }
    }
    i == b.len()
}

pub fn to_uint32(n: f64) -> u32 {
    if !n.is_finite() {
        return 0;
    }
    n.trunc().rem_euclid(4294967296.0) as u32
}

pub fn to_int32(n: f64) -> i32 {
    to_uint32(n) as i32
}

pub fn to_uint16(n: f64) -> u16 {
    to_uint32(n) as u16
}

/// ToIntegerOrInfinity
pub fn to_integer(n: f64) -> f64 {
    if n.is_nan() {
        0.0
    } else {
        n.trunc() + 0.0 // normalizes -0
    }
}

/// `**` / Math.pow with ECMAScript edge cases.
pub fn js_pow(x: f64, y: f64) -> f64 {
    if y.is_nan() {
        return f64::NAN;
    }
    if y == 0.0 {
        return 1.0;
    }
    if (x == 1.0 || x == -1.0) && y.is_infinite() {
        return f64::NAN;
    }
    x.powf(y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formatting_matches_js() {
        let cases = [
            (1.0, "1"),
            (-1.5, "-1.5"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (1.5e-7, "1.5e-7"),
            (-0.0, "0"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (100.0, "100"),
            (2.5e25, "2.5e+25"),
        ];
        for (n, s) in cases {
            assert_eq!(number_to_string(n), s, "{n:?}");
        }
    }

    #[test]
    fn string_to_number_matches_js() {
        let u = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        assert_eq!(string_to_number(&u("  42  ")), 42.0);
        assert_eq!(string_to_number(&u("")), 0.0);
        assert_eq!(string_to_number(&u("0x1F")), 31.0);
        assert_eq!(string_to_number(&u("1e3")), 1000.0);
        assert_eq!(string_to_number(&u(".5")), 0.5);
        assert_eq!(string_to_number(&u("5.")), 5.0);
        assert_eq!(string_to_number(&u("-Infinity")), f64::NEG_INFINITY);
        assert!(string_to_number(&u("inf")).is_nan());
        assert!(string_to_number(&u("1_000")).is_nan());
        assert!(string_to_number(&u("-0x10")).is_nan());
        assert!(string_to_number(&u("12px")).is_nan());
    }

    #[test]
    fn radix() {
        assert_eq!(number_to_radix_string(255.0, 16), "ff");
        assert_eq!(number_to_radix_string(-10.0, 2), "-1010");
        assert_eq!(number_to_radix_string(0.5, 2), "0.1");
    }

    #[test]
    fn int_conversions() {
        assert_eq!(to_int32(4294967296.0 + 5.0), 5);
        assert_eq!(to_int32(-1.0), -1);
        assert_eq!(to_uint32(-1.0), 4294967295);
        assert_eq!(to_int32(f64::NAN), 0);
    }
}
