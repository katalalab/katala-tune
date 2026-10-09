//! AI エージェントの残り枠（tune-core だけの表）。今は Codex だけ（codex app-server の account/rateLimits/read）。
//! - ai_limits: 機体×ツール×窓の長さ（分）ごとの、最後に報告された使用率とリセット時刻。窓は長さで見分ける（長さの無い窓は 0）
//! - ai_limits_status: 機体×ツールの、最後に問い合わせた時刻と結果（ok・empty・no_codex・no_python・error）
//!
//! 読めた（ok・empty）ときはその機体の窓を置き換える（報告の無かった窓は消える = 画面では「未報告」）。
//! 失敗（error・no_python）のときは前に読めた窓を残す（「最後に観測した値」として時刻つきで出す）。codex が無いときは消す。

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use super::{Result, Store, row_json};
use crate::js;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ai_limits (
  node_id TEXT NOT NULL, tool TEXT NOT NULL, window_mins INTEGER NOT NULL, used_pct REAL, resets_at INTEGER, observed_at INTEGER NOT NULL,
  PRIMARY KEY (node_id, tool, window_mins)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS ai_limits_status (
  node_id TEXT NOT NULL, tool TEXT NOT NULL, checked_at INTEGER NOT NULL, state TEXT NOT NULL, error TEXT, probe_version TEXT,
  PRIMARY KEY (node_id, tool)
) WITHOUT ROWID;
";

/// 1 つの窓（長さ・使用率・リセット時刻だけ）
#[derive(Clone, Debug, PartialEq)]
pub struct LimitWindow {
    /// 窓の長さ（分）。報告に無ければ None
    pub mins: Option<i64>,
    pub used_pct: f64,
    /// リセット時刻（epoch 秒）
    pub resets_at: Option<i64>,
}

/// 問い合わせの結果（保存する形）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LimitsReport {
    /// ok・empty・no_codex・no_python・error
    pub state: String,
    pub windows: Vec<LimitWindow>,
    pub error: Option<String>,
    pub probe_version: Option<String>,
}

impl Store {
    /// 問い合わせの結果を残す
    pub fn save_limits(&self, node_id: &str, tool: &str, r: &LimitsReport, now: i64) -> Result<()> {
        let (state, windows) = (r.state.as_str(), &r.windows);
        self.tx(|c| {
            if matches!(state, "ok" | "empty" | "no_codex") {
                c.execute("DELETE FROM ai_limits WHERE node_id = ? AND tool = ?", params![node_id, tool])?;
                for w in windows {
                    c.execute(
                        "INSERT OR REPLACE INTO ai_limits (node_id, tool, window_mins, used_pct, resets_at, observed_at) VALUES (?, ?, ?, ?, ?, ?)",
                        params![node_id, tool, w.mins.unwrap_or(0), w.used_pct, w.resets_at, now],
                    )?;
                }
            }
            c.execute(
                "INSERT OR REPLACE INTO ai_limits_status (node_id, tool, checked_at, state, error, probe_version) VALUES (?, ?, ?, ?, ?, ?)",
                params![node_id, tool, now, state, r.error.as_deref().map(|e| js::slice16(e, 300)), r.probe_version],
            )?;
            Ok(())
        })
    }

    /// 最後に問い合わせた時刻（無ければ None）
    pub fn limits_checked_at(&self, node_id: &str, tool: &str) -> Result<Option<i64>> {
        self.conn.query_row("SELECT checked_at FROM ai_limits_status WHERE node_id = ? AND tool = ?", params![node_id, tool], |r| r.get(0)).optional()
    }

    /// 機体×ツールごとの残り枠。[{ node_id, tool, state, checked_at, error, windows: [{ window_mins, used_pct, resets_at, observed_at }] }]
    pub fn ai_limits(&self) -> Result<Vec<Value>> {
        let status: Vec<Value> = self
            .conn
            .prepare("SELECT node_id, tool, state, checked_at, error, probe_version FROM ai_limits_status ORDER BY node_id, tool")?
            .query_map([], row_json)?
            .collect::<Result<_>>()?;
        let wins: Vec<Value> = self
            .conn
            .prepare("SELECT node_id, tool, window_mins, used_pct, resets_at, observed_at FROM ai_limits ORDER BY node_id, tool, window_mins")?
            .query_map([], row_json)?
            .collect::<Result<_>>()?;
        Ok(status
            .into_iter()
            .map(|mut s| {
                let (n, t) = (js::string(s.get("node_id")), js::string(s.get("tool")));
                let w: Vec<Value> = wins.iter().filter(|w| js::is_str(w.get("node_id"), &n) && js::is_str(w.get("tool"), &t)).cloned().collect();
                s["windows"] = json!(w);
                s
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_replaces_windows_and_errors_keep_the_last_observation() {
        let db = Store::open_in_memory().unwrap();
        let w = |m: i64, p: f64| LimitWindow { mins: Some(m), used_pct: p, resets_at: Some(1_900_000_000) };
        let rep = |state: &str, windows: Vec<LimitWindow>, error: Option<&str>| LimitsReport {
            state: state.into(),
            windows,
            error: error.map(str::to_string),
            probe_version: Some("v".into()),
        };
        db.save_limits("n1", "codex", &rep("ok", vec![w(300, 12.0), w(10080, 40.0)], None), 1).unwrap();
        assert_eq!(db.limits_checked_at("n1", "codex").unwrap(), Some(1));
        // 失敗: 前の窓は残る（最後に観測した値）
        db.save_limits("n1", "codex", &rep("error", vec![], Some("timeout")), 2).unwrap();
        let l = &db.ai_limits().unwrap()[0];
        assert_eq!((l["state"].clone(), l["windows"].as_array().unwrap().len(), l["windows"][0]["observed_at"].clone()), (json!("error"), 2, json!(1)));
        // 読めたが 7 日の窓が無い → 7 日は消える（画面では「未報告」）
        db.save_limits("n1", "codex", &rep("ok", vec![w(300, 50.0)], None), 3).unwrap();
        let l = &db.ai_limits().unwrap()[0];
        assert_eq!(l["windows"].as_array().unwrap().len(), 1);
        assert_eq!((l["windows"][0]["window_mins"].clone(), l["windows"][0]["used_pct"].clone()), (json!(300), json!(50)));
        // codex が無い → 窓を消す
        db.save_limits("n1", "codex", &rep("no_codex", vec![], None), 4).unwrap();
        assert!(db.ai_limits().unwrap()[0]["windows"].as_array().unwrap().is_empty());
    }
}
