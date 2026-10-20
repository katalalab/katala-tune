//! Deterministic public graph exports. The database keeps internal receipts;
//! these views intentionally expose a narrow, presentation-safe subset.

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportView {
    All,
    Hosts,
    Dependencies,
    Cicd,
}

impl ExportView {
    fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Hosts => "hosts",
            Self::Dependencies => "dependencies",
            Self::Cicd => "cicd",
        }
    }

    fn entity(self, kind: &str) -> bool {
        match self {
            Self::All => true,
            Self::Hosts => matches!(
                kind,
                "docker_engine"
                    | "docker_container"
                    | "docker_network"
                    | "docker_volume"
                    | "docker_mount"
                    | "host"
                    | "environment"
                    | "repository"
                    | "checkout"
                    | "worktree"
            ),
            Self::Dependencies => matches!(kind, "repository" | "checkout" | "workspace" | "package"),
            Self::Cicd => matches!(kind, "repository" | "workflow" | "job" | "artifact" | "deployment"),
        }
    }

    fn edge(self, kind: &str) -> bool {
        match self {
            Self::All => true,
            Self::Hosts => matches!(
                kind,
                "docker_contains"
                    | "docker_network_member"
                    | "docker_mounts"
                    | "contains_environment"
                    | "contains_checkout"
                    | "checkout_of"
                    | "worktree_of"
                    | "contains_workspace"
            ),
            Self::Dependencies => {
                matches!(kind, "contains_checkout" | "contains_workspace" | "contains_package" | "workspace_member" | "declares_dependency" | "resolves_to")
            }
            Self::Cicd => matches!(kind, "has_workflow" | "has_job" | "needs_job" | "uses_action" | "uses_workflow" | "runs_on" | "deploys_to" | "stores_in"),
        }
    }
}

fn known_entity(kind: &str) -> bool {
    matches!(
        kind,
        "docker_engine"
            | "docker_container"
            | "docker_network"
            | "docker_volume"
            | "docker_mount"
            | "host"
            | "environment"
            | "repository"
            | "checkout"
            | "worktree"
            | "workspace"
            | "package"
            | "configuration"
            | "workflow"
            | "job"
            | "artifact"
            | "deployment"
            | "installed_artifact"
    )
}

fn known_edge(kind: &str) -> bool {
    matches!(
        kind,
        "docker_contains"
            | "docker_network_member"
            | "docker_mounts"
            | "contains_environment"
            | "contains_checkout"
            | "checkout_of"
            | "worktree_of"
            | "contains_workspace"
            | "contains_package"
            | "workspace_member"
            | "declares_dependency"
            | "resolves_to"
            | "has_workflow"
            | "has_job"
            | "needs_job"
            | "uses_action"
            | "uses_workflow"
            | "runs_on"
            | "reads_config"
            | "installed_from"
            | "deploys_to"
            | "stores_in"
            | "collects_from"
    )
}

fn opaque_id(value: &str, namespace: &str) -> bool {
    let mut parts = value.split('.');
    matches!((parts.next(), parts.next(), parts.next(), parts.next(), parts.next(), parts.next()), (Some("kt"), Some("topology"), Some("v1"), Some(found), Some(hash), None) if found == namespace && hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn currentness(receipts: &std::collections::BTreeMap<&str, (&str, i64)>, scope_ids: &[Value]) -> (&'static str, Option<i64>) {
    if scope_ids.is_empty() {
        return ("unknown", None);
    }
    let mut state = "complete";
    let mut observed_at = None;
    for value in scope_ids {
        let Some(scope_id) = value.as_str() else { return ("unknown", None) };
        let Some((status, at)) = receipts.get(scope_id).copied() else { return ("unknown", None) };
        observed_at = observed_at.max(Some(at));
        match status {
            "failed" => state = "failed",
            "partial" if state != "failed" => state = "partial",
            "partial" => {}
            "complete" => {}
            _ => return ("unknown", None),
        }
    }
    (state, observed_at)
}

fn public_view(source: &Value, view: ExportView) -> Value {
    let scope_rows = source["scopes"].as_array().into_iter().flatten().collect::<Vec<_>>();
    let mut receipt_counts = std::collections::BTreeMap::<&str, usize>::new();
    for receipt in &scope_rows {
        if let Some(scope_id) = receipt["scope_id"].as_str() {
            *receipt_counts.entry(scope_id).or_default() += 1;
        }
    }
    let receipts = scope_rows
        .into_iter()
        .filter_map(|receipt| {
            let scope_id = receipt["scope_id"].as_str()?;
            (receipt_counts.get(scope_id) == Some(&1))
                .then_some((scope_id, (receipt["status"].as_str()?, receipt["observed_at"].as_i64().filter(|at| *at >= 0)?)))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let entity_scopes = source["entities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entity| Some((entity["id"].as_str()?, entity["scope_ids"].as_array()?.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    let edge_scopes = source["edges"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|edge| Some((edge["id"].as_str()?, edge["scope_ids"].as_array()?.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut entities = source["entities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entity| {
            let kind = entity["kind"].as_str()?;
            let id = entity["id"].as_str()?;
            (known_entity(kind) && opaque_id(id, kind) && view.entity(kind)).then_some((id, kind))
        })
        .collect::<Vec<_>>();
    entities.sort_unstable();
    let mut ordinal = std::collections::BTreeMap::new();
    let mut public_ids = std::collections::BTreeMap::new();
    let entities = entities
        .into_iter()
        .enumerate()
        .map(|(index, (id, kind))| {
            let next = ordinal.entry(kind).or_insert(0usize);
            *next += 1;
            let scopes = entity_scopes.get(id).cloned().unwrap_or_default();
            let (currentness, observed_at_ms) = currentness(&receipts, &scopes);
            let public_id = format!("entity-{}", index + 1);
            public_ids.insert(id, public_id.clone());
            json!({"id":public_id,"kind":kind,"label":format!("{kind}-{next}"),"currentness":currentness,"observed_at_ms":observed_at_ms})
        })
        .collect::<Vec<_>>();
    let mut edges = source["edges"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|edge| {
            let kind = edge["kind"].as_str()?;
            let id = edge["id"].as_str()?;
            let from = edge["from_id"].as_str()?;
            let to = edge["to_id"].as_str()?;
            (known_edge(kind) && opaque_id(id, "edge") && view.edge(kind)).then(|| {
                let from = public_ids.get(from)?;
                let to = public_ids.get(to)?;
                let scopes = edge_scopes.get(id).cloned().unwrap_or_default();
                let (currentness, observed_at_ms) = currentness(&receipts, &scopes);
                Some((id, json!({"kind":kind,"from":from,"to":to,"currentness":currentness,"observed_at_ms":observed_at_ms})))
            })
        })
        .flatten()
        .collect::<Vec<_>>();
    edges.sort_by_key(|(left, _)| *left);
    let edges = edges
        .into_iter()
        .enumerate()
        .map(|(index, (_, mut edge))| {
            edge["id"] = Value::String(format!("edge-{}", index + 1));
            edge
        })
        .collect::<Vec<_>>();
    json!({"schema":"katala_topology.public.v1", "view":view.name(), "entities":entities, "edges":edges})
}

/// Byte-stable JSON with no receipt fields, attributes or failure text.
pub fn public_json(source: &Value, view: ExportView) -> String {
    serde_json::to_string(&public_view(source, view)).unwrap_or_else(|_| "{\"schema\":\"katala_topology.public.v1\"}".into())
}

fn mermaid_text(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '"' => "&quot;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '&' => "&amp;".into(),
            '\n' | '\r' => " ".into(),
            c if c.is_control() => String::new(),
            c => c.to_string(),
        })
        .collect()
}

/// Mermaid ids are local ordinals; labels and edge kinds derive only from
/// allowlisted enum values.
pub fn public_mermaid(source: &Value, view: ExportView) -> String {
    let graph = public_view(source, view);
    let entities = graph["entities"].as_array().cloned().unwrap_or_default();
    let mut names = std::collections::BTreeMap::new();
    let mut lines = vec!["flowchart LR".to_owned()];
    for (index, entity) in entities.iter().enumerate() {
        let id = entity["id"].as_str().unwrap_or_default();
        let name = format!("n{index}");
        names.insert(id, name.clone());
        let label = mermaid_text(entity["label"].as_str().unwrap_or("redacted"));
        let state = entity["currentness"].as_str().unwrap_or("unknown");
        lines.push(format!("  {name}[\"{label} ({state})\"]"));
    }
    for edge in graph["edges"].as_array().into_iter().flatten() {
        if let (Some(from), Some(to)) = (edge["from"].as_str().and_then(|id| names.get(id)), edge["to"].as_str().and_then(|id| names.get(id))) {
            let kind = edge["kind"].as_str().unwrap_or_default();
            let state = edge["currentness"].as_str().unwrap_or("unknown");
            lines.push(format!("  {from} -->|{kind} ({state})| {to}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{ExportView, currentness, mermaid_text, public_json, public_mermaid};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    #[test]
    fn mermaid_text_escapes_markup_and_controls() {
        assert_eq!(mermaid_text("<a>&\"\n"), "&lt;a&gt;&amp;&quot; ");
    }

    #[test]
    fn public_export_keeps_aggregated_currentness_without_scope_names() {
        let id = "kt.topology.v1.host.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let graph = json!({"entities":[{"id":id,"kind":"host","scope_ids":["private-a"]}],"edges":[],"scopes":[{"scope_id":"private-a","status":"partial","observed_at":1}]});
        let exported: serde_json::Value = serde_json::from_str(&public_json(&graph, ExportView::All)).unwrap();
        assert_eq!(exported["entities"][0]["currentness"], "partial");
        assert_eq!(exported["entities"][0]["observed_at_ms"], 1);
        assert!(!public_json(&graph, ExportView::All).contains("private-a"));
        assert!(public_mermaid(&graph, ExportView::All).contains("(partial)"));
        let absent = json!({"entities":[{"id":id,"kind":"host","scope_ids":["scope-201"]}],"edges":[],"scopes":[]});
        let absent: serde_json::Value = serde_json::from_str(&public_json(&absent, ExportView::All)).unwrap();
        assert_eq!(absent["entities"][0]["currentness"], "unknown", "a scope outside the bounded receipt projection is never represented as current");
        let empty = json!({"entities":[{"id":id,"kind":"host","scope_ids":[]}],"edges":[],"scopes":[]});
        let empty: serde_json::Value = serde_json::from_str(&public_json(&empty, ExportView::All)).unwrap();
        let receipts = BTreeMap::from([("complete", ("complete", 1)), ("partial", ("partial", 2)), ("failed", ("failed", 3))]);
        assert_eq!(empty["entities"][0]["currentness"], "unknown");
        assert_eq!(currentness(&receipts, &[Value::Null]), ("unknown", None));
        assert_eq!(currentness(&receipts, &[json!("complete"), json!(7)]), ("unknown", None));
        assert_eq!(currentness(&receipts, &[json!("complete"), json!("partial")]), ("partial", Some(2)));
        assert_eq!(currentness(&receipts, &[json!("partial"), json!("failed")]), ("failed", Some(3)));
        assert_eq!(currentness(&receipts, &[json!("failed"), json!("partial"), json!("complete")]), ("failed", Some(3)));
        assert_eq!(currentness(&receipts, &[json!("partial"), json!("missing")]), ("unknown", None));
    }

    #[test]
    fn public_export_treats_invalid_or_duplicate_receipts_as_unknown() {
        let id = "kt.topology.v1.host.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        for receipt in [
            json!({"scope_id":"scope","status":"complete"}),
            json!({"scope_id":"scope","status":"complete","observed_at":null}),
            json!({"scope_id":"scope","status":"complete","observed_at":-1}),
        ] {
            let graph = json!({"entities":[{"id":id,"kind":"host","scope_ids":["scope"]}],"edges":[],"scopes":[receipt]});
            let exported: Value = serde_json::from_str(&public_json(&graph, ExportView::All)).unwrap();
            assert_eq!(exported["entities"][0]["currentness"], "unknown");
            assert!(exported["entities"][0]["observed_at_ms"].is_null());
        }
        let graph = json!({"entities":[{"id":id,"kind":"host","scope_ids":["scope"]}],"edges":[],"scopes":[
            {"scope_id":"scope","status":"complete","observed_at":1},
            {"scope_id":"scope","status":"failed","observed_at":2}
        ]});
        let exported: Value = serde_json::from_str(&public_json(&graph, ExportView::All)).unwrap();
        assert_eq!(exported["entities"][0]["currentness"], "unknown");
        assert!(exported["entities"][0]["observed_at_ms"].is_null());
    }
}
