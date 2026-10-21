//! 検証用 CLI。Tauri なしで tune-core を動かす。
//!
//!   tune paths                       台帳と DB の場所
//!   tune probe [機体 ...]            調査して JSON を標準出力へ（保存しない。npm run probe と同じ）
//!   tune logs [--db <dir>] [機体 ...] ログを取り込む（既定は一時ディレクトリの DB。npm run logs と同じ）
//!   tune ledger [--json]             対象台帳（対象・除外・未観測とその理由）。台帳の status（online / retired / no-ssh / paused / out-of-scope）を見る。
//!                                    読み取りだけで、台帳も DB も書き換えず、機体へ接続もしない
//!   tune last                        DB の前回の分析結果（所見・スコアつき）を JSON で
//!   tune power                       電力・Clockと計算条件を JSON で（読み取り専用）
//!   tune capabilities [機体 ...] firmware 読取（省略はこの機体、保存なし）
//!   tune maintenance-plan [機体 ...] 読取更新と実行不可の保守計画
//!   tune network [--probe] [機体 ...] 軽量な接続診断（省略はこの機体。--probeだけHTTPS到達性・時間も測る）
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
//!   tune hygiene [--ghq-root D] [--root D]... [--max-repos N] [--budget 秒] [--repos] [機体 ...]
//!                                    リポジトリの衛生監査（dirty・未 push・stash・ghq のパスと origin の不一致。opt-in・読み取り専用・保存しない。
//!                                    --repos でリポジトリごとの行も出す。台帳の repo_hygiene: false の機体と共用機は対象外）
//!   tune export [--since ms]         常時監視の集計と最新の分析結果を写しの形で（ハブとして写される側。読み取り専用）
//!   tune hub-pull                    ハブから 1 回写す（台帳の機体へ SSH して tune export を呼ぶ）
//!   tune monitor ['{"enabled":true,"hub":"id"}']  常時監視とハブの設定を見る・変える
//!   tune schedule ['{"logs_minutes":60}']          自動スキャンの間隔を見る・変える（画面と同じ範囲だけ受け付ける）
//!   tune metrics [機体] [--minutes N] 分ごとの集計（既定は直近 60 分）
//!   tune dashboard [--minutes N]     ダッシュボードと同じ JSON（ライブの段階は入らない）
//!   tune update ['{"auto":false}']  自動更新の設定と状態（版・最後に確かめた時刻・用意した版・同梱の道具）を見る・変える
//!   tune monitor-run [--seconds N]   常時監視を N 秒（既定 180、最大 3600）だけ画面なしで動かし、ダッシュボードの JSON を出す（確認用）
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
mod topology_native;
mod topology_reader;

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tune_core::db::Store;
use tune_core::engine::{Engine, NoHost};
use tune_core::{
    collect, logs, nodes, rules,
    topology::export::{ExportView, public_json, public_mermaid},
};

fn usage() -> ExitCode {
    eprintln!(
        "使い方: tune <paths|probe|hygiene|logs|ledger|last|power|network|capabilities|maintenance-plan|status|analyze|inventory|ai|ai-summary|ai-provenance|ai-trace|ai-verify|ai-limits|netsec|live|export|hub-pull|monitor|schedule|metrics|dashboard|monitor-run|update|topology-refresh|topology-watch|topology-export> [...]（詳しくは crates/tune-cli/src/main.rs の先頭）"
    );
    ExitCode::from(2)
}

/// `--name 数` の数（無い・読めなければ None）
fn flag(rest: &[String], name: &str) -> Option<i64> {
    rest.iter().position(|a| a == name).and_then(|i| rest.get(i + 1)).and_then(|v| v.parse().ok())
}

fn topology_node_ids(rest: &[String], watch: bool) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    let mut index = 0;
    while index < rest.len() {
        if watch && matches!(rest[index].as_str(), "--iterations" | "--interval-seconds") {
            if rest.get(index + 1).is_none() {
                return Err(format!("{} requires a value", rest[index]));
            }
            index += 2;
            continue;
        }
        if rest[index].starts_with('-') {
            return Err(format!("unknown topology option: {}", rest[index]));
        }
        ids.push(rest[index].clone());
        index += 1;
    }
    let config = nodes::load_config(&nodes::user_config_path())?;
    if let Some(id) = ids.iter().find(|id| config.node(id).is_none()) {
        return Err(format!("unknown topology node id: {id}"));
    }
    Ok(ids)
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string(v).unwrap_or_default());
}

/// 運用対象（online）の機体だけを返す。除外（retired など）には接続せず、理由を標準エラーに出す
fn targets(ids: &[String]) -> Result<Vec<nodes::Node>, String> {
    let cfg = nodes::load_config(&nodes::user_config_path())?;
    let selected: Vec<nodes::Node> = cfg.nodes.into_iter().filter(|n| ids.is_empty() || ids.contains(&n.id)).collect();
    let (targets, excluded) = nodes::partition(&selected);
    for x in &excluded {
        eprintln!(
            "{}: 対象外（{}）{}",
            x["node_id"].as_str().unwrap_or("?"),
            x["state"].as_str().unwrap_or("?"),
            x["reason"].as_str().filter(|r| !r.is_empty()).map(|r| format!(" {r}")).unwrap_or_default()
        );
    }
    Ok(targets)
}

/// 台帳と既存の観測だけ読む。初回閲覧でも見本・DB を作らない
fn ledger() -> Result<Value, String> {
    let cfg = nodes::load_config(&nodes::user_config_path())?;
    let dir = nodes::data_dir();
    let exists = dir.join(tune_core::db::DB_FILE).try_exists().map_err(|e| e.to_string())?;
    let db = if exists { Some(Store::open_read_only(&dir).map_err(|e| e.to_string())?) } else { None };
    let mut observed = std::collections::HashMap::new();
    let mut errors = std::collections::HashMap::new();
    for n in &cfg.nodes {
        let at = match &db {
            Some(db) => db.last_snapshots(&n.id, 1).map_err(|e| e.to_string())?.first().map(|s| s.at),
            None => None,
        };
        if let Some(db) = &db {
            errors.insert(n.id.clone(), db.probe_error(&n.id, at).map_err(|e| e.to_string())?);
        }
        observed.insert(n.id.clone(), at);
    }
    Ok(nodes::target_ledger(&cfg.nodes, |id| nodes::Observation {
        last_ok_at: observed.get(id).copied().flatten(),
        last_error: errors.get(id).cloned().flatten(),
    }))
}

/// 対象台帳を人が読む表にする
fn ledger_table(v: &Value) -> String {
    let mut out = String::new();
    for r in v["rows"].as_array().into_iter().flatten() {
        let status = match r["status"].as_str() {
            Some("target") => "対象",
            Some("unobserved") => "未観測",
            _ => "除外",
        };
        let reason = r["reason"].as_str().unwrap_or("");
        let date = r["date"].as_str().map(|d| format!(" [{d}]")).unwrap_or_default();
        out.push_str(&format!(
            "{}\t{}\t{}\t{}{}{}\n",
            r["node_id"].as_str().unwrap_or("?"),
            status,
            r["state"].as_str().unwrap_or("?"),
            if reason.is_empty() { "-" } else { "" },
            reason,
            date
        ));
    }
    let c = &v["counts"];
    out.push_str(&format!("対象 {} / 未観測 {} / 除外 {}（全 {} 台）\n", c["target"], c["unobserved"], c["excluded"], c["total"]));
    out
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

/// リポジトリの衛生監査（scripts/repo-hygiene.js と同じ）。保存しない
async fn hygiene(rest: &[String]) -> Result<(), String> {
    use tune_core::hygiene;
    let mut opts = json!({ "roots": [] });
    let (mut ids, mut with_repos) = (Vec::new(), false);
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} の値が無い"));
        match a.as_str() {
            "--ghq-root" => opts["ghq_root"] = json!(val(a)?),
            "--root" => {
                let v = val(a)?;
                if let Some(r) = opts["roots"].as_array_mut() {
                    r.push(json!(v));
                }
            }
            "--max-repos" => opts["max_repos"] = json!(val(a)?.parse::<i64>().map_err(|e| e.to_string())?),
            "--budget" => opts["budget_s"] = json!(val(a)?.parse::<i64>().map_err(|e| e.to_string())?),
            "--repos" => with_repos = true,
            x if x.starts_with("--") => return Err(format!("知らない引数: {x}")),
            x => ids.push(x.to_string()),
        }
    }
    let want = targets(&ids)?;
    let (run, skipped): (Vec<_>, Vec<_>) = want.into_iter().partition(hygiene::enabled);
    let results = hygiene::audit_all(&run, &opts).await;
    let mut nodes_out = Vec::new();
    for r in &results {
        let id = r["node_id"].as_str().unwrap_or("?");
        if r["ok"] != true {
            eprintln!("{id}: NG {}", r["error"].as_str().unwrap_or(""));
            nodes_out.push(json!({ "node_id": id, "ok": false, "error": r["error"] }));
            continue;
        }
        let findings = hygiene::analyze(r);
        let repos = r["repos"].as_array().map_or(&[][..], Vec::as_slice);
        let observed = repos.iter().filter(|x| x["state"] == "ok").count();
        let unobserved = repos.len() - observed + r["skipped"].as_u64().unwrap_or(0) as usize;
        eprintln!("{id}: 見つけた {} 件・未観測 {unobserved} 件・{:.1}s・所見 {}", r["enumerated"], r["wall_s"].as_f64().unwrap_or(0.0), findings.len());
        let mut o = r.clone();
        if !with_repos {
            o.as_object_mut().map(|m| m.remove("repos"));
        }
        o["observed"] = json!(observed);
        o["findings"] = json!(findings);
        nodes_out.push(o);
    }
    let ok: Vec<Value> = results.into_iter().filter(|r| r["ok"] == true).collect();
    print(&json!({ "nodes": nodes_out, "skipped_nodes": skipped.iter().map(|n| &n.id).collect::<Vec<_>>(), "matrix": hygiene::matrix(&ok) }));
    Ok(())
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

fn main() -> ExitCode {
    let started = std::time::Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|cmd| cmd == "topology-reader") {
        return topology_reader::main(&args[1..], started);
    }
    if let Some(entry) = args.first().filter(|entry| matches!(entry.as_str(), "topology-native-reader" | "topology-supervise" | "topology-verify")) {
        return topology_native::main(entry, &args[1..], started);
    }
    match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime.block_on(async_main(args)),
        Err(_) => ExitCode::from(1),
    }
}

async fn async_main(args: Vec<String>) -> ExitCode {
    let Some(cmd) = args.first() else { return usage() };
    let rest = &args[1..];
    let res: Result<(), String> = match cmd.as_str() {
        "topology-refresh" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => match topology_node_ids(rest, false) {
                Ok(ids) => {
                    print(&e.topology_refresh_nodes("cli", &ids).await);
                    Ok(())
                }
                Err(error) => Err(error),
            },
            Err(e) => Err(e),
        },
        "topology-watch" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => match topology_node_ids(rest, true) {
                Ok(ids) => {
                    let iterations = flag(rest, "--iterations").unwrap_or(1).clamp(1, 100) as usize;
                    let seconds = flag(rest, "--interval-seconds")
                        .unwrap_or_else(|| e.schedule().get("topology_minutes").and_then(Value::as_i64).unwrap_or(1).saturating_mul(60))
                        .clamp(1, 86_400) as u64;
                    for n in 0..iterations {
                        print(&e.topology_refresh_nodes("watch", &ids).await);
                        if n + 1 < iterations {
                            tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                        }
                    }
                    Ok(())
                }
                Err(error) => Err(error),
            },
            Err(e) => Err(e),
        },
        "topology-export" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => {
                let view = match e.topology_accepted_view() {
                    Ok(view) => view,
                    Err(error) => {
                        eprintln!("{error}");
                        return ExitCode::from(1);
                    }
                };
                let export_view = match rest.windows(2).find(|pair| pair[0] == "--view").map(|pair| pair[1].as_str()).unwrap_or("all") {
                    "all" => ExportView::All,
                    "hosts" => ExportView::Hosts,
                    "dependencies" => ExportView::Dependencies,
                    "cicd" => ExportView::Cicd,
                    _ => return usage(),
                };
                match rest.windows(2).find(|pair| pair[0] == "--format").map(|pair| pair[1].as_str()).unwrap_or("json") {
                    "json" => println!("{}", public_json(&view, export_view)),
                    "mermaid" => println!("{}", public_mermaid(&view, export_view)),
                    _ => return usage(),
                }
                Ok(())
            }
            Err(e) => Err(e),
        },
        "capabilities" | "maintenance-plan" => {
            if rest.iter().any(|id| id.starts_with('-')) {
                Err("未対応のオプション".into())
            } else {
                match nodes::load_config(&nodes::user_config_path()).and_then(|cfg| tune_core::capabilities::select(&cfg.nodes, rest)) {
                    Ok(selected) => tune_core::capabilities::check(&selected).await.map(|evidence| {
                        let output = if cmd == "maintenance-plan" { tune_core::capabilities::maintenance_plan(&evidence) } else { evidence };
                        print(&output);
                    }),
                    Err(error) => Err(error),
                }
            }
        }
        "network" => {
            let active = rest.iter().any(|a| a == "--probe");
            let ids: Vec<_> = rest.iter().filter(|a| a.as_str() != "--probe").cloned().collect();
            match nodes::load_config(&nodes::user_config_path()).and_then(|cfg| tune_core::network::select(&cfg.nodes, &ids)) {
                Ok(t) => tune_core::network::check(&t, active).await.map(|v| print(&v)),
                Err(e) => Err(e),
            }
        }
        "paths" => {
            print(
                &json!({ "config": nodes::user_config_path(), "data_dir": nodes::data_dir(), "db": nodes::data_dir().join(tune_core::db::DB_FILE), "local_host": nodes::local_host() }),
            );
            Ok(())
        }
        "hygiene" => hygiene(rest).await,
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
        "ledger" => ledger().map(|v| if rest.iter().any(|a| a == "--json") { print(&v) } else { print!("{}", ledger_table(&v)) }),
        "last" | "status" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => {
                if cmd == "last" {
                    e.last().map(|v| print(&Value::Array(v)))
                } else if rest.iter().any(|a| a == "--recompute") {
                    e.status().map(|v| print(&v))
                } else {
                    e.status_snapshot().map(|v| print(&v))
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
        "export" => {
            // 1 分ごとに写されるので、台帳・ログの移行などは動かさず DB だけ開く
            let since = flag(rest, "--since").unwrap_or(0);
            tune_core::db::Store::open(&nodes::data_dir())
                .map_err(|e| e.to_string())
                .and_then(|d| tune_core::monitor::export(&d, since, &nodes::local_host()))
                .map(|v| print(&v))
        }
        "update" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => {
                let r = match rest.first().filter(|a| a.starts_with('{')).map(|a| serde_json::from_str::<Value>(a)) {
                    Some(Ok(p)) => match p.get("auto").and_then(Value::as_bool) {
                        Some(on) => e.with_db(|d| d.set_meta("update", &json!({ "auto": on }))),
                        None => Err("{\"auto\": true|false} だけ受け付ける".into()),
                    },
                    Some(Err(x)) => Err(format!("JSON を読めない: {x}")),
                    None => Ok(()),
                };
                r.and_then(|()| {
                    e.with_db(|d| {
                        let auto = d.get_meta("update")?.and_then(|u| u.get("auto").and_then(Value::as_bool)).unwrap_or(true);
                        Ok(json!({ "version": tune_core::helpers::VERSION, "auto": auto, "state": d.get_meta("updateState")?, "helpers": d.get_meta("helpers")? }))
                    })
                })
                .map(|v| print(&v))
            }
            Err(e) => Err(e),
        },
        "hub-pull" | "monitor" | "schedule" | "metrics" | "dashboard" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => {
                let patch = || {
                    rest.first()
                        .filter(|a| a.starts_with('{'))
                        .map(|a| serde_json::from_str::<Value>(a).map_err(|x| format!("JSON を読めない: {x}")))
                        .transpose()
                };
                let minutes = flag(rest, "--minutes").unwrap_or(60);
                match cmd.as_str() {
                    "hub-pull" => tune_core::monitor::pull(&e).await,
                    "monitor" => patch().and_then(|p| {
                        let cfg = e.reload_config();
                        let s = match p {
                            Some(p) => e.with_db(|d| Ok(tune_core::monitor::set_settings(d, &cfg, &p)))??,
                            None => e.with_db(|d| Ok(tune_core::monitor::settings(d)))?,
                        };
                        let pull = e.with_db(|d| d.get_meta("hubPull"))?;
                        Ok(json!({ "monitor": s.json(), "hub_pull": pull }))
                    }),
                    "schedule" => patch().and_then(|p| match p {
                        Some(p) => e.set_schedule(&p),
                        None => Ok(e.schedule()),
                    }),
                    "metrics" => {
                        let node = rest.first().filter(|a| !a.starts_with("--")).cloned();
                        let now = tune_core::db::now_ms();
                        e.with_db(|d| d.metrics(node.as_deref(), now - minutes * 60_000, i64::MAX))
                            .map(|rows| Value::Array(rows.iter().map(tune_core::db::metrics::MinuteRow::json).collect()))
                    }
                    _ => tune_core::monitor::dashboard(&e, None, minutes),
                }
                .map(|v| print(&v))
            }
            Err(e) => Err(e),
        },
        "monitor-run" => match Engine::open(nodes::user_config_path(), nodes::data_dir(), Arc::new(NoHost)) {
            Ok(e) => {
                use tune_core::live::{Live, ProcessRoute};
                let secs = flag(rest, "--seconds").unwrap_or(180).clamp(15, 3600) as u64;
                let live = Live::new(Arc::new(ProcessRoute), tune_core::monitor::live_opts(e.config().nodes.len()), Box::new(|_| {}));
                tokio::spawn(live.clone().run_ticker());
                let until = std::time::Instant::now() + std::time::Duration::from_secs(secs);
                // 同梱のサンプラー（CLI の隣。Windows だけ）があれば、古い機体へ入れ替えを送る（アプリと同じ）
                let agent = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("tune-agent.exe")))
                    .filter(|_| cfg!(windows))
                    .and_then(|p| std::fs::read(p).ok())
                    .map(Arc::new);
                tune_core::monitor::run(e.clone(), live.clone(), |ev| eprintln!("monitor: {ev}"), Some(until), agent).await;
                let v = tune_core::monitor::dashboard(&e, Some(&live), 60);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                live.shutdown(deadline.saturating_duration_since(std::time::Instant::now())).await;
                if !e.finish_monitor_journal(deadline.saturating_duration_since(std::time::Instant::now())).await {
                    Err("収集記録の終了時保存を確認できません".into())
                } else {
                    v.map(|mut v| {
                        v["journal"] = e.monitor_journal_health();
                        print(&v)
                    })
                }
            }
            Err(e) => Err(e),
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
