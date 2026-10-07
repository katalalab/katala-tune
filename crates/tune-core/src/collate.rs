//! `String.prototype.localeCompare` の近似（ICU の root 照合 = en-US と同じ順）。
//! 道具の一覧・デッキの下書きの並び（lib/inventory.js の matrix、lib/dogu.js の deckDraft）を JS 版と揃えるために使う。
//!
//! Unicode 照合アルゴリズム（UCA）を小さくしたもの: 文字ごとに (第1, 第2, 第3) の重みを作り、
//! 文字列全体の第1重み（文字の種類）→ 第2重み（アクセント・濁点）→ 第3重み（大文字小文字・かなの種類・全角）の順に比べる。
//! - ASCII の記号・数字・英字は ICU の実際の順（tests/parity_inventory.rs で node と比べている）
//! - Latin-1 と Latin Extended-A のアクセント付き文字は元の英字＋アクセント（æ・œ・ß は2文字に展開）
//! - ひらがな・カタカナは同じ第1重み（濁点は第2、小書き・カタカナ・半角は第3）
//! - 漢字は符号位置の順（基本の範囲は部首・画数の順とほぼ同じ）。他の文字は種類ごとに符号位置の順
//!
//! 照合の言語は固定（root）。Electron 版は OS の言語（日本語なら ja の照合）で並べるので、漢字や長音記号の並びは違うことがある。

use std::cmp::Ordering;

/// ICU（root）での記号の順（ASCII の記号と、そのあいだに入るよく使う記号）。空白の後、数字の前。
/// ASCII の英字は a A b B …（大文字は第3重みで小文字の後）、数字は 0〜9
const PUNCT_ORDER: &[char] = &[
    '_', '-', '–', '—', '・', ',', '、', ';', ':', '!', '¡', '?', '¿', '.', '…', '。', '·', '\'', '‘', '’', '"', '“', '”', '«', '»', '(', ')', '[', ']', '{',
    '}', '「', '」', '『', '』', '【', '】', '§', '¶', '@', '*', '/', '\\', '&', '#', '%', '•', '`', '^', '°', '©', '®', '+', '±', '÷', '×', '<', '=', '>',
    '|', '~',
];
/// 通貨記号（数字の直前）
const CURRENCY_ORDER: &[char] = &['$', '£', '¥', '€'];

const G_SPACE: u32 = 0x0100_0000;
const G_PUNCT: u32 = 0x0200_0000;
const G_SYMBOL: u32 = 0x0300_0000;
const G_CURRENCY: u32 = 0x0380_0000;
const G_DIGIT: u32 = 0x0400_0000;
const G_LATIN: u32 = 0x0500_0000;
const G_LETTER: u32 = 0x0600_0000;
const G_KANA: u32 = 0x0700_0000;
const G_HAN: u32 = 0x0800_0000;

/// アクセントの第2重み（UCA の順: 鋭・重・短・曲折・ハーチェク・輪・分音・二重鋭・チルダ・上点・セディーユ・オゴネク・長音・線）
const ACUTE: u16 = 0x21;
const GRAVE: u16 = 0x22;
const BREVE: u16 = 0x23;
const CIRCUMFLEX: u16 = 0x24;
const CARON: u16 = 0x25;
const RING: u16 = 0x26;
const DIAERESIS: u16 = 0x27;
const DOUBLE_ACUTE: u16 = 0x28;
const TILDE: u16 = 0x29;
const DOT: u16 = 0x2A;
const CEDILLA: u16 = 0x30;
const OGONEK: u16 = 0x31;
const MACRON: u16 = 0x32;
const STROKE: u16 = 0x40;
const BASE: u16 = 0x05;

#[derive(Clone, Copy, Debug)]
struct Ce {
    p: u32,
    s: u16,
    t: u16,
}

fn ce(p: u32, s: u16, t: u16) -> Ce {
    Ce { p, s, t }
}

fn latin(letter: char) -> u32 {
    G_LATIN + (letter as u32 - 'a' as u32) * 16
}

/// 結合用のアクセント（U+0300〜）。第1重みを持たず、前の文字の第2重みに足す
fn combining(c: char) -> Option<u16> {
    Some(match c {
        '\u{0301}' => ACUTE,
        '\u{0300}' => GRAVE,
        '\u{0306}' => BREVE,
        '\u{0302}' => CIRCUMFLEX,
        '\u{030C}' => CARON,
        '\u{030A}' => RING,
        '\u{0308}' => DIAERESIS,
        '\u{030B}' => DOUBLE_ACUTE,
        '\u{0303}' => TILDE,
        '\u{0307}' => DOT,
        '\u{0327}' => CEDILLA,
        '\u{0328}' => OGONEK,
        '\u{0304}' => MACRON,
        '\u{3099}' | '\u{FF9E}' => 0x60,
        '\u{309A}' | '\u{FF9F}' => 0x61,
        '\u{0300}'..='\u{036F}' => 0x50,
        _ => return None,
    })
}

/// Latin-1 Supplement（U+00C0〜U+00FF）: (元の英字, アクセント)。'*' は個別の扱い
const LATIN1: &str = "AAAAAA*CEEEEIIII*NOOOOO*OUUUUY**aaaaaa*ceeeeiiii*nooooo*ouuuuy*y";
const LATIN1_ACC: [u16; 64] = [
    GRAVE, ACUTE, CIRCUMFLEX, TILDE, DIAERESIS, RING, 0, CEDILLA, GRAVE, ACUTE, CIRCUMFLEX, DIAERESIS, GRAVE, ACUTE, CIRCUMFLEX, DIAERESIS, 0, TILDE, GRAVE,
    ACUTE, CIRCUMFLEX, TILDE, DIAERESIS, 0, STROKE, GRAVE, ACUTE, CIRCUMFLEX, DIAERESIS, ACUTE, 0, 0, GRAVE, ACUTE, CIRCUMFLEX, TILDE, DIAERESIS, RING, 0,
    CEDILLA, GRAVE, ACUTE, CIRCUMFLEX, DIAERESIS, GRAVE, ACUTE, CIRCUMFLEX, DIAERESIS, 0, TILDE, GRAVE, ACUTE, CIRCUMFLEX, TILDE, DIAERESIS, 0, STROKE, GRAVE,
    ACUTE, CIRCUMFLEX, DIAERESIS, ACUTE, 0, DIAERESIS,
];
/// Latin Extended-A（U+0100〜U+017F）の元の英字。'*' は個別の扱い
const LATIN_EXT_A: &str = "AaAaAaCcCcCcCcDdDdEeEeEeEeEeGgGgGgGgHhHhIiIiIiIiI***JjKk*LlLlLlLlLlNnNnNn***OoOoOo**RrRrRrSsSsSsSsTtTtTtUuUuUuUuUuUuWwYyYZzZzZzs";
const LATIN_EXT_A_ACC: &[u16] = &[
    MACRON,
    MACRON,
    BREVE,
    BREVE,
    OGONEK,
    OGONEK,
    ACUTE,
    ACUTE,
    CIRCUMFLEX,
    CIRCUMFLEX,
    DOT,
    DOT,
    CARON,
    CARON,
    CARON,
    CARON,
    STROKE,
    STROKE,
    MACRON,
    MACRON,
    BREVE,
    BREVE,
    DOT,
    DOT,
    OGONEK,
    OGONEK,
    CARON,
    CARON,
    CIRCUMFLEX,
    CIRCUMFLEX,
    BREVE,
    BREVE,
    DOT,
    DOT,
    CEDILLA,
    CEDILLA,
    CIRCUMFLEX,
    CIRCUMFLEX,
    STROKE,
    STROKE,
    TILDE,
    TILDE,
    MACRON,
    MACRON,
    BREVE,
    BREVE,
    OGONEK,
    OGONEK,
    DOT,
    0,
    0,
    0,
    CIRCUMFLEX,
    CIRCUMFLEX,
    CEDILLA,
    CEDILLA,
    0,
    ACUTE,
    ACUTE,
    CEDILLA,
    CEDILLA,
    CARON,
    CARON,
    DOT,
    DOT,
    STROKE,
    STROKE,
    ACUTE,
    ACUTE,
    CEDILLA,
    CEDILLA,
    CARON,
    CARON,
    0,
    0,
    0,
    MACRON,
    MACRON,
    BREVE,
    BREVE,
    DOUBLE_ACUTE,
    DOUBLE_ACUTE,
    0,
    0,
    ACUTE,
    ACUTE,
    CEDILLA,
    CEDILLA,
    CARON,
    CARON,
    ACUTE,
    ACUTE,
    CIRCUMFLEX,
    CIRCUMFLEX,
    CEDILLA,
    CEDILLA,
    CARON,
    CARON,
    CEDILLA,
    CEDILLA,
    CARON,
    CARON,
    STROKE,
    STROKE,
    TILDE,
    TILDE,
    MACRON,
    MACRON,
    BREVE,
    BREVE,
    RING,
    RING,
    DOUBLE_ACUTE,
    DOUBLE_ACUTE,
    OGONEK,
    OGONEK,
    CIRCUMFLEX,
    CIRCUMFLEX,
    CIRCUMFLEX,
    CIRCUMFLEX,
    DIAERESIS,
    ACUTE,
    ACUTE,
    DOT,
    DOT,
    CARON,
    CARON,
    0,
];

/// ひらがな（U+3041〜U+3096）の (清音の符号位置, 濁点 1・半濁点 2, 小書き)
fn kana(h: u32) -> Option<(u32, u16, bool)> {
    const SMALL: &[u32] = &[0x3041, 0x3043, 0x3045, 0x3047, 0x3049, 0x3063, 0x3083, 0x3085, 0x3087, 0x308E];
    if !(0x3041..=0x3096).contains(&h) {
        return None;
    }
    if SMALL.contains(&h) {
        return Some((h + 1, 0, true));
    }
    match h {
        0x3095 => return Some((0x304B, 0, true)),  // ゕ
        0x3096 => return Some((0x3051, 0, true)),  // ゖ
        0x3094 => return Some((0x3046, 1, false)), // ゔ
        _ => {}
    }
    // か〜ぢ（か行・さ行・た行の前半）は清音の次が濁音
    if (0x304B..=0x3062).contains(&h) {
        return Some(if (h - 0x304B) % 2 == 1 { (h - 1, 1, false) } else { (h, 0, false) });
    }
    // つ・づ、て・で、と・ど
    match h {
        0x3065 | 0x3067 | 0x3069 => return Some((h - 1, 1, false)),
        _ => {}
    }
    // は行: は ば ぱ ひ び ぴ …
    if (0x306F..=0x307D).contains(&h) {
        let k = (h - 0x306F) % 3;
        return Some((h - k, k as u16, false));
    }
    Some((h, 0, false))
}

const HALF_KANA: &str = "ヲァィゥェォャュョッーアイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワン";

fn elements(s: &str) -> Vec<Ce> {
    let mut out: Vec<Ce> = Vec::new();
    for c in s.chars() {
        if let Some(acc) = combining(c) {
            if let Some(last) = out.last_mut() {
                last.s = last.s.saturating_add(acc);
            }
            continue;
        }
        push(&mut out, c);
    }
    out
}

fn push(out: &mut Vec<Ce>, c: char) {
    let cp = c as u32;
    // 制御文字・書式文字（U+FEFF など）は無視する
    let control = (cp < 0x20 && !(0x09..=0x0D).contains(&cp)) || ((0x7F..0xA0).contains(&cp) && cp != 0x85);
    if control || matches!(cp, 0x200B..=0x200F | 0x2060..=0x2064 | 0xFEFF | 0xAD) {
        return;
    }
    // 空白
    match cp {
        0x09..=0x0D => return out.push(ce(G_SPACE + cp - 0x09, BASE, 0)),
        0x85 => return out.push(ce(G_SPACE + 5, BASE, 0)),
        0x2028 | 0x2029 => return out.push(ce(G_SPACE + 6 + cp - 0x2028, BASE, 0)),
        0x20 => return out.push(ce(G_SPACE + 0x10, BASE, 0)),
        0x3000 => return out.push(ce(G_SPACE + 0x10, BASE, 1)),
        0xA0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F => return out.push(ce(G_SPACE + 0x10, BASE, 2)),
        _ => {}
    }
    // 全角の ASCII は ASCII と同じ第1重み
    if (0xFF01..=0xFF5E).contains(&cp) {
        let before = out.len();
        push(out, char::from_u32(cp - 0xFEE0).unwrap_or(c));
        for e in &mut out[before..] {
            e.t += 2;
        }
        return;
    }
    if (0xFF66..=0xFF9D).contains(&cp) {
        if let Some(k) = HALF_KANA.chars().nth((cp - 0xFF66) as usize) {
            let before = out.len();
            push(out, k);
            for e in &mut out[before..] {
                e.t += 2;
            }
        }
        return;
    }
    if c.is_ascii_digit() {
        return out.push(ce(G_DIGIT + (cp - '0' as u32) * 16, BASE, 0));
    }
    if c.is_ascii_alphabetic() {
        return out.push(ce(latin(c.to_ascii_lowercase()), BASE, u16::from(c.is_ascii_uppercase())));
    }
    if let Some(i) = PUNCT_ORDER.iter().position(|x| *x == c) {
        return out.push(ce(G_PUNCT + i as u32 * 16, BASE, 0));
    }
    if let Some(i) = CURRENCY_ORDER.iter().position(|x| *x == c) {
        return out.push(ce(G_CURRENCY + i as u32, BASE, 0));
    }
    // 上付き・丸数字は数字の第3重み違い
    let digit = match cp {
        0xB9 => Some(1),
        0xB2 | 0xB3 => Some(cp - 0xB0),
        0x2070 => Some(0),
        0x2074..=0x2079 => Some(cp - 0x2070),
        0x2460..=0x2468 => Some(cp - 0x245F),
        _ => None,
    };
    if let Some(d) = digit {
        return out.push(ce(G_DIGIT + d * 16, BASE, 3));
    }
    // ローマ数字（小）ⅰ は i の第3重み違い
    if cp == 0x2170 {
        return out.push(ce(latin('i'), BASE, 1));
    }
    if cp == 0x2122 {
        out.push(ce(latin('t'), BASE, 3));
        return out.push(ce(latin('m'), BASE, 3));
    }
    if (0xC0..=0xFF).contains(&cp) {
        let i = (cp - 0xC0) as usize;
        let base = LATIN1.as_bytes()[i] as char;
        if base != '*' {
            let upper = base.is_ascii_uppercase();
            return out.push(ce(latin(base.to_ascii_lowercase()), BASE + LATIN1_ACC[i], u16::from(upper)));
        }
        match cp {
            0xC6 | 0xE6 => {
                let t = if cp == 0xC6 { 5 } else { 4 };
                out.push(ce(latin('a'), BASE, t));
                return out.push(ce(latin('e'), BASE, t));
            }
            0xD0 | 0xF0 => return out.push(ce(latin('d') + 2, BASE, u16::from(cp == 0xD0))),
            0xDE | 0xFE => return out.push(ce(latin('z') + 4, BASE, u16::from(cp == 0xDE))),
            0xDF => {
                out.push(ce(latin('s'), BASE, 4));
                return out.push(ce(latin('s'), BASE, 4));
            }
            _ => {}
        }
    }
    if (0x100..=0x17F).contains(&cp) {
        let i = (cp - 0x100) as usize;
        let base = LATIN_EXT_A.as_bytes()[i] as char;
        if base != '*' {
            let upper = base.is_ascii_uppercase();
            return out.push(ce(latin(base.to_ascii_lowercase()), BASE + LATIN_EXT_A_ACC.get(i).copied().unwrap_or(0), u16::from(upper)));
        }
        match cp {
            0x131 => return out.push(ce(latin('i') + 2, BASE, 0)),
            0x132 | 0x133 => {
                let t = if cp == 0x132 { 5 } else { 4 };
                out.push(ce(latin('i'), BASE, t));
                return out.push(ce(latin('j'), BASE, t));
            }
            0x138 => return out.push(ce(latin('q') + 2, BASE, 0)),
            0x149 => {
                let apos = PUNCT_ORDER.iter().position(|x| *x == '\'').unwrap_or(0) as u32;
                out.push(ce(G_PUNCT + apos * 16, BASE, 0));
                return out.push(ce(latin('n'), BASE, 0));
            }
            0x14A | 0x14B => return out.push(ce(latin('n') + 2, BASE, u16::from(cp == 0x14A))),
            0x152 | 0x153 => {
                let t = if cp == 0x152 { 5 } else { 4 };
                out.push(ce(latin('o'), BASE, t));
                return out.push(ce(latin('e'), BASE, t));
            }
            _ => {}
        }
    }
    // ひらがな・カタカナ（ー と踊り字は記号の側）
    let hira = match cp {
        0x30A1..=0x30F6 => Some((cp - 0x60, true)),
        0x30F7..=0x30FA => Some((cp - 0x30F7 + 0x308F, true)),
        0x3041..=0x3096 => Some((cp, false)),
        _ => None,
    };
    if let Some((h, kata)) = hira {
        let (h, voiced, small) = if (0x30F7..=0x30FA).contains(&cp) { (h, 1, false) } else { kana(h).unwrap_or((h, 0, false)) };
        let t = u16::from(!small) + if kata { 2 } else { 0 };
        return out.push(ce(G_KANA + h, BASE + voiced * 0x10, t));
    }
    // 長音記号は記号の側（絵文字の後、通貨記号の前）
    if cp == 0x30FC {
        return out.push(ce(G_SYMBOL + 0x0011_0000, BASE, 0));
    }
    if matches!(cp, 0x3005 | 0x309D | 0x309E | 0x30FD | 0x30FE) {
        return out.push(ce(G_PUNCT - 0x100 + (cp & 0xFF), BASE, 0));
    }
    if matches!(cp, 0x4E00..=0x9FFF | 0xF900..=0xFAFF) {
        return out.push(ce(G_HAN + cp, BASE, 0));
    }
    if matches!(cp, 0x3400..=0x4DBF | 0x20000..=0x3FFFF) {
        return out.push(ce(G_HAN + 0x0010_0000 + cp, BASE, 0));
    }
    if c.is_numeric() || c.is_alphabetic() {
        let mut lower = c.to_lowercase();
        let l = match (lower.next(), lower.next()) {
            (Some(l), None) => l,
            _ => c,
        };
        return out.push(ce(G_LETTER + l as u32, BASE, u16::from(l != c)));
    }
    // その他の記号（絵文字・ー など）は ~ と $ のあいだ
    out.push(ce(G_SYMBOL + cp, BASE, 0))
}

/// a.localeCompare(b) と同じ向き（Less = a が前）
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (x, y) = (elements(a), elements(b));
    let p = x.iter().map(|e| e.p).cmp(y.iter().map(|e| e.p));
    if p != Ordering::Equal {
        return p;
    }
    let s = x.iter().map(|e| e.s).cmp(y.iter().map(|e| e.s));
    if s != Ordering::Equal {
        return s;
    }
    x.iter().map(|e| e.t).cmp(y.iter().map(|e| e.t))
}

/// 既定の Array.prototype.sort（UTF-16 の符号単位の順）
pub fn utf16_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(xs: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = xs.iter().map(|s| s.to_string()).collect();
        v.sort_by(|a, b| locale_compare(a, b));
        v
    }

    #[test]
    fn ascii_follows_icu_root() {
        let mut all: Vec<String> = (32u8..127).map(|c| (c as char).to_string()).collect();
        all.sort_by(|a, b| locale_compare(a, b));
        let expected = " _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789aAbBcCdDeEfFgGhHiIjJkKlLmMnNoOpPqQrRsStTuUvVwWxXyYzZ";
        assert_eq!(all.concat(), expected);
    }

    #[test]
    fn levels_and_scripts() {
        assert_eq!(
            sorted(&["ab", "aB", "Ab", "AB", "a-b", "a_b", "A_b", "ab1", "ab10", "ab2", "Ab2", "ab-2"]),
            ["a_b", "A_b", "a-b", "ab", "aB", "Ab", "AB", "ab-2", "ab1", "ab10", "ab2", "Ab2"]
        );
        assert_eq!(
            sorted(&["Example App", "example-app", "exampleapp", "Example", "example 2"]),
            ["Example", "example 2", "Example App", "example-app", "exampleapp"]
        );
        assert_eq!(sorted(&["é", "e\u{301}", "è", "ê", "ë", "ē", "f", "e"]), ["e", "é", "e\u{301}", "è", "ê", "ë", "ē", "f"]);
        assert_eq!(sorted(&["か", "が", "カ", "ガ", "あ", "ア", "ぁ"]), ["ぁ", "あ", "ア", "か", "カ", "が", "ガ"]);
        assert_eq!(sorted(&["秀丸", "zoom", "α", "あ", "1"]), ["1", "zoom", "α", "あ", "秀丸"]);
        assert_eq!(sorted(&["ae", "æ", "ad", "af"]), ["ad", "ae", "æ", "af"]);
        assert_eq!(locale_compare("a", "a"), Ordering::Equal);
    }

    #[test]
    fn default_sort_is_utf16() {
        let mut v = vec!["b", "B", "a", "\u{FF21}", "\u{1F600}"];
        v.sort_by(|a, b| utf16_cmp(a, b));
        assert_eq!(v, ["B", "a", "b", "\u{1F600}", "\u{FF21}"]);
    }
}
