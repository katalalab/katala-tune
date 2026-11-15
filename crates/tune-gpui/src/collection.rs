//! Explicit collection jobs. Saved-data navigation never starts this worker.
use crate::panels::{Page, Row, Tone};
use serde_json::{Value, json};
use std::sync::{Arc, mpsc};
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
    Network,
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Probe => "全機分析",
            Self::Logs => "ログ収集",
            Self::Inventory => "道具の棚卸し",
            Self::Ai => "AI取り込み",
            Self::Specs => "詳細仕様の収集",
            Self::Network => "ネットワーク確認",
        }
    }
    pub fn for_page(page: Page) -> Option<Self> {
        match page {
            Page::Logs => Some(Self::Logs),
            Page::Tools => Some(Self::Inventory),
            Page::Ai => Some(Self::Ai),
            Page::Specs => Some(Self::Specs),
            Page::Network => Some(Self::Network),
            _ => None,
        }
    }
}
pub struct Receipt {
    pub kind: Kind,
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
        let mut out = Self { kind, complete: 0, partial: 0, failed: 0, skipped: 0, omitted: 0, busy: value["busy"] == true, rows: Vec::new() };
        let mut skipped_ids = std::collections::HashSet::new();
        if problem(&value["error"]) {
            out.add(value, Outcome::Failed, "収集を開始・完了できません");
        } else if !out.busy {
            let key = if matches!(kind, Kind::Specs | Kind::Network) { "nodes" } else { "results" };
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
            // 収集の結果の行は元に戻せない（元に戻せるのは実行記録の行だけ）
            undo: None,
        });
    }
    pub fn summary(&self) -> String {
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
        Self::from_result(kind, &json!({"error":error}), vec![])
    }
}
#[derive(Default)]
pub struct Job {
    pub running: Option<Kind>,
    pub result: Option<Receipt>,
    receiver: Option<mpsc::Receiver<Receipt>>,
}
impl Job {
    pub fn begin(&mut self, kind: Kind) -> Option<mpsc::Sender<Receipt>> {
        if self.running.is_some() {
            return None;
        }
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.running = Some(kind);
        Some(sender)
    }
    pub fn poll(&mut self) -> bool {
        let Some(receiver) = &self.receiver else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => Receipt::error(self.running.unwrap_or(Kind::Probe), "収集タスクが停止しました"),
        };
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
    if kind == Kind::Network {
        // 到達性の × や未取得は、ネットワークの所見であって収集の失敗ではない。成功とは分けて「部分」に数える
        let flags = NET_FLAGS.map(|(key, label)| match row["data"][key].as_bool() {
            Some(true) => format!("{label} ○"),
            Some(false) => format!("{label} ×"),
            None => format!("{label} –"),
        });
        let all_ok = NET_FLAGS.iter().all(|(key, _)| row["data"][key] == true);
        return (if all_ok { Outcome::Complete } else { Outcome::Partial }, flags.join(" · "));
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

const NET_FLAGS: [(&str, &str); 5] = [
    ("interface_up", "インターフェース"),
    ("link_active", "リンク"),
    ("ip_address_present", "IP"),
    ("default_route_present", "既定経路"),
    ("dns_configured", "DNS"),
];

/// 受動の到達性確認の対象外の理由（tune-core の選択と同じ条件。選べない機体が一台でもあると全体が拒否されるので、先に外す）
pub fn network_refusal(node: &Node) -> Option<String> {
    node.refusal().or_else(|| {
        if node.get("connectivity") == Some(&Value::Bool(false)) {
            Some("台帳で接続診断を止めています".into())
        } else if !node.is_mac() && !node.is_windows() {
            Some("接続診断は macOS / Windows に対応しています".into())
        } else {
            None
        }
    })
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
            Kind::Specs => tune_core::specs::select(std::slice::from_ref(node), std::slice::from_ref(&node.id)).err(),
            Kind::Network => network_refusal(node),
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
pub async fn run(engine: Arc<Engine>, kind: Kind, ids: Option<Vec<String>>) -> Receipt {
    let cfg = engine.reload_config();
    if let Some(error) = engine.config_error() {
        return Receipt::error(kind, &error);
    }
    let (targets, excluded) = match select(kind, &cfg.nodes, ids.as_deref()) {
        Ok(v) => v,
        Err(error) => return Receipt::error(kind, &error),
    };
    if targets.is_empty() {
        return Receipt::from_result(kind, &json!({if matches!(kind, Kind::Specs | Kind::Network) { "nodes" } else { "results" }: []}), excluded);
    }
    let mut value = match kind {
        Kind::Probe => engine.run_probe(ids, false).await,
        Kind::Logs => engine.sync_logs(targets.clone()).await,
        Kind::Inventory => engine.run_inventory(ids).await,
        Kind::Ai => engine.ai_sync(ids).await,
        Kind::Network => match engine.network_check(&targets.iter().map(|n| n.id.clone()).collect::<Vec<_>>(), false).await {
            Ok(value) => value,
            Err(error) => return Receipt::error(kind, &error),
        },
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
    if matches!(kind, Kind::Specs | Kind::Network)
        && let Some(rows) = value["nodes"].as_array_mut()
    {
        rows.retain(|row| targets.iter().any(|node| row["node_id"] == node.id));
    }
    Receipt::from_result(kind, &value, excluded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tune_core::engine::NoHost;

    #[test]
    fn network_false_and_missing_flags_are_partial_not_failed() {
        let all = json!({"interface_up":true,"link_active":true,"ip_address_present":true,"default_route_present":true,"dns_configured":true});
        let mut no_route = all.clone();
        no_route["default_route_present"] = json!(false);
        let mut unknown = all.clone();
        unknown.as_object_mut().unwrap().remove("dns_configured");
        let r = Receipt::from_result(
            Kind::Network,
            &json!({"nodes":[
                {"node_id":"good","ok":true,"data":all},
                {"node_id":"no-route","ok":true,"data":no_route},
                {"node_id":"unknown","ok":true,"data":unknown},
                {"node_id":"down","ok":false,"error":"ssh failed"},
                {"node_id":"off","skipped":true}
            ]}),
            vec![],
        );
        assert_eq!((r.complete, r.partial, r.failed, r.skipped), (1, 2, 1, 1));
        assert!(r.rows[1].detail.contains("既定経路 ×"));
        assert!(r.rows[2].detail.contains("DNS –"));
    }

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
        let unknown = run(engine.clone(), Kind::Logs, Some(vec!["unknown".into()])).await;
        assert_eq!(unknown.failed, 1);
        for kind in [Kind::Probe, Kind::Logs, Kind::Inventory, Kind::Ai, Kind::Specs] {
            let r = run(engine.clone(), kind, None).await;
            assert_eq!((r.complete, r.failed, r.skipped), (0, 0, 1), "excluded nodes must not start {kind:?}");
        }
        std::fs::write(&config, "{broken synthetic ledger").unwrap();
        for kind in [Kind::Probe, Kind::Logs, Kind::Inventory, Kind::Ai, Kind::Specs] {
            let r = run(engine.clone(), kind, None).await;
            assert_eq!(r.failed, 1, "must fail closed for {kind:?}");
            assert_eq!(r.complete, 0);
        }
        for marker in ["lastProbeAt", "lastLogsAt", "lastInventoryAt", "lastAiAt"] {
            assert_eq!(engine.with_db(|d| d.get_meta(marker)).unwrap(), None);
        }
    }
}
