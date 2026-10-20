//! 常時監視（分ごとの集計）・ハブとの同期・ダッシュボードの集計。
//!
//! - 常時監視: アプリが開いているあいだ（メニューバー・通知領域に居るあいだも）ライブのサンプラーを流し続け、
//!   終わった 1 分ごとに平均と最大を `metrics_minute` に残す（14 日）。画面が見ているときは同じ流れを 1 秒ごとに描く
//! - ハブ: Mac・Windows のどの操作卓でも同じ数字を見るため、1 台（ハブ）だけが集め、ほかの操作卓は 1 分ごとに
//!   ハブの集計と分析結果を SSH で写す（`tune export`）。ハブの数字が 5 分より古ければ、写す側が自分で集める
//! - 詳細（分析・ログ）は 1 時間ごと（engine の既定）。ハブの分析結果が新しいあいだ、写す側は分析を重ねない

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::collect;
use crate::db::metrics::MinuteRow;
use crate::db::{Store, now_ms};
use crate::engine::Engine;
use crate::live::{Live, Opts, Sample};
use crate::monitor_journal::{Category, Data, Kind, Policy};
use crate::nodes::{Config, Node};

pub const MINUTE: i64 = 60_000;
pub const KEEP_DAYS: i64 = 14;
/// ハブの一番新しい分がハブの時計でこれより古ければ、ハブは集めていないとみなす
pub const HUB_STALE_MS: i64 = 5 * MINUTE;
/// つなぎ直しても続かなかった機体（電源が落ちている・オフライン）を次に試すまで
pub const RETRY_DISCONNECTED_MS: i64 = 5 * MINUTE;
/// 1 回に写す範囲（1 ページ）。残りは `next` から続けて写す（保持している 14 日ぶんを欠かさない）
pub const EXPORT_MAX_MS: i64 = 24 * 60 * MINUTE;
/// 1 回の写しで続けて読むページの上限（14 日ぶん＋余裕）
pub const PULL_MAX_PAGES: usize = 16;
/// ハブの分析結果がハブの時計でこれより古ければ、写す側も自分で分析する（分析は 1 時間ごと）
pub const HUB_ANALYSIS_STALE_MS: i64 = 2 * 60 * MINUTE;
/// 集計に使う値（Sample::point のキー）
const KEYS: [&str; 15] =
    ["cpu", "mem", "swap", "commit", "dr", "dw", "rx", "tx", "power_cpu_w", "power_soc_w", "power_platform_w", "power_gpu_w", "gpu", "gmem", "gtemp"];

fn metric_keys() -> Vec<String> {
    let mut keys: Vec<String> = KEYS.iter().map(|s| (*s).to_string()).collect();
    for id in 0..crate::live::MAX_GPUS {
        for suffix in ["used_mib", "total_mib", "free_mib", "pct"] {
            keys.push(format!("gpu_{id}_vram_{suffix}"));
        }
    }
    keys
}

// ---------------------------------------------------------------------------
// 設定（meta の monitor）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    /// 常時監視（このアプリで集める）
    pub enabled: bool,
    /// 写す元の操作卓（台帳の id）。None なら自分で集める
    pub hub: Option<String>,
}

impl Settings {
    pub fn json(&self) -> Value {
        json!({ "enabled": self.enabled, "hub": self.hub })
    }
}

pub fn settings(db: &Store) -> Settings {
    checked_settings(db).unwrap_or_default()
}

fn checked_settings(db: &Store) -> rusqlite::Result<Settings> {
    let v = db.get_meta("monitor")?.unwrap_or(Value::Null);
    Ok(Settings {
        enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        hub: v.get("hub").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
    })
}

/// 変える。hub は台帳にあり、この機体でないものだけ（"" か null で外す）
pub fn set_settings(db: &Store, cfg: &Config, patch: &Value) -> Result<Settings, String> {
    let mut s = settings(db);
    if let Some(b) = patch.get("enabled").and_then(Value::as_bool) {
        s.enabled = b;
    }
    match patch.get("hub") {
        Some(Value::Null) => s.hub = None,
        Some(Value::String(h)) if h.is_empty() => s.hub = None,
        Some(Value::String(h)) => {
            let n = cfg.node(h).ok_or_else(|| format!("台帳に無い機体: {h}"))?;
            if let Some(err) = n.refusal() {
                return Err(err);
            }
            if n.local {
                return Err("この機体はハブにできない（自分から写すことになる）".into());
            }
            if !(n.is_mac() || n.is_windows()) {
                return Err(format!("この OS（{}）の操作卓からは写せない", n.os));
            }
            s.hub = Some(h.clone());
        }
        Some(_) => return Err("hub は機体の id か null".into()),
        None => {}
    }
    db.set_meta("monitor", &s.json()).map_err(|e| e.to_string())?;
    Ok(s)
}

// ---------------------------------------------------------------------------
// 集計
// ---------------------------------------------------------------------------

fn round(x: f64, d: i32) -> f64 {
    let m = 10f64.powi(d);
    (x * m).round() / m
}

/// 1 機体の点から、[from, until) に入る終わった分ごとの集計（項目ごとに [平均, 最大]）。
/// procs（最新の上位プロセス）は一番新しい分にだけ付ける
pub fn rollup(node_id: &str, samples: &[Sample], procs: Option<&Value>, from: i64, until: i64, src: &str) -> Vec<MinuteRow> {
    let mut by_minute: BTreeMap<i64, Vec<Value>> = BTreeMap::new();
    for s in samples {
        let m = s.t.div_euclid(MINUTE) * MINUTE;
        if m >= from && m + MINUTE <= until {
            by_minute.entry(m).or_default().push(s.point());
        }
    }
    let last = by_minute.keys().next_back().copied();
    let keys = metric_keys();
    by_minute
        .into_iter()
        .map(|(minute, pts)| {
            let mut v = Map::new();
            for k in &keys {
                let xs: Vec<f64> = pts.iter().filter_map(|p| p.get(k).and_then(Value::as_f64)).collect();
                if !xs.is_empty() {
                    let avg = xs.iter().sum::<f64>() / xs.len() as f64;
                    let max = xs.iter().copied().fold(f64::MIN, f64::max);
                    v.insert(k.clone(), json!([round(avg, 2), round(max, 2)]));
                }
            }
            if Some(minute) == last
                && let Some(p) = procs
            {
                let top = |k: &str, unit: &str| {
                    p.get(k)
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .map(|x| json!([x.get("name").cloned().unwrap_or(Value::Null), x.get(unit).cloned().unwrap_or(Value::Null)]))
                };
                if let Some(t) = top("top_cpu", "cpu") {
                    v.insert("top_cpu".into(), t);
                }
                if let Some(t) = top("top_mem", "mem_mb") {
                    v.insert("top_mem".into(), t);
                }
            }
            MinuteRow { node_id: node_id.to_string(), minute, n: pts.len() as i64, v: Value::Object(v), src: Some(src.to_string()) }
        })
        .collect()
}

/// 期間の行を max 個までの点にまとめる（平均は平均、最大は最大）。{ t: [...], cpu: [...], cpu_max: [...], ... }
pub fn series(rows: &[MinuteRow], since: i64, until: i64, max: usize) -> Value {
    let span = (until - since).max(MINUTE);
    let buckets = max.max(1) as i64;
    let width = (span / buckets).max(MINUTE);
    let keys = metric_keys();
    let mut acc: BTreeMap<i64, HashMap<&str, (f64, usize, f64)>> = BTreeMap::new();
    for r in rows {
        let b = since + (r.minute - since).div_euclid(width) * width;
        let e = acc.entry(b).or_default();
        for k in keys.iter().map(String::as_str) {
            if let Some([a, m]) = r.v.get(k).and_then(Value::as_array).and_then(|x| x.iter().map(Value::as_f64).collect::<Option<Vec<_>>>()).as_deref() {
                let x = e.entry(k).or_insert((0.0, 0, f64::MIN));
                x.0 += a;
                x.1 += 1;
                x.2 = x.2.max(*m);
            }
        }
    }
    let mut out = Map::new();
    out.insert("t".into(), json!(acc.keys().collect::<Vec<_>>()));
    for k in keys.iter().map(String::as_str) {
        if acc.values().any(|e| e.contains_key(k)) {
            out.insert(k.into(), json!(acc.values().map(|e| e.get(k).map(|(s, n, _)| round(s / *n as f64, 2))).collect::<Vec<_>>()));
            out.insert(format!("{k}_max"), json!(acc.values().map(|e| e.get(k).map(|x| round(x.2, 2))).collect::<Vec<_>>()));
        }
    }
    Value::Object(out)
}

/// 直近の行から出す注意（MDM の「対応が要るもの」）
pub fn alerts(rows: &[MinuteRow], now: i64, monitoring: bool) -> Vec<Value> {
    let recent: Vec<&MinuteRow> = rows.iter().filter(|r| r.minute >= now - 6 * MINUTE).collect();
    let mut out = Vec::new();
    let latest = rows.iter().map(|r| r.minute).max();
    if monitoring && latest.is_none_or(|m| now - m > 4 * MINUTE) {
        let ago = latest.map(|m| (now - m) / MINUTE);
        out.push(json!({ "id": "stale", "severity": "warn", "title": "数字が届いていない", "detail": ago.map_or("まだ 1 分も集計していない".to_string(), |a| format!("最後の集計は {a} 分前")) }));
    }
    let avg = |k: &str| {
        let xs: Vec<f64> = recent.iter().filter_map(|r| r.v.get(k).and_then(|x| x.get(0)).and_then(Value::as_f64)).collect();
        (xs.len() >= 3).then(|| xs.iter().sum::<f64>() / xs.len() as f64)
    };
    let max = |k: &str| recent.iter().filter_map(|r| r.v.get(k).and_then(|x| x.get(1)).and_then(Value::as_f64)).reduce(f64::max);
    if let Some(c) = avg("cpu").filter(|c| *c >= 90.0) {
        out.push(json!({ "id": "cpu", "severity": "warn", "title": "CPU の高負荷が続いている", "detail": format!("直近の平均 {c:.0}%") }));
    }
    if let Some(m) = avg("mem").filter(|m| *m >= 92.0) {
        out.push(json!({ "id": "mem", "severity": "warn", "title": "メモリが足りない", "detail": format!("直近の平均 {m:.0}%") }));
    }
    if let Some(c) = max("commit").filter(|c| *c >= 90.0) {
        out.push(json!({ "id": "commit", "severity": "critical", "title": "コミット（仮想メモリ）が上限に近い", "detail": format!("最大 {c:.0}%") }));
    }
    if let Some(t) = max("gtemp").filter(|t| *t >= 85.0) {
        out.push(json!({ "id": "gtemp", "severity": "warn", "title": "GPU の温度が高い", "detail": format!("最大 {t:.0}℃") }));
    }
    out
}

// ---------------------------------------------------------------------------
// ハブとの同期
// ---------------------------------------------------------------------------

/// この操作卓の集計と最新の分析結果を写しの形で（`tune export`）
/// 1 回は [since, since + 24 時間) まで。続きがあれば `more: true` と次の起点 `next` を返す（写す側は next から続ける）
pub fn export(db: &Store, since: i64, host: &str) -> Result<Value, String> {
    let e = |x: rusqlite::Error| x.to_string();
    let now = now_ms();
    let oldest = db.oldest_metric_minute().map_err(e)?.unwrap_or(now);
    let since = since.max(now - KEEP_DAYS * 24 * 60 * MINUTE).max(oldest.min(now));
    let end = since.saturating_add(EXPORT_MAX_MS);
    let more = end < now;
    let rows = db.metrics(None, since, if more { end } else { i64::MAX }).map_err(e)?;
    let snaps = db.snapshots_since(since).map_err(e)?;
    let latest: Map<String, Value> = db.latest_metric_minutes().map_err(e)?.into_iter().map(|(k, v)| (k, json!(v))).collect();
    Ok(json!({
        "v": 1, "host": host, "at": now, "since": since, "next": if more { end } else { now }, "more": more,
        "monitor": settings(db).json(), "latest": latest, "latest_snapshot": db.latest_snapshot_at().map_err(e)?,
        "metrics": rows.iter().map(MinuteRow::json).collect::<Vec<_>>(), "snapshots": snaps,
    }))
}

/// 写しを取り込む。戻り値は数とハブの新しさ
pub fn import(db: &Store, x: &Value) -> Result<Value, String> {
    if x.get("v").and_then(Value::as_i64) != Some(1) {
        return Err("写しの形が違う（v が 1 でない）".into());
    }
    let host = x.get("host").and_then(Value::as_str).unwrap_or("hub").to_string();
    let at = x.get("at").and_then(Value::as_i64).ok_or("写しに at が無い")?;
    let rows: Vec<MinuteRow> = x
        .get("metrics")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(MinuteRow::from_json)
                .map(|mut r| {
                    r.src.get_or_insert_with(|| host.clone());
                    r
                })
                .collect()
        })
        .unwrap_or_default();
    db.add_metrics(&rows).map_err(|e| e.to_string())?;
    let mut snaps = 0;
    for s in x.get("snapshots").and_then(Value::as_array).into_iter().flatten() {
        let (Some(node), Some(t)) = (s.get("node_id").and_then(Value::as_str), s.get("at").and_then(Value::as_i64)) else { continue };
        let have = db.last_snapshots(node, 1).map_err(|e| e.to_string())?.first().map(|x| x.at);
        if have.is_some_and(|h| h >= t) {
            continue;
        }
        let data = s.get("data").cloned().unwrap_or(Value::Null);
        db.add_snapshot(
            node,
            t,
            s.get("wall_s").and_then(Value::as_f64),
            s.get("score").and_then(Value::as_i64),
            s.get("findings").unwrap_or(&json!([])),
            &data,
            s.get("summary").unwrap_or(&json!({})),
        )
        .map_err(|e| e.to_string())?;
        snaps += 1;
    }
    let freshest = x.get("latest").and_then(Value::as_object).and_then(|m| m.values().filter_map(Value::as_i64).max());
    let analysis = x.get("latest_snapshot").and_then(Value::as_i64);
    Ok(json!({
        "rows": rows.len(), "snapshots": snaps, "hub_at": at, "freshest_minute": freshest,
        "fresh": freshest.is_some_and(|f| at - f <= HUB_STALE_MS + MINUTE),
        "latest_snapshot": analysis, "snapshot_fresh": analysis.is_some_and(|s| at - s <= HUB_ANALYSIS_STALE_MS),
        "more": x.get("more").and_then(Value::as_bool).unwrap_or(false),
        "next": x.get("next").and_then(Value::as_i64).unwrap_or(at),
    }))
}

/// ハブの操作卓に置いた CLI（配布の手順は docs/architecture.md）
pub fn hub_command(hub: &Node, since: i64) -> String {
    let exe = if hub.is_windows() { "~/.katala-tune/bin/tune.exe" } else { "~/.katala-tune/bin/tune" };
    format!("{exe} export --since {since}")
}

/// ハブから写す（続きがあれば next から最大 16 ページまで続ける）。結果は meta の hubPull に残す（画面に出す）
pub async fn pull(engine: &Engine) -> Result<Value, String> {
    let s = engine.with_db(|d| Ok(settings(d)))?;
    let hub_id = s.hub.ok_or("ハブを選んでいない")?;
    let cfg = engine.reload_config();
    let hub = cfg.node(&hub_id).ok_or_else(|| format!("台帳に無い機体: {hub_id}"))?.clone();
    let t0 = now_ms();
    let (mut rows, mut snaps, mut pages) = (0u64, 0u64, 0usize);
    let res: Result<Value, String> = async {
        if let Some(err) = hub.refusal() {
            return Err(err);
        }
        let mut last = Value::Null;
        while pages < PULL_MAX_PAGES {
            let cursor = engine.with_db(|d| d.get_meta("hubCursor"))?.and_then(|v| v.as_i64()).unwrap_or(0);
            // 前回の終わりより 2 分前から（その後に書かれた分の集計を取りこぼさない。置き換えなので重なってよい）
            let since = cursor.saturating_sub(2 * MINUTE).max(0);
            let r = collect::run("ssh", &collect::ssh_args(&hub.alias, &hub_command(&hub, since)), None, Duration::from_secs(60)).await;
            let line = r.out.lines().rev().find(|l| l.trim_start().starts_with('{')).ok_or_else(|| {
                let e = r.err.trim();
                format!(
                    "ハブから写しが届かない（exit {}）{}",
                    r.code_str(),
                    if e.is_empty() { String::new() } else { format!(": {}", e.lines().last().unwrap_or(e)) }
                )
            })?;
            let x: Value = serde_json::from_str(line).map_err(|e| format!("写しを読めない: {e}"))?;
            let got = engine.with_db(|d| Ok(import(d, &x)))??;
            // 取り込めた範囲の終わりまでだけ進める（途中までのページで先へ飛ばない）
            engine.with_db(|d| d.set_meta("hubCursor", &got["next"]))?;
            pages += 1;
            rows += got["rows"].as_u64().unwrap_or(0);
            snaps += got["snapshots"].as_u64().unwrap_or(0);
            let more = got["more"].as_bool() == Some(true);
            last = got;
            if !more {
                break;
            }
        }
        Ok(last)
    }
    .await;
    let mut rec = json!({ "hub": hub_id, "at": now_ms(), "ms": now_ms() - t0, "pages": pages, "rows": rows, "snapshots": snaps });
    match &res {
        Ok(v) => {
            rec["ok"] = json!(true);
            for k in ["hub_at", "freshest_minute", "fresh", "latest_snapshot", "snapshot_fresh", "more"] {
                rec[k] = v[k].clone();
            }
        }
        Err(e) => {
            rec["ok"] = json!(false);
            rec["error"] = json!(e);
        }
    }
    engine.with_db(|d| d.set_meta("hubPull", &rec))?;
    if snaps > 0 {
        engine.compute_checks(true, true);
    }
    res.map(|_| rec)
}

/// 最後の写しが成功していて新しい（ハブの数字・分析結果の新しさはこの中身で見る）
fn last_pull_ok(db: &Store, now: i64) -> Option<Value> {
    last_pull_ok_for(db, now, settings(db).hub.is_some())
}

fn last_pull_ok_for(db: &Store, now: i64, has_hub: bool) -> Option<Value> {
    if !has_hub {
        return None;
    }
    let p = db.get_meta("hubPull").ok().flatten()?;
    (p.get("ok").and_then(Value::as_bool) == Some(true) && p.get("at").and_then(Value::as_i64).is_some_and(|at| now - at <= HUB_STALE_MS)).then_some(p)
}

/// ハブを写していて、ハブの数字（1 分ごとの集計）が新しい（自分で集めなくてよい）
pub fn hub_fresh(db: &Store, now: i64) -> bool {
    last_pull_ok(db, now).is_some_and(|p| p.get("fresh").and_then(Value::as_bool) == Some(true))
}

/// ハブを写していて、ハブの分析結果が新しい（自分で分析しなくてよい）。数字が新しくても分析が止まっていれば false
pub fn hub_analysis_fresh(db: &Store, now: i64) -> bool {
    last_pull_ok(db, now).is_some_and(|p| p.get("snapshot_fresh").and_then(Value::as_bool) == Some(true))
}

/// このアプリで集めるか（常時監視が入っていて、写しているハブが新しくないとき）
pub fn should_collect(db: &Store, now: i64) -> bool {
    settings(db).enabled && !hub_fresh(db, now)
}

// ---------------------------------------------------------------------------
// 常時監視の繰り返し（Tauri の殻と `tune monitor-run` が使う）
// ---------------------------------------------------------------------------

/// 常時監視は台帳の全機体を流すので、同時に流せる数を台帳の数まで広げる（画面だけのときの既定は 6 台）
pub fn live_opts(nodes: usize) -> Opts {
    let d = Opts::default();
    Opts { max_streams: nodes.max(d.max_streams), ..d }
}

/// サンプラーを入れ替えた機体を次に試すまで（失敗・入れ替え直後の同じ版の見直し）
pub const AGENT_PUSH_EVERY_MS: i64 = 60 * MINUTE;

fn collection_decision(db: &Store, now: i64) -> rusqlite::Result<(Settings, bool)> {
    let s = checked_settings(db)?;
    let fresh = last_pull_ok_for(db, now, s.hub.is_some()).is_some_and(|p| p.get("fresh").and_then(Value::as_bool) == Some(true));
    let collect = s.enabled && !fresh;
    Ok((s, collect))
}

/// 既存の5分policy。時計/再試行動作は変更せず決定だけを記録する。
fn disconnected_ready(id: &str, down: bool, now: i64, retry_at: &mut HashMap<String, i64>, journal: &crate::monitor_journal::Journal) -> bool {
    if !down {
        retry_at.remove(id);
        return true;
    }
    let initial = !retry_at.contains_key(id);
    let at = *retry_at.entry(id.to_string()).or_insert(now + RETRY_DISCONNECTED_MS);
    if initial {
        journal.record(
            Some(id),
            0,
            0,
            Kind::Retry,
            Data { policy: Some(Policy::MonitorDisconnected), delay_ms: Some(RETRY_DISCONNECTED_MS as u64), ..Data::default() },
        );
    }
    if now >= at {
        retry_at.insert(id.to_string(), now + RETRY_DISCONNECTED_MS);
        journal.record(Some(id), 0, 0, Kind::Retry, Data { policy: Some(Policy::MonitorDisconnected), delay_ms: Some(0), ..Data::default() });
        true
    } else {
        false
    }
}

/// 保存ACKと失敗を分ける。
fn save_minute_rows(engine: &Engine, journal: &crate::monitor_journal::Journal, rows: &[MinuteRow]) -> bool {
    let result = engine.with_db(|d| Ok(d.add_metrics(rows).map_err(|e| Category::database(&e))));
    let category = match &result {
        Ok(Ok(_)) => None,
        Ok(Err(c)) => Some(*c),
        Err(_) => Some(Category::Unknown),
    };
    for row in rows {
        // minuteは複数attemptを含み得る。generation/attempt=0は個別producer未対応を示す。
        journal.record(
            Some(&row.node_id),
            0,
            0,
            Kind::MinuteCommit,
            Data {
                minute: Some(row.minute),
                rows: Some(1),
                samples: Some(row.n.max(0) as u64),
                committed: Some(category.is_none()),
                category,
                ..Data::default()
            },
        );
    }
    category.is_none()
}

fn commit_minute_rows(
    engine: &Engine,
    journal: &crate::monitor_journal::Journal,
    rows: &[MinuteRow],
    rolled: &mut HashMap<String, i64>,
    event: impl FnOnce(Value),
) -> bool {
    if rows.is_empty() {
        return false;
    }
    if !save_minute_rows(engine, journal, rows) {
        return false;
    }
    for row in rows {
        rolled.insert(row.node_id.clone(), row.minute);
    }
    event(json!({ "rows": rows.len() }));
    true
}

/// 15 秒ごとに: 常時監視が入っていてハブの数字が新しくなければ全機体のライブに合図を送って保ち、
/// 終わった分を集計して保存し、ハブを選んでいれば 1 分ごとに写す。14 日より古い集計は 1 日 1 回消す。
/// agent（同梱のサンプラー。Windows の操作卓だけ）があれば、hello の版が古い・無い Windows の機体へ入れ替えを送る（helpers.rs）。
/// until（CLI の確認用）を過ぎたら戻る。event には {"rows": n} / {"pulled": true} / {"agent": …} を渡す
pub async fn run(
    engine: Arc<Engine>,
    live: Arc<Live>,
    event: impl Fn(Value) + Send + Sync + 'static,
    until: Option<std::time::Instant>,
    agent: Option<Arc<Vec<u8>>>,
) {
    let event = Arc::new(event);
    let journal = engine.start_monitor_journal();
    live.bind_journal(journal.clone());
    let mut previous_policy = None;
    let mut rolled: HashMap<String, i64> = HashMap::new();
    let mut retry_at: HashMap<String, i64> = HashMap::new();
    let mut pushed_at: HashMap<String, i64> = HashMap::new();
    let (mut last_pull, mut last_prune) = (0i64, 0i64);
    let host = crate::nodes::local_host();
    let mut iv = tokio::time::interval(Duration::from_secs(15));
    loop {
        iv.tick().await;
        let now = now_ms();
        let decision = engine.with_db(|d| collection_decision(d, now));
        let unknown = decision.is_err();
        let (s, collect) = decision.unwrap_or_default();
        let policy = if unknown {
            Policy::SettingsUnknown
        } else if collect {
            Policy::Local
        } else if s.enabled {
            Policy::Hub
        } else {
            Policy::Disabled
        };
        if previous_policy != Some(policy) {
            journal.record(None, 0, 0, Kind::Policy, Data { policy: Some(policy), ..Data::default() });
            previous_policy = Some(policy);
        }
        if s.hub.is_some() && now - last_pull >= MINUTE - 5_000 {
            last_pull = now;
            if pull(&engine).await.is_ok() {
                event(json!({ "pulled": true }));
            }
        }
        let cfg = engine.reload_config();
        live.retain_active(&cfg.nodes);
        if collect {
            // 流れていれば合図、止まっていれば流し直す。つなぎ直しても続かなかった機体は 5 分おきにだけ試す
            let targets: Vec<Node> = cfg
                .nodes
                .iter()
                .filter(|n| n.is_active() && (n.is_mac() || n.is_windows()))
                .filter(|n| {
                    let down = live.brief(&n.id).is_some_and(|b| b["state"] == "stopped" && b["reason"] == "disconnected");
                    disconnected_ready(&n.id, down, now, &mut retry_at, &journal)
                })
                .cloned()
                .collect();
            let r = live.start(&targets, &[]);
            // 上限で断られた機体（アプリを開いたあとで台帳に足したなど）は黙って欠かさず知らせる。画面にも注意として出る
            let refused: Vec<&Value> = r["refused"].as_array().into_iter().flatten().filter(|x| x["reason"] == "limit").map(|x| &x["id"]).collect();
            if !refused.is_empty() {
                event(json!({ "refused": refused, "limit": r["limit"] }));
            }
            // サンプラーの版が古い・無い Windows の機体へ同梱の物を送り、入れ替わったらつなぎ直す（1 機体 1 時間に 1 回まで）
            if let Some(bytes) = &agent {
                for n in cfg.nodes.iter().filter(|n| crate::helpers::agent_outdated(n, live.brief(&n.id).as_ref())) {
                    if pushed_at.get(&n.id).is_some_and(|t| now - t < AGENT_PUSH_EVERY_MS) {
                        continue;
                    }
                    pushed_at.insert(n.id.clone(), now);
                    let (n, bytes, live, event) = (n.clone(), bytes.clone(), live.clone(), event.clone());
                    tokio::spawn(async move {
                        let r = crate::helpers::push_agent(&n, &bytes).await;
                        if r.is_ok() {
                            live.stop(Some(std::slice::from_ref(&n.id)), crate::live::Stop::Restart);
                        }
                        event(json!({ "agent": { "node": n.id, "version": crate::helpers::VERSION, "ok": r.is_ok(), "detail": r.unwrap_or_else(|e| e) } }));
                    });
                }
            }
        }
        // ハブの数字を写しているあいだは、手元のライブの点で同じ分を上書きしない
        if collect || s.hub.is_none() {
            let until_min = now.div_euclid(MINUTE) * MINUTE;
            let mut rows = Vec::new();
            for n in &cfg.nodes {
                let from = rolled.get(&n.id).map_or(until_min - 5 * MINUTE, |m| m + MINUTE);
                if from >= until_min {
                    continue;
                }
                let got = rollup(&n.id, &live.samples(&n.id), live.procs(&n.id).as_ref(), from, until_min, &host);
                rows.extend(got);
            }
            commit_minute_rows(&engine, &journal, &rows, &mut rolled, &*event);
        }
        if now - last_prune >= 24 * 60 * MINUTE {
            last_prune = now;
            let _ = engine.with_db(|d| d.prune_metrics(now - KEEP_DAYS * 24 * 60 * MINUTE));
        }
        if until.is_some_and(|u| std::time::Instant::now() >= u) {
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// ダッシュボード
// ---------------------------------------------------------------------------

/// 機体ごとの今・直近 60 分・分析の要約・注意と、全体の状態を 1 つに（画面はこれを描くだけ）
pub fn dashboard(engine: &Engine, live: Option<&Live>, minutes: i64) -> Result<Value, String> {
    let now = now_ms();
    let minutes = minutes.clamp(10, 24 * 60);
    let cfg = engine.config();
    let since = now - minutes * MINUTE;
    let (s, rows, pull, collecting) = engine.with_db(|d| {
        Ok((settings(d), d.metrics(None, since - 6 * MINUTE, i64::MAX)?, d.get_meta("hubPull")?.unwrap_or(Value::Null), should_collect(d, now)))
    })?;
    let lasts: HashMap<String, Value> = engine.last()?.into_iter().filter_map(|r| Some((r.get("node_id")?.as_str()?.to_string(), r))).collect();
    let mut by_node: HashMap<&str, Vec<MinuteRow>> = HashMap::new();
    for r in &rows {
        by_node.entry(r.node_id.as_str()).or_default().push(r.clone());
    }
    let monitoring = s.enabled || s.hub.is_some();
    let mut nodes = Vec::new();
    for n in &cfg.nodes {
        let rs = by_node.remove(n.id.as_str()).unwrap_or_default();
        // 所見は画面の「機体」と同じく、保存した分析結果から作り直したもの（題名・根拠・スコアつき）
        let r = lasts.get(n.id.as_str());
        let findings: Vec<Value> = r.and_then(|r| r.get("findings")).and_then(Value::as_array).cloned().unwrap_or_default();
        let count = |s: &str| findings.iter().filter(|f| f.get("severity").and_then(Value::as_str) == Some(s)).count();
        let last = r.map(|r| {
            let d = &r["data"];
            let disks: Vec<&Value> = d.get("disk").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
            let low_disk = disks.iter().filter_map(|x| Some((x.get("mount")?.as_str()?, x.get("free_pct")?.as_f64()?))).min_by(|a, b| a.1.total_cmp(&b.1));
            json!({
                "at": r.get("at"), "score": r.get("score"),
                "os": d.pointer("/host/os"), "cpu": d.pointer("/host/cpu"), "model": d.pointer("/host/model"),
                "uptime_h": d.pointer("/host/uptime_h"), "mem_total_gb": d.pointer("/memory/total_gb"),
                "disk_low": low_disk.map(|(m, p)| json!({ "mount": m, "free_pct": p })),
                "defender_realtime": d.pointer("/defender/realtime"),
                "power_plan": d.pointer("/power/plan_name"),
                "bugchecks_7d": d.pointer("/stability_7d/bugcheck_1001"),
                "critical": count("critical"), "warn": count("warn"), "info": count("info"),
            })
        });
        let brief = live.and_then(|l| l.brief(&n.id));
        let mut al = alerts(&rs, now, monitoring && (collecting || s.hub.is_some()));
        if brief.as_ref().is_some_and(|b| b["reason"] == "limit") {
            let limit = live.map_or(0, |l| l.opts().max_streams);
            al.push(json!({ "id": "limit", "severity": "warn", "title": "同時に流せる台数の上限で集めていない", "detail": format!("上限 {limit} 台。アプリを開き直すと台帳の台数まで広がる") }));
        }
        for f in findings.iter().filter(|f| f.get("severity").and_then(Value::as_str) == Some("critical")).take(3) {
            al.push(json!({ "id": format!("finding:{}", f.get("id").and_then(Value::as_str).unwrap_or("")), "severity": "critical", "title": f.get("title"), "detail": f.get("detail") }));
        }
        let now_point = rs.last().map(|r| json!({ "minute": r.minute, "v": r.v, "src": r.src }));
        let in_range: Vec<MinuteRow> = rs.iter().filter(|r| r.minute >= since).cloned().collect();
        nodes.push(json!({
            "id": n.id, "os": n.os, "local": n.local, "shared": n.shared,
            "live": brief, "latest": now_point, "series": series(&in_range, since, now, 60),
            "last": last, "alerts": al,
        }));
    }
    let (update, auto_update, helpers) = engine.with_db(|d| Ok((d.get_meta("updateState")?, d.get_meta("update")?, d.get_meta("helpers")?)))?;
    let app = json!({
        "version": crate::helpers::VERSION, "update": update, "helpers": helpers,
        "auto_update": auto_update.as_ref().and_then(|u| u.get("auto")).and_then(Value::as_bool).unwrap_or(true),
    });
    Ok(json!({
        "at": now, "minutes": minutes, "monitor": s.json(), "collecting": collecting, "hub_pull": pull, "app": app,
        "local_host": crate::nodes::local_host(), "schedule": engine.schedule(), "nodes": nodes,
        "journal": engine.monitor_journal_health(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{Gpu, Mem};

    fn sample(t: i64, cpu: f64, mem: f64, gtemp: Option<f64>) -> Sample {
        Sample {
            t,
            cpu: Some(cpu),
            mem: Some(Mem { used_pct: Some(mem), ..Default::default() }),
            gpu: gtemp.map(|g| vec![Gpu { util: Some(10.0), temp_c: Some(g), ..Default::default() }]),
            ..Default::default()
        }
    }

    #[test]
    fn vram_adapter_values_survive_minute_rollup_series_and_sqlite() {
        let s = Sample {
            t: 1000,
            gpu: Some(vec![
                Gpu { adapter_index: Some(0), mem_used_mb: Some(1024.0), mem_total_mb: Some(4096.0), mem_free_mb: Some(3000.0), ..Default::default() },
                Gpu { adapter_index: Some(1), mem_total_mb: Some(8192.0), ..Default::default() },
            ]),
            ..Default::default()
        };
        let rows = rollup("a", &[s], None, 0, MINUTE, "test");
        assert_eq!(rows[0].v["gpu_0_vram_free_mib"], json!([3000.0, 3000.0]));
        assert!(rows[0].v.get("gpu_1_vram_used_mib").is_none());
        let db = Store::open_in_memory().unwrap();
        db.add_metrics(&rows).unwrap();
        let saved = db.metrics(Some("a"), 0, MINUTE).unwrap();
        let values = series(&saved, 0, MINUTE, 60);
        assert_eq!(values["gpu_0_vram_pct"], json!([25.0]));
        assert!(values.get("gpu_1_vram_pct").is_none());
    }

    #[test]
    fn rollup_keeps_only_finished_minutes() {
        let base = 10 * MINUTE;
        let pts: Vec<Sample> = (0..150).map(|i| sample(base + i * 1000, (i % 10) as f64, 50.0, Some(40.0 + (i % 3) as f64))).collect();
        let procs = json!({"top_cpu": [{"name": "rustc", "cpu": 800.0}], "top_mem": [{"name": "vmmemWSL", "mem_mb": 4900.0}]});
        // 2 分 30 秒ぶん。until = 12 分なら、10 分と 11 分の 2 行（12 分は終わっていない）
        let rows = rollup("a", &pts, Some(&procs), 0, base + 2 * MINUTE, "me");
        assert_eq!(rows.iter().map(|r| (r.minute, r.n)).collect::<Vec<_>>(), [(base, 60), (base + MINUTE, 60)]);
        assert_eq!(rows[0].v["cpu"], json!([4.5, 9.0]));
        assert_eq!(rows[0].v["gtemp"], json!([41.0, 42.0]));
        assert!(rows[0].v.get("top_cpu").is_none(), "上位プロセスは一番新しい分にだけ");
        assert_eq!(rows[1].v["top_cpu"], json!(["rustc", 800.0]));
        assert_eq!(rows[1].v["top_mem"], json!(["vmmemWSL", 4900.0]));
        assert!(rows[0].v.get("swap").is_none(), "無い値は入れない");
        // from より前の分は入れない。until までに終わった分は、点が途中で途切れていても入れる
        let later = rollup("a", &pts, None, base + MINUTE, base + 3 * MINUTE, "me");
        assert_eq!(later.iter().map(|r| (r.minute, r.n)).collect::<Vec<_>>(), [(base + MINUTE, 60), (base + 2 * MINUTE, 30)]);
    }

    #[test]
    fn series_buckets_average_and_max() {
        let rows: Vec<MinuteRow> = (0..4)
            .map(|i| MinuteRow { node_id: "a".into(), minute: i * MINUTE, n: 60, v: json!({"cpu": [i as f64 * 10.0, i as f64 * 10.0 + 5.0]}), src: None })
            .collect();
        let s = series(&rows, 0, 4 * MINUTE, 2);
        assert_eq!(s["t"], json!([0, 2 * MINUTE]));
        assert_eq!(s["cpu"], json!([5.0, 25.0]));
        assert_eq!(s["cpu_max"], json!([15.0, 35.0]));
        assert!(s.get("mem").is_none());
    }

    #[test]
    fn alerts_need_sustained_values() {
        let now = 100 * MINUTE;
        let row = |m: i64, cpu: f64, commit: f64| MinuteRow {
            node_id: "a".into(),
            minute: now - m * MINUTE,
            n: 60,
            v: json!({"cpu": [cpu, cpu], "commit": [commit, commit]}),
            src: None,
        };
        let ids = |a: Vec<Value>| a.iter().map(|x| x["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(ids(alerts(&[row(1, 95.0, 50.0), row(2, 95.0, 50.0)], now, true)), Vec::<String>::new(), "2 分だけでは出さない");
        assert_eq!(ids(alerts(&[row(1, 95.0, 50.0), row(2, 95.0, 50.0), row(3, 92.0, 91.0)], now, true)), ["cpu", "commit"]);
        assert_eq!(ids(alerts(&[row(10, 10.0, 10.0)], now, true)), ["stale"]);
        assert_eq!(ids(alerts(&[], now, false)), Vec::<String>::new(), "監視していなければ「届いていない」は出さない");
    }

    #[test]
    fn export_import_round_trip_keeps_newer_snapshots() {
        let hub = Store::open_in_memory().unwrap();
        let now = now_ms();
        let m = now.div_euclid(MINUTE) * MINUTE - MINUTE;
        hub.add_metrics(&[MinuteRow { node_id: "a".into(), minute: m, n: 60, v: json!({"cpu": [1.0, 2.0]}), src: Some("hub".into()) }]).unwrap();
        hub.add_snapshot(
            "a",
            now - 1000,
            Some(2.0),
            Some(70),
            &json!([{"id": "x", "severity": "critical"}]),
            &json!({"host": {"os": "w"}}),
            &json!({"critical": 1}),
        )
        .unwrap();
        let x = export(&hub, 0, "hub-host").unwrap();
        assert_eq!(x["latest"]["a"], m);
        let me = Store::open_in_memory().unwrap();
        me.add_snapshot("a", now, None, Some(99), &json!([]), &json!({}), &json!({})).unwrap();
        let r = import(&me, &x).unwrap();
        assert_eq!(
            (r["rows"].as_u64(), r["snapshots"].as_u64(), r["fresh"].as_bool()),
            (Some(1), Some(0), Some(true)),
            "手元の方が新しい分析結果は置き換えない"
        );
        assert_eq!(me.metrics(Some("a"), 0, i64::MAX).unwrap()[0].src.as_deref(), Some("hub"));
        let me2 = Store::open_in_memory().unwrap();
        assert_eq!(import(&me2, &x).unwrap()["snapshots"], 1);
        assert_eq!(me2.last_snapshots("a", 1).unwrap()[0].data["host"]["os"], "w");
        assert!(import(&me2, &json!({"v": 2})).is_err());
    }

    #[test]
    fn settings_validate_the_hub() {
        let db = Store::open_in_memory().unwrap();
        let cfg = crate::nodes::parse_config(
            r#"{"nodes":[{"id":"me","alias":"me","os":"windows","local_hostname":"h"},{"id":"mac","alias":"mac","os":"macos"},{"id":"pi","alias":"pi","os":"linux"}]}"#,
            std::path::Path::new("x.json"),
            "h",
        )
        .unwrap();
        assert_eq!(settings(&db), Settings::default());
        assert_eq!(set_settings(&db, &cfg, &json!({"enabled": true, "hub": "mac"})).unwrap(), Settings { enabled: true, hub: Some("mac".into()) });
        assert!(set_settings(&db, &cfg, &json!({"hub": "me"})).is_err(), "自分はハブにしない");
        assert!(set_settings(&db, &cfg, &json!({"hub": "nope"})).is_err());
        assert!(set_settings(&db, &cfg, &json!({"hub": "pi"})).is_err());
        assert_eq!(set_settings(&db, &cfg, &json!({"hub": null})).unwrap().hub, None);
        assert!(!hub_fresh(&db, now_ms()));
        set_settings(&db, &cfg, &json!({"hub": "mac"})).unwrap();
        db.set_meta("hubPull", &json!({"ok": true, "fresh": true, "at": now_ms()})).unwrap();
        assert!(hub_fresh(&db, now_ms()));
        assert!(!should_collect(&db, now_ms()), "ハブが新しければ自分では集めない");
        db.set_meta("hubPull", &json!({"ok": true, "fresh": true, "at": now_ms() - HUB_STALE_MS - 1})).unwrap();
        assert!(should_collect(&db, now_ms()), "ハブが古ければ自分で集める");
        // 数字が新しくても、ハブの分析結果が古ければ自分で分析する
        db.set_meta("hubPull", &json!({"ok": true, "fresh": true, "snapshot_fresh": false, "at": now_ms()})).unwrap();
        assert!(hub_fresh(&db, now_ms()) && !hub_analysis_fresh(&db, now_ms()));
        db.set_meta("hubPull", &json!({"ok": true, "fresh": true, "snapshot_fresh": true, "at": now_ms()})).unwrap();
        assert!(hub_analysis_fresh(&db, now_ms()));
    }

    #[test]
    fn excluded_nodes_cannot_be_selected_as_hubs() {
        let db = Store::open_in_memory().unwrap();
        for status in ["retired", "no-ssh", "paused", "out-of-scope", "invalid"] {
            let cfg = crate::nodes::parse_config(
                &json!({"nodes": [{"id": "hub", "alias": "hub", "os": "macos", "status": status}]}).to_string(),
                std::path::Path::new("x.json"),
                "h",
            )
            .unwrap();
            assert_eq!(set_settings(&db, &cfg, &json!({"hub": "hub"})).unwrap_err(), cfg.node("hub").unwrap().refusal().unwrap());
            assert_eq!(settings(&db).hub, None);
        }
    }

    #[test]
    fn export_pages_through_the_retained_range_without_gaps() {
        let hub = Store::open_in_memory().unwrap();
        let now = now_ms();
        let day = 24 * 60 * MINUTE;
        let row = |m: i64| MinuteRow { node_id: "a".into(), minute: m.div_euclid(MINUTE) * MINUTE, n: 60, v: json!({"cpu": [1.0, 1.0]}), src: None };
        // 3 日前・2 日前・今の 3 行（24 時間を超える欠け）。14 日より前の行は写さない
        let minutes = [now - 3 * day, now - 2 * day + MINUTE, now - 2 * MINUTE];
        hub.add_metrics(&minutes.map(row)).unwrap();
        hub.add_metrics(&[row(now - 20 * day)]).unwrap();
        let me = Store::open_in_memory().unwrap();
        let (mut cursor, mut pages) = (0i64, 0);
        loop {
            let x = export(&hub, cursor.saturating_sub(2 * MINUTE), "hub").unwrap();
            let got = import(&me, &x).unwrap();
            cursor = got["next"].as_i64().unwrap();
            pages += 1;
            if got["more"] != true || pages > PULL_MAX_PAGES {
                break;
            }
        }
        let have: Vec<i64> = me.metrics(Some("a"), 0, i64::MAX).unwrap().iter().map(|r| r.minute).collect();
        assert_eq!(have, minutes.map(|m| m.div_euclid(MINUTE) * MINUTE), "24 時間を超えても全部写る（14 日より前は除く）");
        assert!((2..=PULL_MAX_PAGES).contains(&pages), "{pages} ページ");
        assert!(cursor >= now - MINUTE, "最後は今まで進む");
        // 分析結果の新しさは集計とは別に返す
        let x = export(&hub, cursor, "hub").unwrap();
        assert_eq!(import(&me, &x).unwrap()["snapshot_fresh"], false, "分析結果が無ければ新しくない");
        hub.add_snapshot("a", now_ms(), None, None, &json!([]), &json!({}), &json!({})).unwrap();
        let x = export(&hub, cursor, "hub").unwrap();
        assert_eq!(import(&me, &x).unwrap()["snapshot_fresh"], true);
    }

    #[test]
    fn monitor_streams_cover_the_whole_ledger() {
        assert_eq!(live_opts(3).max_streams, Opts::default().max_streams);
        assert_eq!(live_opts(9).max_streams, 9);
    }
    #[tokio::test]
    async fn minute_commit_ack_reports_only_actual_database_success() {
        use crate::{
            engine::NoHost,
            monitor_journal::{Filter, Journal},
        };
        let dir = std::env::temp_dir().join(format!("monitor-commit-{}-{}", std::process::id(), now_ms()));
        let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(NoHost)).unwrap();
        let journal = Journal::start(engine.db.clone());
        let row = MinuteRow { node_id: "fixture".into(), minute: 60_000, n: 60, v: json!({"cpu":[5,5]}), src: None };
        assert!(save_minute_rows(&engine, &journal, std::slice::from_ref(&row)));
        engine.with_db(|d| d.conn().execute_batch("PRAGMA query_only=ON")).unwrap();
        let mut failed = row;
        failed.minute = 120_000;
        assert!(!save_minute_rows(&engine, &journal, &[failed]));
        engine.with_db(|d| d.conn().execute_batch("PRAGMA query_only=OFF")).unwrap();
        assert!(journal.flush(Duration::from_secs(2)).await);
        let records = engine.with_db(|d| d.monitor_events(&Filter { kind: Some(Kind::MinuteCommit), ..Default::default() })).unwrap();
        let xs = records["events"].as_array().unwrap();
        assert_eq!(xs.len(), 2);
        assert_eq!(xs[0]["data"]["committed"], false);
        assert_eq!(xs[0]["data"]["minute"], 120_000);
        assert_eq!(xs[1]["data"]["committed"], true);
        assert_eq!(xs[1]["data"]["minute"], 60_000);
        assert_eq!(engine.with_db(|d| d.metrics(Some("fixture"), 0, 180_000)).unwrap().len(), 1);
        assert!(journal.shutdown(Duration::from_secs(2)).await);
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn rolled_minutes_advance_only_after_batch_commit_and_retry_without_duplicates() {
        use crate::{engine::NoHost, monitor_journal::Filter};
        let dir = std::env::temp_dir().join(format!("monitor-watermark-{}-{}", std::process::id(), now_ms()));
        let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(NoHost)).unwrap();
        let journal = engine.start_monitor_journal();
        let initial: HashMap<String, i64> = ["a", "b"].map(|id| (id.into(), MINUTE)).into();
        let mut rolled = initial.clone();
        let before: Vec<MinuteRow> =
            ["a", "b"].into_iter().flat_map(|id| rollup(id, &[sample(MINUTE + 1000, 1.0, 25.0, None)], None, MINUTE, 2 * MINUTE, "fixture")).collect();
        engine.with_db(|d| d.add_metrics(&before)).unwrap();
        engine
            .with_db(|d| {
                d.conn().execute_batch(
                    "CREATE TEMP TRIGGER fail_second BEFORE INSERT ON metrics_minute WHEN NEW.node_id='b' BEGIN SELECT RAISE(ABORT,'fixture'); END",
                )
            })
            .unwrap();
        // runと同じ水位から同じ残存sampleを再集計する。失敗した分を次のtickで除外してはならない。
        let pending = |watermark: &HashMap<String, i64>| -> Vec<MinuteRow> {
            ["a", "b"]
                .into_iter()
                .flat_map(|id| {
                    let from = watermark.get(id).map_or(-2 * MINUTE, |m| m + MINUTE);
                    rollup(id, &[sample(2 * MINUTE + 1000, 5.0, 25.0, None)], None, from, 3 * MINUTE, "fixture")
                })
                .collect()
        };
        let rows = pending(&rolled);
        let mut emitted = Vec::new();
        let failed_ack = commit_minute_rows(&engine, &journal, &rows, &mut rolled, |v| emitted.push(v));
        let failed_watermark = rolled.clone();
        let failed_emitted = emitted.len();
        let after_failure = engine.with_db(|d| d.metrics(None, 0, i64::MAX)).unwrap();
        engine.with_db(|d| d.conn().execute_batch("DROP TRIGGER fail_second")).unwrap();
        let retry = pending(&rolled);
        let retry_ack = commit_minute_rows(&engine, &journal, &retry, &mut rolled, |v| emitted.push(v));
        let replay_ack = commit_minute_rows(&engine, &journal, &rows, &mut rolled, |v| emitted.push(v));
        let saved = engine.with_db(|d| d.metrics(None, 0, i64::MAX)).unwrap();
        assert!(journal.flush(Duration::from_secs(2)).await);
        let records = engine.with_db(|d| d.monitor_events(&Filter { kind: Some(Kind::MinuteCommit), ..Default::default() })).unwrap();
        assert!(journal.shutdown(Duration::from_secs(2)).await);
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
        assert!(!failed_ack);
        assert_eq!(after_failure, before, "the first insert also rolls back when the second row fails");
        assert_eq!(failed_emitted, 0, "no rows success notification on false ACK");
        assert_eq!(failed_watermark, initial, "failed batch must leave both watermarks eligible for retry");
        assert_eq!(retry, rows, "next tick retries the same finished minutes still in the ring");
        assert!(retry_ack && replay_ack);
        assert_eq!(emitted, [json!({"rows":2}), json!({"rows":2})]);
        assert_eq!(saved.len(), 4, "replaying the same node/minute uses the existing UPSERT key");
        assert_eq!(rolled, ["a", "b"].map(|id| (id.into(), 2 * MINUTE)).into());
        let xs = records["events"].as_array().unwrap();
        assert_eq!(xs.iter().filter(|x| x["data"]["committed"] == false).count(), 2);
        assert_eq!(xs.iter().filter(|x| x["data"]["committed"] == true).count(), 4);
    }
    // 同じ変換/rollup/Store経路を通す小さいmicrobenchmark。OS/SSH/driver性能の証明ではない。
    async fn journal_microbench(enabled: bool) {
        use crate::{
            engine::NoHost,
            live::{Msg, parse_line},
            monitor_journal::{Filter, Journal},
        };
        let dir = std::env::temp_dir().join(format!("monitor-cost-{}-{}", std::process::id(), now_ms()));
        let engine = Engine::open(dir.join("nodes.json"), dir.join("data"), Arc::new(NoHost)).unwrap();
        let journal: Option<Journal> = enabled.then(|| engine.start_monitor_journal());
        let start = std::time::Instant::now();
        let base = now_ms().div_euclid(MINUTE) * MINUTE - 10 * MINUTE;
        let mut processed = 0;
        let mut peak = 0;
        let mut seq_drops = 0;
        for minute in 0..10 {
            let mut rows = Vec::new();
            for node in 0..6 {
                let id = format!("fixture-{node}");
                let mut samples = Vec::new();
                let mut previous = None;
                for second in 0..60 {
                    let seq = (minute * 60 + second) as u64;
                    let line = format!(
                        r#"{{"type":"s","seq":{seq},"cpu":5,"mem":{{"used_pct":25}},"gpu":[{{"adapter_index":0,"mem_used_mb":0,"mem_total_mb":8192,"mem_free_mb":8000,"mem_used_pct":0}}]}}"#
                    );
                    let Some(Msg::Sample(raw)) = parse_line(&line) else { panic!("fixture parse") };
                    let (sample, _, _) = Sample::from_raw(*raw, base + minute * MINUTE + second * 1000);
                    if previous.is_some_and(|p| sample.seq != Some(p + 1)) {
                        seq_drops += 1;
                    }
                    previous = sample.seq;
                    samples.push(sample);
                    processed += 1;
                }
                rows.extend(rollup(&id, &samples, None, base + minute * MINUTE, base + (minute + 1) * MINUTE, "fixture"));
            }
            if let Some(j) = &journal {
                assert!(save_minute_rows(&engine, j, &rows));
                peak = peak.max(j.health()["queue_depth"].as_u64().unwrap());
            } else {
                engine.with_db(|d| d.add_metrics(&rows)).unwrap();
            }
        }
        if let Some(j) = &journal {
            assert!(j.flush(Duration::from_secs(5)).await);
            assert_eq!(j.health()["dropped_total"], 0);
        }
        let elapsed = start.elapsed().as_micros();
        let events = engine.with_db(|d| d.monitor_events(&Filter::default())).unwrap()["retention"]["rows"].as_i64().unwrap();
        let rows = engine.with_db(|d| d.metrics(None, base, base + 10 * MINUTE)).unwrap();
        assert_eq!(rows.len(), 60);
        assert_eq!(processed, 3600);
        assert_eq!(seq_drops, 0);
        let bytes = ["katala-tune.db", "katala-tune.db-wal", "katala-tune.db-shm"]
            .iter()
            .filter_map(|p| std::fs::metadata(dir.join("data").join(p)).ok())
            .map(|m| m.len())
            .sum::<u64>();
        println!(
            "{}",
            json!({"enabled":enabled,"nodes":6,"logical_minutes":10,"samples":processed,"metric_rows":rows.len(),"seq_drops":seq_drops,"queue_peak":peak,"event_rows":events,"elapsed_us":elapsed,"database_plus_wal_shm_bytes":bytes})
        );
        if let Some(j) = journal {
            assert!(j.shutdown(Duration::from_secs(2)).await);
        }
        drop(engine);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    #[ignore = "bounded synthetic performance receipt, run explicitly"]
    async fn journal_microbench_off() {
        journal_microbench(false).await;
    }
    #[tokio::test]
    #[ignore = "bounded synthetic performance receipt, run explicitly"]
    async fn journal_microbench_on() {
        journal_microbench(true).await;
    }
    #[tokio::test]
    async fn disconnected_retry_waits_five_minutes_without_polling_the_host() {
        let db = Arc::new(std::sync::Mutex::new(Store::open_in_memory().unwrap()));
        let journal = crate::monitor_journal::Journal::start(db.clone());
        let mut retry = HashMap::new();
        let now = 1000;
        assert!(!disconnected_ready("fixture", true, now, &mut retry, &journal));
        assert!(!disconnected_ready("fixture", true, now + RETRY_DISCONNECTED_MS - 1, &mut retry, &journal));
        assert!(disconnected_ready("fixture", true, now + RETRY_DISCONNECTED_MS, &mut retry, &journal));
        assert!(!disconnected_ready("fixture", true, now + RETRY_DISCONNECTED_MS + 1, &mut retry, &journal));
        assert!(disconnected_ready("fixture", false, now + RETRY_DISCONNECTED_MS + 1, &mut retry, &journal));
        assert!(retry.is_empty());
        assert!(journal.flush(Duration::from_secs(2)).await);
        let v = db.lock().unwrap().monitor_events(&crate::monitor_journal::Filter { kind: Some(Kind::Retry), ..Default::default() }).unwrap();
        assert_eq!(v["events"].as_array().unwrap().len(), 2);
        assert_eq!(v["events"][0]["data"]["delay_ms"], 0);
        assert_eq!(v["events"][1]["data"]["delay_ms"], RETRY_DISCONNECTED_MS);
        assert!(journal.shutdown(Duration::from_secs(2)).await);
    }
    #[test]
    fn settings_read_failure_is_unknown_instead_of_intentional_disabled() {
        let db = Store::open_in_memory().unwrap();
        db.conn().execute_batch("DROP TABLE meta").unwrap();
        assert!(collection_decision(&db, 1000).is_err(), "a failed settings read must not be labelled disabled");
    }
}
