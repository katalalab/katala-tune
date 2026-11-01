use serde_json::Value;

pub const REMOTE_DISABLED: &str = "遠隔監査は機体ごとの権限と接続先が未確認のため未対応です";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoAudit {
    pub node: String,
    pub path: String,
    pub branch: Option<String>,
    pub dirty: Option<u64>,
    pub unpushed: Option<u64>,
    pub behind: Option<u64>,
    pub local_only: Option<u64>,
    pub previous: bool,
    pub local: bool,
    pub at: Option<i64>,
    pub error: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditIssue {
    pub node: String,
    pub at: Option<i64>,
    pub reason: &'static str,
    pub previous: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Audit {
    pub repos: Vec<RepoAudit>,
    pub incomplete_nodes: Vec<String>,
    pub issues: Vec<AuditIssue>,
    pub observed_nodes: Vec<String>,
    pub identity: Option<AuditIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    Local,
    Fleet,
}

pub fn targets(nodes: &[tune_core::nodes::Node], scope: Scope) -> Vec<tune_core::nodes::Node> {
    if scope == Scope::Fleet {
        return Vec::new();
    }
    let ambiguous = crate::recent::local_ambiguous(nodes);
    let mut counts = std::collections::HashMap::new();
    for node in nodes {
        *counts.entry(node.id.as_str()).or_insert(0usize) += 1;
    }
    nodes
        .iter()
        .filter(|node| {
            let valid_id = node
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty() && id.len() <= 256 && !id.chars().any(char::is_control) && counts.get(id) == Some(&1));
            let valid_host = node.get("local_hostname").and_then(Value::as_str).and_then(crate::recent::normalized_hostname).is_some();
            let valid_setting = matches!(node.get("repo_hygiene"), None | Some(Value::Bool(_)));
            valid_id
                && valid_host
                && valid_setting
                && matches!(node.get("shared"), None | Some(Value::Bool(false)))
                && (node.is_mac() || node.is_windows())
                && tune_core::hygiene::enabled(node)
                && node.local
                && !ambiguous
        })
        .cloned()
        .collect()
}

pub fn targets_for_host(nodes: &[tune_core::nodes::Node], scope: Scope, host: &str) -> Vec<tune_core::nodes::Node> {
    if crate::recent::normalized_hostname(host).is_none() { Vec::new() } else { targets(&crate::recent::resolved_nodes(nodes, host), scope) }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditIdentity {
    pub node: String,
    pub fingerprint: String,
}

/// Reuse the existing local audit eligibility checks and bind the unique row.
pub fn identity_for_host(nodes: &[tune_core::nodes::Node], host: &str) -> Option<AuditIdentity> {
    let selected = targets_for_host(nodes, Scope::Local, host);
    if selected.len() != 1 {
        return None;
    }
    let node = selected.first()?;
    let fingerprint = crate::source::fingerprint_for_node(nodes, host, &node.id)?;
    Some(AuditIdentity { node: node.id.clone(), fingerprint })
}

pub fn identity_matches(nodes: Option<&[tune_core::nodes::Node]>, host: &str, expected: &AuditIdentity) -> bool {
    nodes.and_then(|nodes| identity_for_host(nodes, host)).as_ref() == Some(expected)
}

/// Re-read immediately before opening an audit location. Do not expose errors.
pub fn revalidate_identity(config_path: &std::path::Path, host: &str, expected: &AuditIdentity) -> bool {
    let current = tune_core::nodes::load_config(config_path).ok();
    identity_matches(current.as_ref().map(|config| config.nodes.as_slice()), host, expected)
}

pub fn selection_warnings(selected: &[tune_core::nodes::Node], resolved: &[tune_core::nodes::Node], at: i64) -> Vec<AuditIssue> {
    let mut issues = Vec::new();
    if selected.iter().any(|node| node.get("shared").is_none()) {
        issues.push(AuditIssue {
            node: "この機体".into(), at: Some(at), reason: "共有設定は未記入（明示操作でこの機体のみ監査）", previous: false
        });
    }
    if crate::recent::local_ambiguous(resolved)
        || resolved.iter().any(|node| {
            !matches!(node.get("shared"), None | Some(Value::Bool(_)))
                || !matches!(node.get("repo_hygiene"), None | Some(Value::Bool(_)))
                || node.get("local_hostname").and_then(Value::as_str).and_then(crate::recent::normalized_hostname).is_none()
        })
    {
        issues.push(AuditIssue { node: "台帳".into(), at: Some(at), reason: "対象不明の機体を除外", previous: false });
    }
    issues
}

/// Preserve unknown repositories and incomplete node results.
pub fn project(results: &[Value]) -> Audit {
    let mut audit = Audit::default();
    for original in results {
        // 再正規化しても観測時刻・機体 ID・収集失敗は元の結果から引き継ぐ。
        let normalized = tune_core::hygiene::normalize(original);
        let result = &normalized;
        let node = original.get("node_id").and_then(Value::as_str).unwrap_or("不明").to_owned();
        let at = original.get("at").and_then(Value::as_i64);
        audit.observed_nodes.push(node.clone());
        let no_roots = result.get("roots").and_then(Value::as_array).is_none_or(Vec::is_empty);
        let enumerated = result.get("enumerated").and_then(Value::as_u64);
        let truncated = result.get("truncated").and_then(Value::as_bool);
        let skipped = result.get("skipped").and_then(Value::as_u64);
        let completion_unknown = enumerated.is_none() || truncated.is_none() || skipped.is_none();
        let fields_incomplete = match result.get("incomplete_fields") {
            None => false,
            Some(Value::Array(fields)) => !fields.is_empty(),
            Some(_) => true,
        };
        let node_ok = original.get("ok").and_then(Value::as_bool) == Some(true);
        let mut incomplete = !node_ok
            || truncated != Some(false)
            || skipped != Some(0)
            || completion_unknown
            || fields_incomplete
            || result.get("error").is_some_and(|value| !value.is_null())
            || no_roots;
        if let Some(repos) = result.get("repos").and_then(Value::as_array) {
            for repo in repos {
                let row_ok = repo.get("state").and_then(Value::as_str) == Some("ok");
                let observed = node_ok && row_ok;
                if !observed {
                    incomplete = true;
                }
                let dirty = if observed {
                    repo.get("tracked")
                        .and_then(Value::as_u64)
                        .and_then(|tracked| repo.get("untracked").and_then(Value::as_u64).and_then(|untracked| tracked.checked_add(untracked)))
                } else {
                    None
                };
                let unpushed = if observed {
                    match repo.get("upstream").and_then(Value::as_bool) {
                        Some(true) => repo.get("ahead").and_then(Value::as_u64),
                        Some(false) => None,
                        None => None,
                    }
                } else {
                    None
                };
                audit.repos.push(RepoAudit {
                    node: node.clone(),
                    path: repo.get("path").and_then(Value::as_str).unwrap_or("不明").to_owned(),
                    branch: repo.get("branch").and_then(Value::as_str).map(str::to_owned),
                    dirty,
                    unpushed,
                    behind: (observed && repo.get("upstream").and_then(Value::as_bool) == Some(true))
                        .then(|| repo.get("behind").and_then(Value::as_u64))
                        .flatten(),
                    local_only: (observed && repo.get("upstream").and_then(Value::as_bool) == Some(false))
                        .then(|| repo.get("local_only").and_then(Value::as_u64))
                        .flatten(),
                    previous: false,
                    local: false,
                    at,
                    error: if !row_ok {
                        Some(safe_error(repo.get("reason").and_then(Value::as_str)))
                    } else if !node_ok {
                        Some("機体の監査に失敗したため未確認")
                    } else {
                        None
                    },
                });
            }
        } else {
            incomplete = true;
        }
        if incomplete && !audit.incomplete_nodes.contains(&node) {
            let error = if node_ok { result.get("error") } else { original.get("error") };
            let reason = if let Some(error) = error.and_then(Value::as_str).filter(|error| !error.is_empty()) {
                safe_error(Some(error))
            } else if !node_ok {
                safe_error(None)
            } else if no_roots {
                "探索先を確認できません"
            } else if truncated == Some(true) {
                "時間・件数の上限に到達"
            } else if completion_unknown {
                "探索の完了状態が不明"
            } else if fields_incomplete {
                "監査データの一部が不明"
            } else {
                "一部を観測できません"
            };
            audit.issues.push(AuditIssue { node: node.clone(), at, reason, previous: false });
            audit.incomplete_nodes.push(node);
        }
    }
    audit
}

/// Retain old successful rows for failed, incomplete, or unrequested nodes.
pub fn preserve_previous(previous: &Audit, next: &mut Audit) {
    for old in &previous.issues {
        if !next.observed_nodes.contains(&old.node) {
            let mut retained = old.clone();
            retained.previous = true;
            if !next.incomplete_nodes.contains(&old.node) {
                next.incomplete_nodes.push(old.node.clone());
            }
            next.issues.push(retained);
        }
    }
    for old in &previous.repos {
        let incomplete = next.incomplete_nodes.contains(&old.node) || !next.observed_nodes.contains(&old.node);
        let refreshed = next.repos.iter().any(|new| new.node == old.node && new.path == old.path && new.error.is_none() && !new.previous);
        if incomplete && !refreshed && old.error.is_none() {
            let mut retained = old.clone();
            retained.previous = true;
            next.repos.push(retained);
        }
    }
}

fn safe_error(error: Option<&str>) -> &'static str {
    match error {
        Some("timeout_or_unreachable") => "タイムアウトまたは接続不可",
        Some("timeout") => "タイムアウト",
        Some("incomplete") => "監査の完了を確認できません",
        Some("probe_failed") => "監査情報を確認できません",
        Some("no_python") => "Python を利用できません",
        Some("git_missing") => "Git を利用できません",
        Some("git_failed") => "Git の読み取りに失敗",
        Some("over_limit") => "時間・件数の上限に到達",
        Some("walk_incomplete") => "探索先の一部を確認できません",
        Some("excluded") => "運用対象外",
        _ => "詳細不明",
    }
}

/// Run the existing bounded, read-only hygiene probe only when explicitly requested.
pub fn collect(config_path: &std::path::Path, scope: Scope) -> Result<Audit, &'static str> {
    if scope == Scope::Fleet {
        return Err(REMOTE_DISABLED);
    }
    let config = tune_core::nodes::load_config(config_path).map_err(|_| "台帳を読み取れません")?;
    let host = tune_core::nodes::local_host();
    if crate::recent::normalized_hostname(&host).is_none() {
        return Err("この機体の識別が不明なため監査できません");
    }
    let identity = identity_for_host(&config.nodes, &host).ok_or("監査対象の機体を一意に照合できません")?;
    let resolved = crate::recent::resolved_nodes(&config.nodes, &host);
    let nodes = targets(&resolved, scope);
    if nodes.is_empty() {
        return Err("監査対象の機体がありません。共有設定と hostname を確認してください");
    }
    let local: Vec<_> = nodes.iter().filter(|node| node.local).map(|node| node.id.clone()).collect();
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().map_err(|_| "監査を開始できません")?;
    let results = runtime.block_on(tune_core::hygiene::audit_all(&nodes, &serde_json::json!({"budget_s":20,"cmd_s":5,"max_repos":1000})));
    let mut audit = project(&results);
    audit.issues.extend(selection_warnings(&nodes, &resolved, tune_core::db::now_ms()));
    for repo in &mut audit.repos {
        repo.local = local.contains(&repo.node);
    }
    let current = tune_core::nodes::load_config(config_path).ok();
    if !identity_matches(current.as_ref().map(|config| config.nodes.as_slice()), &tune_core::nodes::local_host(), &identity) {
        return Err("台帳の監査対象を再照合できないため結果を破棄しました");
    }
    audit.identity = Some(identity);
    Ok(audit)
}
