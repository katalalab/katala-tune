//! 検証用 CLI。Tauri なしで tune-core を動かす。
//!
//!   tune paths                       台帳と DB の場所
//!   tune probe [機体 ...]            調査して JSON を標準出力へ（保存しない。npm run probe と同じ）
//!   tune logs [--db <dir>] [機体 ...] ログを取り込む（既定は一時ディレクトリの DB。npm run logs と同じ）
//!   tune last                        DB の前回の分析結果（所見・スコアつき）を JSON で
//!   tune status [--recompute]        DB の状態（機能チェック）を JSON で。--recompute で計算し直して保存する
//!   tune analyze <snapshot.json>     snapshot（probe の data）から所見を作る
//!
//! 台帳と DB は Electron 版と同じ場所（KATALA_TUNE_CONFIG・KATALA_TUNE_DATA_DIR で差し替えられる）。
//! 変更操作（actions）はここからは実行しない（確認ダイアログを通すため、アプリからだけ）。

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tune_core::db::Store;
use tune_core::engine::{Engine, NoHost};
use tune_core::{collect, logs, nodes, rules};

fn usage() -> ExitCode {
    eprintln!("使い方: tune <paths|probe|logs|last|status|analyze> [...]（詳しくは crates/tune-cli/src/main.rs の先頭）");
    ExitCode::from(2)
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string(v).unwrap_or_default());
}

fn targets(ids: &[String]) -> Result<Vec<nodes::Node>, String> {
    let cfg = nodes::load_config(&nodes::user_config_path())?;
    Ok(cfg.nodes.into_iter().filter(|n| ids.is_empty() || ids.contains(&n.id)).collect())
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { return usage() };
    let rest = &args[1..];
    let res: Result<(), String> = match cmd.as_str() {
        "paths" => {
            print(
                &json!({ "config": nodes::user_config_path(), "data_dir": nodes::data_dir(), "db": nodes::data_dir().join(tune_core::db::DB_FILE), "local_host": nodes::local_host() }),
            );
            Ok(())
        }
        "probe" => match targets(rest) {
            Ok(t) => {
                let rs = collect::probe_all(&t, |r| {
                    let id = r["node_id"].as_str().unwrap_or("?");
                    let wall = r["wall_s"].as_f64().map_or("-".into(), |w| format!("{w:.1}s"));
                    let state =
                        if r["ok"] == true { "ok".to_string() } else { format!("NG {}", r["error"].as_str().unwrap_or("").lines().last().unwrap_or("")) };
                    eprintln!("{id}: {state} {wall}");
                })
                .await;
                print(&Value::Array(rs));
                Ok(())
            }
            Err(e) => Err(e),
        },
        "logs" => {
            let mut ids = rest.to_vec();
            let dir = match ids.iter().position(|a| a == "--db") {
                Some(i) if i + 1 < ids.len() => {
                    let d = ids.remove(i + 1);
                    ids.remove(i);
                    std::path::PathBuf::from(d)
                }
                _ => std::env::temp_dir().join("katala-tune-cli"),
            };
            match (targets(&ids), Store::open(&dir)) {
                (Ok(t), Ok(db)) => {
                    let t0 = std::time::Instant::now();
                    let db = Arc::new(Mutex::new(db));
                    logs::sync_all(db, &t, |r| {
                        let id = r["node_id"].as_str().unwrap_or("?");
                        let line = match r.get("error") {
                            Some(e) => format!("NG {}", e.as_str().unwrap_or("").lines().last().unwrap_or("")),
                            None => r["sources"]
                                .as_object()
                                .map(|m| {
                                    m.iter()
                                        .map(|(k, v)| match v.get("error") {
                                            Some(e) => format!("{k}:NG {}", e.as_str().unwrap_or("").chars().take(60).collect::<String>()),
                                            None => format!(
                                                "{k}:+{}/{}{}",
                                                v["inserted"],
                                                v["fetched"],
                                                if v["dropped"].as_i64().unwrap_or(0) > 0 { format!(" drop{}", v["dropped"]) } else { String::new() }
                                            ),
                                        })
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                })
                                .unwrap_or_default(),
                        };
                        eprintln!("{id}: {line}");
                    })
                    .await;
                    eprintln!("done {:.1}s db={}", t0.elapsed().as_secs_f64(), dir.display());
                    Ok(())
                }
                (Err(e), _) => Err(e),
                (_, Err(e)) => Err(e.to_string()),
            }
        }
        "last" | "status" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => {
                if cmd == "last" {
                    e.last().map(|v| print(&Value::Array(v)))
                } else if rest.iter().any(|a| a == "--recompute") {
                    e.status().map(|v| print(&v))
                } else {
                    e.with_db(|d| Ok(json!({ "checks": d.checks()?, "events": d.check_events(150)?, "counts": d.status_counts()?, "lastProbeAt": d.get_meta("lastProbeAt")?, "lastLogsAt": d.get_meta("lastLogsAt")? })))
                        .map(|mut v| {
                            v["schedule"] = e.schedule();
                            print(&v)
                        })
                }
            }
            Err(e) => Err(e),
        },
        "analyze" => match rest.first().map(std::fs::read_to_string) {
            Some(Ok(text)) => match serde_json::from_str::<Value>(&text) {
                Ok(snap) => {
                    let f = rules::analyze(&snap, &json!({}));
                    print(&json!({ "score": rules::score(&f), "findings": f }));
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            },
            Some(Err(e)) => Err(e.to_string()),
            None => return usage(),
        },
        _ => return usage(),
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
