//! 常時監視の殻。中身は tune-core の `monitor`（集計・ハブとの同期・ダッシュボード）。
//!
//! 15 秒ごとに: 常時監視が入っていてハブの数字が新しくなければ、全機体のライブの流れに合図を送って保つ
//! （画面が見ていなくても、ウィンドウを閉じてメニューバー・通知領域に居るあいだも）。
//! 終わった 1 分ぶんを集計して保存し、ハブを選んでいれば 1 分ごとに写す。14 日より古い集計は 1 日 1 回消す。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, State};
use tune_core::db::now_ms;
use tune_core::engine::Engine;
use tune_core::live::Live;
use tune_core::monitor::{self, KEEP_DAYS, MINUTE};
use tune_core::nodes::{self, Node};

pub fn start(app: AppHandle, engine: Arc<Engine>, live: Arc<Live>) {
    tauri::async_runtime::spawn(async move {
        let mut rolled: HashMap<String, i64> = HashMap::new();
        let (mut last_pull, mut last_prune) = (0i64, 0i64);
        let host = nodes::local_host();
        let mut iv = tokio::time::interval(Duration::from_secs(15));
        loop {
            iv.tick().await;
            let now = now_ms();
            let (s, collect) = engine.with_db(|d| Ok((monitor::settings(d), monitor::should_collect(d, now)))).unwrap_or_default();
            if s.hub.is_some() && now - last_pull >= MINUTE - 5_000 {
                last_pull = now;
                if monitor::pull(&engine).await.is_ok() {
                    let _ = app.emit("monitor", json!({ "pulled": true }));
                }
            }
            let cfg = engine.config();
            if collect {
                let targets: Vec<Node> = cfg.nodes.iter().filter(|n| n.is_mac() || n.is_windows()).cloned().collect();
                // 流れていれば合図、止まっていれば流し直す（接続切れで止まったものも次の回に試す）
                live.start(&targets, &[]);
            }
            // ハブの数字を写しているあいだは、手元のライブの点で同じ分を上書きしない
            if collect || s.hub.is_none() {
                let until = now.div_euclid(MINUTE) * MINUTE;
                let mut rows = Vec::new();
                for n in &cfg.nodes {
                    let from = rolled.get(&n.id).map_or(until - 5 * MINUTE, |m| m + MINUTE);
                    if from >= until {
                        continue;
                    }
                    let got = monitor::rollup(&n.id, &live.samples(&n.id), live.procs(&n.id).as_ref(), from, until, &host);
                    if let Some(last) = got.last() {
                        rolled.insert(n.id.clone(), last.minute);
                    }
                    rows.extend(got);
                }
                if !rows.is_empty() && engine.with_db(|d| d.add_metrics(&rows)).is_ok() {
                    let _ = app.emit("monitor", json!({ "rows": rows.len() }));
                }
            }
            if now - last_prune >= 24 * 60 * MINUTE {
                last_prune = now;
                let _ = engine.with_db(|d| d.prune_metrics(now - KEEP_DAYS * 24 * 60 * MINUTE));
            }
        }
    });
}

/// 常時監視でライブを保っているか（ウィンドウを閉じても・画面が止めても流し続ける）
pub fn collecting(engine: &Engine) -> bool {
    engine.with_db(|d| Ok(monitor::should_collect(d, now_ms()))).unwrap_or(false)
}

#[tauri::command]
pub async fn dashboard(e: State<'_, Arc<Engine>>, l: State<'_, Arc<Live>>, minutes: Option<i64>) -> Result<Value, String> {
    monitor::dashboard(&e, Some(&l), minutes.unwrap_or(60))
}

/// 常時監視とハブの設定を見る・変える（patch が無ければ見るだけ）
#[tauri::command]
pub async fn monitor_settings(e: State<'_, Arc<Engine>>, patch: Option<Value>) -> Result<Value, String> {
    let cfg = e.reload_config();
    let s = match patch {
        Some(p) => e.with_db(|d| Ok(monitor::set_settings(d, &cfg, &p)))??,
        None => e.with_db(|d| Ok(monitor::settings(d)))?,
    };
    Ok(json!({ "monitor": s.json(), "hub_pull": e.with_db(|d| d.get_meta("hubPull"))? }))
}

#[tauri::command]
pub async fn hub_pull(e: State<'_, Arc<Engine>>) -> Result<Value, String> {
    monitor::pull(&e).await
}
