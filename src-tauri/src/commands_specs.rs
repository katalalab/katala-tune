use serde_json::Value;
use std::sync::Arc;
use tauri::State;
use tune_core::engine::Engine;

#[tauri::command]
pub async fn specs_view(e: State<'_, Arc<Engine>>) -> Result<Value, String> {
    e.specs_view()
}

#[tauri::command]
pub async fn specs_refresh(e: State<'_, Arc<Engine>>, ids: Vec<String>) -> Result<Value, String> {
    let e = e.inner().clone();
    e.specs_refresh(ids).await
}
