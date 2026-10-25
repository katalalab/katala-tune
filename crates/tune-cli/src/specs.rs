//! Validate before initializing Engine; saved reports only open existing state read-only.
use serde_json::{Value, json};
use std::sync::Arc;
use tune_core::{
    db::Store,
    engine::{Engine, NoHost},
    nodes,
};

pub fn audit_targets(rest: &[String]) -> Result<Vec<nodes::Node>, String> {
    if let Some(option) = rest.iter().find(|arg| arg.starts_with('-')) {
        return Err(format!("unknown versions option: {option}"));
    }
    let config = nodes::load_config(&nodes::user_config_path())?;
    let ids = if rest.is_empty() {
        config.nodes.iter().filter(|n| n.is_active() && tune_core::specs::enabled(n)).map(|n| n.id.clone()).collect()
    } else {
        rest.to_vec()
    };
    tune_core::specs::select(&config.nodes, &ids)
}

pub async fn run(rest: &[String]) -> Result<Value, String> {
    let mut saved = false;
    let mut ids = Vec::new();
    for arg in rest {
        match arg.as_str() {
            "--saved" if !saved => saved = true,
            option if option.starts_with('-') => return Err(format!("unknown specs option: {option}")),
            id => ids.push(id.to_string()),
        }
    }
    let config = nodes::load_config(&nodes::user_config_path())?;
    if ids.len() > 64 || ids.iter().any(|id| config.node(id).is_none()) {
        return Err("未知の機体です".into());
    }
    if saved {
        let dir = nodes::data_dir();
        let exists = dir.join(tune_core::db::DB_FILE).try_exists().map_err(|e| e.to_string())?;
        let db = if exists { Some(Store::open_read_only(&dir).map_err(|e| e.to_string())?) } else { None };
        let reports = config
            .nodes
            .iter()
            .filter(|node| ids.is_empty() || ids.contains(&node.id))
            .map(|node| match &db {
                Some(db) => tune_core::engine_specs::load_node(db, node),
                None => Ok(json!({"node_id":node.id,"os":node.os,"report":null,"last_attempt":null,"stale":true,
                    "eligible":tune_core::specs::select(std::slice::from_ref(node),std::slice::from_ref(&node.id)).is_ok()})),
            })
            .collect::<Result<Vec<_>, String>>()?;
        return Ok(json!({"nodes":reports,"read_only":true,"at":tune_core::db::now_ms()}));
    }
    if ids.is_empty() {
        ids = config.nodes.iter().filter(|n| n.is_active() && tune_core::specs::enabled(n)).map(|n| n.id.clone()).collect();
    }
    tune_core::specs::select(&config.nodes, &ids)?;
    let engine = Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost))?;
    engine.specs_refresh(ids).await
}
