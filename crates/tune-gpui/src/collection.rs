//! Explicit collection jobs. Saved-data navigation never starts this worker.
use crate::{
    network,
    panels::{Page, Panel, Row, Tone},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, mpsc};

pub type Progress = Arc<Mutex<Option<Panel>>>;
use tune_core::{engine::Engine, nodes::Node};

const MAX_ROWS: usize = 24;
const MAX_DETAIL: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Probe,
    Logs,
    Inventory,
    Ai,
    Specs,
    Network(bool),
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Probe => "全機分析",
            Self::Logs => "ログ収集",
            Self::Inventory => "道具の棚卸し",
            Self::Ai => "AI取り込み",
            Self::Specs => "詳細仕様の収集",
            Self::Network(false) => "接続設定の確認",
            Self::Network(true) => "外部到達の診断",
        }
    }
    pub fn for_page(page: Page) -> Option<Self> {
        match page {
            Page::Logs => Some(Self::Logs),
            Page::Tools => Some(Self::Inventory),
            Page::Ai => Some(Self::Ai),
            Page::Specs => Some(Self::Specs),
            _ => None,
        }
    }
}
pub struct Receipt {
    pub kind: Kind,
    pub network: Option<Panel>,
    pub diagnosis_warnings: usize,
    pub complete: usize,
    pub partial: usize,
    pub failed: usize,
    pub skipped: usize,
    pub omitted: usize,
    pub busy: bool,
    pub rows: Vec<Row>,
}
impl Receipt {
    fn from_result(kind: Kind, value: &Value, excluded: Vec<Value>) -> Self {
        let mut out = Self {
            kind,
            network: None,
            diagnosis_warnings: 0,
            complete: 0,
            partial: 0,
            failed: 0,
            skipped: 0,
            omitted: 0,
            busy: value["busy"] == true,
            rows: Vec::new(),
        };
        let mut skipped_ids = std::collections::HashSet::new();
        if problem(&value["error"]) {
            out.add(value, Outcome::Failed, "収集を開始・完了できません");
        } else if !out.busy {
            let key = if kind == Kind::Specs { "nodes" } else { "results" };
            if let Some(rows) = value[key].as_array() {
                for row in rows {
                    let (state, reason) = classify(kind, row);
                    if state == Outcome::Skipped {
                        skipped_ids.insert(row["node_id"].as_str().unwrap_or("").to_owned());
                    }
                    out.add(row, state, &reason);
                }
                if kind == Kind::Probe {
                    // Engine's completed callbacks can omit a panicked task,
                    // while done still counts its fallback result.
                    let missing = value["done"].as_u64().unwrap_or(0).saturating_sub(rows.len() as u64) as usize;
                    let shown = missing.min(MAX_ROWS - out.rows.len());
                    for _ in 0..shown {
                        out.add(&json!({"error":"内部タスクの結果が未報告です"}), Outcome::Failed, "分析の完了件数に対する結果欠落");
                    }
                    out.failed += missing - shown;
                    out.omitted += missing - shown;
                }
            } else {
                out.add(&json!({"error":"収集結果が返されませんでした"}), Outcome::Failed, "結果不明");
            }
        }
        for row in excluded.into_iter().chain(value["skipped"].as_array().into_iter().flatten().cloned()) {
            if skipped_ids.insert(row["node_id"].as_str().unwrap_or("").to_owned()) {
                out.add(&row, Outcome::Skipped, "現在の台帳・収集許可による対象外");
            }
        }
        out
    }
    fn add(&mut self, value: &Value, state: Outcome, reason: &str) {
        match state {
            Outcome::Complete => self.complete += 1,
            Outcome::Partial => self.partial += 1,
            Outcome::Failed => self.failed += 1,
            Outcome::Skipped => self.skipped += 1,
        }
        if self.rows.len() >= MAX_ROWS {
            self.omitted += 1;
            return;
        }
        let (badge, tone) = match state {
            Outcome::Complete => ("成功", Tone::Good),
            Outcome::Partial => ("部分成功・未完了", Tone::Warning),
            Outcome::Failed => ("失敗・結果不明", Tone::Error),
            Outcome::Skipped => ("対象外", Tone::Muted),
        };
        self.rows.push(Row {
            title: bounded(value["node_id"].as_str().unwrap_or("収集全体・未報告の機体"), 128),
            badge: badge.into(),
            tone,
            detail: bounded(&format!("{reason}\n{}", serde_json::to_string_pretty(value).unwrap_or_default()), MAX_DETAIL),
        });
    }
    fn network_result(rows: &[Value], active: bool, total: usize) -> Self {
        let mut result = Self {
            kind: Kind::Network(active),
            network: Some(network::panel(rows, active, total, false)),
            diagnosis_warnings: 0,
            complete: 0,
            partial: 0,
            failed: 0,
            skipped: 0,
            omitted: rows.len().saturating_sub(MAX_ROWS),
            busy: false,
            rows: vec![],
        };
        for value in rows {
            if value["skipped"] == true {
                result.skipped += 1;
            } else if value["ok"] == true {
                result.complete += 1;
            } else {
                result.failed += 1;
            }
            result.diagnosis_warnings += usize::from(network::warnings(value));
            if result.rows.len() < MAX_ROWS {
                result.rows.push(network::row(value, active));
            }
        }
        result
    }
    pub fn summary(&self) -> String {
        if matches!(self.kind, Kind::Network(_)) {
            return format!(
                "{}: report取得成功 {} · 取得失敗 {} · 対象外 {} · 診断の注意・欠測 {}（成功の内数） · 表示省略 {}",
                self.kind.label(),
                self.complete,
                self.failed,
                self.skipped,
                self.diagnosis_warnings,
                self.omitted
            );
        }
        if self.busy {
            return format!("{}: 別の収集が実行中のため開始されませんでした", self.kind.label());
        }
        format!(
            "{}: 成功 {} · 部分 {} · 失敗 {} · 対象外 {} · 表示省略 {}",
            self.kind.label(),
            self.complete,
            self.partial,
            self.failed,
            self.skipped,
            self.omitted
        )
    }
    pub fn copy_text(&self) -> String {
        std::iter::once(self.summary()).chain(self.rows.iter().map(|r| format!("{} · {}\n{}", r.title, r.badge, r.detail))).collect::<Vec<_>>().join("\n\n")
    }
    pub fn error(kind: Kind, error: &str) -> Self {
        if let Kind::Network(active) = kind {
            return Self::network_result(&[json!({"ok":false,"error":"request_failed"})], active, 1);
        }
        Self::from_result(kind, &json!({"error":error}), vec![])
    }
}
#[derive(Default)]
pub struct Job {
    pub running: Option<Kind>,
    pub result: Option<Receipt>,
    pub network: Option<Panel>,
    pub progress: Progress,
    receiver: Option<mpsc::Receiver<Receipt>>,
}
impl Job {
    pub fn restore_network(&self, page: Page, loaded: Option<Result<Panel, String>>) -> Option<Result<Panel, String>> {
        if page == Page::Network { self.network.clone().map(Ok).or(loaded) } else { loaded }
    }

    pub fn begin(&mut self, kind: Kind) -> Option<mpsc::Sender<Receipt>> {
        if self.running.is_some() {
            return None;
        }
        let (sender, receiver) = mpsc::channel();
        self.progress.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        self.receiver = Some(receiver);
        self.running = Some(kind);
        Some(sender)
    }
    pub fn poll(&mut self) -> bool {
        let changed = if let Some(panel) = self.progress.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            self.network = Some(panel);
            true
        } else {
            false
        };
        let Some(receiver) = &self.receiver else {
            return changed;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return changed,
            Err(mpsc::TryRecvError::Disconnected) => Receipt::error(self.running.unwrap_or(Kind::Probe), "収集タスクが停止しました"),
        };
        if let Some(panel) = &result.network {
            self.network = Some(panel.clone());
        }
        self.result = Some(result);
        self.running = None;
        self.receiver = None;
        true
    }
}

#[derive(PartialEq)]
enum Outcome {
    Complete,
    Partial,
    Failed,
    Skipped,
}
fn bounded(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() { format!("{prefix}\n（以降省略）") } else { prefix }
}
fn problem(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|n| n != 0.),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
/// The privacy-filtered probe contains nested per-provider diagnostics. Do not
/// infer success from its top-level ok alone, or expose a raw database snapshot.
fn diagnostics(value: &Value, depth: usize, issues: &mut Vec<String>) {
    if depth > 8 || issues.len() >= 16 {
        return;
    }
    match value {
        Value::Object(fields) => {
            for key in ["error", "errors"] {
                if problem(&value[key]) {
                    issues.push(format!("{key}: {}", bounded(&value[key].to_string(), 180)));
                }
            }
            for (key, item) in fields {
                if ["error", "errors"].contains(&key.as_str()) {
                    continue;
                }
                if (key == "status" && matches!(item.as_str(), Some("partial" | "error" | "failed")))
                    || (key.ends_with("availability") && matches!(item.as_str(), Some("partial" | "unavailable" | "missing")))
                {
                    issues.push(format!("{key}: {item}"));
                }
                diagnostics(item, depth + 1, issues);
            }
        }
        Value::Array(items) => {
            for item in items {
                diagnostics(item, depth + 1, issues);
            }
        }
        _ => {}
    }
}
fn classify(kind: Kind, row: &Value) -> (Outcome, String) {
    if !row.is_object() {
        return (Outcome::Failed, "収集タスクの結果がありません".into());
    }
    if row["skipped"] == true {
        return (Outcome::Skipped, "収集せず対象外".into());
    }
    if problem(&row["error"]) {
        return (Outcome::Failed, bounded(&row["error"].to_string(), 400));
    }
    if kind == Kind::Logs {
        let Some(sources) = row["sources"].as_object().filter(|s| !s.is_empty()) else {
            return (Outcome::Failed, "収集元の結果がありません".into());
        };
        let errors: Vec<_> =
            sources.iter().filter(|(_, v)| problem(&v["error"])).map(|(name, v)| format!("{name}: {}", bounded(&v["error"].to_string(), 180))).collect();
        if !errors.is_empty() {
            return (if errors.len() == sources.len() { Outcome::Failed } else { Outcome::Partial }, bounded(&errors.join(" / "), 500));
        }
        return (Outcome::Complete, "収集元の取り込み成功（0件や未検出のnoteは詳細参照）".into());
    }
    if kind == Kind::Specs {
        if row["last_attempt"]["ok"] != true {
            return (Outcome::Failed, format!("今回の仕様収集は失敗・未報告。前回成功とは別です: {}", bounded(&row["last_attempt"].to_string(), 400)));
        }
        if row["report"]["ok"] != true {
            return (Outcome::Failed, "今回の有効な仕様reportがありません".into());
        }
        let partial =
            row["report"]["sections"].as_object().is_some_and(|s| s.values().any(|v| !matches!(v["status"].as_str(), Some("ok" | "empty" | "unsupported"))));
        return (
            if partial { Outcome::Partial } else { Outcome::Complete },
            "詳細仕様の今回の試行。非対応・正常な空結果は失敗に数えません。取得元と欠測は詳細参照".into(),
        );
    }
    if row["ok"] != true {
        return (Outcome::Failed, "成功を確認できません".into());
    }
    let mut issues = Vec::new();
    match kind {
        Kind::Probe => {
            if row["diagnostics_unavailable"] == true {
                issues.push(format!(
                    "今回の保存済み詳細と照合できません: {}",
                    row["diagnostics_error"].as_str().unwrap_or("実行ID・時刻に一致する結果がありません")
                ));
            }
            diagnostics(&row["data"], 0, &mut issues);
        }
        Kind::Inventory => {
            for key in ["skipped", "failedSources", "errors"] {
                if problem(&row[key]) {
                    issues.push(format!("{key}: {}", bounded(&row[key].to_string(), 300)));
                }
            }
        }
        Kind::Ai => {
            for key in ["truncated", "errors", "rejected"] {
                if problem(&row[key]) {
                    issues.push(format!("{key}: {}", bounded(&row[key].to_string(), 200)));
                }
            }
            if matches!(row["limits"].as_str(), Some("error" | "no_python")) {
                issues.push(format!("残り枠の取得失敗: {}", row["limits"]));
            }
        }
        _ => {}
    }
    (
        if issues.is_empty() { Outcome::Complete } else { Outcome::Partial },
        if issues.is_empty() { "今回の収集・保存が成功しました".into() } else { bounded(&issues.join(" / "), 600) },
    )
}

fn select(kind: Kind, nodes: &[Node], ids: Option<&[String]>) -> Result<(Vec<Node>, Vec<Value>), String> {
    if ids.is_some_and(|ids| ids.iter().any(|id| !nodes.iter().any(|n| &n.id == id))) {
        return Err("未知の機体です".into());
    }
    let mut targets = Vec::new();
    let mut excluded = Vec::new();
    for node in nodes.iter().filter(|n| ids.is_none_or(|ids| ids.is_empty() || ids.contains(&n.id))) {
        let reason = node.refusal().or_else(|| match kind {
            Kind::Ai if !tune_core::ai_sessions::enabled(node) => Some("現在の台帳でAI取り込みが許可されていません（共用機は明示opt-inが必要）".into()),
            Kind::Network(_) => tune_core::network::select(std::slice::from_ref(node), std::slice::from_ref(&node.id)).err(),
            Kind::Specs => tune_core::specs::select(std::slice::from_ref(node), std::slice::from_ref(&node.id)).err(),
            _ => None,
        });
        if let Some(reason) = reason {
            excluded.push(json!({"node_id":node.id,"reason":reason}));
        } else {
            targets.push(node.clone());
        }
    }
    Ok((targets, excluded))
}

/// Called on the job's background thread, never from navigation or rendering.
pub async fn run(engine: Arc<Engine>, kind: Kind, ids: Option<Vec<String>>, progress: Progress) -> Receipt {
    let cfg = engine.reload_config();
    if let Some(error) = engine.config_error() {
        return Receipt::error(kind, &error);
    }
    let (targets, excluded) = match select(kind, &cfg.nodes, ids.as_deref()) {
        Ok(v) => v,
        Err(error) => return Receipt::error(kind, &error),
    };
    if let Kind::Network(active) = kind {
        let ids = targets.iter().map(|n| n.id.clone()).chain(excluded.iter().filter_map(|v| v["node_id"].as_str().map(str::to_owned))).collect();
        return run_network(ids, active, progress, move |id, active| {
            let engine = engine.clone();
            let skipped = excluded.iter().any(|v| v["node_id"] == id);
            async move {
                if skipped {
                    return Ok(json!({"nodes":[{"node_id":id,"ok":false,"skipped":true,"error":"policy_refused"}]}));
                }
                // The core API reads the ledger again immediately before every
                // node; one error never suppresses the remaining targets.
                engine.network_check(&[id], active).await
            }
        })
        .await;
    }
    if targets.is_empty() {
        return Receipt::from_result(kind, &json!({if kind == Kind::Specs { "nodes" } else { "results" }: []}), excluded);
    }
    let mut value = match kind {
        Kind::Network(_) => unreachable!("network jobs return through the sequential worker"),
        Kind::Probe => engine.run_probe(ids, false).await,
        Kind::Logs => engine.sync_logs(targets.clone()).await,
        Kind::Inventory => engine.run_inventory(ids).await,
        Kind::Ai => engine.ai_sync(ids).await,
        Kind::Specs => match engine.specs_refresh(targets.iter().map(|n| n.id.clone()).collect()).await {
            Ok(value) => value,
            Err(error) => return Receipt::error(kind, &error),
        },
    };
    if kind == Kind::Probe
        && let Some(results) = value["results"].as_array_mut()
    {
        // Exact attempt matching prevents an old successful snapshot from hiding
        // a new failed/partial run. Engine.last applies current privacy policy.
        let saved = engine.last();
        for result in results.iter_mut().filter(|r| r["ok"] == true) {
            if let Some(current) = saved.as_ref().ok().and_then(|rows| rows.iter().find(|r| r["node_id"] == result["node_id"] && r["at"] == result["at"])) {
                result["data"] = current["data"].clone();
            } else {
                result["diagnostics_unavailable"] = json!(true);
                if let Err(error) = &saved {
                    result["diagnostics_error"] = json!(bounded(error, 400));
                }
            }
        }
    }
    if kind == Kind::Specs
        && let Some(rows) = value["nodes"].as_array_mut()
    {
        rows.retain(|row| targets.iter().any(|node| row["node_id"] == node.id));
    }
    Receipt::from_result(kind, &value, excluded)
}

async fn run_network<F, Fut>(ids: Vec<String>, active: bool, progress: Progress, mut check: F) -> Receipt
where
    F: FnMut(String, bool) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let total = ids.len();
    let mut rows = Vec::new();
    for id in ids {
        let value = match check(id.clone(), active).await {
            Ok(value) => value["nodes"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["node_id"] == id))
                .cloned()
                .unwrap_or_else(|| json!({"node_id":id,"ok":false,"error":"invalid_report"})),
            Err(_) => json!({"node_id":id,"at":tune_core::db::now_ms(),"ok":false,"error":"request_failed"}),
        };
        rows.push(value);
        // A single replaceable slot bounds pending UI work even for slow frames.
        *progress.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(network::panel(&rows, active, total, true));
    }
    Receipt::network_result(&rows, active, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tune_core::engine::NoHost;

    #[test]
    fn source_errors_and_null_slots_are_not_complete_log_results() {
        let r = Receipt::from_result(
            Kind::Logs,
            &json!({"results":[
                {"node_id":"empty","sources":{"kernel":{"fetched":0,"inserted":0}}},
                {"node_id":"partial","sources":{"kernel":{"inserted":2},"auth":{"error":"no-permission"}}},
                {"node_id":"failed","error":"synthetic failed","sources":{}},
                null,
                {"node_id":"paused","skipped":true,"reason":"paused","sources":{}}
            ]}),
            vec![],
        );
        assert_eq!((r.complete, r.partial, r.failed, r.skipped), (1, 1, 2, 1));
        assert!(r.copy_text().contains("no-permission"));
    }
    #[test]
    fn inventory_failed_sources_and_ai_limits_failures_stay_partial() {
        let inventory = Receipt::from_result(
            Kind::Inventory,
            &json!({"results":[{"node_id":"example","ok":true,"total":4,"skipped":["brew"],"errors":["synthetic provider failure"]}]}),
            vec![],
        );
        assert_eq!((inventory.complete, inventory.partial, inventory.failed), (0, 1, 0));
        let ai = Receipt::from_result(
            Kind::Ai,
            &json!({"results":[
                {"node_id":"truncated","ok":true,"sessions":3,"truncated":true,"errors":1,"rejected":1,"limits":null},
                {"node_id":"limits-failed","ok":true,"sessions":0,"errors":0,"limits":"error"},
                {"node_id":"empty","ok":true,"sessions":0,"errors":0,"limits":"empty"}, null
            ]}),
            vec![],
        );
        assert_eq!((ai.complete, ai.partial, ai.failed), (1, 2, 1));
    }
    #[test]
    fn previous_spec_success_cannot_hide_the_latest_failed_attempt() {
        let r = Receipt::from_result(
            Kind::Specs,
            &json!({"nodes":[
                {"node_id":"old","report":{"ok":true,"at":1},"last_attempt":{"ok":false,"at":2,"error":"synthetic timeout"}},
                {"node_id":"partial","report":{"ok":true,"sections":{"cpu":{"status":"partial","components":[{}]}}},"last_attempt":{"ok":true}},
                {"node_id":"missing","report":null,"last_attempt":null}
            ]}),
            vec![],
        );
        assert_eq!((r.complete, r.partial, r.failed), (0, 1, 2));
        assert!(r.copy_text().contains("synthetic timeout"));
    }
    #[test]
    fn uncollected_spec_sections_remain_missing_in_the_collection_result() {
        let r = Receipt::from_result(
            Kind::Specs,
            &json!({"nodes":[
                {"node_id":"example","report":{"ok":true,"sections":{"cpu":{"status":"ok"},"firmware":{"status":"not_collected"}}},"last_attempt":{"ok":true}},
                {"node_id":"unsupported-empty","report":{"ok":true,"sections":{"cpu":{"status":"ok"},"firmware":{"status":"unsupported"},"battery":{"status":"empty"}}},"last_attempt":{"ok":true}}
            ]}),
            vec![],
        );
        assert_eq!((r.complete, r.partial, r.failed), (1, 1, 0));
    }
    #[test]
    fn probe_source_failure_and_incomplete_diagnostics_do_not_count_as_complete() {
        let r = Receipt::from_result(
            Kind::Probe,
            &json!({"results":[
                {"node_id":"partial","ok":true,"data":{"netsec":{"errors":{"listen":"synthetic denied"}}}},
                {"node_id":"cpu-missing","ok":true,"data":{"cpu_busy":null,"cpu_busy_availability":"unavailable"}},
                {"node_id":"no-detail","ok":true,"diagnostics_unavailable":true},
                {"node_id":"complete","ok":true,"data":{"netsec":{"errors":{}}}}
            ]}),
            vec![],
        );
        assert_eq!((r.complete, r.partial, r.failed), (1, 3, 0));
    }
    #[test]
    fn completed_probe_count_does_not_hide_missing_internal_task_results() {
        let r = Receipt::from_result(Kind::Probe, &json!({"done":2,"results":[{"node_id":"reported","ok":true,"data":{}}]}), vec![]);
        assert_eq!((r.complete, r.failed), (1, 1));
        assert!(r.copy_text().contains("未報告"));
        let bounded = Receipt::from_result(Kind::Probe, &json!({"done":1000,"results":[]}), vec![]);
        assert_eq!((bounded.failed, bounded.rows.len(), bounded.omitted), (1000, 24, 976));
    }
    #[test]
    fn result_rows_and_copy_are_bounded_without_hiding_failure_counts() {
        let rows: Vec<_> = (0..100).map(|i| json!({"node_id":format!("example-{i}"),"ok":false,"error":"日".repeat(20_000)})).collect();
        let r = Receipt::from_result(Kind::Probe, &json!({"results":rows}), vec![]);
        assert_eq!((r.failed, r.rows.len(), r.omitted), (100, 24, 76));
        assert!(r.copy_text().len() < 256_000);
        assert!(r.copy_text().contains("省略"));
    }
    #[test]
    fn one_job_rejects_double_click_and_retains_the_previous_receipt() {
        let mut job = Job::default();
        let first = job.begin(Kind::Logs).expect("first click starts");
        assert!(job.begin(Kind::Ai).is_none());
        first.send(Receipt::from_result(Kind::Logs, &json!({"results":[{"node_id":"example","error":"synthetic failure"}]}), vec![])).ok().unwrap();
        assert!(job.poll());
        assert_eq!(job.running, None);
        assert_eq!(job.result.as_ref().unwrap().failed, 1);
        let next = job.begin(Kind::Inventory).expect("next explicit click starts");
        assert_eq!(job.result.as_ref().unwrap().kind, Kind::Logs);
        drop(next);
        assert!(job.poll());
        assert_eq!(job.running, None);
        assert_eq!(job.result.as_ref().unwrap().failed, 1);
    }
    fn node(value: Value) -> Node {
        Node::from_value(&value, "synthetic-host")
    }
    #[test]
    fn current_policy_excludes_paused_and_non_opted_in_shared_nodes() {
        let nodes = vec![
            node(json!({"id":"paused","os":"macos","status":"paused"})),
            node(json!({"id":"shared","alias":"synthetic","os":"macos","shared":true})),
            node(json!({"id":"opted-in","alias":"synthetic","os":"macos","shared":true,"ai_sessions":true,"specs":true})),
        ];
        let (ai, skipped) = select(Kind::Ai, &nodes, None).unwrap();
        assert_eq!(ai.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["opted-in"]);
        assert_eq!(skipped.len(), 2);
        let (specs, _) = select(Kind::Specs, &nodes, None).unwrap();
        assert_eq!(specs.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["opted-in"]);
        assert!(select(Kind::Logs, &nodes, Some(&["unknown".into()])).is_err());
    }
    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[tokio::test]
    async fn worker_reloads_and_refuses_invalid_or_unknown_config_without_attempt_markers() {
        let f = Fixture(std::env::temp_dir().join(format!("tune-ui-collection-{}-{}", std::process::id(), tune_core::db::now_ms())));
        std::fs::create_dir_all(&f.0).unwrap();
        let config = f.0.join("nodes.json");
        std::fs::write(&config, json!({"nodes":[{"id":"paused","os":"macos","status":"paused"}]}).to_string()).unwrap();
        let engine = Engine::open(config.clone(), f.0.join("data"), Arc::new(NoHost)).unwrap();
        let unknown = run(engine.clone(), Kind::Logs, Some(vec!["unknown".into()]), Progress::default()).await;
        assert_eq!(unknown.failed, 1);
        for kind in [Kind::Probe, Kind::Logs, Kind::Inventory, Kind::Ai, Kind::Specs] {
            let r = run(engine.clone(), kind, None, Progress::default()).await;
            assert_eq!((r.complete, r.failed, r.skipped), (0, 0, 1), "excluded nodes must not start {kind:?}");
        }
        std::fs::write(&config, "{broken synthetic ledger").unwrap();
        for kind in [Kind::Probe, Kind::Logs, Kind::Inventory, Kind::Ai, Kind::Specs] {
            let r = run(engine.clone(), kind, None, Progress::default()).await;
            assert_eq!(r.failed, 1, "must fail closed for {kind:?}");
            assert_eq!(r.complete, 0);
        }
        for marker in ["lastProbeAt", "lastLogsAt", "lastInventoryAt", "lastAiAt"] {
            assert_eq!(engine.with_db(|d| d.get_meta(marker)).unwrap(), None);
        }
    }
    struct NetworkFake;
    impl tune_core::collect::Runner for NetworkFake {
        fn run<'a>(&'a self, _cmd: &'a str, args: &'a [String], _input: Option<&'a [u8]>, _timeout: std::time::Duration) -> tune_core::collect::RunFuture<'a> {
            let active = args.iter().any(|a| a == "--probe");
            Box::pin(async move {
                tune_core::collect::RunResult {
                    code: Some(0),
                    err: "fixture-private-body".into(),
                    out: json!({
                        "schema":"katala_network_check.v1","platform":"darwin","active_probes":active,
                        "default_route_present":true,"ip_address_present":true,"dns_configured":true,"interface_up":true,"link_active":true,
                        "https":{"named":{"state":"reachable","http_status":403,"timings":{"total_ms":12.5}},"fixed_ip":{"state":"tls_failed"}},
                        "private_ip":"192.0.2.33","body":"fixture-private-body"
                    })
                    .to_string(),
                }
            })
        }
    }
    #[tokio::test]
    async fn network_worker_continues_after_request_failure_and_preserves_active_choice() {
        for active in [false, true] {
            let progress = Progress::default();
            let calls = Arc::new(Mutex::new(Vec::new()));
            let observed = calls.clone();
            let result = run_network(vec!["first".into(), "second".into(), "third".into()], active, progress.clone(), move |id, requested_active| {
                observed.lock().unwrap().push((id.clone(), requested_active));
                async move {
                    if id == "first" {
                        return Err("192.0.2.33 fixture-private-body".into());
                    }
                    let node = Node::from_value(&json!({"id":id,"os":"macos","local_hostname":"synthetic","shared":true,"network":false}), "synthetic");
                    Ok(json!({"nodes":[tune_core::network::check_one(&NetworkFake,&node,requested_active).await]}))
                }
            })
            .await;
            assert_eq!(*calls.lock().unwrap(), [("first".into(), active), ("second".into(), active), ("third".into(), active)]);
            assert_eq!((result.complete, result.failed, result.diagnosis_warnings), (2, 1, if active { 2 } else { 0 }));
            assert!(!result.copy_text().contains("fixture-private") && !result.copy_text().contains("192.0.2.33"));
            let page = result.network.unwrap();
            assert_eq!(page.rows.len(), 3);
            assert!(page.rows[1].detail.contains(if active { "HTTP 403" } else { "HTTPS未実行" }));
            assert_eq!(progress.lock().unwrap().as_ref().unwrap().rows.len(), 3);
        }
    }
    #[test]
    fn network_cache_survives_other_jobs_and_progress_is_coalesced() {
        let mut job = Job::default();
        let sender = job.begin(Kind::Network(false)).unwrap();
        assert!(job.begin(Kind::Probe).is_none());
        for i in 1..=100 {
            *job.progress.lock().unwrap() = Some(network::panel(&[json!({"node_id":format!("example-{i}"),"ok":false})], false, 100, true));
        }
        assert!(job.poll());
        assert_eq!(job.network.as_ref().unwrap().rows[0].title, "example-100");
        assert!(job.progress.lock().unwrap().is_none());
        let mut result = Receipt::from_result(Kind::Network(false), &json!({"results":[]}), vec![]);
        result.network = job.network.clone();
        sender.send(result).ok().unwrap();
        assert!(job.poll());
        let sender = job.begin(Kind::Logs).unwrap();
        sender.send(Receipt::error(Kind::Logs, "synthetic failure")).ok().unwrap();
        assert!(job.poll());
        assert_eq!(job.result.as_ref().unwrap().kind, Kind::Logs);
        assert_eq!(job.network.as_ref().unwrap().rows[0].title, "example-100");
        assert!(job.restore_network(Page::Logs, None).is_none());
        let restored = job.restore_network(Page::Network, Some(Ok(network::initial(&[])))).unwrap().unwrap();
        assert_eq!(restored.rows[0].title, "example-100");
        let offline = job.restore_network(Page::Network, Some(Err("synthetic unavailable ledger".into()))).unwrap().unwrap();
        assert_eq!(offline.rows[0].title, "example-100");
    }

    #[test]
    fn network_selection_uses_connectivity_independently_of_shared_and_network() {
        let nodes = vec![
            node(json!({"id":"shared","os":"macos","shared":true,"network":false})),
            node(json!({"id":"disabled","os":"macos","connectivity":false})),
            node(json!({"id":"paused","os":"macos","status":"paused"})),
            node(json!({"id":"unsupported","os":"linux"})),
        ];
        let (targets, excluded) = select(Kind::Network(false), &nodes, None).unwrap();
        assert_eq!(targets.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["shared"]);
        assert_eq!(excluded.len(), 3);
        assert!(select(Kind::Network(true), &nodes, Some(&["unknown".into()])).is_err());
    }
    #[tokio::test]
    async fn network_engine_and_worker_recheck_latest_ledger_and_fail_closed() {
        let f = Fixture(std::env::temp_dir().join(format!("tune-ui-network-{}-{}", std::process::id(), tune_core::db::now_ms())));
        std::fs::create_dir_all(&f.0).unwrap();
        let config = f.0.join("nodes.json");
        std::fs::write(&config, json!({"nodes":[{"id":"example","os":"macos","connectivity":false}]}).to_string()).unwrap();
        let engine = Engine::open(config.clone(), f.0.join("data"), Arc::new(NoHost)).unwrap();
        for active in [false, true] {
            assert!(engine.network_check(&["unknown".into()], active).await.is_err());
            assert!(engine.network_check(&["example".into()], active).await.is_err());
            let result = run(engine.clone(), Kind::Network(active), None, Progress::default()).await;
            assert_eq!((result.complete, result.failed, result.skipped), (0, 0, 1));
        }
        std::fs::write(&config, json!({"nodes":[{"id":"example","os":"macos","status":"paused"}]}).to_string()).unwrap();
        assert!(engine.network_check(&["example".into()], true).await.is_err());
        std::fs::write(&config, "{broken synthetic ledger").unwrap();
        assert!(engine.network_check(&["example".into()], false).await.is_err());
        let result = run(engine.clone(), Kind::Network(false), None, Progress::default()).await;
        assert_eq!((result.complete, result.failed), (0, 1));
        assert!(engine.config_error().is_some());
        assert!(!result.copy_text().contains("nodes.json"));
    }
}
