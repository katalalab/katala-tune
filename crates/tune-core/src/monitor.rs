//! 常時監視（分ごとの集計）・ハブとの同期・ダッシュボードの集計。
//!
//! - 常時監視: アプリが開いているあいだ（メニューバー・通知領域に居るあいだも）ライブのサンプラーを流し続け、
//!   終わった 1 分ごとに平均と最大を `metrics_minute` に残す（14 日）。画面が見ているときは同じ流れを 1 秒ごとに描く
//! - ハブ: Mac・Windows のどの操作卓でも同じ数字を見るため、1 台（ハブ）だけが集め、ほかの操作卓は 1 分ごとに
//!   ハブの集計と分析結果を SSH で写す（`tune export`）。ハブの数字が 5 分より古ければ、写す側が自分で集める
//! - 詳細（分析・ログ）は 1 時間ごと（engine の既定）。ハブの分析結果が新しいあいだ、写す側は分析を重ねない

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::collect;
use crate::db::metrics::MinuteRow;
use crate::db::{Store, now_ms};
use crate::engine::Engine;
use crate::live::{Live, Sample};
use crate::nodes::{Config, Node};

pub const MINUTE: i64 = 60_000;
pub const KEEP_DAYS: i64 = 14;
/// ハブの一番新しい分がハブの時計でこれより古ければ、ハブは集めていないとみなす
pub const HUB_STALE_MS: i64 = 5 * MINUTE;
/// 一度に写す範囲の上限（初回でも 24 時間ぶん）
pub const EXPORT_MAX_MS: i64 = 24 * 60 * MINUTE;
/// 集計に使う値（Sample::point のキー）
const KEYS: [&str; 15] =
    ["cpu", "mem", "swap", "commit", "dr", "dw", "rx", "tx", "power_cpu_w", "power_soc_w", "power_platform_w", "power_gpu_w", "gpu", "gmem", "gtemp"];

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
    let v = db.get_meta("monitor").ok().flatten().unwrap_or(Value::Null);
    Settings {
        enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        hub: v.get("hub").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
    }
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
    by_minute
        .into_iter()
        .map(|(minute, pts)| {
            let mut v = Map::new();
            for k in KEYS {
                let xs: Vec<f64> = pts.iter().filter_map(|p| p.get(k).and_then(Value::as_f64)).collect();
                if !xs.is_empty() {
                    let avg = xs.iter().sum::<f64>() / xs.len() as f64;
                    let max = xs.iter().copied().fold(f64::MIN, f64::max);
                    v.insert(k.into(), json!([round(avg, 2), round(max, 2)]));
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
    let mut acc: BTreeMap<i64, HashMap<&str, (f64, usize, f64)>> = BTreeMap::new();
    for r in rows {
        let b = since + (r.minute - since).div_euclid(width) * width;
        let e = acc.entry(b).or_default();
        for k in KEYS {
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
    for k in KEYS {
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
pub fn export(db: &Store, since: i64, host: &str) -> Result<Value, String> {
    let now = now_ms();
    let since = since.max(now - EXPORT_MAX_MS);
    let rows = db.metrics(None, since, i64::MAX).map_err(|e| e.to_string())?;
    let snaps = db.snapshots_since(since).map_err(|e| e.to_string())?;
    let latest: Map<String, Value> = db.latest_metric_minutes().map_err(|e| e.to_string())?.into_iter().map(|(k, v)| (k, json!(v))).collect();
    Ok(json!({
        "v": 1, "host": host, "at": now, "since": since, "monitor": settings(db).json(),
        "latest": latest, "metrics": rows.iter().map(MinuteRow::json).collect::<Vec<_>>(), "snapshots": snaps,
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
    Ok(
        json!({ "rows": rows.len(), "snapshots": snaps, "hub_at": at, "freshest_minute": freshest, "fresh": freshest.is_some_and(|f| at - f <= HUB_STALE_MS + MINUTE) }),
    )
}

/// ハブの操作卓に置いた CLI（配布の手順は docs/architecture.md）
pub fn hub_command(hub: &Node, since: i64) -> String {
    let exe = if hub.is_windows() { "~/.katala-tune/bin/tune.exe" } else { "~/.katala-tune/bin/tune" };
    format!("{exe} export --since {since}")
}

/// ハブから 1 回写す。結果は meta の hubPull に残す（画面に出す）
pub async fn pull(engine: &Engine) -> Result<Value, String> {
    let s = engine.with_db(|d| Ok(settings(d)))?;
    let hub_id = s.hub.ok_or("ハブを選んでいない")?;
    let cfg = engine.reload_config();
    let hub = cfg.node(&hub_id).ok_or_else(|| format!("台帳に無い機体: {hub_id}"))?.clone();
    let cursor = engine.with_db(|d| d.get_meta("hubCursor"))?.and_then(|v| v.as_i64()).unwrap_or(0);
    let since = cursor.saturating_sub(2 * MINUTE).max(0);
    let t0 = now_ms();
    let r = collect::run("ssh", &collect::ssh_args(&hub.alias, &hub_command(&hub, since)), None, Duration::from_secs(60)).await;
    let res = (|| {
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
        if let Some(at) = x.get("at").and_then(Value::as_i64) {
            engine.with_db(|d| d.set_meta("hubCursor", &json!(at)))?;
        }
        Ok::<Value, String>(got)
    })();
    let mut rec = json!({ "hub": hub_id, "at": now_ms(), "ms": now_ms() - t0 });
    match &res {
        Ok(v) => {
            rec["ok"] = json!(true);
            for k in ["rows", "snapshots", "hub_at", "freshest_minute", "fresh"] {
                rec[k] = v[k].clone();
            }
        }
        Err(e) => {
            rec["ok"] = json!(false);
            rec["error"] = json!(e);
        }
    }
    engine.with_db(|d| d.set_meta("hubPull", &rec))?;
    if res.as_ref().is_ok_and(|v| v["snapshots"].as_i64().unwrap_or(0) > 0) {
        engine.compute_checks(true, true);
    }
    res.map(|_| rec)
}

/// ハブを写していて、ハブの数字が新しい（自分で集めなくてよい）
pub fn hub_fresh(db: &Store, now: i64) -> bool {
    if settings(db).hub.is_none() {
        return false;
    }
    let p = db.get_meta("hubPull").ok().flatten().unwrap_or(Value::Null);
    p.get("ok").and_then(Value::as_bool) == Some(true)
        && p.get("fresh").and_then(Value::as_bool) == Some(true)
        && p.get("at").and_then(Value::as_i64).is_some_and(|at| now - at <= HUB_STALE_MS)
}

/// このアプリで集めるか（常時監視が入っていて、写しているハブが新しくないとき）
pub fn should_collect(db: &Store, now: i64) -> bool {
    settings(db).enabled && !hub_fresh(db, now)
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
    let mut by_node: HashMap<&str, Vec<MinuteRow>> = HashMap::new();
    for r in &rows {
        by_node.entry(r.node_id.as_str()).or_default().push(r.clone());
    }
    let monitoring = s.enabled || s.hub.is_some();
    let mut nodes = Vec::new();
    for n in &cfg.nodes {
        let rs = by_node.remove(n.id.as_str()).unwrap_or_default();
        let snap = engine.with_db(|d| d.last_snapshots(&n.id, 1))?.into_iter().next();
        let findings = engine.with_db(|d| d.snapshot_findings(&n.id))?.unwrap_or_default();
        let count = |s: &str| findings.iter().filter(|f| f.get("severity").and_then(Value::as_str) == Some(s)).count();
        let last = snap.map(|sn| {
            let d = &sn.data;
            let disks: Vec<&Value> = d.get("disk").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
            let low_disk = disks.iter().filter_map(|x| Some((x.get("mount")?.as_str()?, x.get("free_pct")?.as_f64()?))).min_by(|a, b| a.1.total_cmp(&b.1));
            json!({
                "at": sn.at,
                "os": d.pointer("/host/os"), "cpu": d.pointer("/host/cpu"), "model": d.pointer("/host/model"),
                "uptime_h": d.pointer("/host/uptime_h"), "mem_total_gb": d.pointer("/memory/total_gb"),
                "disk_low": low_disk.map(|(m, p)| json!({ "mount": m, "free_pct": p })),
                "defender_realtime": d.pointer("/defender/realtime"),
                "power_plan": d.pointer("/power/plan_name"),
                "bugchecks_7d": d.pointer("/stability_7d/bugcheck_1001"),
                "score": crate::rules::score(&findings), "critical": count("critical"), "warn": count("warn"), "info": count("info"),
            })
        });
        let brief = live.and_then(|l| l.brief(&n.id));
        let mut al = alerts(&rs, now, monitoring && (collecting || s.hub.is_some()));
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
    Ok(json!({
        "at": now, "minutes": minutes, "monitor": s.json(), "collecting": collecting, "hub_pull": pull,
        "local_host": crate::nodes::local_host(), "schedule": engine.schedule(), "nodes": nodes,
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
    }
}
