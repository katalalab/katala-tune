//! 費用の推定（API 単価換算）。単価表は data/prices.json（公式の料金ページから使うモデルの分だけ写したもの。取得日・出典・版つき）を埋め込む。
//! - 表示は「API 単価換算の推定」。サブスク（定額）の実請求ではない
//! - 単価表に無いモデルは 0 円にせず「単価不明」（[`cost`] が None）
//! - 数え方:
//!   - Claude Code: 入力・出力・キャッシュ読み・キャッシュ書き（5 分と 1 時間で別の単価。内訳の無い古い記録は 5 分として数える）。
//!     推論（thinking）は出力に含まれているので足さない
//!   - Codex: 入力は `input − cached`（調査の時点で引いてある）を通常の単価、`cached` をキャッシュ読みの単価。推論は出力に含まれているので足さない
//! - 割増（Claude の高速モード・US のデータ所在、OpenAI の長いコンテキスト）は単価表に載せていない。
//!   高速モード・US は調査がモデル名に `@fast`・`@us` を付けるので「単価不明」になる。長いコンテキストは区別できないので、推定は低めに出うる

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde_json::{Value, json};

pub const PRICES_JSON: &str = include_str!("../../../data/prices.json");

/// 1 モデルの単価（100 万トークンあたりの米ドル）
#[derive(Clone, Debug, PartialEq)]
pub struct Price {
    pub key: String,
    pub label: String,
    pub source: String,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    /// Claude の 5 分のキャッシュ書き
    pub cache_write_5m: Option<f64>,
    /// Claude の 1 時間のキャッシュ書き
    pub cache_write_1h: Option<f64>,
    /// OpenAI のキャッシュ書き
    pub cache_write: Option<f64>,
}

/// 単価表
#[derive(Clone, Debug)]
pub struct Table {
    pub version: String,
    pub fetched_at: String,
    pub sources: Value,
    pub models: BTreeMap<String, Price>,
}

/// 費用に使うトークンの量（tok_reasoning は出力に含まれているので使わない）
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    /// cache_write のうち 1 時間のキャッシュ書き（Claude）
    pub cache_write_1h: i64,
}

impl Tokens {
    pub fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cache_write_1h += o.cache_write_1h;
    }

    pub fn to_json(&self) -> Value {
        json!({ "tok_in": self.input, "tok_out": self.output, "tok_cache_read": self.cache_read, "tok_cache_write": self.cache_write, "tok_cache_write_1h": self.cache_write_1h })
    }
}

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|x| x.is_finite() && *x >= 0.0)
}

/// 単価表を読む（壊れた行は飛ばす。input・output・cache_read が無いモデルは載せない）
pub fn parse(text: &str) -> Result<Table, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("単価表を読めない: {e}"))?;
    let version = v.get("version").and_then(Value::as_str).ok_or("単価表に version が無い")?.to_string();
    let mut models = BTreeMap::new();
    for (k, m) in v.get("models").and_then(Value::as_object).into_iter().flatten() {
        let (Some(input), Some(output), Some(cache_read)) = (num(m.get("input")), num(m.get("output")), num(m.get("cache_read"))) else { continue };
        models.insert(
            k.clone(),
            Price {
                key: k.clone(),
                label: m.get("label").and_then(Value::as_str).unwrap_or(k).to_string(),
                source: m.get("source").and_then(Value::as_str).unwrap_or("").to_string(),
                input,
                output,
                cache_read,
                cache_write_5m: num(m.get("cache_write_5m")),
                cache_write_1h: num(m.get("cache_write_1h")),
                cache_write: num(m.get("cache_write")),
            },
        );
    }
    Ok(Table {
        version,
        fetched_at: v.get("fetched_at").and_then(Value::as_str).unwrap_or("").to_string(),
        sources: v.get("sources").cloned().unwrap_or(Value::Null),
        models,
    })
}

static TABLE: LazyLock<Table> = LazyLock::new(|| parse(PRICES_JSON).expect("data/prices.json を読めない"));

/// 同梱の単価表
pub fn table() -> &'static Table {
    &TABLE
}

impl Table {
    /// モデル名から単価を探す。そのまま無ければ、末尾の日付（-20251001 など）を外して探す
    pub fn lookup(&self, model: &str) -> Option<&Price> {
        if let Some(p) = self.models.get(model) {
            return Some(p);
        }
        let (head, tail) = model.rsplit_once('-')?;
        if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) { self.models.get(head) } else { None }
    }

    /// 費用（米ドル）。単価表に無いモデル・必要な単価が無いときは None（単価不明。0 円にしない）
    pub fn cost(&self, tool: &str, model: Option<&str>, t: &Tokens) -> Option<f64> {
        let p = self.lookup(model?)?;
        let m = 1_000_000.0;
        let base = t.input as f64 * p.input + t.output as f64 * p.output + t.cache_read as f64 * p.cache_read;
        let write = if tool == "claude" {
            let h1 = t.cache_write_1h.clamp(0, t.cache_write.max(0));
            let m5 = t.cache_write - h1;
            let a = if m5 > 0 { m5 as f64 * p.cache_write_5m? } else { 0.0 };
            let b = if h1 > 0 { h1 as f64 * p.cache_write_1h? } else { 0.0 };
            a + b
        } else if t.cache_write > 0 {
            t.cache_write as f64 * p.cache_write?
        } else {
            0.0
        };
        Some((base + write) / m)
    }

    /// 画面に出す単価表の見出し（版・取得日・出典）
    pub fn meta(&self) -> Value {
        json!({ "version": self.version, "fetched_at": self.fetched_at, "sources": self.sources, "models": self.models.len() })
    }

    /// 1 モデルの単価（画面のモデル別の表に添える）
    pub fn price_json(&self, model: Option<&str>) -> Value {
        match model.and_then(|m| self.lookup(m)) {
            Some(p) => json!({ "key": p.key, "label": p.label, "source": p.source, "input": p.input, "output": p.output, "cache_read": p.cache_read,
                               "cache_write_5m": p.cache_write_5m, "cache_write_1h": p.cache_write_1h, "cache_write": p.cache_write }),
            None => Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_table_has_version_sources_and_models() {
        let t = table();
        assert!(!t.version.is_empty() && !t.fetched_at.is_empty());
        for s in ["anthropic", "openai"] {
            let u = t.sources[s]["url"].as_str().unwrap();
            assert!(u.starts_with("https://"), "{s} の出典");
        }
        // どのモデルも出典が sources にある
        assert!(t.models.values().all(|p| t.sources.get(&p.source).is_some()), "出典の無いモデル");
        assert!(t.models.len() >= 10);
    }

    #[test]
    fn lookup_strips_a_date_suffix_only() {
        let t = table();
        assert_eq!(t.lookup("claude-haiku-4-5-20251001").map(|p| p.key.as_str()), Some("claude-haiku-4-5"));
        assert!(t.lookup("claude-opus-5-5").is_some());
        assert!(t.lookup("claude-opus-5-5@fast").is_none(), "高速モードは単価不明");
        assert!(t.lookup("claude-opus-5-x").is_none());
        assert!(t.lookup("unknown-model").is_none());
    }

    #[test]
    fn claude_cost_splits_cache_writes_and_ignores_reasoning() {
        let t = parse(
            r#"{ "version": "t", "models": { "m": { "input": 2, "output": 10, "cache_read": 0.2, "cache_write_5m": 2.5, "cache_write_1h": 4 },
                                            "o": { "input": 1, "output": 4, "cache_read": 0.1 } } }"#,
        )
        .unwrap();
        // 入力 1M・出力 1M・キャッシュ読み 10M・キャッシュ書き 3M（うち 1 時間 2M）
        let tok = Tokens { input: 1_000_000, output: 1_000_000, cache_read: 10_000_000, cache_write: 3_000_000, cache_write_1h: 2_000_000 };
        let c = t.cost("claude", Some("m"), &tok).unwrap();
        assert!((c - (2.0 + 10.0 + 2.0 + 2.5 + 8.0)).abs() < 1e-9, "{c}");
        // Codex: input − cached（調査で引き済み）・cached・出力。キャッシュ書きの単価が無くても、書きが 0 なら出せる
        let x = Tokens { input: 1_000_000, output: 500_000, cache_read: 4_000_000, ..Default::default() };
        assert!((t.cost("codex", Some("o"), &x).unwrap() - (1.0 + 2.0 + 0.4)).abs() < 1e-9);
        // 単価が無い（書きがあるのに書きの単価が無い・モデル不明）は None
        assert_eq!(t.cost("codex", Some("o"), &Tokens { cache_write: 1, ..x }), None);
        assert_eq!(t.cost("claude", Some("o"), &tok), None);
        assert_eq!(t.cost("claude", None, &tok), None);
        assert_eq!(t.cost("claude", Some("nope"), &tok), None);
    }

    #[test]
    fn broken_rows_are_skipped() {
        let t = parse(r#"{ "version": "t", "models": { "a": { "input": 1, "output": 2 }, "b": { "input": -1, "output": 1, "cache_read": 0 } } }"#).unwrap();
        assert!(t.models.is_empty());
        assert!(parse("{}").is_err());
    }
}
