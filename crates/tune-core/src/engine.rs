//! main.js の画面以外の部分（分析・ログ取り込み・状態の計算・自動スキャンの判断・変更操作の流れ）。
//! 画面・常駐・通知・ダイアログは呼び出し側（src-tauri）が `Host` として渡す。tune-cli も同じものを使う。

use std::collections::HashMap;
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
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
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
        });
        e.reload_config();
        Ok(e)
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
        // 道具の棚卸しの間隔（main.js と同じ既定。棚卸しそのものの移植は別の作業）
        m.insert("inventory_hours".into(), Value::from(DEFAULT_INVENTORY_HOURS));
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
        let data = result.get("data").unwrap_or(&Value::Null);
        let mut findings = rules::analyze(data, node.map_or(&empty, |n| &n.raw));
        findings.extend(logs::log_findings(db, &node_id, now_ms()).unwrap_or_default());
        rules::sort_findings(&mut findings);
        if node.is_some_and(|n| n.shared) {
            rules::block_actions(&mut findings);
        }
        let mut out = result.as_object().cloned().unwrap_or_default();
        out.insert("score".into(), Value::from(rules::score(&findings)));
        out.insert("compare".into(), rules::compare(prev_data, Some(data)));
        out.insert("findings".into(), Value::Array(findings));
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
            let r = json!({ "node_id": n.id, "ok": true, "data": last.data, "at": last.at, "wall_s": last.wall_s });
            let mut full = self.enrich(&db, &cfg, &r, snaps.get(1).map(|s| &s.data));
            if let Value::Object(m) = &mut full {
                m.insert("stale".into(), Value::Bool(true));
            }
            out.push(full);
        }
        Ok(out)
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
                        let snap = json!({ "node_id": n.id, "ok": true, "data": l.data, "at": l.at, "wall_s": l.wall_s });
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
        let (checks, events, lp, ll) = self.with_db(|d| Ok((d.checks()?, d.check_events(150)?, d.get_meta("lastProbeAt")?, d.get_meta("lastLogsAt")?)))?;
        Ok(json!({
            "checks": checks, "events": events, "schedule": self.schedule(),
            "lastProbeAt": lp, "lastLogsAt": ll, "openAtLogin": self.host.open_at_login(),
            "counts": self.status_counts(), "probing": self.is_probing(), "syncing": self.is_syncing(),
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
        for (k, lo, hi) in [("probe_minutes", 5.0, 1440.0), ("logs_minutes", 5.0, 1440.0), ("inventory_hours", 1.0, 168.0)] {
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
            let full = self.enrich(&db, &cfg, r, prev.as_ref().map(|p| &p.data));
            if js::truthy(full.get("ok")) {
                lock(&self.last_probe_error).remove(&node_id);
                let findings: Vec<Value> = js::arr(full.get("findings")).iter().map(|f| json!({ "id": f.get("id"), "severity": f.get("severity") })).collect();
                let data = full.get("data").cloned().unwrap_or(Value::Null);
                let summary = summarize(&data, js::arr(full.get("findings")));
                let at = full.get("at").and_then(Value::as_i64).unwrap_or_else(now_ms);
                let _ = db.add_snapshot(
                    &node_id,
                    at,
                    full.get("wall_s").and_then(Value::as_f64),
                    full.get("score").and_then(Value::as_i64),
                    &Value::Array(findings),
                    &data,
                    &summary,
                );
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

    /// 確認してから変更操作を実行し、実行記録に残す（main.js の confirmAndRun）。
    /// 実行の直前に台帳（共用機の指定・保護リスト）を読み直し、読めなければ実行しない。確認の後にもう一度読み直してから実行する
    pub async fn confirm_and_run(&self, node_id: &str, action: &Value, title: &str, undo_of: Option<&str>, confirm: Confirm) -> Result<Value, String> {
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
        if !confirm(title.to_string(), format!("{}\n\n実行するコマンド:\n{}", p.describe, p.script)).await {
            return Ok(json!({ "ok": false, "cancelled": true }));
        }
        let fresh = match nodes::load_config(&self.config_path) {
            Ok(c) => c,
            Err(e) => return refused(format!("台帳を読めないので実行しない: {e}")),
        };
        let Some(node) = fresh.node(node_id) else { return refused("台帳に無い機体".into()) };
        let r = match actions::execute(node, action, Some(&fresh.protect)).await {
            Ok(r) => r,
            Err(e) => return refused(e),
        };
        let entry = json!({
            "id": format!("{}-{}", now_ms(), rand36(5)), "at": now_ms(), "node_id": node.id,
            "type": action.get("type"), "params": action.get("params"), "label": title, "ok": r.ok,
            "output": js::trim(&format!("{}\n{}", r.outcome, r.output)), "undo": r.undo, "undo_of": undo_of,
        });
        // 書き込みの失敗は成功扱いにしない（画面へエラーとして返す）
        self.with_db(|d| d.add_action(&entry)).map_err(|e| format!("実行記録を書けなかった（操作は実行済み: {}）: {e}", r.outcome))?;
        Ok(json!({ "ok": r.ok, "code": r.code, "outcome": r.outcome, "output": r.output, "undo": r.undo, "entry": entry }))
    }

    /// 実行記録から元に戻す
    pub async fn undo(&self, entry_id: &str, confirm: Confirm) -> Result<Value, String> {
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
        self.confirm_and_run(&node_id, &undo, &title, Some(entry_id), confirm).await
    }
}

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
