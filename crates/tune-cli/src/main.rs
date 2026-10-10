//! 検証用 CLI。Tauri なしで tune-core を動かす。
//!
//!   tune paths                       台帳と DB の場所
//!   tune probe [機体 ...]            調査して JSON を標準出力へ（保存しない。npm run probe と同じ）
//!   tune logs [--db <dir>] [機体 ...] ログを取り込む（既定は一時ディレクトリの DB。npm run logs と同じ）
//!   tune last                        DB の前回の分析結果（所見・スコアつき）を JSON で
//!   tune power                       電力・Clockと計算条件を JSON で（読み取り専用）
//!   tune status [--recompute]        DB の状態（機能チェック）を JSON で。--recompute で計算し直して保存する
//!   tune analyze <snapshot.json>     snapshot（probe の data）から所見を作る
//!   tune inventory [機体 ...]        道具の棚卸しを DB に保存し、結果を JSON で（DB は KATALA_TUNE_DATA_DIR で写しを指す）
//!   tune ai [機体 ...]               AI エージェントのセッションを取り込み、結果を JSON で（同上。続きは DB が覚える）
//!   tune ai-summary [日数 [機体]]    DB の AI エージェントのセッションの集計を JSON で（重複を除いた量・費用の推定つき）
//!   tune ai-provenance               出どころの台帳の様子（連鎖の検算・区間の状態・調査スクリプトと単価表の版）
//!   tune ai-trace 機体 <ファイルの鍵 | 日の epoch ms>   数字の出どころ（1 ファイルの区間と検算か、その日の内訳）
//!   tune ai-verify 機体 ファイルの鍵  区間を元ファイルと照合する（機体で読み直して指紋だけを受け取る。読み取り専用）
//!   tune ai-limits [機体 ...]        Codex の残り枠だけを問い合わせる（codex app-server。間隔の下限を見ない。共用機は呼ばない）
//!   tune netsec [--probe] [機体 ...]  ネットワークとセキュリティ（「セキュリティ」の画面と同じ JSON）。--probe で先に分析して保存する
//!                                    （DB は KATALA_TUNE_DATA_DIR で写しを指す）
//!   tune live [--seconds N] 機体 ...  ライブ表示のサンプラーを N 秒（既定 30、最大 300）流し、件数・間隔・遅延・
//!                                    サンプラー自身の負荷を JSON で（読み取り専用。機体は必ず指定する）
//!
//!   tune agent-pair <host[:port]>    tune-agent とペアリング（コードは標準入力から。引数には書かない）
//!   tune agent-peers                 ペア済みの tune-agent（公開鍵の指紋だけ）
//!   tune agent-probe <名前|指紋|addr> ペア済みの tune-agent で調査を 1 回（SSH を使わない。src/agent.rs）
//!   tune agent-unpair <名前|指紋>    ペア済みの tune-agent を台帳から消す
//!
//! 台帳と DB は Electron 版と同じ場所（KATALA_TUNE_CONFIG・KATALA_TUNE_DATA_DIR で差し替えられる）。
//! 変更操作（actions）はここからは実行しない（確認ダイアログを通すため、アプリからだけ）。

mod agent;

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tune_core::db::Store;
use tune_core::engine::{Engine, NoHost};
use tune_core::{collect, logs, nodes, rules};

fn usage() -> ExitCode {
    eprintln!(
        "使い方: tune <paths|probe|logs|last|power|status|analyze|inventory|ai|ai-summary|ai-provenance|ai-trace|ai-verify|ai-limits|netsec|live> [...]（詳しくは crates/tune-cli/src/main.rs の先頭）"
    );
    ExitCode::from(2)
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string(v).unwrap_or_default());
}

fn targets(ids: &[String]) -> Result<Vec<nodes::Node>, String> {
    let cfg = nodes::load_config(&nodes::user_config_path())?;
    Ok(cfg.nodes.into_iter().filter(|n| ids.is_empty() || ids.contains(&n.id)).collect())
}

/// 中央値・95 パーセンタイル・最大（ミリ秒）
fn spread(mut xs: Vec<f64>) -> Value {
    if xs.is_empty() {
        return Value::Null;
    }
    xs.sort_by(f64::total_cmp);
    let at = |q: f64| xs[((xs.len() - 1) as f64 * q).round() as usize];
    json!({ "median": at(0.5).round(), "p95": at(0.95).round(), "max": at(1.0).round() })
}

/// ライブ表示のサンプラーを流して、届き方を測る（画面なし）
async fn live(rest: &[String]) -> Result<(), String> {
    use tune_core::live::{Live, Opts, ProcessRoute};
    let mut ids = rest.to_vec();
    let mut secs = 30u64;
    if let Some(i) = ids.iter().position(|a| a == "--seconds") {
        let v = ids.get(i + 1).ok_or("--seconds の値が無い")?.parse::<u64>().map_err(|e| e.to_string())?;
        secs = v.clamp(1, 300);
        ids.drain(i..i + 2);
    }
    if ids.is_empty() {
        return Err("流す機体を指定してください（全機体には流さない）".into());
    }
    let t = targets(&ids)?;
    if t.is_empty() {
        return Err("台帳に無い機体".into());
    }
    let live = Live::new(
        Arc::new(ProcessRoute),
        Opts::default(),
        Box::new(|ev: Value| {
            for (id, n) in ev["nodes"].as_object().into_iter().flatten() {
                let pts = n["points"].as_array().map_or(0, Vec::len);
                let detail = n["detail"].as_str().map(|d| format!(" {d}")).unwrap_or_default();
                eprintln!("{id}: {} +{pts}{}{detail}", n["state"].as_str().unwrap_or("?"), if n.get("procs").is_some() { " procs" } else { "" });
            }
        }),
    );
    tokio::spawn(live.clone().run_ticker());
    let started_at = tune_core::db::now_ms();
    let r = live.start(&t, &[]);
    eprintln!("start: {}", r["started"]);
    let t0 = std::time::Instant::now();
    let mut last_hb = 0;
    while t0.elapsed().as_secs() < secs {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        // 画面と同じく 30 秒ごとに合図を送る
        if t0.elapsed().as_secs() / 30 > last_hb {
            last_hb = t0.elapsed().as_secs() / 30;
            live.start(&t, &[]);
        }
    }
    let status = live.status();
    let mut out = serde_json::Map::new();
    for n in &t {
        let ss = live.samples(&n.id);
        let view = live.view(&n.id).unwrap_or(Value::Null);
        let gaps: Vec<f64> = ss.windows(2).map(|w| (w[1].t - w[0].t) as f64).collect();
        let lags: Vec<f64> = ss.iter().filter_map(|s| s.src_t.map(|x| (s.t - x) as f64)).collect();
        out.insert(
            n.id.clone(),
            json!({
                "samples": ss.len(),
                "first_sample_ms": ss.first().map(|s| s.t - started_at),
                "interval_ms": spread(gaps),
                "lag_ms": spread(lags),
                "sampler_cpu_pct_of_one_core": status[&n.id]["load"],
                "sampler_rss_mb": view["rss_mb"],
                "state": status[&n.id]["state"],
                "detail": status[&n.id]["detail"],
                "info": view["info"],
                "last": ss.last().map(|s| json!({ "cpu": s.cpu, "cores": s.cores.len(), "mem": s.mem, "disk": s.disk, "net": s.net, "gpu": s.gpu, "power":s.power, "power_gpu_w":s.point()["power_gpu_w"] })),
                "procs": view["procs"].get("count"),
            }),
        );
    }
    let t1 = std::time::Instant::now();
    live.shutdown(std::time::Duration::from_secs(10)).await;
    eprintln!("stopped in {} ms", t1.elapsed().as_millis());
    print(&Value::Object(out));
    Ok(())
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
        "power" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => e.power_report().map(|v| print(&v)),
            Err(e) => Err(e),
        },
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
        "inventory" | "ai" | "ai-summary" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => {
                let ids = (!rest.is_empty() && cmd != "ai-summary").then(|| rest.to_vec());
                let t0 = std::time::Instant::now();
                let v = match cmd.as_str() {
                    "inventory" => Ok(e.run_inventory(ids).await),
                    "ai" => Ok(e.ai_sync(ids).await),
                    _ => {
                        let days = rest.first().and_then(|d| d.parse::<i64>().ok()).unwrap_or(30);
                        e.ai_summary(&json!({ "days": days, "node_id": rest.get(1) }))
                    }
                };
                eprintln!("done {:.1}s db={}", t0.elapsed().as_secs_f64(), e.data_dir().display());
                v.map(|v| print(&v))
            }
            Err(e) => Err(e),
        },
        "ai-provenance" | "ai-trace" | "ai-verify" | "ai-limits" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => {
                let t0 = std::time::Instant::now();
                let target = |r: &[String]| -> Value {
                    let mut f = json!({ "node_id": r.first().cloned().unwrap_or_default() });
                    match r.get(1) {
                        Some(x) if x.parse::<i64>().is_ok() => f["day"] = json!(x.parse::<i64>().unwrap_or(0)),
                        Some(x) => f["file"] = json!(x),
                        None => {}
                    }
                    f
                };
                let v = match cmd.as_str() {
                    "ai-provenance" => e.ai_provenance(),
                    "ai-trace" => e.ai_trace(&target(rest)),
                    "ai-verify" => e.ai_verify(&target(rest)).await,
                    _ => Ok(e.ai_limits_sync((!rest.is_empty()).then(|| rest.to_vec())).await),
                };
                eprintln!("done {:.1}s db={}", t0.elapsed().as_secs_f64(), e.data_dir().display());
                v.map(|v| print(&v))
            }
            Err(e) => Err(e),
        },
        "netsec" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => {
                let probe = rest.iter().any(|a| a == "--probe");
                let ids: Vec<String> = rest.iter().filter(|a| *a != "--probe").cloned().collect();
                if probe {
                    let t0 = std::time::Instant::now();
                    e.run_probe(Some(ids), false).await;
                    eprintln!("probe {:.1}s db={}", t0.elapsed().as_secs_f64(), e.data_dir().display());
                } else {
                    e.compute_checks(false, false);
                }
                e.netsec_view(&json!({})).map(|v| print(&v))
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
        "live" => live(rest).await,
        c if c.starts_with("agent-") => agent::run(c, rest).await,
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
