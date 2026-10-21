use std::sync::Arc;

use serde_json::Value;
use tauri::{Emitter, State};
use tune_core::{
    engine::Engine,
    topology::export::{ExportView, public_json, public_mermaid},
};

#[tauri::command]
pub async fn topology_view(e: State<'_, Arc<Engine>>, request: Option<tune_core::db::TopologyUiRequest>) -> Result<Value, String> {
    e.topology_ui_view(&request.unwrap_or_default())
}

#[tauri::command]
pub async fn topology_refresh(app: tauri::AppHandle, e: State<'_, Arc<Engine>>) -> Result<Value, String> {
    let receipt = e.topology_refresh("manual").await;
    let _ = app.emit("topology-updated", &receipt);
    Ok(receipt)
}

#[tauri::command]
pub async fn topology_export(e: State<'_, Arc<Engine>>, view: String, format: String) -> Result<String, String> {
    let view = match view.as_str() {
        "all" => ExportView::All,
        "hosts" => ExportView::Hosts,
        "dependencies" => ExportView::Dependencies,
        "cicd" => ExportView::Cicd,
        _ => return Err("unknown topology export view".into()),
    };
    let graph = e.topology_accepted_view()?;
    match format.as_str() {
        "json" => Ok(public_json(&graph, view)),
        "mermaid" => Ok(public_mermaid(&graph, view)),
        _ => Err("unknown topology export format".into()),
    }
}
