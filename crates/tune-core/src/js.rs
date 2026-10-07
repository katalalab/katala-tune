//! JS（lib/*.js）と同じ結果を出すための小さな道具。
//! 判定の仕様は JS 版なので、数値の文字列化・toFixed・Math.round・真偽・比較・UTF-16 単位の slice を JS の意味で行う。
//! snapshot は probe の出力そのまま（serde_json::Value）を扱い、`None` を JS の undefined として扱う。

use serde_json::{Map, Value};

/// JS の値の見え方。`None` は undefined
pub type Jv<'a> = Option<&'a Value>;

/// JS の `\s`（と String.prototype.trim）が空白とみなす文字
pub const WS: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

pub fn is_ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\x0B' | '\x0C' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `v?.k`。オブジェクトでなければ undefined
pub fn get<'a>(v: Jv<'a>, k: &str) -> Jv<'a> {
    match v {
        Some(Value::Object(m)) => m.get(k),
        _ => None,
    }
}

/// 配列の要素（配列でなければ空）。`for (const x of v || [])` に相当
pub fn arr(v: Jv<'_>) -> &[Value] {
    match v {
        Some(Value::Array(a)) => a,
        _ => &[],
    }
}

pub fn truthy(v: Jv<'_>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(f64::NAN);
            f != 0.0 && !f.is_nan()
        }
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `v != null`
pub fn present(v: Jv<'_>) -> bool {
    !matches!(v, None | Some(Value::Null))
}

/// `a || b`
pub fn or<'a>(a: Jv<'a>, b: Jv<'a>) -> Jv<'a> {
    if truthy(a) { a } else { b }
}

/// `a ?? b`
pub fn nullish<'a>(a: Jv<'a>, b: Jv<'a>) -> Jv<'a> {
    if present(a) { a } else { b }
}

/// ToNumber
pub fn num(v: Jv<'_>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => str_to_num(s),
        Some(Value::Array(_)) => str_to_num(&string(v)),
        Some(Value::Object(_)) => f64::NAN,
    }
}

/// `Number(s)`（StringToNumber）
pub fn str_to_num(s: &str) -> f64 {
    let t = trim(s);
    if t.is_empty() {
        return 0.0;
    }
    let (sign, body) = match t.as_bytes()[0] {
        b'+' => (1.0, &t[1..]),
        b'-' => (-1.0, &t[1..]),
        _ => (1.0, t),
    };
    if body == "Infinity" {
        return sign * f64::INFINITY;
    }
    if sign > 0.0 && !t.starts_with('+') {
        for (p, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
            if let Some(d) = t.strip_prefix(p) {
                if d.is_empty() || !d.chars().all(|c| c.is_digit(radix)) {
                    return f64::NAN;
                }
                return d.chars().fold(0.0, |a, c| a * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0)));
            }
        }
    }
    if !is_decimal_literal(body) {
        return f64::NAN;
    }
    body.parse::<f64>().map(|f| sign * f).unwrap_or(f64::NAN)
}

fn is_decimal_literal(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
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

/// String(v)・テンプレート文字列での埋め込み
pub fn string(v: Jv<'_>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => num_str(n.as_f64().unwrap_or(f64::NAN)),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => join(a, ","),
        Some(Value::Object(_)) => "[object Object]".into(),
    }
}

/// Array.prototype.join（null / undefined は空文字）
pub fn join(a: &[Value], sep: &str) -> String {
    a.iter().map(|x| if x.is_null() { String::new() } else { string(Some(x)) }).collect::<Vec<_>>().join(sep)
}

/// Number.prototype.toString()
pub fn num_str(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if x == 0.0 {
        return "0".into();
    }
    // Rust の {:e} は最短で往復できる桁を出す（JS と同じ桁になる）
    let e = format!("{:e}", x.abs());
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i64;
    let n = exp.parse::<i64>().unwrap_or(0) + 1;
    let sign = if x < 0.0 { "-" } else { "" };
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let es = if n > 0 { "+" } else { "-" };
        if k == 1 { format!("{digits}e{es}{}", (n - 1).abs()) } else { format!("{}.{}e{es}{}", &digits[..1], &digits[1..], (n - 1).abs()) }
    };
    format!("{sign}{body}")
}

/// Number.prototype.toFixed(f)。正確な10進値で、ちょうど半分は大きい方へ
pub fn to_fixed(x: f64, f: usize) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.abs() >= 1e21 {
        return num_str(x);
    }
    let neg = x < 0.0;
    let exact = format!("{:.1100}", x.abs());
    let (int_part, frac_part) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut digits: Vec<u8> = int_part.bytes().chain(frac_part.bytes().take(f)).map(|b| b - b'0').collect();
    let next = frac_part.as_bytes().get(f).map(|b| b - b'0').unwrap_or(0);
    if next >= 5 {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, 1);
                break;
            }
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let s: String = digits.iter().map(|d| char::from(b'0' + d)).collect();
    let int_len = s.len() - f;
    let int_s = s[..int_len].trim_start_matches('0');
    let int_s = if int_s.is_empty() { "0" } else { int_s };
    let body = if f == 0 { int_s.to_string() } else { format!("{int_s}.{}", &s[int_len..]) };
    if neg { format!("-{body}") } else { body }
}

/// `+x.toFixed(f)`
pub fn fixed_num(x: f64, f: usize) -> f64 {
    str_to_num(&to_fixed(x, f))
}

/// Math.round
pub fn round(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let r = x.floor();
    if x - r >= 0.5 { r + 1.0 } else { r }
}

/// Math.max（NaN が1つでもあれば NaN、空なら -Infinity）
pub fn max(xs: &[f64]) -> f64 {
    xs.iter().fold(f64::NEG_INFINITY, |a, &b| if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) })
}

/// Math.min
pub fn min(xs: &[f64]) -> f64 {
    xs.iter().fold(f64::INFINITY, |a, &b| if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) })
}

/// String.prototype.trim
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_ws)
}

/// UTF-16 単位の長さ（JS の length）
pub fn len16(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `s.slice(0, n)`（UTF-16 単位）。サロゲート対の途中で切れるときは U+FFFD にする
/// （JS の文字列を UTF-8 にしたとき＝ハッシュ・SQLite・画面と同じになる）
pub fn slice16(s: &str, n: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.len_utf16();
        if used + w > n {
            if used < n {
                out.push('\u{FFFD}');
            }
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// `s.slice(-n)`（UTF-16 単位、末尾 n）
pub fn slice16_tail(s: &str, n: usize) -> String {
    let total = len16(s);
    if total <= n {
        return s.to_string();
    }
    let skip = total - n;
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.len_utf16();
        if used >= skip {
            out.push(c);
        } else if used + w > skip {
            out.push('\u{FFFD}');
        }
        used += w;
    }
    out
}

/// `/^C:/i.test(String(v))`
pub fn is_c_drive(v: Jv<'_>) -> bool {
    let s = string(v);
    let mut c = s.chars();
    matches!(c.next(), Some('C' | 'c')) && c.next() == Some(':')
}

/// `x === 'str'`
pub fn is_str(v: Jv<'_>, s: &str) -> bool {
    matches!(v, Some(Value::String(x)) if x == s)
}

/// 厳密等価（===）
pub fn strict_eq(a: Jv<'_>, b: Jv<'_>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(Value::Number(x)), Some(Value::Number(y))) => x.as_f64() == y.as_f64(),
        (Some(Value::Null), Some(Value::Null)) => true,
        (Some(Value::Bool(x)), Some(Value::Bool(y))) => x == y,
        (Some(Value::String(x)), Some(Value::String(y))) => x == y,
        // オブジェクト・配列は参照の比較。別々に読んだものは等しくない
        _ => false,
    }
}

/// JS の数を JSON に（整数なら整数として。NaN・Infinity は null になる＝JSON.stringify と同じ）
pub fn jnum(x: f64) -> Value {
    if !x.is_finite() {
        return Value::Null;
    }
    if x.fract() == 0.0 && x.abs() < 9.007_199_254_740_992e15 {
        return Value::from(x as i64);
    }
    serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null)
}

/// オブジェクトを組み立てる（値が None のキーは JS の undefined と同じく入れない）
#[derive(Default)]
pub struct Obj(Map<String, Value>);

impl Obj {
    pub fn new() -> Self {
        Self(Map::new())
    }
    pub fn set(mut self, k: &str, v: impl Into<Value>) -> Self {
        self.0.insert(k.into(), v.into());
        self
    }
    pub fn opt(mut self, k: &str, v: Option<Value>) -> Self {
        if let Some(v) = v {
            self.0.insert(k.into(), v);
        }
        self
    }
    pub fn build(self) -> Value {
        Value::Object(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_to_string_like_js() {
        assert_eq!(num_str(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(num_str(5.0), "5");
        assert_eq!(num_str(-0.0), "0");
        assert_eq!(num_str(1e21), "1e+21");
        assert_eq!(num_str(1.5e-7), "1.5e-7");
        assert_eq!(num_str(0.000001), "0.000001");
        assert_eq!(num_str(123456789012345680000.0), "123456789012345680000");
        assert_eq!(num_str(-12.5), "-12.5");
    }

    #[test]
    fn to_fixed_like_js() {
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(-2.5, 0), "-3");
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(0.125, 2), "0.13");
        assert_eq!(to_fixed(-0.0001, 2), "-0.00");
        assert_eq!(to_fixed(99.95, 1), "100.0");
        assert_eq!(to_fixed(f64::NAN, 1), "NaN");
        assert_eq!(to_fixed(f64::INFINITY, 0), "Infinity");
        assert_eq!(to_fixed(0.0, 1), "0.0");
        assert_eq!(to_fixed(1234.5678, 3), "1234.568");
    }

    #[test]
    fn round_and_max_like_js() {
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -2.0);
        assert_eq!(round(0.49999999999999994), 0.0);
        assert!(max(&[1.0, f64::NAN]).is_nan());
        assert_eq!(max(&[]), f64::NEG_INFINITY);
    }

    #[test]
    fn string_to_number_like_js() {
        assert_eq!(str_to_num(" 12 "), 12.0);
        assert_eq!(str_to_num(""), 0.0);
        assert_eq!(str_to_num("0x1F"), 31.0);
        assert!(str_to_num("inf").is_nan());
        assert!(str_to_num("1_000").is_nan());
        assert_eq!(str_to_num("-Infinity"), f64::NEG_INFINITY);
        assert_eq!(str_to_num(".5e1"), 5.0);
    }

    #[test]
    fn utf16_slices() {
        assert_eq!(len16("a😀"), 3);
        assert_eq!(slice16("a😀b", 2), "a\u{FFFD}");
        assert_eq!(slice16("a😀b", 3), "a😀");
        assert_eq!(slice16_tail("a😀b", 2), "\u{FFFD}b");
    }
}
