//! 道具の棚卸し・Do-gu・AI エージェントのセッションの流れ（main.js の runInventory・inventoryView・dogu-* と、AI の取り込み・集計）。
//! [`Engine`] のメソッドとして足す（engine.rs は分析・ログ・状態・変更操作の流れ）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use crate::ai_sessions::{self, CATCH_UP_MINUTES, Fetched};
use crate::db::now_ms;
use crate::dogu::{self, Http, Matcher};
use crate::engine::{Confirm, Engine};
use crate::inventory::{self, items_of, matrix};
use crate::js;
use crate::nodes::{Config, Node};

/// 棚卸し・AI の取り込みの実行中の印と、機体ごとの前回のエラー
#[derive(Default)]
pub struct ToolsState {
    inventorying: AtomicBool,
    ai_syncing: AtomicBool,
    last_inventory_error: Mutex<HashMap<String, String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

static NEXT_DOGU_PUBLISH_ID: AtomicU64 = AtomicU64::new(0);

fn dogu_publish_action_id(at: i64) -> String {
    format!("{at}-dogu-{}-{}", std::process::id(), NEXT_DOGU_PUBLISH_ID.fetch_add(1, Ordering::Relaxed))
}

/// 今の台帳で AI のセッションを取り込む機体。「続きあり」はこの機体の続きの位置だけで判定する
/// （台帳から外した機体・`ai_sessions: false` にした機体の古い位置で、空の取り込みを 2 分ごとに回し続けない）
fn ai_node_ids(cfg: &Config) -> Vec<&str> {
    cfg.nodes.iter().filter(|n| ai_sessions::enabled(n)).map(|n| n.id.as_str()).collect()
}

impl Engine {
    pub fn is_inventorying(&self) -> bool {
        self.tools.inventorying.load(Ordering::SeqCst)
    }

    pub fn is_ai_syncing(&self) -> bool {
        self.tools.ai_syncing.load(Ordering::SeqCst)
    }

    // ---- 道具の棚卸し ----

    /// 道具の棚卸し（読み取り専用）。変化は inventory_events に残る。実行中なら `{ busy: true }`
    pub async fn run_inventory(self: &Arc<Self>, ids: Option<Vec<String>>) -> Value {
        if self.tools.inventorying.swap(true, Ordering::SeqCst) {
            return json!({ "busy": true });
        }
        self.host().state_changed();
        let cfg = self.reload_config();
        let all = ids.as_ref().is_none_or(Vec::is_empty);
        let targets: Vec<Node> = cfg.nodes.iter().filter(|n| all || ids.as_ref().is_some_and(|i| i.contains(&n.id))).cloned().collect();
        let mut set = tokio::task::JoinSet::new();
        for (i, n) in targets.iter().cloned().enumerate() {
            let me = self.clone();
            set.spawn(async move { (i, me.inventory_one(&n).await) });
        }
        let mut results: Vec<Value> = vec![Value::Null; targets.len()];
        while let Some(j) = set.join_next().await {
            if let Ok((i, r)) = j {
                results[i] = r;
            }
        }
        if all {
            let _ = self.with_db(|d| d.set_meta("lastInventoryAt", &Value::from(now_ms())));
        }
        self.tools.inventorying.store(false, Ordering::SeqCst);
        self.host().state_changed();
        json!({ "results": results })
    }

    async fn inventory_one(&self, n: &Node) -> Value {
        let r = inventory::inventory_node(n).await;
        let out = if js::truthy(r.get("ok")) {
            let skip: Vec<String> = js::arr(r.get("failedSources")).iter().map(|s| js::string(Some(s))).collect();
            let at = r.get("at").and_then(Value::as_i64).unwrap_or_else(now_ms);
            match self.with_db(|d| d.save_inventory(&n.id, &items_of(&r), at, &skip)) {
                Ok(c) => {
                    lock(&self.tools.last_inventory_error).remove(&n.id);
                    let mut o = serde_json::to_value(&c).unwrap_or(Value::Null);
                    o["node_id"] = json!(n.id);
                    o["ok"] = json!(true);
                    o["wall_s"] = r["wall_s"].clone();
                    o["errors"] = r["errors"].clone();
                    o
                }
                Err(e) => {
                    let msg = format!("棚卸しは終わったが保存できなかった: {e}");
                    lock(&self.tools.last_inventory_error).insert(n.id.clone(), msg.clone());
                    json!({ "node_id": n.id, "ok": false, "error": msg })
                }
            }
        } else {
            let e = js::string(r.get("error"));
            lock(&self.tools.last_inventory_error).insert(n.id.clone(), e.clone());
            json!({ "node_id": n.id, "ok": false, "error": e })
        };
        self.host().inventory_result(&out);
        out
    }

    /// 道具の一覧（機体×道具。inventoryView と同じ形）。Do-gu のマスターは取得済みのときだけ照合に使う（ここでは取りにいかない）。
    /// all: 依存として入ったものも含める（既定は自分で入れたものだけ）。下書きはいつも自分で入れたものだけから作る
    pub fn inventory_view(&self, all: bool) -> Result<Value, String> {
        let cfg = self.config();
        let (rows, events, cached, exclude, last_at, marks) = self.with_db(|d| {
            Ok((
                d.inventory(None, false)?,
                d.inventory_events(100)?,
                d.get_meta(dogu::META_TOOLS)?,
                d.get_meta(dogu::META_EXCLUDE)?,
                d.get_meta("lastInventoryAt")?,
                d.inventory_nodes()?,
            ))
        })?;
        let tools: Option<Vec<Value>> = cached.as_ref().map(|c| js::arr(c.get("tools")).to_vec());
        let matcher = tools.as_ref().map(|t| Matcher::new(t));
        let f = |r: &Value| matcher.as_ref().and_then(|m| m.slug(r));
        let slug_fn: Option<&inventory::SlugFn<'_>> = if matcher.is_some() { Some(&f) } else { None };
        let explicit = matrix(&rows, slug_fn, true);
        let shown = if all { matrix(&rows, slug_fn, false) } else { explicit.clone() };
        let cats: HashMap<String, Value> =
            tools.iter().flatten().map(|t| (js::string(t.get("slug")), t.get("category").cloned().unwrap_or(Value::Null))).collect();
        let groups: Vec<Value> = shown
            .iter()
            .map(|g| {
                let mut v = g.to_json();
                let cat = g.slug.as_ref().and_then(|s| cats.get(s)).filter(|c| js::truthy(Some(c))).cloned().unwrap_or(Value::Null);
                v["category"] = cat;
                v
            })
            .collect();
        let errors = lock(&self.tools.last_inventory_error).clone();
        let nodes: Vec<Value> = cfg
            .nodes
            .iter()
            .map(|n| {
                let count = rows.iter().filter(|r| js::is_str(r.get("node_id"), &n.id) && js::truthy(r.get("explicit"))).count();
                let last_ok = marks.iter().find(|m| js::is_str(m.get("node_id"), &n.id)).and_then(|m| m.get("last_ok_at").cloned()).unwrap_or(Value::Null);
                json!({ "id": n.id, "os": n.os, "count": count, "error": errors.get(&n.id), "last_ok_at": last_ok })
            })
            .collect();
        let exclude: Vec<String> = exclude.as_ref().map(|e| js::arr(Some(e)).iter().map(|s| js::string(Some(s))).collect()).unwrap_or_default();
        let dogu = match (&cached, &tools) {
            (Some(c), Some(t)) => json!({
                "at": c.get("at"), "tools": t.len(), "matched": explicit.iter().filter(|g| g.slug.is_some()).count(),
                "draft": dogu::deck_draft(&explicit, t, &exclude), "exclude": exclude,
            }),
            _ => Value::Null,
        };
        Ok(json!({
            "nodes": nodes, "groups": groups, "events": events, "sources": inventory::sources_json(),
            "lastInventoryAt": last_at, "inventorying": self.is_inventorying(), "dogu": dogu, "all": all,
        }))
    }

    /// Do-gu の共通マスターを取りにいく（画面のボタンを押したときだけ。送るものは無い）。失敗は `{ error }`
    pub async fn dogu_refresh(&self, http: &dyn Http, all: bool) -> Result<Value, String> {
        match dogu::tools(&self.db, http, true).await {
            Ok(_) => self.inventory_view(all),
            Err(e) => Ok(json!({ "error": e })),
        }
    }

    /// デッキの下書きから外す道具（slug の一覧で置き換える）
    pub fn dogu_exclude(&self, slugs: &Value, all: bool) -> Result<Value, String> {
        let mut list: Vec<String> = Vec::new();
        for s in js::arr(Some(slugs)) {
            let s = js::string(Some(s));
            if !list.contains(&s) {
                list.push(s);
            }
        }
        self.with_db(|d| d.set_meta(dogu::META_EXCLUDE, &json!(list)))?;
        self.inventory_view(all)
    }

    /// デッキへの登録（外への送信）。機体を変える操作ではないが、同じ型で扱う:
    /// 検証（下書きにある slug だけ）→ 全件を見せた確認 → 確認後に下書きを作り直して再検証（変わっていたら送らない）→ 送信 → 実行記録。
    /// 既にある道具への紐づけだけを送り、新しい道具は作らない。リトライしない
    pub async fn dogu_publish(&self, slugs: &Value, http: &dyn Http, key: Option<String>, confirm: Confirm) -> Result<Value, String> {
        let refused = |m: String| Ok(json!({ "ok": false, "refused": m }));
        let draft_now = || -> Result<Option<Vec<Value>>, String> {
            let v = self.inventory_view(false)?;
            Ok(v.get("dogu").and_then(|d| d.get("draft")).map(|d| js::arr(Some(d)).to_vec()))
        };
        let Some(draft) = draft_now()? else { return refused("先に Do-gu の一覧を取得してください".into()) };
        let plan = dogu::plan_publish(&draft, slugs);
        if let Some(r) = plan.get("refused") {
            return refused(js::string(Some(r)));
        }
        let Some(key) = key else {
            return refused(format!(
                "API キーが見つからない。{}/howto で発行し、環境変数 DO_GU_API_KEY か ~/.config/do-gu/api_key に保存してください",
                dogu::BASE
            ));
        };
        let login = match dogu::me(http, &key).await {
            Ok(v) => js::string(v.get("login")),
            Err(e) => return refused(e),
        };
        let list = js::arr(plan.get("items"))
            .iter()
            .map(|d| format!("・{}（{}）", js::string(d.get("name")), js::string(d.get("category"))))
            .collect::<Vec<_>>()
            .join("\n");
        let n = js::arr(plan.get("pick")).len();
        let title = format!("Do-gu のデッキに {n} 件を登録します");
        let detail = format!(
            "登録すると {}/@{login} で誰でも見られる公開ページに載ります。\n送るのは既にある道具への紐づけだけで、新しい道具は作りません。\n\n{list}",
            dogu::BASE
        );
        if !confirm(title, detail).await {
            return Ok(json!({ "ok": false, "cancelled": true }));
        }
        let fresh = dogu::plan_publish(&draft_now()?.unwrap_or_default(), slugs);
        if !dogu::same_plan(&plan, &fresh) {
            return refused("確認のあいだに下書きが変わったので送らなかった。もう一度確認してください".into());
        }
        let pick: Vec<String> = js::arr(fresh.get("pick")).iter().map(|s| js::string(Some(s))).collect();
        let (ok, res) = match dogu::publish(http, &key, &pick).await {
            Ok(v) => (true, v),
            Err(e) => (false, json!({ "error": e })),
        };
        let at = now_ms();
        let entry = json!({
            "id": dogu_publish_action_id(at), "at": at, "node_id": "_app", "type": "dogu_publish", "params": { "slugs": pick },
            "label": format!("Do-gu に {} 件を登録", pick.len()), "ok": ok, "output": js::slice16(&res.to_string(), 4000), "undo": null, "undo_of": null,
        });
        // 記録を書けなくても送信は済んでいる。成功扱いにせず、その旨を返す
        self.with_db(|d| d.add_action(&entry)).map_err(|e| format!("実行記録を書けなかった（Do-gu への送信は{}）: {e}", if ok { "済み" } else { "失敗" }))?;
        Ok(json!({ "ok": ok, "login": login, "url": format!("{}/@{login}", dogu::BASE), "result": res }))
    }

    // ---- AI エージェントのセッション ----

    /// 各機体のセッションを取り込む（続きの位置から差分だけ）。実行中なら `{ busy: true }`
    pub async fn ai_sync(self: &Arc<Self>, ids: Option<Vec<String>>) -> Value {
        if self.tools.ai_syncing.swap(true, Ordering::SeqCst) {
            return json!({ "busy": true });
        }
        self.host().state_changed();
        let t0 = now_ms();
        let cfg = self.reload_config();
        let all = ids.as_ref().is_none_or(Vec::is_empty);
        let targets: Vec<Node> =
            cfg.nodes.iter().filter(|n| ai_sessions::enabled(n) && (all || ids.as_ref().is_some_and(|i| i.contains(&n.id)))).cloned().collect();
        let mut set = tokio::task::JoinSet::new();
        for (i, n) in targets.iter().cloned().enumerate() {
            let me = self.clone();
            set.spawn(async move { (i, me.ai_sync_one(&n).await) });
        }
        let mut results: Vec<Value> = vec![Value::Null; targets.len()];
        while let Some(j) = set.join_next().await {
            if let Ok((i, r)) = j {
                results[i] = r;
            }
        }
        if all {
            let _ = self.with_db(|d| d.set_meta("lastAiAt", &Value::from(now_ms())));
        }
        self.tools.ai_syncing.store(false, Ordering::SeqCst);
        self.host().state_changed();
        json!({ "results": results, "ms": now_ms() - t0 })
    }

    async fn ai_sync_one(&self, n: &Node) -> Value {
        let files = self.with_db(|d| d.ai_files(&n.id)).unwrap_or_default();
        let (fetched, wall_s) = ai_sessions::fetch(n, &files).await;
        let now = now_ms();
        let out = match fetched {
            Fetched::Ok(v) => match self.with_db(|d| d.ai_ingest(&n.id, &v, now)) {
                Ok(r) => json!({
                    "node_id": n.id, "ok": true, "sessions": r.sessions, "hours": r.hours, "files": r.files, "truncated": js::truthy(v.get("truncated")),
                    "files_total": v.get("files_total"), "files_changed": v.get("files_changed"), "bytes_pending": v.get("bytes_pending"),
                    "bytes_read": v.get("bytes_read"), "elapsed_s": v.get("elapsed_s"), "errors": js::arr(v.get("errors")).len(), "wall_s": wall_s,
                }),
                Err(e) => {
                    let msg = format!("読んだが保存できなかった: {e}");
                    let _ = self.with_db(|d| d.ai_error(&n.id, &msg, false, now));
                    json!({ "node_id": n.id, "ok": false, "error": msg, "wall_s": wall_s })
                }
            },
            Fetched::NoPython(m) => {
                let _ = self.with_db(|d| d.ai_error(&n.id, &m, true, now));
                json!({ "node_id": n.id, "ok": false, "no_python": true, "error": m, "wall_s": wall_s })
            }
            Fetched::Error(e) => {
                let _ = self.with_db(|d| d.ai_error(&n.id, &e, false, now));
                json!({ "node_id": n.id, "ok": false, "error": e, "wall_s": wall_s })
            }
        };
        self.host().ai_synced(&out);
        out
    }

    /// 画面の AI（集計結果）。f は [`crate::db::Store::ai_summary`] と同じ。機体ごとの取り込みの様子も付ける
    pub fn ai_summary(&self, f: &Value) -> Result<Value, String> {
        let cfg = self.config();
        let now = now_ms();
        let ids = ai_node_ids(&cfg);
        let (mut s, cursors, last_at, pending) =
            self.with_db(|d| Ok((d.ai_summary(f, now)?, d.ai_cursors()?, d.get_meta("lastAiAt")?, d.ai_pending(&ids)?)))?;
        let nodes: Vec<Value> = cfg
            .nodes
            .iter()
            .map(|n| {
                let c = cursors.iter().find(|c| js::is_str(c.get("node_id"), &n.id)).cloned().unwrap_or_else(|| json!({}));
                let mut o = json!({ "id": n.id, "os": n.os, "shared": n.shared, "enabled": ai_sessions::enabled(n) });
                if let (Value::Object(m), Value::Object(cm)) = (&mut o, c) {
                    for (k, v) in cm {
                        if k != "node_id" {
                            m.insert(k, v);
                        }
                    }
                }
                o
            })
            .collect();
        let sch = self.schedule();
        if let Value::Object(m) = &mut s {
            m.insert("nodes".into(), Value::Array(nodes));
            m.insert("ingesting".into(), Value::Bool(self.is_ai_syncing()));
            m.insert("pending".into(), Value::Bool(pending));
            m.insert("lastAiAt".into(), last_at.unwrap_or(Value::Null));
            m.insert("schedule".into(), json!({ "enabled": sch.get("enabled"), "ai_minutes": sch.get("ai_minutes"), "catch_up_minutes": CATCH_UP_MINUTES }));
        }
        Ok(s)
    }

    /// セッションの一覧（ページング）
    pub fn ai_sessions(&self, f: &Value) -> Result<Value, String> {
        self.with_db(|d| d.ai_session_list(f, now_ms()))
    }

    /// 1分ごとの自動スキャンの続き: 棚卸し（既定 24 時間ごと）と AI の取り込み（既定 30 分ごと、続きがあれば 2 分後）
    pub async fn tick_tools(self: &Arc<Self>) {
        let s = self.schedule();
        if !js::truthy(s.get("enabled")) {
            return;
        }
        let now = now_ms() as f64;
        let since = |k: &str| now - self.with_db(|d| d.get_meta(k)).ok().flatten().map_or(0.0, |v| js::num(Some(&v)));
        if !self.is_inventorying() && since("lastInventoryAt") >= js::num(s.get("inventory_hours")) * 3_600_000.0 {
            self.run_inventory(None).await;
        }
        if !self.is_ai_syncing() {
            let cfg = self.reload_config();
            let pending = self.with_db(|d| d.ai_pending(&ai_node_ids(&cfg))).unwrap_or(false);
            let mins = if pending { CATCH_UP_MINUTES as f64 } else { js::num(s.get("ai_minutes")) };
            if mins.is_finite() && since("lastAiAt") >= mins * 60_000.0 {
                self.ai_sync(None).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dogu::{HttpFuture, HttpRequest, HttpResponse};
    use crate::engine::NoHost;

    /// 偽の HTTP。送られた要求を記録し、決めた応答を返す（本物の Do-gu には送らない）
    struct FakeHttp {
        sent: Mutex<Vec<HttpRequest>>,
        decks: Result<(u16, String), String>,
    }

    impl Http for FakeHttp {
        fn send(&self, req: HttpRequest) -> HttpFuture<'_> {
            lock(&self.sent).push(req.clone());
            let r = if req.url.ends_with("/api/me") {
                Ok(HttpResponse { status: 200, body: "{\"login\":\"someone\"}".into() })
            } else if req.url.ends_with("/api/tools") {
                Ok(HttpResponse {
                    status: 200,
                    body: json!({ "tools": [
                        { "slug": "jq", "name": "jq", "category": "cli", "website_url": "https://example.com", "extra": 1 },
                        { "slug": "gh", "name": "GitHub CLI", "category": "cli" },
                        { "slug": "ripgrep", "name": "ripgrep", "category": "cli" }
                    ] })
                    .to_string(),
                })
            } else {
                self.decks.clone().map(|(status, body)| HttpResponse { status, body })
            };
            Box::pin(async move { r })
        }
    }

    fn engine(tag: &str) -> (Arc<Engine>, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("kt-tools-{tag}-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&base).unwrap();
        let cfg = base.join("nodes.json");
        std::fs::write(&cfg, r#"{ "nodes": [{ "id": "n1", "alias": "n1", "os": "macos" }, { "id": "n2", "alias": "n2", "os": "windows" }] }"#).unwrap();
        let e = Engine::open(cfg, base.join("data"), Arc::new(NoHost)).unwrap();
        let it = |s: &str, n: &str| crate::inventory::Item { source: s.into(), name: n.into(), version: Some("1".into()), explicit: true, extra: None };
        e.with_db(|d| {
            d.save_inventory("n1", &[it("brew", "jq"), it("brew", "gh"), it("brew", "ripgrep")], 1000, &[])?;
            d.save_inventory("n2", &[it("scoop", "jq")], 1000, &[])
        })
        .unwrap();
        (e, base)
    }

    fn approve(seen: Arc<Mutex<Vec<(String, String)>>>, answer: bool) -> Confirm {
        Box::new(move |t, d| {
            lock(&seen).push((t, d));
            Box::pin(async move { answer })
        })
    }

    #[test]
    fn dogu_publish_action_ids_are_unique_with_same_timestamp() {
        let first = dogu_publish_action_id(1_700_000_000_000);
        let second = dogu_publish_action_id(1_700_000_000_000);

        assert_ne!(first, second);
        assert!(first.contains(&format!("-{}-", std::process::id())));
    }

    #[tokio::test]
    async fn view_and_draft_follow_the_master() {
        let (e, base) = engine("view");
        let v = e.inventory_view(false).unwrap();
        assert_eq!(v["dogu"], Value::Null, "マスターを取りにいくのはボタンを押したときだけ");
        assert_eq!(v["nodes"][0]["count"], json!(3));
        let http = FakeHttp { sent: Mutex::new(vec![]), decks: Err("not used".into()) };
        let v = e.dogu_refresh(&http, false).await.unwrap();
        assert_eq!(v["dogu"]["tools"], json!(3));
        assert_eq!(v["dogu"]["draft"].as_array().unwrap().len(), 3);
        let jq = v["groups"].as_array().unwrap().iter().find(|g| g["slug"] == "jq").unwrap().clone();
        assert_eq!((jq["node_count"].clone(), jq["category"].clone()), (json!(2), json!("cli")));
        // マスターは使う項目だけ保存する
        let cached = e.with_db(|d| d.get_meta(dogu::META_TOOLS)).unwrap().unwrap();
        assert_eq!(cached["tools"][0], json!({ "slug": "jq", "name": "jq", "category": "cli", "website_url": "https://example.com", "icon_url": null }));
        let v = e.dogu_exclude(&json!(["gh", "gh"]), false).unwrap();
        assert_eq!(v["dogu"]["exclude"], json!(["gh"]));
        assert!(!v["dogu"]["draft"].as_array().unwrap().iter().any(|d| d["slug"] == "gh"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn publish_sends_only_draft_slugs_once_and_records_it() {
        let (e, base) = engine("publish");
        let http = FakeHttp { sent: Mutex::new(vec![]), decks: Ok((200, "{\"added\":2}".into())) };
        e.dogu_refresh(&http, false).await.unwrap();
        lock(&http.sent).clear();
        let seen = Arc::new(Mutex::new(vec![]));
        let r = e.dogu_publish(&json!(["jq", "ripgrep", "not-in-draft"]), &http, Some("k".into()), approve(seen.clone(), true)).await.unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["url"], json!(format!("{}/@someone", dogu::BASE)));
        let (title, detail) = lock(&seen)[0].clone();
        assert!(title.contains("2 件") && detail.contains("公開ページ") && detail.contains("・jq（cli）"));
        let sent = lock(&http.sent).clone();
        let posts: Vec<&HttpRequest> = sent.iter().filter(|r| r.method == "POST").collect();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].bearer.as_deref(), Some("k"));
        // 既にある道具への紐づけ（slug）だけ。新しい道具（name・category）は送らない
        let body: Value = serde_json::from_str(posts[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body, json!({ "items": [{ "tool": { "slug": "jq" } }, { "tool": { "slug": "ripgrep" } }] }));
        let log = e.with_db(|d| d.actions(10)).unwrap();
        assert_eq!(
            (log[0]["type"].clone(), log[0]["node_id"].clone(), log[0]["params"].clone()),
            (json!("dogu_publish"), json!("_app"), json!({ "slugs": ["jq", "ripgrep"] }))
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn publish_refuses_or_stops_without_sending() {
        let (e, base) = engine("refuse");
        let http = FakeHttp { sent: Mutex::new(vec![]), decks: Ok((200, "{}".into())) };
        let seen = Arc::new(Mutex::new(vec![]));
        // マスターが無い
        let r = e.dogu_publish(&json!(["jq"]), &http, Some("k".into()), approve(seen.clone(), true)).await.unwrap();
        assert!(js::string(r.get("refused")).contains("Do-gu の一覧"));
        e.dogu_refresh(&http, false).await.unwrap();
        lock(&http.sent).clear();
        // 下書きに無いものだけ・キーが無い・確認でやめた
        assert_eq!(e.dogu_publish(&json!(["nope"]), &http, Some("k".into()), approve(seen.clone(), true)).await.unwrap()["refused"], "送る道具がない");
        assert!(js::string(e.dogu_publish(&json!(["jq"]), &http, None, approve(seen.clone(), true)).await.unwrap().get("refused")).contains("API キー"));
        assert_eq!(e.dogu_publish(&json!(["jq"]), &http, Some("k".into()), approve(seen.clone(), false)).await.unwrap()["cancelled"], json!(true));
        // 確認のあいだに jq が除外された → 送らない
        let e2 = e.clone();
        let changing: Confirm = Box::new(move |_, _| {
            e2.dogu_exclude(&json!(["jq"]), false).unwrap();
            Box::pin(async { true })
        });
        let r = e.dogu_publish(&json!(["jq", "gh"]), &http, Some("k".into()), changing).await.unwrap();
        assert!(js::string(r.get("refused")).contains("下書きが変わった"));
        assert!(lock(&http.sent).iter().all(|r| r.method != "POST"), "どの場合も POST していない");
        assert!(e.with_db(|d| d.actions(10)).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn publish_failure_is_not_retried_and_is_recorded() {
        let (e, base) = engine("fail");
        let http = FakeHttp { sent: Mutex::new(vec![]), decks: Ok((500, "{\"error\":\"down\"}".into())) };
        e.dogu_refresh(&http, false).await.unwrap();
        lock(&http.sent).clear();
        let r = e.dogu_publish(&json!(["jq"]), &http, Some("k".into()), approve(Arc::new(Mutex::new(vec![])), true)).await.unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(js::string(r["result"].get("error")).contains("Do-gu 500"));
        assert_eq!(lock(&http.sent).iter().filter(|r| r.method == "POST").count(), 1, "リトライしない");
        let log = e.with_db(|d| d.actions(10)).unwrap();
        assert_eq!(log[0]["ok"], json!(false));
        // 接続できない（curl の失敗）も同じ
        let down = FakeHttp { sent: Mutex::new(vec![]), decks: Err("Do-gu に接続できない: timeout".into()) };
        let r = e.dogu_publish(&json!(["jq"]), &down, Some("k".into()), approve(Arc::new(Mutex::new(vec![])), true)).await.unwrap();
        assert_eq!((r["ok"].clone(), lock(&down.sent).iter().filter(|r| r.method == "POST").count()), (json!(false), 1));
        let log = e.with_db(|d| d.actions(10)).unwrap();
        assert_eq!(log.len(), 2, "失敗した送信もそれぞれ記録する");
        assert!(log.iter().all(|entry| entry["ok"] == json!(false)));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn schedule_has_ai_minutes_and_tick_skips_when_disabled() {
        let (e, base) = engine("sched");
        assert_eq!(e.schedule()["ai_minutes"], json!(ai_sessions::DEFAULT_AI_MINUTES));
        let s = e.set_schedule(&json!({ "ai_minutes": 15, "enabled": false })).unwrap();
        assert_eq!((s["ai_minutes"].clone(), s["enabled"].clone()), (json!(15), json!(false)));
        assert_eq!(e.set_schedule(&json!({ "ai_minutes": 2 })).unwrap()["ai_minutes"], json!(15), "5 分未満は受け付けない");
        // 自動スキャンがオフなら何も動かさない（台帳の機体に ssh しない）
        e.tick_tools().await;
        assert_eq!(e.with_db(|d| d.get_meta("lastInventoryAt")).unwrap(), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn pending_only_counts_nodes_the_ledger_still_ingests() {
        let base = std::env::temp_dir().join(format!("kt-tools-pending-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&base).unwrap();
        let cfg = base.join("nodes.json");
        // n1 は取り込まない設定、n2 は共用機（既定で取り込まない）。取り込む対象が無いので、取り込みが走っても ssh しない
        let ledger = r#"{ "id": "n1", "alias": "n1", "os": "macos", "ai_sessions": false }, { "id": "n2", "alias": "n2", "os": "windows", "shared": true }"#;
        std::fs::write(&cfg, format!(r#"{{ "nodes": [{ledger}] }}"#)).unwrap();
        let e = Engine::open(cfg.clone(), base.join("data"), Arc::new(NoHost)).unwrap();
        // 古い続きの位置: 台帳から外した機体（gone）と、取り込まない n1・n2
        let rest = json!({ "sessions": [], "cursors": {}, "gone": [], "truncated": true });
        e.with_db(|d| ["gone", "n1", "n2"].iter().try_for_each(|id| d.ai_ingest(id, &rest, 1).map(|_| ()))).unwrap();
        assert_eq!(e.ai_summary(&json!({})).unwrap()["pending"], json!(false), "取り込まない機体の続きは「続きあり」にしない");
        // 前回の取り込みは 3 分前。続きありと見なすと 2 分ごとの取り込み（対象 0 台）が走って lastAiAt が進む
        let last = now_ms() - 3 * 60_000;
        e.with_db(|d| {
            d.set_meta("lastAiAt", &json!(last))?;
            d.set_meta("lastInventoryAt", &json!(now_ms()))
        })
        .unwrap();
        e.tick_tools().await;
        assert_eq!(e.with_db(|d| d.get_meta("lastAiAt")).unwrap(), Some(json!(last)), "30 分ごとの間隔のまま");
        // 今の台帳で取り込む機体（n3）に続きがあれば「続きあり」（ここでは画面の集計だけを見て、取り込みは走らせない）
        std::fs::write(&cfg, format!(r#"{{ "nodes": [{ledger}, {{ "id": "n3", "alias": "n3", "os": "macos" }}] }}"#)).unwrap();
        e.reload_config();
        e.with_db(|d| d.ai_ingest("n3", &rest, 2).map(|_| ())).unwrap();
        assert_eq!(e.ai_summary(&json!({})).unwrap()["pending"], json!(true));
        let _ = std::fs::remove_dir_all(&base);
    }
}
