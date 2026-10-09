//! ネットワークとセキュリティの記録（tune-core だけの表。Electron 版は読み書きしない）。
//!
//! - `net_items`・`net_events`: 自動起動（常駐）の今の一覧と増減の記録（道具の棚卸しの inventory・inventory_events と同じ型）
//! - `net_baseline`: 機体ごと・部分ごとの初回の時刻（`persist:<種類>` は初回の記録、`peers` は接続先を覚え始めた時刻）
//! - `net_peers`: 外向きの接続の (プロセス, 宛先, ポート)。宛先はこの DB にだけ置き、30 日見なければ消す
//!
//! 判定（増減・初めて）は crate::netsec の純粋な関数（JS 版と同じ）で決め、ここは読み書きだけ。

use rusqlite::types::Value as SqlValue;
use rusqlite::{OptionalExtension, params, params_from_iter};
use serde_json::{Value, json};

use super::{Result, Store, row_json};
use crate::netsec::{PEER_KEEP_DAYS, persist_kinds_of};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS net_items (
  node_id TEXT NOT NULL, kind TEXT NOT NULL, key TEXT NOT NULL, program TEXT,
  first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, removed_at INTEGER,
  PRIMARY KEY (node_id, kind, key)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS net_events (
  id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, node_id TEXT NOT NULL, kind TEXT NOT NULL, key TEXT NOT NULL,
  change TEXT NOT NULL, program TEXT, from_program TEXT
);
CREATE INDEX IF NOT EXISTS net_events_ts ON net_events (ts DESC);
CREATE TABLE IF NOT EXISTS net_baseline (node_id TEXT NOT NULL, part TEXT NOT NULL, since INTEGER NOT NULL, PRIMARY KEY (node_id, part)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS net_peers (
  node_id TEXT NOT NULL, proc TEXT NOT NULL, addr TEXT NOT NULL, port INTEGER NOT NULL,
  first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, seen INTEGER NOT NULL DEFAULT 1, n_max INTEGER NOT NULL DEFAULT 1, public INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (node_id, proc, addr, port)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS net_peers_last ON net_peers (last_seen);
";

/// 常駐の増減の記録の保持（日）
pub const NET_EVENT_DAYS: i64 = 180;
const DAY_MS: i64 = 86_400_000;

fn s(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
}

impl Store {
    /// 機体の自動起動の一覧（消えたものも removed_at 付きで）。`{ kind, key, program, removed_at }`
    pub fn net_items(&self, node_id: &str) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached("SELECT kind, key, program, removed_at FROM net_items WHERE node_id = ? ORDER BY kind, key")?;
        st.query_map([node_id], row_json)?.collect()
    }

    /// 初回を済ませた部分（prefix を除いた名前）。prefix = "persist:" なら種類の一覧
    pub fn net_baselined(&self, node_id: &str, prefix: &str) -> Result<Vec<String>> {
        let mut st = self.conn.prepare_cached("SELECT part FROM net_baseline WHERE node_id = ? ORDER BY part")?;
        let parts: Vec<String> = st.query_map([node_id], |r| r.get::<_, String>(0))?.collect::<Result<_>>()?;
        Ok(parts.into_iter().filter_map(|p| p.strip_prefix(prefix).map(str::to_string)).collect())
    }

    pub fn net_baseline_since(&self, node_id: &str, part: &str) -> Result<Option<i64>> {
        self.conn.query_row("SELECT since FROM net_baseline WHERE node_id = ? AND part = ?", params![node_id, part], |r| r.get(0)).optional()
    }

    pub fn net_set_baseline(&self, node_id: &str, part: &str, since: i64) -> Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO net_baseline (node_id, part, since) VALUES (?, ?, ?)", params![node_id, part, since])?;
        Ok(())
    }

    /// diff_persist の結果を書く（1つのトランザクション）。ok_kinds = 今回取れた種類（取れなかった種類の一覧は触らない）
    pub fn net_apply_persist(&self, node_id: &str, items: &[Value], diff: &Value, ok_kinds: &[String], at: i64) -> Result<()> {
        self.tx(|c| {
            let mut up = c.prepare_cached(
                "INSERT INTO net_items (node_id, kind, key, program, first_seen, last_seen, removed_at) VALUES (?, ?, ?, ?, ?, ?, NULL)
                 ON CONFLICT (node_id, kind, key) DO UPDATE SET program = excluded.program, last_seen = excluded.last_seen,
                 first_seen = CASE WHEN net_items.removed_at IS NULL THEN net_items.first_seen ELSE excluded.first_seen END, removed_at = NULL",
            )?;
            for it in items {
                let Some(kind) = s(it.get("kind")) else { continue };
                if !ok_kinds.iter().any(|k| k == kind) {
                    continue;
                }
                up.execute(params![node_id, kind, s(it.get("key")).unwrap_or_default(), s(it.get("program")), at, at])?;
            }
            let mut rm = c.prepare_cached("UPDATE net_items SET removed_at = ? WHERE node_id = ? AND kind = ? AND key = ?")?;
            let mut ev = c.prepare_cached("INSERT INTO net_events (ts, node_id, kind, key, change, program, from_program) VALUES (?, ?, ?, ?, ?, ?, ?)")?;
            for ch in crate::js::arr(diff.get("changes")) {
                let (kind, key, change) =
                    (s(ch.get("kind")).unwrap_or_default(), s(ch.get("key")).unwrap_or_default(), s(ch.get("change")).unwrap_or_default());
                if change == "removed" {
                    rm.execute(params![at, node_id, kind, key])?;
                }
                ev.execute(params![at, node_id, kind, key, change, s(ch.get("program")), s(ch.get("from"))])?;
            }
            let mut base = c.prepare_cached("INSERT OR IGNORE INTO net_baseline (node_id, part, since) VALUES (?, ?, ?)")?;
            for k in crate::js::arr(diff.get("baseline")) {
                if let Some(k) = k.as_str() {
                    base.execute(params![node_id, format!("persist:{k}"), at])?;
                }
            }
            Ok(())
        })
    }

    /// 30 日見ていない宛先を消す（全機体）
    pub fn net_prune_peers(&self, now: i64) -> Result<()> {
        self.conn.execute("DELETE FROM net_peers WHERE last_seen < ?", [now - PEER_KEEP_DAYS * DAY_MS])?;
        Ok(())
    }

    /// 覚えている宛先 `{ proc, addr, port }`
    pub fn net_peers_known(&self, node_id: &str) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached("SELECT proc, addr, port FROM net_peers WHERE node_id = ? ORDER BY proc, addr, port")?;
        st.query_map([node_id], row_json)?.collect()
    }

    pub fn net_peers_count(&self, node_id: &str) -> Result<i64> {
        self.conn.query_row("SELECT count(*) FROM net_peers WHERE node_id = ?", [node_id], |r| r.get(0))
    }

    /// 今回の標本を覚える（初めてなら first_seen、見るたびに last_seen と回数）
    pub fn net_upsert_peers(&self, node_id: &str, sample: &[Value], at: i64) -> Result<()> {
        self.tx(|c| {
            let mut up = c.prepare_cached(
                "INSERT INTO net_peers (node_id, proc, addr, port, first_seen, last_seen, seen, n_max, public) VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?)
                 ON CONFLICT (node_id, proc, addr, port) DO UPDATE SET last_seen = excluded.last_seen, seen = net_peers.seen + 1,
                 n_max = max(net_peers.n_max, excluded.n_max), public = excluded.public",
            )?;
            for p in sample {
                let (Some(proc_), Some(addr), Some(port)) = (s(p.get("proc")), s(p.get("addr")), p.get("port").and_then(Value::as_f64)) else { continue };
                let n = p.get("n").and_then(Value::as_f64).unwrap_or(1.0) as i64;
                up.execute(params![node_id, proc_, addr, port as i64, at, at, n, matches!(p.get("public"), Some(Value::Bool(true)))])?;
            }
            Ok(())
        })
    }

    /// 常駐の増減の記録（新しい順）
    pub fn net_events(&self, limit: i64) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached(
            "SELECT ts, node_id, kind, key, change, program, from_program FROM net_events WHERE change != 'baseline' ORDER BY ts DESC, id DESC LIMIT ?",
        )?;
        st.query_map([limit], row_json)?.collect()
    }

    /// 常駐の増減を許可された機体で絞ってから件数を制限する。
    pub fn net_events_for_nodes(&self, node_ids: &[String], limit: i64) -> Result<Vec<Value>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; node_ids.len()].join(",");
        let sql = format!(
            "SELECT ts, node_id, kind, key, change, program, from_program FROM net_events
             WHERE change != 'baseline' AND node_id IN ({marks}) ORDER BY ts DESC, id DESC LIMIT ?"
        );
        let mut args: Vec<SqlValue> = node_ids.iter().cloned().map(SqlValue::Text).collect();
        args.push(SqlValue::Integer(limit));
        let mut st = self.conn.prepare(&sql)?;
        st.query_map(params_from_iter(args), row_json)?.collect()
    }

    /// since 以降に初めて見た宛先（新しい順）
    pub fn net_peers_recent(&self, since: i64, limit: i64) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached(
            "SELECT node_id, proc, addr, port, first_seen, last_seen, seen, n_max, public FROM net_peers WHERE first_seen >= ?
             ORDER BY first_seen DESC, node_id, proc, addr, port LIMIT ?",
        )?;
        let rows: Vec<Value> = st.query_map(params![since, limit], row_json)?.collect::<Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(|mut r| {
                r["public"] = json!(r.get("public").and_then(Value::as_i64) == Some(1));
                r
            })
            .collect())
    }

    /// 最近の宛先を許可された機体で絞ってから件数を制限する。
    pub fn net_peers_recent_for_nodes(&self, since: i64, node_ids: &[String], limit: i64) -> Result<Vec<Value>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; node_ids.len()].join(",");
        let sql = format!(
            "SELECT node_id, proc, addr, port, first_seen, last_seen, seen, n_max, public FROM net_peers
             WHERE first_seen >= ? AND node_id IN ({marks}) ORDER BY first_seen DESC, node_id, proc, addr, port LIMIT ?"
        );
        let mut args = vec![SqlValue::Integer(since)];
        args.extend(node_ids.iter().cloned().map(SqlValue::Text));
        args.push(SqlValue::Integer(limit));
        let mut st = self.conn.prepare(&sql)?;
        let rows: Vec<Value> = st.query_map(params_from_iter(args), row_json)?.collect::<Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(|mut r| {
                r["public"] = json!(r.get("public").and_then(Value::as_i64) == Some(1));
                r
            })
            .collect())
    }

    /// ログインの記録（mac_auth・win_security）の送り元ごとの件数。24 時間と 7 日
    pub fn net_login_counts(&self, now: i64) -> Result<Vec<Value>> {
        let mut st = self.conn.prepare_cached(
            "SELECT node_id, source, provider, event_id, sum(ts >= ?) AS n_24h, count(*) AS n_7d, max(ts) AS last_ts FROM logs
             WHERE source IN ('mac_auth', 'win_security') AND ts >= ? GROUP BY node_id, source, provider, event_id
             ORDER BY n_24h DESC, n_7d DESC, node_id, provider LIMIT 500",
        )?;
        st.query_map(params![now - DAY_MS, now - 7 * DAY_MS], row_json)?.collect()
    }

    /// ログイン集計を許可された機体で絞ってから件数を制限する。
    pub fn net_login_counts_for_nodes(&self, now: i64, node_ids: &[String], limit: i64) -> Result<Vec<Value>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; node_ids.len()].join(",");
        let sql = format!(
            "SELECT node_id, source, provider, event_id, sum(ts >= ?) AS n_24h, count(*) AS n_7d, max(ts) AS last_ts FROM logs
             WHERE source IN ('mac_auth', 'win_security') AND ts >= ? AND node_id IN ({marks}) GROUP BY node_id, source, provider, event_id
             ORDER BY n_24h DESC, n_7d DESC, node_id, provider LIMIT ?"
        );
        let mut args = vec![SqlValue::Integer(now - DAY_MS), SqlValue::Integer(now - 7 * DAY_MS)];
        args.extend(node_ids.iter().cloned().map(SqlValue::Text));
        args.push(SqlValue::Integer(limit));
        let mut st = self.conn.prepare(&sql)?;
        st.query_map(params_from_iter(args), row_json)?.collect()
    }

    /// 保持期限を過ぎたもの（増減の記録 180 日、宛先 30 日）
    pub fn net_prune(&self, now: i64) -> Result<()> {
        self.conn.execute("DELETE FROM net_events WHERE ts < ?", [now - NET_EVENT_DAYS * DAY_MS])?;
        self.net_prune_peers(now)
    }

    /// 機体の今の自動起動の一覧（画面用。消えたものは含めない）
    pub fn net_persist_list(&self, node_id: &str, os: &str) -> Result<Vec<Value>> {
        let kinds = persist_kinds_of(os);
        let mut st = self
            .conn
            .prepare_cached("SELECT kind, key, program, first_seen, last_seen FROM net_items WHERE node_id = ? AND removed_at IS NULL ORDER BY kind, key")?;
        let rows: Vec<Value> = st.query_map([node_id], row_json)?.collect::<Result<_>>()?;
        Ok(rows.into_iter().filter(|r| r.get("kind").and_then(Value::as_str).is_some_and(|k| kinds.contains(&k))).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netsec::{diff_persist, normalize};

    fn items(keys: &[(&str, &str)]) -> Vec<Value> {
        let raw = json!({ "persist": keys.iter().map(|(k, p)| json!({ "kind": "launchd", "key": k, "program": p })).collect::<Vec<_>>() });
        crate::js::arr(normalize(&raw, "mac").get("persist")).to_vec()
    }

    #[test]
    fn first_run_is_a_baseline_and_failures_do_not_count_as_removals() {
        let d = Store::open_in_memory().unwrap();
        let kinds = vec!["launchd".to_string()];
        // 初回は記録だけ
        let r = d.net_persist_step("n1", &items(&[("user:a", "a"), ("user:b", "b")]), &kinds, &[], 1000).unwrap();
        assert_eq!((r["changes"].as_array().unwrap().len(), r["baseline"].clone()), (0, json!(["launchd"])));
        // 増えた・変わった・減った
        let r = d.net_persist_step("n1", &items(&[("user:a", "a2"), ("user:c", "c")]), &kinds, &[], 2000).unwrap();
        let ch: Vec<(String, String)> =
            r["changes"].as_array().unwrap().iter().map(|c| (c["key"].as_str().unwrap().to_string(), c["change"].as_str().unwrap().to_string())).collect();
        assert_eq!(ch, vec![("user:a".into(), "changed".into()), ("user:c".into(), "added".into()), ("user:b".into(), "removed".into())]);
        assert_eq!(d.net_events(10).unwrap().len(), 3);
        // 取れなかった種類は空と見なさない（削除にしない）
        let r = d.net_persist_step("n1", &[], &kinds, &kinds, 3000).unwrap();
        assert!(r["changes"].as_array().unwrap().is_empty());
        let live: Vec<String> =
            d.net_items("n1").unwrap().iter().filter(|r| r["removed_at"].is_null()).map(|r| r["key"].as_str().unwrap().to_string()).collect();
        assert_eq!(live, vec!["user:a", "user:c"]);
        // 消えたものが戻ったら「増えた」
        let r = d.net_persist_step("n1", &items(&[("user:a", "a2"), ("user:b", "b"), ("user:c", "c")]), &kinds, &[], 4000).unwrap();
        assert_eq!(r["changes"][0]["change"], "added");
        let _ = diff_persist(&[], &[], &kinds, &[], &[]);
    }

    #[test]
    fn peers_are_learned_for_seven_days_and_kept_for_thirty() {
        let d = Store::open_in_memory().unwrap();
        let day = 86_400_000;
        let s = |proc_: &str, addr: &str, port: i64| json!({ "proc": proc_, "addr": addr, "port": port, "n": 1, "public": true });
        let r = d.net_peers_step("n1", &[s("app", "198.51.100.1", 443)], 0).unwrap();
        assert_eq!((r["learning"].clone(), r["new"].as_array().unwrap().len()), (json!(true), 0));
        let r = d.net_peers_step("n1", &[s("app", "198.51.100.2", 443), s("tool", "203.0.113.9", 4444)], 3 * day).unwrap();
        assert_eq!(r["new"].as_array().unwrap().len(), 0, "覚えている途中は知らせない");
        let r = d.net_peers_step("n1", &[s("app", "198.51.100.3", 443), s("app", "198.51.100.1", 8443), s("new", "203.0.113.1", 443)], 8 * day).unwrap();
        let whys: Vec<(String, String)> =
            r["new"].as_array().unwrap().iter().map(|p| (p["proc"].as_str().unwrap().to_string(), p["why"].as_str().unwrap().to_string())).collect();
        assert_eq!(whys, vec![("new".into(), "proc".into()), ("app".into(), "port".into()), ("app".into(), "dest".into())]);
        // 30 日見なかった宛先は消え、また初めてになる
        d.net_peers_step("n1", &[], 40 * day).unwrap();
        assert_eq!(d.net_peers_count("n1").unwrap(), 0);
        assert_eq!(d.net_baseline_since("n1", "peers").unwrap(), Some(0), "覚え始めた時刻は残す（集計は残す）");
    }

    #[test]
    fn allowed_nodes_are_filtered_before_view_limits() {
        let d = Store::open_in_memory().unwrap();
        let allowed_id = "allowed') OR 1=1 --";
        for i in 0..501_i64 {
            if i < 301 {
                d.conn
                    .execute(
                        "INSERT INTO net_events (ts, node_id, kind, key, change) VALUES (?, 'hidden', 'run', ?, 'added')",
                        params![10_000 + i, format!("hidden-{i}")],
                    )
                    .unwrap();
                d.conn
                    .execute(
                        "INSERT INTO net_peers (node_id, proc, addr, port, first_seen, last_seen, seen, n_max, public) VALUES ('hidden', ?, ?, 443, ?, ?, 1, 1, 1)",
                        params![format!("hidden-{i}"), format!("192.0.2.{}", i % 250 + 1), 10_000 + i, 10_000 + i],
                    )
                    .unwrap();
            }
            d.conn
                .execute(
                    "INSERT INTO logs (node_id, source, uid, ts, level, provider, event_id, message, fingerprint, ingested_at) VALUES ('hidden', 'win_security', ?, 9999, 'warn', ?, '4625', 'hidden', ?, 9999)",
                    params![format!("hidden-{i}"), format!("provider-{i}.example"), format!("fp-{i}")],
                )
                .unwrap();
        }
        d.conn.execute("INSERT INTO net_events (ts, node_id, kind, key, change) VALUES (1, ?, 'run', 'allowed-event', 'added')", [allowed_id]).unwrap();
        d.conn
            .execute(
                "INSERT INTO net_peers (node_id, proc, addr, port, first_seen, last_seen, seen, n_max, public) VALUES (?, 'allowed-proc', '198.51.100.1', 8443, 1, 1, 1, 1, 1)",
                [allowed_id],
            )
            .unwrap();
        d.conn
            .execute(
                "INSERT INTO logs (node_id, source, uid, ts, level, provider, event_id, message, fingerprint, ingested_at) VALUES (?, 'win_security', 'allowed-log', 9999, 'warn', '198.51.100.1', '4625', 'allowed', 'allowed-fp', 9999)",
                [allowed_id],
            )
            .unwrap();

        let allowed = vec![allowed_id.to_string()];
        assert_eq!(d.net_events_for_nodes(&allowed, 300).unwrap()[0]["key"], "allowed-event");
        assert_eq!(d.net_peers_recent_for_nodes(0, &allowed, 300).unwrap()[0]["proc"], "allowed-proc");
        assert_eq!(d.net_login_counts_for_nodes(10_000, &allowed, 500).unwrap()[0]["node_id"], allowed_id);
        assert!(d.net_events_for_nodes(&[], 300).unwrap().is_empty());
        assert!(d.net_peers_recent_for_nodes(0, &[], 300).unwrap().is_empty());
        assert!(d.net_login_counts_for_nodes(10_000, &[], 500).unwrap().is_empty());
    }
}
