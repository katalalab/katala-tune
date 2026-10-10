//! main.js の画面以外の部分（分析・ログ取り込み・状態の計算・自動スキャンの判断・変更操作の流れ）。
//! 画面・常駐・通知・ダイアログは呼び出し側（src-tauri）が `Host` として渡す。tune-cli も同じものを使う。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::actions;
use crate::collect;
use crate::db::{Change, Check, Store, now_ms};
use crate::health;
use crate::js::{self, Obj};
use crate::logs;
use crate::nodes::{self, Config, Node};
use crate::rules;

/// 呼び出し側（アプリ）への知らせ。どれも既定では何もしない
pub trait Host: Send + Sync + 'static {
    /// 1台の分析が終わった（画面の `onProbeResult`）
    fn probe_result(&self, _r: &Value) {}
    /// 1台のログ取り込みが終わった（画面の `onLogsSynced`）
    fn logs_synced(&self, _r: &Value) {}
    /// 状態が更新された（画面の `onChecksUpdated`）
    fn checks_updated(&self) {}
    /// 異常になった・異常から戻った（通知）
    fn notify(&self, _important: &[Change]) {}
    /// 分析中・取り込み中・件数が変わった（メニューバーの表示）
    fn state_changed(&self) {}
    /// ログイン時に起動する設定か
    fn open_at_login(&self) -> bool {
        false
    }
    /// 1台の道具の棚卸しが終わった（画面の `onInventoryResult`）
    fn inventory_result(&self, _r: &Value) {}
    /// 1台の AI エージェントのセッションの取り込みが終わった（画面の `onAiSynced`）
    fn ai_synced(&self, _r: &Value) {}
}

/// 何もしない Host（CLI・テスト用）
pub struct NoHost;
impl Host for NoHost {}

/// 確認ダイアログ。(見出し, 本文) を見せ、操作者が「実行する」を選んだときだけ true
pub type Confirm = Box<dyn FnOnce(String, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> + Send>;

pub const DEFAULT_PROBE_MINUTES: i64 = 60;
pub const DEFAULT_LOGS_MINUTES: i64 = 15;
pub const DEFAULT_INVENTORY_HOURS: i64 = 24;

pub struct Engine {
    host: Arc<dyn Host>,
    pub db: Arc<Mutex<Store>>,
    cfg: RwLock<(Config, Option<String>)>,
    config_path: PathBuf,
    data_dir: PathBuf,
    probing: AtomicBool,
    syncing: AtomicBool,
    last_probe_error: Mutex<HashMap<String, String>>,
    fleet: Mutex<(Option<Value>, Option<i64>)>,
    action_targets: Mutex<HashSet<String>>,
    /// 道具の棚卸し・AI の取り込みの状態（engine_tools.rs）
    pub(crate) tools: crate::engine_tools::ToolsState,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct ActionTargetLock<'a> {
    targets: &'a Mutex<HashSet<String>>,
    node_id: String,
}

impl Drop for ActionTargetLock<'_> {
    fn drop(&mut self) {
        lock(self.targets).remove(&self.node_id);
    }
}

/// main.js の summarize。履歴の表に入れる要約
pub fn summarize(data: &Value, findings: &[Value]) -> Value {
    let d = Some(data);
    let pick = |v: Option<&Value>| js::nullish(v, None).cloned().unwrap_or(Value::Null);
    let count = |s: &str| findings.iter().filter(|f| js::is_str(f.get("severity"), s)).count();
    json!({
        "bench_ms": pick(js::get(js::get(d, "bench"), "median_ms")),
        "cpu": pick(js::get(d, "cpu_busy")),
        "mem_avail": pick(js::get(js::get(d, "memory"), "available_pct")),
        "swap_gb": pick(js::get(js::get(d, "memory"), "swap_used_gb")),
        "critical": count("critical"),
        "warn": count("warn"),
    })
}

impl Engine {
    /// 台帳（無ければ見本を置く）と DB を開く。以前の JSON Lines があれば一度だけ取り込む
    pub fn open(config_path: PathBuf, data_dir: PathBuf, host: Arc<dyn Host>) -> Result<Arc<Engine>, String> {
        let _ = nodes::ensure_config(&config_path);
        let store = Store::open(&data_dir).map_err(|e| format!("DB を開けない（{}）: {e}", data_dir.display()))?;
        let _ = store.import_legacy(&data_dir, |e| summarize(e.get("data").unwrap_or(&Value::Null), js::arr(e.get("findings"))));
        let e = Arc::new(Engine {
            host,
            db: Arc::new(Mutex::new(store)),
            cfg: RwLock::new((Config::default(), None)),
            config_path,
            data_dir,
            probing: AtomicBool::new(false),
            syncing: AtomicBool::new(false),
            last_probe_error: Mutex::new(HashMap::new()),
            fleet: Mutex::new((None, None)),
            action_targets: Mutex::new(HashSet::new()),
            tools: Default::default(),
        });
        e.reload_config();
        Ok(e)
    }

    /// 呼び出し側への知らせ（engine_tools.rs から使う）
    pub(crate) fn host(&self) -> &Arc<dyn Host> {
        &self.host
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn is_probing(&self) -> bool {
        self.probing.load(Ordering::SeqCst)
    }

    pub fn is_syncing(&self) -> bool {
        self.syncing.load(Ordering::SeqCst)
    }

    /// 台帳を読み直す。読めなければ前の台帳のまま、エラーを覚えておく
    pub fn reload_config(&self) -> Config {
        let mut g = self.cfg.write().unwrap_or_else(PoisonError::into_inner);
        match nodes::load_config(&self.config_path) {
            Ok(c) => *g = (c, None),
            Err(e) => g.1 = Some(e),
        }
        g.0.clone()
    }

    pub fn config(&self) -> Config {
        self.cfg.read().unwrap_or_else(PoisonError::into_inner).0.clone()
    }

    pub fn config_error(&self) -> Option<String> {
        self.cfg.read().unwrap_or_else(PoisonError::into_inner).1.clone()
    }

    /// DB を短く借りる
    pub fn with_db<T>(&self, f: impl FnOnce(&Store) -> rusqlite::Result<T>) -> Result<T, String> {
        f(&lock(&self.db)).map_err(|e| e.to_string())
    }

    /// 自動スキャンの設定（既定 → 台帳の schedule → 画面で変えた値 の順に上書き）
    pub fn schedule(&self) -> Value {
        let mut m = Map::new();
        m.insert("enabled".into(), Value::Bool(true));
        m.insert("probe_minutes".into(), Value::from(DEFAULT_PROBE_MINUTES));
        m.insert("logs_minutes".into(), Value::from(DEFAULT_LOGS_MINUTES));
        // 道具の棚卸しの間隔（main.js と同じ既定）と、AI エージェントのセッションの取り込みの間隔（engine_tools.rs）
        m.insert("inventory_hours".into(), Value::from(DEFAULT_INVENTORY_HOURS));
        m.insert("ai_minutes".into(), Value::from(crate::ai_sessions::DEFAULT_AI_MINUTES));
        if let Some(Value::Object(c)) = self.config().schedule {
            m.extend(c);
        }
        if let Ok(Some(Value::Object(s))) = self.with_db(|d| d.get_meta("schedule")) {
            m.extend(s);
        }
        Value::Object(m)
    }

    /// snapshot に所見（probe 由来＋ログ由来）と前回比を付ける（main.js の enrich）
    pub fn enrich(&self, db: &Store, cfg: &Config, result: &Value, prev_data: Option<&Value>) -> Value {
        if !js::truthy(result.get("ok")) {
            return result.clone();
        }
        let node_id = js::string(result.get("node_id"));
        let node = cfg.node(&node_id);
        let empty = Value::Object(Map::new());
        let node_raw = node.map_or(&empty, |n| &n.raw);
        let data = crate::netsec::snapshot_for_node(result.get("data").cloned().unwrap_or(Value::Null), node_raw);
        let prev_data = prev_data.map(|d| crate::netsec::snapshot_for_node(d.clone(), node_raw));
        let mut findings = rules::analyze(&data, node_raw);
        findings.extend(crate::netsec::filter_log_findings(logs::log_findings(db, &node_id, now_ms()).unwrap_or_default(), node_raw));
        rules::sort_findings(&mut findings);
        if node.is_some_and(|n| n.shared) {
            rules::block_actions(&mut findings);
        }
        let mut out = result.as_object().cloned().unwrap_or_default();
        out.insert("score".into(), Value::from(rules::score(&findings)));
        out.insert("compare".into(), rules::compare(prev_data.as_ref(), Some(&data)));
        out.insert("findings".into(), Value::Array(findings));
        out.insert("data".into(), data);
        Value::Object(out)
    }

    /// 前回の分析結果（画面の `last`）。機体ごとに最新の snapshot と所見
    pub fn last(&self) -> Result<Vec<Value>, String> {
        let cfg = self.config();
        let db = lock(&self.db);
        let mut out = Vec::new();
        for n in &cfg.nodes {
            let snaps = db.last_snapshots(&n.id, 2).map_err(|e| e.to_string())?;
            let Some(last) = snaps.first() else { continue };
            let data = crate::netsec::snapshot_for_node(last.data.clone(), &n.raw);
            let prev_data = snaps.get(1).map(|s| crate::netsec::snapshot_for_node(s.data.clone(), &n.raw));
            let r = json!({ "node_id": n.id, "ok": true, "data": data, "at": last.at, "wall_s": last.wall_s });
            let mut full = self.enrich(&db, &cfg, &r, prev_data.as_ref());
            if let Value::Object(m) = &mut full {
                m.insert("stale".into(), Value::Bool(true));
            }
            out.push(full);
        }
        Ok(out)
    }

    pub fn logs_query(&self, filter: &Value) -> Result<Vec<Value>, String> {
        let nodes: Vec<Value> = self.config().nodes.into_iter().map(|n| n.raw).collect();
        self.with_db(|d| d.query_logs(filter)).map(|rows| crate::netsec::filter_log_rows(rows, &nodes))
    }

    pub fn logs_signatures(&self, filter: &Value) -> Result<Vec<Value>, String> {
        let nodes: Vec<Value> = self.config().nodes.into_iter().map(|n| n.raw).collect();
        self.with_db(|d| d.signatures(filter)).map(|rows| crate::netsec::filter_log_signatures(rows, &nodes))
    }

    pub fn logs_cursors(&self) -> Result<Vec<Value>, String> {
        let nodes: Vec<Value> = self.config().nodes.into_iter().map(|n| n.raw).collect();
        self.with_db(|d| d.cursors()).map(|rows| crate::netsec::filter_log_rows(rows, &nodes))
    }

    /// 状態（機能チェック）を計算して保存する。
    /// notify: 異常化・回復を知らせるか。broadcast: 画面へ「更新された」を送るか（画面からの問い合わせでは送らない。送ると往復し続ける）
    pub fn compute_checks(&self, notify: bool, broadcast: bool) -> Vec<Change> {
        let now = now_ms();
        let cfg = self.config();
        let sch = self.schedule();
        let open_at_login = self.host.open_at_login();
        let errors = lock(&self.last_probe_error).clone();
        let (fleet, fleet_at) = lock(&self.fleet).clone();
        let mut changed = Vec::new();
        {
            let db = lock(&self.db);
            let cursors = db.cursors().unwrap_or_default();
            for n in &cfg.nodes {
                let last = db.last_snapshots(&n.id, 1).ok().and_then(|v| v.into_iter().next());
                let (snap, findings) = match &last {
                    Some(l) => {
                        let data = crate::netsec::snapshot_for_node(l.data.clone(), &n.raw);
                        let snap = json!({ "node_id": n.id, "ok": true, "data": data, "at": l.at, "wall_s": l.wall_s });
                        let full = self.enrich(&db, &cfg, &snap, None);
                        (Some(snap), js::arr(full.get("findings")).to_vec())
                    }
                    None => (None, Vec::new()),
                };
                let ctx = Obj::new()
                    .set("now", now)
                    .set("cursors", Value::Array(cursors.clone()))
                    .opt("expect", n.get("expect").cloned())
                    .set("schedule", sch.clone())
                    .opt("lastError", errors.get(&n.id).cloned().map(Value::from))
                    .build();
                let checks = health::node_checks(&n.raw, snap.as_ref(), &findings, &ctx);
                changed.extend(db.save_checks(&n.id, &checks, now).unwrap_or_default());
            }
            let (check, bytes) = db.integrity().unwrap_or_else(|e| (e.to_string(), 0));
            let scheduler = {
                let mut m = sch.as_object().cloned().unwrap_or_default();
                m.insert("lastProbeAt".into(), db.get_meta("lastProbeAt").ok().flatten().unwrap_or(Value::Null));
                m.insert("lastLogsAt".into(), db.get_meta("lastLogsAt").ok().flatten().unwrap_or(Value::Null));
                Value::Object(m)
            };
            let ctx = Obj::new()
                .set("now", now)
                .opt("configError", self.config_error().map(Value::from))
                .set("example", cfg.is_example())
                .set("nodeCount", cfg.nodes.len())
                .set("protectCount", cfg.protect.len())
                .set("dbCheck", check)
                .set("dbBytes", bytes)
                .set("scheduler", scheduler)
                .opt("fleet", cfg.fleet.as_ref().map(|_| fleet.clone().unwrap_or(Value::Null)))
                .opt("fleetAt", fleet_at.map(Value::from))
                .set("openAtLogin", open_at_login)
                .build();
            let app: Vec<Check> = health::app_checks(&ctx);
            changed.extend(db.save_checks("_app", &app, now).unwrap_or_default());
        }
        if notify {
            let important: Vec<Change> = changed.iter().filter(|c| c.from.is_some()).filter(|c| is_important(c)).cloned().collect();
            if !important.is_empty() {
                self.host.notify(&important);
            }
        }
        self.host.state_changed();
        if broadcast {
            self.host.checks_updated();
        }
        changed
    }

    /// 状態ごとの件数
    pub fn status_counts(&self) -> Value {
        json!(self.with_db(|d| d.status_counts()).unwrap_or_default())
    }

    /// 画面の「状態」（問い合わせでは更新通知を送らない）
    pub fn status(&self) -> Result<Value, String> {
        self.compute_checks(false, false);
        let cfg = self.config();
        let nodes: Vec<Value> = cfg.nodes.into_iter().map(|n| n.raw).collect();
        let (checks, events, lp, ll) = self.with_db(|d| Ok((d.checks()?, d.check_events(150)?, d.get_meta("lastProbeAt")?, d.get_meta("lastLogsAt")?)))?;
        let checks = crate::netsec::filter_check_rows(checks, &nodes);
        let events = crate::netsec::filter_check_rows(events, &nodes);
        let mut counts = std::collections::BTreeMap::from([("ok", 0_i64), ("warn", 0), ("fail", 0), ("unknown", 0)]);
        for row in &checks {
            if let Some(status) = row.get("status").and_then(Value::as_str) {
                *counts.entry(status).or_default() += 1;
            }
        }
        Ok(json!({
            "checks": checks, "events": events, "schedule": self.schedule(),
            "lastProbeAt": lp, "lastLogsAt": ll, "openAtLogin": self.host.open_at_login(),
            "counts": counts, "probing": self.is_probing(), "syncing": self.is_syncing(),
        }))
    }

    /// 画面で自動スキャンの設定を変える（5〜1440 分の整数だけ受け付ける）
    pub fn set_schedule(&self, patch: &Value) -> Result<Value, String> {
        let mut s = match self.with_db(|d| d.get_meta("schedule"))? {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        };
        if let Some(Value::Bool(b)) = patch.get("enabled") {
            s.insert("enabled".into(), Value::Bool(*b));
        }
        for (k, lo, hi) in [("probe_minutes", 5.0, 1440.0), ("logs_minutes", 5.0, 1440.0), ("inventory_hours", 1.0, 168.0), ("ai_minutes", 5.0, 1440.0)] {
            if let Some(v) = patch.get(k).and_then(Value::as_f64).filter(|v| v.fract() == 0.0 && (lo..=hi).contains(v)) {
                s.insert(k.into(), Value::from(v as i64));
            }
        }
        self.with_db(|d| d.set_meta("schedule", &Value::Object(s)))?;
        self.compute_checks(false, false);
        Ok(self.schedule())
    }

    /// 全機（ids が空）か指定の機体を分析する。実行中なら `{ busy: true }`
    pub async fn run_probe(self: &Arc<Self>, ids: Option<Vec<String>>, auto: bool) -> Value {
        if self.probing.swap(true, Ordering::SeqCst) {
            return json!({ "busy": true });
        }
        self.host.state_changed();
        let cfg = self.reload_config();
        let all = ids.as_ref().is_none_or(Vec::is_empty);
        let targets: Vec<Node> = cfg.nodes.iter().filter(|n| all || ids.as_ref().is_some_and(|i| i.contains(&n.id))).cloned().collect();
        let me = self.clone();
        let results = collect::probe_all(&targets, move |r| me.on_probe_result(r)).await;
        if all {
            let _ = self.with_db(|d| d.set_meta("lastProbeAt", &Value::from(now_ms())));
        }
        self.probing.store(false, Ordering::SeqCst);
        self.compute_checks(true, true);
        json!({ "done": results.len(), "auto": auto })
    }

    fn on_probe_result(&self, r: &Value) {
        let cfg = self.config();
        let node_id = js::string(r.get("node_id"));
        let full = {
            let db = lock(&self.db);
            let prev = db.last_snapshots(&node_id, 1).ok().and_then(|v| v.into_iter().next());
            // ネットワークとセキュリティ: 正規化・前回の待ち受けとの比較・常駐の増減・初めての接続先（宛先は DB にだけ置く。netsec.rs）
            let r = &crate::netsec::ingest(&db, cfg.node(&node_id), r, prev.as_ref().map(|p| &p.data));
            let mut full = self.enrich(&db, &cfg, r, prev.as_ref().map(|p| &p.data));
            if js::truthy(full.get("ok")) {
                let findings: Vec<Value> = js::arr(full.get("findings")).iter().map(|f| json!({ "id": f.get("id"), "severity": f.get("severity") })).collect();
                let data = full.get("data").cloned().unwrap_or(Value::Null);
                let summary = summarize(&data, js::arr(full.get("findings")));
                let at = full.get("at").and_then(Value::as_i64).unwrap_or_else(now_ms);
                let saved = db.add_snapshot(
                    &node_id,
                    at,
                    full.get("wall_s").and_then(Value::as_f64),
                    full.get("score").and_then(Value::as_i64),
                    &Value::Array(findings),
                    &data,
                    &summary,
                );
                // 保存できなかった分析は成功として報告しない（状態の「分析」も異常にする）
                match saved {
                    Ok(_) => {
                        lock(&self.last_probe_error).remove(&node_id);
                    }
                    Err(e) => {
                        let msg = format!("分析は終わったが保存できなかった: {e}");
                        lock(&self.last_probe_error).insert(node_id.clone(), msg.clone());
                        if let Value::Object(m) = &mut full {
                            m.insert("ok".into(), Value::Bool(false));
                            m.insert("error".into(), Value::String(msg));
                        }
                    }
                }
            } else {
                lock(&self.last_probe_error).insert(node_id, js::string(full.get("error")));
            }
            full
        };
        self.host.probe_result(&full);
    }

    /// ログを取り込む。実行中なら `{ busy: true }`
    pub async fn sync_logs(self: &Arc<Self>, targets: Vec<Node>) -> Value {
        if self.syncing.swap(true, Ordering::SeqCst) {
            return json!({ "busy": true });
        }
        self.host.state_changed();
        let t0 = now_ms();
        let host = self.host.clone();
        let results = logs::sync_all(self.db.clone(), &targets, move |r| host.logs_synced(r)).await;
        let _ = self.with_db(|d| d.prune(now_ms()));
        if targets.len() == self.config().nodes.len() {
            let _ = self.with_db(|d| d.set_meta("lastLogsAt", &Value::from(now_ms())));
        }
        self.syncing.store(false, Ordering::SeqCst);
        self.compute_checks(true, true);
        json!({ "done": results.len(), "ms": now_ms() - t0, "results": results })
    }

    /// 1分ごとに呼ぶ。期限が来たものだけ動かす（スリープ明けでも溜まった分を1回で済ませる）
    pub async fn tick(self: &Arc<Self>) {
        let s = self.schedule();
        if !js::truthy(s.get("enabled")) || self.is_probing() || self.is_syncing() {
            return;
        }
        let now = now_ms() as f64;
        let meta = |k: &str| self.with_db(|d| d.get_meta(k)).ok().flatten();
        let since = |k: &str| now - js::num(js::or(meta(k).as_ref(), Some(&Value::from(0))));
        let mins = |k: &str| js::num(s.get(k)) * 60000.0;
        if since("lastProbeAt") >= mins("probe_minutes") {
            self.run_probe(None, true).await;
            let nodes = self.reload_config().nodes;
            self.sync_logs(nodes).await;
        } else if since("lastLogsAt") >= mins("logs_minutes") {
            let nodes = self.reload_config().nodes;
            self.sync_logs(nodes).await;
        }
        // 道具の棚卸しと AI エージェントのセッションの取り込み（engine_tools.rs）
        self.tick_tools().await;
    }

    /// katala-fleet の概況（任意）。op-agent が秘密を子プロセスにだけ渡すので、この画面には値が来ない
    pub async fn fleet(&self) -> Value {
        let Some(f) = self.config().fleet else { return json!({ "disabled": true }) };
        let repo = PathBuf::from(&f.repo);
        let cache = if !repo.join(&f.env_file).exists() {
            json!({ "error": format!("katala-fleet が見つからない: {}", f.repo) })
        } else {
            let mut c = collect::command("op-agent");
            c.args(["run", &format!("--env-file={}", f.env_file), "--", "python3", "scripts/fleet-status.py", "--json"])
                .current_dir(&repo)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);
            let res = tokio::time::timeout(Duration::from_secs(60), c.output()).await;
            let (code, out, err) = match res {
                Ok(Ok(o)) => (o.status.code().map_or("null".into(), |c| c.to_string()), collect::decode(&o.stdout), collect::decode(&o.stderr)),
                Ok(Err(e)) => ("null".into(), String::new(), e.to_string()),
                Err(_) => ("null".into(), String::new(), String::new()),
            };
            match serde_json::from_str::<Value>(&out).ok().filter(|v| v.get("nodes").is_some_and(Value::is_array)) {
                Some(v) => {
                    let pick = ["node_id", "state", "last_seen", "metrics", "failing", "running_containers", "agent_processes", "os"];
                    let nodes: Vec<Value> = js::arr(v.get("nodes"))
                        .iter()
                        .map(|n| Value::Object(pick.iter().filter_map(|k| n.get(*k).map(|x| ((*k).to_string(), x.clone()))).collect()))
                        .collect();
                    Obj::new()
                        .opt("now", v.get("now").cloned())
                        .opt("summary", v.get("summary").cloned())
                        .set("nodes", nodes)
                        .opt("attention", v.get("attention").cloned())
                        .build()
                }
                None => {
                    let e = if err.is_empty() { format!("exit {code}") } else { err };
                    let lines: Vec<&str> = js::trim(&e).split('\n').collect();
                    json!({ "error": lines[lines.len().saturating_sub(3)..].join("\n") })
                }
            }
        };
        *lock(&self.fleet) = (Some(cache.clone()), Some(now_ms()));
        cache
    }

    /// 確認してから変更操作を実行し、実行記録に残す（lib/runner.js の confirmAndRun と同じ流れ）。
    /// 承認が無ければ実行しない。確認の後にもう一度台帳を読み直し、接続先が変わっていたり共用機・保護対象になっていたら実行しない（理由を実行記録に残す）。
    /// 実行の直前に「未完了」の記録を書く（書けなければ実行しない）。結果を書く前にアプリが止まっても、成功としては残らない
    pub async fn confirm_and_run(&self, node_id: &str, action: &Value, title: &str, undo_of: Option<&str>, confirm: Confirm) -> Result<Value, String> {
        self.confirm_and_run_with(&collect::System, node_id, action, title, undo_of, confirm).await
    }

    /// `confirm_and_run` の子プロセスの実行口を差し替えられる版（テストは偽の実行器を渡す）
    pub async fn confirm_and_run_with(
        &self,
        runner: &dyn collect::Runner,
        node_id: &str,
        action: &Value,
        title: &str,
        undo_of: Option<&str>,
        confirm: Confirm,
    ) -> Result<Value, String> {
        let _target_lock = {
            let mut targets = lock(&self.action_targets);
            if !targets.insert(node_id.to_string()) {
                return Ok(json!({ "ok": false, "refused": "この機体では別の変更操作を確認または実行中" }));
            }
            ActionTargetLock { targets: &self.action_targets, node_id: node_id.to_string() }
        };
        // MutexGuard を await をまたいで保持せず、確認から実行記録までを機体単位で直列化する。
        let result = async {
            let refused = |m: String| Ok(json!({ "ok": false, "refused": m }));
            let fresh = match nodes::load_config(&self.config_path) {
                Ok(c) => c,
                Err(e) => return refused(format!("台帳を読めないので実行しない: {e}")),
            };
            let Some(node) = fresh.node(node_id) else { return refused("台帳に無い機体".into()) };
            let p = match actions::plan(node, action, Some(&fresh.protect)) {
                Ok(p) => p,
                Err(e) => return refused(e),
            };
            // 承認した接続先（どの機体に、どのシェルで送るか）。確認の後に台帳を読み直したとき、これが変わっていたら実行しない
            let route = (node.alias.clone(), node.os.clone(), node.local);
            if !confirm(title.to_string(), format!("{}\n\n実行するコマンド:\n{}", p.describe, p.script)).await {
                return Ok(json!({ "ok": false, "cancelled": true }));
            }
            // 承認したあとで止めるときは、理由を実行記録に残す（実行していない）。記録できなくても止めたことは変わらない
            let abort = |m: String| {
                let entry = json!({
                    "id": format!("{}-{}", now_ms(), rand36(5)), "at": now_ms(), "node_id": node_id,
                    "type": action.get("type"), "params": action.get("params"), "label": title, "ok": false,
                    "output": format!("中止（実行していない）: {m}"), "undo": null, "undo_of": undo_of,
                });
                let _ = self.with_db(|d| d.add_action(&entry));
                refused(m)
            };
            let fresh = match nodes::load_config(&self.config_path) {
                Ok(c) => c,
                Err(e) => return abort(format!("台帳を読めないので実行しない: {e}")),
            };
            let Some(node) = fresh.node(node_id) else { return abort("台帳に無い機体".into()) };
            if (node.alias.clone(), node.os.clone(), node.local) != route {
                return abort("確認のあいだに台帳の接続先（alias・OS・この機体かどうか）が変わったので実行しない。もう一度確認してください".into());
            }
            // 共用機・保護リスト・引数は、読み直した台帳でもう一度確かめる（実行するコマンドは接続先と操作で決まるので、承認したものと同じ）
            if let Err(e) = actions::plan(node, action, Some(&fresh.protect)) {
                return abort(e);
            }
            // 未完了の記録を先に書く。書けなければ実行しない
            let id = format!("{}-{}", now_ms(), rand36(5));
            let mut entry = json!({
                "id": id, "at": now_ms(), "node_id": node.id,
                "type": action.get("type"), "params": action.get("params"), "label": title, "ok": null,
                "output": PENDING_OUTPUT, "undo": null, "undo_of": undo_of,
            });
            if let Err(e) = self.with_db(|d| d.add_action(&entry)) {
                return refused(format!("実行記録を書けないので実行しない: {e}"));
            }
            let r = match actions::execute_with(runner, node, action, Some(&fresh.protect)).await {
                Ok(r) => r,
                Err(e) => {
                    let output = format!("実行できなかった: {e}");
                    let _ = self.with_db(|d| d.finish_action(&id, false, &output, None));
                    return refused(output);
                }
            };
            let output = js::trim(&format!("{}\n{}", r.outcome, r.output)).to_string();
            // 書き込みの失敗は成功扱いにしない（画面へエラーとして返す）。記録は未完了のまま残る
            // 未更新（false）も書けなかったのと同じ。記録が未完了でなくなっていて、結果を書き込めていない
            match self.with_db(|d| d.finish_action(&id, r.ok, &output, r.undo.as_ref())) {
                Ok(true) => {}
                Ok(false) => {
                    return Err(format!("実行記録を書けなかった（操作は実行済み: {}）: 記録がすでに未完了ではなく、結果を書き込めなかった", r.outcome));
                }
                Err(e) => return Err(format!("実行記録を書けなかった（操作は実行済み: {}）: {e}", r.outcome)),
            }
            entry["ok"] = Value::Bool(r.ok);
            entry["output"] = Value::String(output);
            entry["undo"] = r.undo.clone().unwrap_or(Value::Null);
            Ok(json!({ "ok": r.ok, "code": r.code, "outcome": r.outcome, "output": r.output, "undo": r.undo, "entry": entry }))
        }
        .await;
        result
    }

    /// 実行記録から元に戻す
    pub async fn undo(&self, entry_id: &str, confirm: Confirm) -> Result<Value, String> {
        self.undo_with(&collect::System, entry_id, confirm).await
    }

    /// `undo` の子プロセスの実行口を差し替えられる版
    pub async fn undo_with(&self, runner: &dyn collect::Runner, entry_id: &str, confirm: Confirm) -> Result<Value, String> {
        let all = self.with_db(|d| d.actions(1000))?;
        let Some(entry) = all.iter().find(|a| js::is_str(a.get("id"), entry_id)) else {
            return Ok(json!({ "ok": false, "refused": "元に戻せる記録が無い" }));
        };
        let undo = entry.get("undo").filter(|u| js::truthy(Some(u))).cloned();
        let Some(undo) = undo else { return Ok(json!({ "ok": false, "refused": "元に戻せる記録が無い" })) };
        if all.iter().any(|a| js::is_str(a.get("undo_of"), entry_id) && js::truthy(a.get("ok"))) {
            return Ok(json!({ "ok": false, "refused": "すでに元に戻した" }));
        }
        let node_id = js::string(entry.get("node_id"));
        let title = format!("{node_id}: 元に戻す（{}）", js::string(entry.get("label")));
        self.confirm_and_run_with(runner, &node_id, &undo, &title, Some(entry_id), confirm).await
    }
}

/// 実行を始めたが結果をまだ書いていない記録の説明（lib/runner.js の PENDING_OUTPUT と同じ）
pub const PENDING_OUTPUT: &str = "実行を開始した。結果は未確認（アプリが途中で止まった場合は、機体の状態を確かめてから必要なら手で戻す）";

/// 悪くなって異常（fail）になったとき、異常から戻ったときだけ知らせる（同じ状態が続いても繰り返さない）
pub fn is_important(c: &Change) -> bool {
    c.to == "fail" || (c.from.as_deref() == Some("fail") && c.to == "ok")
}

/// 通知の文面（main.js の notifyChanges）。(見出し, 本文)
pub fn notification_text(important: &[Change]) -> (String, String) {
    let body = important
        .iter()
        .take(4)
        .map(|c| format!("{}: {} {}", if c.scope == "_app" { "アプリ" } else { &c.scope }, c.name, if c.to == "fail" { "異常" } else { "回復" }))
        .collect::<Vec<_>>()
        .join("\n");
    (format!("Katala Tune: {} 件の状態変化", important.len()), body + if important.len() > 4 { "\n…" } else { "" })
}

/// base36 の乱数（実行記録の id の後半。衝突を避けるだけで秘密ではない）
fn rand36(n: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(now_ms());
    let mut x = h.finish();
    (0..n)
        .map(|_| {
            let d = (x % 36) as u32;
            x /= 36;
            char::from_digit(d, 36).unwrap_or('0')
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_does_not_return_legacy_peer_addresses_after_opt_out() {
        let tmp = std::env::temp_dir().join(format!("katala-tune-netsec-test-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&tmp).unwrap();
        let config = tmp.join("nodes.json");
        std::fs::write(&config, r#"{"nodes":[{"id":"shared","alias":"unused","os":"macos","shared":true}]}"#).unwrap();
        let engine = Engine::open(config, tmp.join("data"), Arc::new(NoHost)).unwrap();
        engine
            .with_db(|d| {
                d.add_snapshot(
                    "shared",
                    1,
                    Some(0.1),
                    Some(100),
                    &json!([]),
                    &json!({ "netsec": { "outbound": [{ "addr": "192.0.2.8" }], "peers": { "new": [{ "addr": "192.0.2.9", "proc": "app" }] } } }),
                    &json!({}),
                )
            })
            .unwrap();
        let last = engine.last().unwrap();
        let text = serde_json::to_string(&last).unwrap();
        assert!(!text.contains("192.0.2."));
        assert!(last[0]["data"]["netsec"].get("peers").is_none());
        drop(engine);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn public_results_apply_current_network_policy_to_stored_data() {
        let tmp = std::env::temp_dir().join(format!("katala-tune-netsec-policy-test-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&tmp).unwrap();
        let config = tmp.join("nodes.json");
        std::fs::write(&config, r#"{"nodes":[{"id":"private","alias":"unused","os":"windows","network":false}]}"#).unwrap();
        let engine = Engine::open(config, tmp.join("data"), Arc::new(NoHost)).unwrap();
        let now = now_ms();
        engine
            .with_db(|d| {
                d.add_snapshot(
                    "private",
                    now,
                    Some(0.1),
                    Some(100),
                    &json!([]),
                    &json!({ "probe": "windows", "netsec": { "os": "windows", "listen": [], "peers": { "new": [{ "proc": "app", "port": 443 }] } } }),
                    &json!({}),
                )?;
                let rows: Vec<Value> = (0..11)
                    .map(|i| {
                        json!({ "uid": format!("login-{i}"), "ts": now - i * 1000, "level": "warn", "provider": "192.0.2.1", "event_id": "4625", "message": "failed login" })
                    })
                    .collect();
                d.insert_logs("private", "win_security", &crate::logs::normalize("win_security", &rows), now)?;
                d.cursor_ok("private", "win_security", Some(&json!("11")), 11, &json!(0), None)?;
                d.save_checks(
                    "private",
                    &[crate::db::Check { id: "sec-login".into(), name: "login".into(), status: "fail".into(), detail: Some("192.0.2.1".into()) }],
                    now,
                )?;
                Ok(())
            })
            .unwrap();

        let last = engine.last().unwrap();
        assert!(last[0]["data"].get("netsec").is_none());
        assert!(!serde_json::to_string(&last).unwrap().contains("log-login-"));
        assert!(engine.logs_query(&json!({})).unwrap().is_empty());
        assert!(engine.logs_signatures(&json!({ "since": 0 })).unwrap().is_empty());
        assert!(engine.logs_cursors().unwrap().is_empty());
        let status = engine.status().unwrap();
        let status_text = serde_json::to_string(&status).unwrap();
        assert!(!status_text.contains("sec-login"), "{status_text}");
        assert!(!status_text.contains("192.0.2.1"));
        drop(engine);
        let _ = std::fs::remove_dir_all(tmp);
    }
}
