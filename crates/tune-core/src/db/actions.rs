//! 実行記録（このアプリから実行した変更操作と、元に戻す操作）

use rusqlite::params;
use serde_json::Value;

use super::{Result, Store, row_json, sql};
use crate::js;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tune_actions (
  id TEXT PRIMARY KEY, at INTEGER NOT NULL, node_id TEXT NOT NULL, type TEXT NOT NULL,
  params TEXT, label TEXT, ok INTEGER, output TEXT, undo TEXT, undo_of TEXT
);
CREATE INDEX IF NOT EXISTS tune_actions_at ON tune_actions (at DESC);
";

impl Store {
    /// entry = { id, at, node_id, type, params, label, ok, output, undo, undo_of }（main.js と同じ形）。
    /// ok が null / 無いときは「実行を始めたが結果をまだ書いていない」記録（未完了）。`finish_action` で結果を書く
    pub fn add_action(&self, a: &Value) -> Result<()> {
        let g = |k: &str| a.get(k);
        let params_json = g("params").filter(|v| !v.is_null()).map(Value::to_string).unwrap_or_else(|| "null".into());
        let undo = g("undo").filter(|v| js::truthy(Some(v))).map(Value::to_string);
        self.conn.execute(
            "INSERT INTO tune_actions (id, at, node_id, type, params, label, ok, output, undo, undo_of) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                sql(g("id")),
                sql(g("at")),
                sql(g("node_id")),
                sql(g("type")),
                params_json,
                sql(g("label")),
                g("ok").filter(|v| !v.is_null()).map(|v| i64::from(js::truthy(Some(v)))),
                sql(g("output")),
                undo,
                sql(g("undo_of"))
            ],
        )?;
        Ok(())
    }

    /// 未完了の記録に結果を書く。未完了でなければ何も変えない（false）
    pub fn finish_action(&self, id: &str, ok: bool, output: &str, undo: Option<&Value>) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE tune_actions SET ok = ?, output = ?, undo = ? WHERE id = ? AND ok IS NULL",
            params![i64::from(ok), output, undo.filter(|v| js::truthy(Some(v))).map(Value::to_string), id],
        )?;
        Ok(n == 1)
    }

    /// 新しい順に n 件。params / undo は JSON を戻し、ok は真偽にする。
    /// state: ok / failed / incomplete。結果を書く前に止まった操作（ok が NULL）は、成功にも失敗にも数えない（ok は false）
    pub fn actions(&self, n: i64) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached("SELECT * FROM tune_actions ORDER BY at DESC LIMIT ?")?;
        let rows = st.query_map([n], row_json)?.collect::<Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .map(|mut r| {
                if let Value::Object(m) = &mut r {
                    let state = match m.get("ok") {
                        None | Some(Value::Null) => "incomplete",
                        v if js::truthy(v) => "ok",
                        _ => "failed",
                    };
                    m.insert("state".into(), Value::from(state));
                    let ok = js::truthy(m.get("ok"));
                    m.insert("ok".into(), Value::Bool(ok));
                    let parse = |v: Option<&Value>| match v {
                        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
                        _ => Value::Null,
                    };
                    let p = parse(m.get("params"));
                    m.insert("params".into(), p);
                    let u = parse(m.get("undo"));
                    m.insert("undo".into(), u);
                }
                r
            })
            .collect())
    }
}
