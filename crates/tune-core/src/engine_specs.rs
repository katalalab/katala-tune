//! Private latest specifications and attempt state, independent of performance snapshots.
use crate::{
    db::{Store, now_ms},
    engine::Engine,
    nodes::Node,
    specs,
};
use serde_json::{Value, json};

pub fn save_attempt(db: &Store, node: &Node, result: &Value) -> Result<(), String> {
    let at = result["at"].as_i64().unwrap_or_else(now_ms);
    let mut attempt = json!({"at":at,"ok":false});
    if result["ok"] == true {
        let report = specs::validate_report(&result["report"], &node.os)?;
        if report["ok"] != true {
            return Err("取得可能な情報がありません".into());
        }
        db.set_meta(&format!("specsReport:{}", node.id), &report).map_err(|e| e.to_string())?;
        attempt["ok"] = json!(true);
    } else {
        attempt["error"] = json!(result["error"].as_str().unwrap_or("収集失敗").chars().take(512).collect::<String>());
    }
    db.set_meta(&format!("specsAttempt:{}", node.id), &attempt).map_err(|e| e.to_string())
}

pub fn load_node(db: &Store, node: &Node) -> Result<Value, String> {
    let report = db.get_meta(&format!("specsReport:{}", node.id)).map_err(|e| e.to_string())?.and_then(|v| specs::validate_report(&v, &node.os).ok());
    let last_attempt = db.get_meta(&format!("specsAttempt:{}", node.id)).map_err(|e| e.to_string())?;
    let stale = report.as_ref().is_none_or(|r| now_ms().saturating_sub(r["at"].as_i64().unwrap_or(0)) > 86_400_000)
        || last_attempt.as_ref().is_some_and(|a| a["ok"] != true);
    Ok(
        json!({"node_id":node.id,"os":node.os,"report":report,"last_attempt":last_attempt,"stale":stale,"eligible":specs::select(std::slice::from_ref(node),std::slice::from_ref(&node.id)).is_ok()}),
    )
}

impl Engine {
    pub fn specs_view(&self) -> Result<Value, String> {
        let config = self.reload_config();
        if let Some(error) = self.config_error() {
            return Err(error);
        }
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let nodes = config.nodes.iter().map(|node| load_node(&db, node)).collect::<Result<Vec<_>, _>>()?;
        Ok(json!({"nodes":nodes,"running":self.specs_running.try_lock().is_err(),"at":now_ms()}))
    }

    pub async fn specs_refresh(&self, ids: Vec<String>) -> Result<Value, String> {
        let Ok(_guard) = self.specs_running.try_lock() else {
            let mut view = self.specs_view()?;
            view["busy"] = json!(true);
            return Ok(view);
        };
        let config = self.reload_config();
        if let Some(error) = self.config_error() {
            return Err(error);
        }
        let selected = specs::select(&config.nodes, &ids)?;
        let mut jobs = tokio::task::JoinSet::new();
        // Four machines at a time; each local collector has its own finite provider budget.
        for batch in selected.chunks(4) {
            for node in batch {
                let node = node.clone();
                jobs.spawn(async move {
                    let result = specs::collect_node(&node).await;
                    (node, result)
                });
            }
            while let Some(result) = jobs.join_next().await {
                let (node, result) = result.map_err(|_| "収集タスクが停止しました")?;
                let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                save_attempt(&db, &node, &result)?;
            }
        }
        drop(_guard);
        self.specs_view()
    }
}
