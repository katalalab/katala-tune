//! 出どころの台帳（tune-core だけの表）。どの数字が、どの機体のどのファイルのどの区間から、どの版の調査スクリプトで、いつ取り込まれたかを辿れるようにする。
//! - ingest_run: 取り込み 1 回 = 1 行。調査スクリプトの名前・版・SHA-256、Tune の版、開始・終了、区間の数と行数。
//!   前の行のハッシュとつなぐ（連鎖。[`Store::verify_chain`]）
//! - source_span: 読んだファイルの区間。開始・終了のバイト位置、区間の SHA-256、行数・取り込んだ行数・飛ばした行数、
//!   終わりの手前の数 KB の SHA-256（次の続きの検算に使う）、元ファイルと照合した結果
//!
//! 決まりごと:
//! - 生きている区間の (機体, ファイル, 開始, 終了) は一意（部分インデックス）。同じ区間をもう一度取り込もうとすると拒否する
//! - 続き（add）の区間は「前回の終了 = 今回の開始」でなければ拒否する（[`insert_span`] に continuation を渡す）
//! - ファイルを最初から読み直したときは、前の区間を消さずに「置き換え済み」（superseded_by）にする。連鎖の検算が崩れないように
//!
//! AI エージェントのセッションの取り込みで使う。道具の棚卸しでも、取り込み元ごとの「区間」
//! （file = "inventory:<取り込み元>"、0〜出力の長さ、出力の SHA-256）として同じ形で使える。
//!
//! 連鎖は、行の書き換え・削除に気づくためのもの。DB を丸ごと作り直せる相手（ハッシュも計算し直せる）には効かない。

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Result, Store, row_json};
use crate::js;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ingest_run (
  id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL UNIQUE, kind TEXT NOT NULL, node_id TEXT NOT NULL,
  probe TEXT NOT NULL, probe_version TEXT, probe_sha256 TEXT NOT NULL, tune_version TEXT NOT NULL,
  started_at INTEGER NOT NULL, finished_at INTEGER NOT NULL,
  spans INTEGER NOT NULL DEFAULT 0, rejected INTEGER NOT NULL DEFAULT 0, bytes INTEGER NOT NULL DEFAULT 0,
  lines INTEGER NOT NULL DEFAULT 0, used INTEGER NOT NULL DEFAULT 0, skipped INTEGER NOT NULL DEFAULT 0, notes TEXT,
  prev_hash TEXT NOT NULL, hash TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS ingest_run_node ON ingest_run (node_id, kind, id);
CREATE TABLE IF NOT EXISTS source_span (
  id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL, node_id TEXT NOT NULL, file TEXT NOT NULL,
  byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL, sha256 TEXT NOT NULL,
  lines INTEGER NOT NULL DEFAULT 0, used INTEGER NOT NULL DEFAULT 0, skipped INTEGER NOT NULL DEFAULT 0,
  anchor_len INTEGER, anchor_sha256 TEXT, responses INTEGER NOT NULL DEFAULT 0, tok_out INTEGER NOT NULL DEFAULT 0,
  superseded_by TEXT, verify_state TEXT, verified_at INTEGER, verify_note TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS source_span_live ON source_span (node_id, file, byte_start, byte_end) WHERE superseded_by IS NULL;
CREATE INDEX IF NOT EXISTS source_span_end ON source_span (node_id, file, byte_end);
CREATE INDEX IF NOT EXISTS source_span_run ON source_span (run_id);
";

/// 連鎖の最初の行の prev_hash
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// 取り込み 1 回の見出し
#[derive(Clone, Debug, PartialEq)]
pub struct RunMeta {
    pub run_id: String,
    pub kind: String,
    pub node_id: String,
    /// 調査スクリプトの名前（probes/ のファイル名）
    pub probe: String,
    pub probe_version: Option<String>,
    /// 調査スクリプト（続きの位置を埋め込む前のひな形）の SHA-256
    pub probe_sha256: String,
    pub tune_version: String,
    pub started_at: i64,
    pub finished_at: i64,
}

/// 読んだ区間（調査の出力から）
#[derive(Clone, Debug, PartialEq)]
pub struct SpanIn {
    pub file: String,
    pub start: i64,
    pub end: i64,
    pub sha256: String,
    pub lines: i64,
    pub used: i64,
    pub skipped: i64,
    /// 終わりの手前の (長さ, SHA-256)
    pub anchor: Option<(i64, String)>,
}

impl SpanIn {
    /// 調査の span（{ start, end, sha256, lines, used, skipped, anchor: [長さ, sha] }）。形がおかしければ None
    pub fn from_probe(file: &str, v: Option<&Value>) -> Option<SpanIn> {
        let v = v?.as_object()?;
        let int = |k: &str| v.get(k).and_then(Value::as_i64).filter(|x| *x >= 0);
        let sha = v.get("sha256").and_then(Value::as_str).filter(|s| is_hex(s, 64))?.to_string();
        let (start, end) = (int("start")?, int("end")?);
        if end < start || file.is_empty() {
            return None;
        }
        let anchor = v.get("anchor").and_then(Value::as_array).and_then(|a| {
            let n = a.first()?.as_i64().filter(|n| *n >= 0 && *n <= end)?;
            let s = a.get(1)?.as_str().filter(|s| is_hex(s, 64))?;
            Some((n, s.to_string()))
        });
        Some(SpanIn {
            file: file.to_string(),
            start,
            end,
            sha256: sha,
            lines: int("lines").unwrap_or(0),
            used: int("used").unwrap_or(0),
            skipped: int("skipped").unwrap_or(0),
            anchor,
        })
    }
}

/// 区間を書いた結果
#[derive(Clone, Debug, PartialEq)]
pub enum SpanOutcome {
    Inserted(i64),
    /// 拒否した理由（前回の終了と合わない・同じ区間がすでにある）
    Rejected(String),
}

/// 生きている区間の、いちばん後ろの終了位置（無ければ None）
pub(crate) fn last_end(c: &Connection, node_id: &str, file: &str) -> Result<Option<i64>> {
    c.prepare_cached("SELECT max(byte_end) FROM source_span WHERE node_id = ? AND file = ? AND superseded_by IS NULL")?
        .query_row(params![node_id, file], |r| r.get::<_, Option<i64>>(0))
}

/// ファイルを最初から読み直すとき、生きている区間を「置き換え済み」にする。置き換えた区間の id を返す
pub(crate) fn supersede(c: &Connection, node_id: &str, file: &str, run_id: &str) -> Result<Vec<i64>> {
    let ids: Vec<i64> = c
        .prepare_cached("SELECT id FROM source_span WHERE node_id = ? AND file = ? AND superseded_by IS NULL")?
        .query_map(params![node_id, file], |r| r.get(0))?
        .collect::<Result<_>>()?;
    c.prepare_cached("UPDATE source_span SET superseded_by = ? WHERE node_id = ? AND file = ? AND superseded_by IS NULL")?
        .execute(params![run_id, node_id, file])?;
    Ok(ids)
}

/// 区間を書く。continuation（続きの区間）なら、生きている区間の最後の終了 = 今回の開始 でなければ拒否する。
/// 同じ (機体, ファイル, 開始, 終了) の生きている区間があれば拒否する（二重の取り込み）
pub(crate) fn insert_span(c: &Connection, run_id: &str, node_id: &str, s: &SpanIn, continuation: bool) -> Result<SpanOutcome> {
    if continuation {
        let last = last_end(c, node_id, &s.file)?;
        if last != Some(s.start) {
            return Ok(SpanOutcome::Rejected(format!(
                "続きの位置が合わない（前回の終了 {}・今回の開始 {}）",
                last.map_or_else(|| "なし".into(), |x| x.to_string()),
                s.start
            )));
        }
    } else if s.start != 0 {
        return Ok(SpanOutcome::Rejected(format!("最初からの読み直しなのに開始が 0 でない（{}）", s.start)));
    }
    let r = c
        .prepare_cached(
            "INSERT INTO source_span (run_id, node_id, file, byte_start, byte_end, sha256, lines, used, skipped, anchor_len, anchor_sha256)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )?
        .execute(params![
            run_id,
            node_id,
            s.file,
            s.start,
            s.end,
            s.sha256,
            s.lines,
            s.used,
            s.skipped,
            s.anchor.as_ref().map(|a| a.0),
            s.anchor.as_ref().map(|a| a.1.clone())
        ]);
    match r {
        Ok(_) => Ok(SpanOutcome::Inserted(c.last_insert_rowid())),
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
            Ok(SpanOutcome::Rejected(format!("同じ区間（{}〜{}）がすでに取り込まれている", s.start, s.end)))
        }
        Err(e) => Err(e),
    }
}

/// 区間から足した量（検算に使う。応答の数と出力トークン）
pub(crate) fn set_span_counts(c: &Connection, span_id: i64, responses: i64, tok_out: i64) -> Result<()> {
    c.prepare_cached("UPDATE source_span SET responses = ?, tok_out = ? WHERE id = ?")?.execute(params![responses, tok_out, span_id])?;
    Ok(())
}

/// 連鎖のハッシュに入れる、1 回分の中身（行の値と区間の指紋）。JSON の配列で並べるので、区切りの曖昧さが無い
fn run_payload(run: &Value, spans: &[Value]) -> String {
    let g = |k: &str| run.get(k).cloned().unwrap_or(Value::Null);
    let head = json!([
        g("run_id"),
        g("kind"),
        g("node_id"),
        g("probe"),
        g("probe_version"),
        g("probe_sha256"),
        g("tune_version"),
        g("started_at"),
        g("finished_at"),
        g("spans"),
        g("rejected"),
        g("bytes"),
        g("lines"),
        g("used"),
        g("skipped"),
        g("notes")
    ]);
    let mut s = head.to_string();
    for sp in spans {
        let f = |k: &str| sp.get(k).cloned().unwrap_or(Value::Null);
        s.push('\n');
        s.push_str(&json!([f("file"), f("byte_start"), f("byte_end"), f("sha256"), f("lines"), f("used"), f("skipped")]).to_string());
    }
    s
}

fn chain_hash(prev: &str, run: &Value, spans: &[Value]) -> String {
    sha256_hex(format!("{prev}\n{}", run_payload(run, spans)).as_bytes())
}

const SPAN_COLS: &str = "run_id, file, byte_start, byte_end, sha256, lines, used, skipped";

/// 取り込み 1 回を締める: この回の区間を集計して ingest_run に 1 行書き、前の行のハッシュとつなぐ。ハッシュを返す
pub(crate) fn finish_run(c: &Connection, m: &RunMeta, rejected: i64, notes: Option<&Value>) -> Result<String> {
    let spans: Vec<Value> = c
        .prepare_cached(&format!("SELECT {SPAN_COLS} FROM source_span WHERE run_id = ? ORDER BY id"))?
        .query_map([&m.run_id], row_json)?
        .collect::<Result<_>>()?;
    let sum = |k: &str| spans.iter().map(|s| s.get(k).and_then(Value::as_i64).unwrap_or(0)).sum::<i64>();
    let bytes: i64 =
        spans.iter().map(|s| s.get("byte_end").and_then(Value::as_i64).unwrap_or(0) - s.get("byte_start").and_then(Value::as_i64).unwrap_or(0)).sum();
    let notes = notes.filter(|n| !n.is_null()).map(Value::to_string);
    let run = json!({
        "run_id": m.run_id, "kind": m.kind, "node_id": m.node_id, "probe": m.probe, "probe_version": m.probe_version, "probe_sha256": m.probe_sha256,
        "tune_version": m.tune_version, "started_at": m.started_at, "finished_at": m.finished_at, "spans": spans.len() as i64, "rejected": rejected,
        "bytes": bytes, "lines": sum("lines"), "used": sum("used"), "skipped": sum("skipped"), "notes": notes,
    });
    let prev: String = c.query_row("SELECT hash FROM ingest_run ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).optional()?.unwrap_or_else(|| GENESIS.to_string());
    let hash = chain_hash(&prev, &run, &spans);
    c.prepare_cached(
        "INSERT INTO ingest_run (run_id, kind, node_id, probe, probe_version, probe_sha256, tune_version, started_at, finished_at,
           spans, rejected, bytes, lines, used, skipped, notes, prev_hash, hash) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        m.run_id,
        m.kind,
        m.node_id,
        m.probe,
        m.probe_version,
        m.probe_sha256,
        m.tune_version,
        m.started_at,
        m.finished_at,
        spans.len() as i64,
        rejected,
        bytes,
        sum("lines"),
        sum("used"),
        sum("skipped"),
        notes,
        prev,
        hash
    ])?;
    Ok(hash)
}

impl Store {
    /// 続きの検算に使う、区間の終わりの手前の指紋（生きている区間で、終了がちょうど end のもの）
    pub fn span_anchor(&self, node_id: &str, file: &str, end: i64) -> Result<Option<(i64, String)>> {
        self.conn
            .prepare_cached(
                "SELECT anchor_len, anchor_sha256 FROM source_span WHERE node_id = ? AND file = ? AND byte_end = ? AND superseded_by IS NULL
                 AND anchor_len IS NOT NULL AND anchor_sha256 IS NOT NULL ORDER BY id DESC LIMIT 1",
            )?
            .query_row(params![node_id, file, end], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
    }

    /// 連鎖を最初からたどって検算する。{ runs, ok, head, broken: { run_id, id, reason } | null }。
    /// 行の書き換え・削除・入れ替えに気づく。DB を丸ごと作り直せる相手には効かない（ハッシュも計算し直せるため）
    pub fn verify_chain(&self) -> Result<Value> {
        let runs: Vec<Value> = self
            .conn
            .prepare("SELECT id, run_id, kind, node_id, probe, probe_version, probe_sha256, tune_version, started_at, finished_at, spans, rejected, bytes, lines, used, skipped, notes, prev_hash, hash FROM ingest_run ORDER BY id")?
            .query_map([], row_json)?
            .collect::<Result<_>>()?;
        let mut by_run: HashMap<String, Vec<Value>> = HashMap::new();
        let mut st = self.conn.prepare(&format!("SELECT {SPAN_COLS} FROM source_span ORDER BY id"))?;
        for s in st.query_map([], row_json)? {
            let s = s?;
            by_run.entry(js::string(s.get("run_id"))).or_default().push(s);
        }
        let mut prev = GENESIS.to_string();
        let empty = Vec::new();
        for r in &runs {
            let run_id = js::string(r.get("run_id"));
            let broken = |reason: &str| json!({ "runs": runs.len(), "ok": false, "head": Value::Null, "broken": { "run_id": run_id, "id": r.get("id"), "reason": reason } });
            if js::string(r.get("prev_hash")) != prev {
                return Ok(broken("前の行のハッシュと合わない（行が抜けた・入れ替わった）"));
            }
            let h = chain_hash(&prev, r, by_run.get(&run_id).unwrap_or(&empty));
            if js::string(r.get("hash")) != h {
                return Ok(broken("この行か、この回の区間が書き換えられている"));
            }
            prev = h;
        }
        Ok(json!({ "runs": runs.len(), "ok": true, "head": if runs.is_empty() { Value::Null } else { Value::from(prev) }, "broken": Value::Null }))
    }

    /// 元ファイルと照合した結果を残す。state: ok（検証済み）・mismatch（不一致）・gone（元ファイルなし）
    pub fn record_span_verify(&self, span_id: i64, state: &str, note: Option<&str>, now: i64) -> Result<()> {
        self.conn.execute("UPDATE source_span SET verify_state = ?, verified_at = ?, verify_note = ? WHERE id = ?", params![state, now, note, span_id])?;
        Ok(())
    }

    /// 1 ファイルの区間（新しい順。置き換え済みも含む）と、それぞれの取り込みの回
    pub fn spans_of(&self, node_id: &str, file: &str, limit: i64) -> Result<Vec<Value>> {
        self.conn
            .prepare_cached(
                "SELECT s.id, s.run_id, s.file, s.byte_start, s.byte_end, s.sha256, s.lines, s.used, s.skipped, s.anchor_len, s.anchor_sha256,
                        s.responses, s.tok_out, s.superseded_by, s.verify_state, s.verified_at, s.verify_note,
                        (SELECT count(*) FROM ai_responses a WHERE a.span_id = s.id) AS responses_now,
                        r.id AS run_seq, r.probe, r.probe_version, r.probe_sha256, r.tune_version, r.started_at, r.finished_at, r.hash AS run_hash
                 FROM source_span s LEFT JOIN ingest_run r ON r.run_id = s.run_id
                 WHERE s.node_id = ? AND s.file = ? ORDER BY s.id DESC LIMIT ?",
            )?
            .query_map(params![node_id, file, limit.clamp(1, 500)], row_json)?
            .collect()
    }

    /// 台帳の数（画面の「出どころ」）。機体ごとの最後の取り込みの回と、区間の状態の内訳
    pub fn provenance_counts(&self) -> Result<Value> {
        let one = |sql: &str| -> Result<i64> { self.conn.query_row(sql, [], |r| r.get(0)) };
        let states: Vec<Value> = self
            .conn
            .prepare("SELECT coalesce(verify_state, 'unverified') AS state, count(*) AS n FROM source_span WHERE superseded_by IS NULL GROUP BY 1")?
            .query_map([], row_json)?
            .collect::<Result<_>>()?;
        let last: Vec<Value> = self
            .conn
            .prepare(
                "SELECT r.node_id, r.kind, r.run_id, r.probe, r.probe_version, r.probe_sha256, r.tune_version, r.started_at, r.finished_at, r.spans, r.rejected, r.bytes, r.notes
                 FROM ingest_run r JOIN (SELECT node_id, kind, max(id) AS id FROM ingest_run GROUP BY node_id, kind) l ON l.id = r.id ORDER BY r.node_id, r.kind",
            )?
            .query_map([], row_json)?
            .collect::<Result<_>>()?;
        Ok(json!({
            "runs": one("SELECT count(*) FROM ingest_run")?,
            "spans": one("SELECT count(*) FROM source_span WHERE superseded_by IS NULL")?,
            "superseded": one("SELECT count(*) FROM source_span WHERE superseded_by IS NOT NULL")?,
            "rejected": one("SELECT coalesce(sum(rejected), 0) FROM ingest_run")?,
            "states": states,
            "last": last,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(file: &str, start: i64, end: i64) -> SpanIn {
        SpanIn {
            file: file.into(),
            start,
            end,
            sha256: sha256_hex(format!("{file}{start}{end}").as_bytes()),
            lines: 2,
            used: 1,
            skipped: 0,
            anchor: Some((end.min(4), sha256_hex(b"x"))),
        }
    }

    fn meta(run_id: &str) -> RunMeta {
        RunMeta {
            run_id: run_id.into(),
            kind: "ai_sessions".into(),
            node_id: "n1".into(),
            probe: "ai_sessions.py".into(),
            probe_version: Some("v1".into()),
            probe_sha256: sha256_hex(b"probe"),
            tune_version: "0.0.0".into(),
            started_at: 1,
            finished_at: 2,
        }
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn span_from_probe_validates_shape() {
        let ok = json!({ "start": 0, "end": 10, "sha256": sha256_hex(b"a"), "lines": 1, "used": 1, "skipped": 0, "anchor": [10, sha256_hex(b"b")] });
        let s = SpanIn::from_probe("claude:a.jsonl", Some(&ok)).unwrap();
        assert_eq!((s.start, s.end, s.anchor.as_ref().map(|a| a.0)), (0, 10, Some(10)));
        for bad in [
            json!({ "start": 5, "end": 1, "sha256": sha256_hex(b"a") }),
            json!({ "start": 0, "end": 1, "sha256": "zz" }),
            json!({ "start": -1, "end": 1, "sha256": sha256_hex(b"a") }),
        ] {
            assert!(SpanIn::from_probe("f", Some(&bad)).is_none(), "{bad}");
        }
        // 手前の指紋が区間より長い・形が違うときは指紋なし（続きは最初から読み直しになる）
        let long = json!({ "start": 0, "end": 3, "sha256": sha256_hex(b"a"), "anchor": [10, sha256_hex(b"b")] });
        assert_eq!(SpanIn::from_probe("f", Some(&long)).unwrap().anchor, None);
    }

    #[test]
    fn continuation_and_duplicates_are_refused() {
        let db = Store::open_in_memory().unwrap();
        let c = db.conn();
        assert!(matches!(insert_span(c, "r1", "n1", &span("f", 0, 100), false).unwrap(), SpanOutcome::Inserted(_)));
        // 続き: 前回の終了 = 今回の開始
        assert!(matches!(insert_span(c, "r2", "n1", &span("f", 100, 150), true).unwrap(), SpanOutcome::Inserted(_)));
        let SpanOutcome::Rejected(why) = insert_span(c, "r3", "n1", &span("f", 120, 200), true).unwrap() else { panic!() };
        assert!(why.contains("前回の終了 150") && why.contains("今回の開始 120"), "{why}");
        // 同じ区間をもう一度（続きではない形で）→ 二重の取り込み
        let SpanOutcome::Rejected(why) = insert_span(c, "r3", "n1", &span("f", 0, 100), false).unwrap() else { panic!() };
        assert!(why.contains("すでに取り込まれている"), "{why}");
        // 最初から以外の置き換えは拒否
        assert!(matches!(insert_span(c, "r3", "n1", &span("g", 5, 10), false).unwrap(), SpanOutcome::Rejected(_)));
        // 初めてのファイルの続きは拒否（前回の終了なし）
        assert!(matches!(insert_span(c, "r3", "n1", &span("g", 5, 10), true).unwrap(), SpanOutcome::Rejected(_)));
        // 置き換え済みにすれば、同じ区間を取り込み直せる（前の区間は履歴として残る）
        assert_eq!(supersede(c, "n1", "f", "r4").unwrap().len(), 2);
        assert!(matches!(insert_span(c, "r4", "n1", &span("f", 0, 100), false).unwrap(), SpanOutcome::Inserted(_)));
        assert_eq!(last_end(c, "n1", "f").unwrap(), Some(100));
        assert_eq!(db.spans_of("n1", "f", 50).unwrap().len(), 3, "置き換え済みの 2 つと、新しい 1 つ");
        assert_eq!(db.span_anchor("n1", "f", 100).unwrap().map(|a| a.0), Some(4));
        assert_eq!(db.span_anchor("n1", "f", 150).unwrap(), None, "置き換え済みの区間の指紋は使わない");
    }

    #[test]
    fn chain_detects_edits_and_deletions() {
        let db = Store::open_in_memory().unwrap();
        for (i, run) in ["r1", "r2", "r3"].iter().enumerate() {
            let s = i as i64 * 10;
            db.tx(|c| {
                insert_span(c, run, "n1", &span("f", s, s + 10), i > 0)?;
                finish_run(c, &meta(run), 0, None)
            })
            .unwrap();
        }
        let v = db.verify_chain().unwrap();
        assert_eq!((v["ok"].clone(), v["runs"].clone()), (json!(true), json!(3)));
        // 区間の指紋を書き換えた → その回で途切れる
        db.conn().execute("UPDATE source_span SET sha256 = ? WHERE run_id = 'r2'", [sha256_hex(b"forged")]).unwrap();
        let v = db.verify_chain().unwrap();
        assert_eq!((v["ok"].clone(), v["broken"]["run_id"].clone()), (json!(false), json!("r2")));
        db.conn().execute("UPDATE source_span SET sha256 = ? WHERE run_id = 'r2'", [sha256_hex(b"f1020")]).unwrap();
        assert_eq!(db.verify_chain().unwrap()["ok"], json!(true));
        // 行を抜いた → 次の行の prev_hash が合わない
        db.conn().execute("DELETE FROM ingest_run WHERE run_id = 'r2'", []).unwrap();
        let v = db.verify_chain().unwrap();
        assert_eq!(v["broken"]["run_id"], json!("r3"));
        assert!(js::string(v["broken"].get("reason")).contains("前の行"));
    }

    #[test]
    fn verify_results_and_counts() {
        let db = Store::open_in_memory().unwrap();
        let id = db
            .tx(|c| {
                let SpanOutcome::Inserted(id) = insert_span(c, "r1", "n1", &span("f", 0, 10), false)? else { panic!() };
                finish_run(c, &meta("r1"), 1, Some(&json!({ "rejected": [{ "file": "g", "reason": "x" }] })))?;
                Ok(id)
            })
            .unwrap();
        db.record_span_verify(id, "ok", None, 5).unwrap();
        let p = db.provenance_counts().unwrap();
        assert_eq!((p["runs"].clone(), p["spans"].clone(), p["rejected"].clone()), (json!(1), json!(1), json!(1)));
        assert_eq!(p["states"][0], json!({ "state": "ok", "n": 1 }));
        assert_eq!(p["last"][0]["probe_version"], json!("v1"));
        let s = &db.spans_of("n1", "f", 10).unwrap()[0];
        assert_eq!((s["verify_state"].clone(), s["probe"].clone(), s["run_seq"].clone()), (json!("ok"), json!("ai_sessions.py"), json!(1)));
    }
}
