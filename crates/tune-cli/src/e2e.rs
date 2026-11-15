//! One-shot acceptance of collection, analysis, persistence, reload and log sync.
//! Uses a fresh directory and never starts a monitor, scheduler or updater.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tune_core::db::{Store, now_ms};
use tune_core::engine::{Engine, NoHost};
use tune_core::{collect, logs, nodes};

fn error(value: &Value) -> Value {
    let Some(error) = value.get("error") else { return Value::Null };
    if error.is_null() {
        return Value::Null;
    }
    let text = error.as_str().unwrap_or_default();
    if text.contains("approved action helper record") {
        return json!("helper_unavailable");
    }
    // Collectors may put failed stdout (including a partial log payload) in
    // error. Emit only the existing finite classification, never that text.
    json!(collect::classify_failure(&collect::RunResult { code: None, out: String::new(), err: text.to_owned() }))
}

pub async fn run(rest: &[String]) -> Result<(), String> {
    let mut directory = None;
    let mut ids = Vec::new();
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db" if directory.is_none() => {
                directory = Some(PathBuf::from(args.next().filter(|v| !v.is_empty() && !v.starts_with('-')).ok_or("--db requires a new directory")?));
            }
            value if value.starts_with('-') => return Err(format!("unknown e2e option: {value}")),
            value => ids.push(value.to_owned()),
        }
    }
    let directory = directory.ok_or("usage: tune e2e --db <new directory> [node ids ...]")?;
    let config_path = nodes::user_config_path();
    let cfg = nodes::load_config(&config_path)?;
    if cfg.nodes.is_empty() {
        return Err("empty node ledger".into());
    }
    let mut unique = HashSet::new();
    for node in &cfg.nodes {
        if !node.is_mac() && !node.is_windows() {
            return Err("unsupported node platform in ledger".into());
        }
        if !node.raw.get("id").and_then(Value::as_str).is_some_and(|id| !id.trim().is_empty()) || !unique.insert(&node.id) {
            return Err("empty or duplicate node id in ledger".into());
        }
    }
    if let Some(id) = ids.iter().find(|id| cfg.node(id).is_none()) {
        return Err(format!("unknown node id: {id}"));
    }
    let selected: Vec<_> = cfg.nodes.iter().filter(|node| ids.is_empty() || ids.contains(&node.id)).cloned().collect();
    let (targets, _) = nodes::partition(&selected);
    // Exclusive creation rejects existing directories, databases and symlinks.
    std::fs::create_dir(&directory).map_err(|_| "--db must name a new directory with an existing parent".to_string())?;
    let started_at = now_ms();
    let engine = Engine::open(config_path.clone(), directory.clone(), Arc::new(NoHost))?;
    if let Some(error) = engine.config_error() {
        return Err(error);
    }
    let probes = engine.run_probe(Some(selected.iter().map(|n| n.id.clone()).collect()), false).await;
    let synced = engine.sync_logs(targets.clone()).await;
    drop(engine);

    let engine = Engine::open(config_path, directory.clone(), Arc::new(NoHost))?;
    if let Some(error) = engine.config_error() {
        return Err(error);
    }
    let restored = engine.last()?;
    // Also query the read-only connection so verification cannot repair the DB.
    let db = Store::open_read_only(&directory).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    let mut passed = 0;
    for node in &selected {
        let state = node.op_state();
        if !state.active {
            rows.push(json!({"node_id":node.id,"status":"excluded","state":state.state}));
            continue;
        }
        let find =
            |result: &Value| result["results"].as_array().and_then(|rows| rows.iter().find(|row| row["node_id"] == node.id)).cloned().unwrap_or(Value::Null);
        let probe = find(&probes);
        let probe_ok = probe["ok"] == true;
        let snapshot = db.last_snapshots(&node.id, 1).map_err(|e| e.to_string())?.into_iter().next();
        let stored_score: Option<i64> = match &snapshot {
            Some(s) => db
                .conn()
                .query_row("SELECT score FROM tune_snapshots WHERE node_id = ? AND at = ?", (&node.id, s.at), |row| row.get(0))
                .map_err(|e| e.to_string())?,
            None => None,
        };
        let previous = restored.iter().find(|row| row["node_id"] == node.id);
        let reload_ok = probe_ok
            && snapshot
                .as_ref()
                .is_some_and(|s| s.at >= started_at && Some(s.at) == probe["at"].as_i64() && s.data["probe"] == if node.is_mac() { "mac" } else { "windows" })
            && stored_score.is_some()
            && previous.is_some_and(|row| row["ok"] == true && row["at"] == probe["at"] && row["score"].as_i64().is_some());
        let logs = find(&synced);
        let mut sources = Vec::new();
        let mut logs_ok = logs.is_object() && logs.get("error").is_none() && logs.get("skipped") != Some(&Value::Bool(true));
        for source in logs::sources_of(node) {
            let result = &logs["sources"][source];
            let cursor = db.cursor(&node.id, source).map_err(|e| e.to_string())?;
            let persisted = cursor.as_ref().is_some_and(|c| {
                c["last_ok_at"].as_i64().is_some_and(|at| at >= started_at)
                    && c["last_error"].is_null()
                    && c["last_count"] == result["fetched"]
                    && c["cursor"].as_str().is_some_and(|cursor| !cursor.is_empty())
            });
            let ok = result.is_object() && result.get("error").is_none() && persisted;
            logs_ok &= ok;
            let note = logs::source_note(Some(result));
            let source_error = if result.get("error").is_some() {
                error(result)
            } else if !persisted {
                json!("persistence_unverified")
            } else {
                Value::Null
            };
            sources.push(json!({"source":source,"ok":ok,"fetched":result["fetched"],"inserted":result["inserted"],"note":note,"error":source_error}));
        }
        let ok = probe_ok && reload_ok && logs_ok;
        passed += usize::from(ok);
        rows.push(json!({
            "node_id":node.id,"status":if ok {"passed"} else {"failed"},
            "probe":{"ok":probe_ok,"at":probe["at"],"error":error(&probe)},
            "reload":{"ok":reload_ok,"score":stored_score},
            "logs":{"ok":logs_ok,"sources":sources,"error":error(&logs)}
        }));
    }
    let ok = !targets.is_empty() && passed == targets.len();
    super::print(&json!({
        "schema_version":1,"ok":ok,"started_at":started_at,"finished_at":now_ms(),
        "counts":{"targets":targets.len(),"passed":passed,"failed":targets.len()-passed,"excluded":selected.len()-targets.len()},
        "nodes":rows
    }));
    if ok { Ok(()) } else { Err("E2E incomplete: inspect failed nodes; an empty target set is not a pass".into()) }
}
