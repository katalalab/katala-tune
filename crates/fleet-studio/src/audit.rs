use serde_json::Value;

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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    Local,
    Fleet,
}

pub fn targets(nodes: &[tune_core::nodes::Node], scope: Scope) -> Vec<tune_core::nodes::Node> {
    nodes.iter().filter(|node| tune_core::hygiene::enabled(node) && !node.shared && (scope == Scope::Fleet || node.local)).cloned().collect()
}

/// Preserve unknown repositories and incomplete node results.
pub fn project(results: &[Value]) -> Audit {
    let mut audit = Audit::default();
    for result in results {
        let node = result.get("node_id").and_then(Value::as_str).unwrap_or("不明").to_owned();
        let at = result.get("at").and_then(Value::as_i64);
        audit.observed_nodes.push(node.clone());
        let no_roots = result.get("roots").and_then(Value::as_array).is_none_or(Vec::is_empty);
        let mut incomplete = result.get("ok").and_then(Value::as_bool) != Some(true)
            || result.get("truncated").and_then(Value::as_bool) == Some(true)
            || result.get("skipped").is_some_and(|value| value.as_u64().is_none_or(|count| count > 0))
            || result.get("error").is_some_and(|value| !value.is_null())
            || no_roots;
        if let Some(repos) = result.get("repos").and_then(Value::as_array) {
            for repo in repos {
                let observed = repo.get("state").and_then(Value::as_str) == Some("ok");
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
                    error: (!observed).then(|| safe_error(repo.get("reason").and_then(Value::as_str))),
                });
            }
        } else {
            incomplete = true;
        }
        if incomplete && !audit.incomplete_nodes.contains(&node) {
            let reason = if result.get("ok").and_then(Value::as_bool) != Some(true) {
                safe_error(result.get("error").and_then(Value::as_str))
            } else if no_roots {
                "探索先を確認できません"
            } else if result.get("truncated").and_then(Value::as_bool) == Some(true) {
                "時間・件数の上限に到達"
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
        Some("no_python") => "Python を利用できません",
        Some("git_failed") => "Git の読み取りに失敗",
        Some("over_limit") => "時間・件数の上限に到達",
        Some("excluded") => "運用対象外",
        _ => "詳細不明",
    }
}

/// Run the existing bounded, read-only hygiene probe only when explicitly requested.
pub fn collect(config_path: &std::path::Path, scope: Scope) -> Result<Audit, &'static str> {
    let config = tune_core::nodes::load_config(config_path).map_err(|_| "台帳を読み取れません")?;
    let nodes = targets(&config.nodes, scope);
    if nodes.is_empty() {
        return Err("監査対象の機体がありません");
    }
    let local: Vec<_> = nodes.iter().filter(|node| node.local).map(|node| node.id.clone()).collect();
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().map_err(|_| "監査を開始できません")?;
    let results = runtime.block_on(tune_core::hygiene::audit_all(&nodes, &serde_json::json!({"budget_s":20,"cmd_s":5,"max_repos":1000})));
    let mut audit = project(&results);
    for repo in &mut audit.repos {
        repo.local = local.contains(&repo.node);
    }
    Ok(audit)
}
