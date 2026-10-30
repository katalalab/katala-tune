// 台帳の運用状態（status）の受入試験（偽の実行口と架空の台帳 test/fixtures/ledger だけ。実機の ssh は呼ばない）。
// Electron 版の同じ条件は test/ledger.test.js。
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tune_core::collect::{RunFuture, RunResult, Runner, exec_on_with, probe_all_with, probe_node_with};
use tune_core::db::Store;
use tune_core::engine::{Engine, NoHost};
use tune_core::live::{Live, Opts, ProcessRoute};
use tune_core::logs::{DbAccess, sync_all, sync_node};
use tune_core::nodes::{self, Node, Observation, parse_config, partition, target_ledger};
use tune_core::{actions, health, inventory, network};

const FIXTURE: &str = include_str!("../../../test/fixtures/ledger/nodes.json");
const EXCLUDED: [&str; 6] = ["old-tower", "locked-down", "on-hold", "someone-else", "typo-node", "retired-local"];

struct HeldRoute;
impl tune_core::live::Route for HeldRoute {
    fn open(&self, _node: &Node, _opts: &Opts) -> Result<tune_core::live::Source, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (done, ended) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = tx.send(r#"{"type":"hello","v":1,"os":"fake"}"#.into()).await;
            let _ = tx.send(r#"{"type":"s","t":1,"cpu":5,"cores":[5],"mem":{"used_pct":50}}"#.into()).await;
            let _ = stopped.await;
            drop(tx);
            let _ = done.send(Default::default());
        });
        Ok(tune_core::live::Source::new(rx, stop, ended))
    }
}

async fn running(live: &Live, id: &str) {
    for _ in 0..100 {
        if live.status()[id]["state"] == "running" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("sampler did not start: {id}");
}

#[tokio::test]
async fn excluded_heartbeat_stops_an_existing_stream() {
    let live = Live::new(Arc::new(HeldRoute), Opts::default(), Box::new(|_| {}));
    let mut n = node("active-mac");
    assert_eq!(live.start(&[n.clone()], &[])["started"], json!([n.id]));
    running(&live, &n.id).await;
    n.raw["status"] = json!("retired");
    assert_eq!(live.start(&[n], &[])["refused"][0]["reason"], "excluded");
    let active = live.active();
    live.shutdown(Duration::from_secs(1)).await;
    assert!(active.is_empty(), "excluded sampler still active: {active:?}");
}

#[tokio::test]
async fn monitor_stops_streams_removed_or_excluded_from_the_current_ledger() {
    let dir = tmp("stream-reload");
    let cfg_path = dir.join("nodes.json");
    std::fs::write(&cfg_path, r#"{"nodes":[{"id":"active-mac","os":"macos"}]}"#).unwrap();
    let e = Engine::open(cfg_path.clone(), dir.join("data"), Arc::new(NoHost)).unwrap();
    let live = Live::new(Arc::new(HeldRoute), Opts::default(), Box::new(|_| {}));
    let original = e.config().nodes;
    for changed in [r#"{"nodes":[{"id":"active-mac","os":"macos","status":"paused"}]}"#, r#"{"nodes":[]}"#] {
        live.start(&original, &[]);
        running(&live, "active-mac").await;
        std::fs::write(&cfg_path, changed).unwrap();
        // Monitoring is disabled: ledger exclusion still has to close UI streams.
        tune_core::monitor::run(e.clone(), live.clone(), |_| {}, Some(std::time::Instant::now()), None).await;
        assert!(live.active().is_empty(), "excluded stream survived reload");
        for _ in 0..100 {
            if live.status()["active-mac"]["state"] == "stopped" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(live.status()["active-mac"]["reason"], "excluded");
    }
    live.shutdown(Duration::from_secs(1)).await;
    // 一時DBを削除する前に、自分が開始したwriterも明示的に終了する。
    assert!(e.finish_monitor_journal(Duration::from_secs(1)).await);
    let journal_running = e.monitor_journal_health()["running"].as_bool();
    drop(e);
    std::fs::remove_dir_all(dir).unwrap();
    assert_eq!(journal_running, Some(false), "live shutdown alone must not be mistaken for journal shutdown");
}

fn config() -> nodes::Config {
    // retired-local はこの機体（local）として読む
    parse_config(FIXTURE, Path::new("nodes.json"), "fixture-local-host").unwrap()
}

fn node(id: &str) -> Node {
    config().node(id).cloned().unwrap()
}

/// 呼ばれた回数と宛先を記録する偽の実行口（ssh もローカル実行もここを通る）
type ProbeCall = (String, String, bool, bool, bool);

struct Mock {
    calls: Mutex<Vec<(String, Vec<String>)>>,
    probe_calls: Mutex<Vec<ProbeCall>>,
    reply: RunResult,
}

impl Mock {
    fn new(reply: RunResult) -> Arc<Mock> {
        Arc::new(Mock { calls: Mutex::new(Vec::new()), probe_calls: Mutex::new(Vec::new()), reply })
    }
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    fn probe_count(&self) -> usize {
        self.probe_calls.lock().unwrap().len()
    }
}

impl Runner for Mock {
    fn run<'a>(&'a self, cmd: &'a str, args: &'a [String], _input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((cmd.to_string(), args.to_vec()));
            self.reply.clone()
        })
    }

    fn probe_exchange<'a>(&'a self, node: &'a Node, helper_sha256: &'a str, benchmark: bool, network: bool, observe_wsl: bool) -> RunFuture<'a> {
        Box::pin(async move {
            self.probe_calls.lock().unwrap().push((node.alias.clone(), helper_sha256.to_string(), benchmark, network, observe_wsl));
            self.reply.clone()
        })
    }
}

fn ok_reply() -> RunResult {
    RunResult { code: Some(0), out: "{\"probe\":\"windows\",\"cpu_busy\":1}\n".into(), err: String::new() }
}

fn tmp(tag: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("kt-ledger-{tag}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn ledger_without_status_still_loads_and_every_node_is_online() {
    // status を持たない従来の台帳（見本から status 関連の項目を除いたもの）
    let mut legacy: Value = serde_json::from_str(nodes::EXAMPLE).unwrap();
    for n in legacy["nodes"].as_array_mut().unwrap() {
        let o = n.as_object_mut().unwrap();
        o.remove("status");
        o.remove("status_reason");
        o.remove("status_date");
    }
    let c = parse_config(&legacy.to_string(), Path::new("nodes.json"), "h").unwrap();
    assert!(c.nodes.len() >= 3);
    for n in &c.nodes {
        assert_eq!(n.op_state().state, "online");
        assert!(n.is_active() && n.refusal().is_none());
        assert_eq!(n.public()["status"], json!("online"));
        assert_eq!(n.public()["status_reason"], Value::Null);
    }
    let (targets, excluded) = partition(&c.nodes);
    assert_eq!((targets.len(), excluded.len()), (c.nodes.len(), 0));
}

#[test]
fn status_values_are_normalised_and_unreadable_values_fail_safe() {
    assert_eq!(node("on-hold").op_state().state, "paused");
    assert_eq!(node("someone-else").op_state().state, "out-of-scope");
    assert_eq!(node("explicit-online").op_state().state, "online");
    let typo = node("typo-node").op_state();
    assert_eq!((typo.state.as_str(), typo.active, typo.valid), ("invalid", false, false));
    assert!(typo.reason.contains("retried"));
    let old = node("old-tower").op_state();
    assert_eq!((old.reason.as_str(), old.date.as_str()), ("2026-09 に解体", "2026-09-30"));
    assert!(!nodes::op_state(&json!({ "status": 3 })).active);
    assert!(nodes::op_state(&json!({ "status": null })).active);
}

#[tokio::test]
async fn probe_runs_nothing_for_nodes_that_are_not_online() {
    let mut all = config().nodes;
    let helper_sha = "b".repeat(64);
    for node in &mut all {
        if node.id == "explicit-online" {
            node.raw["action_helper"] = json!({
                "schema": "katala-tune/action-helper/v2",
                "source_sha": "a".repeat(40),
                "tune_sha256": helper_sha,
                "manifest_sha256": "c".repeat(64),
                "run_id": 1,
                "attempt": 1,
            });
        }
    }
    let mock = Mock::new(ok_reply());
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let s2 = seen.clone();
    let results = probe_all_with(mock.clone(), &all, move |r| s2.lock().unwrap().push(r["node_id"].as_str().unwrap().to_string())).await;
    assert_eq!(results.iter().map(|r| r["node_id"].as_str().unwrap()).collect::<Vec<_>>(), ["active-mac", "explicit-online"]);
    assert_eq!(mock.count(), 1, "only active Mac retains generic SSH");
    let generic = mock.calls.lock().unwrap().clone();
    assert_eq!(generic[0].0, "ssh");
    assert!(generic[0].1.iter().any(|arg| arg == "active-mac"));
    let probes = mock.probe_calls.lock().unwrap().clone();
    assert_eq!(probes.as_slice(), [("explicit-online".into(), helper_sha, true, true, false)]);

    for id in EXCLUDED {
        let mock = Mock::new(ok_reply());
        for n in [node(id), Node { local: true, ..node(id) }] {
            let r = probe_node_with(mock.as_ref(), &n).await;
            assert_eq!((r["ok"].clone(), r["skipped"].clone(), r["reason"].clone()), (json!(false), json!(true), json!("excluded")), "{id}");
            assert!(exec_on_with(mock.as_ref(), &n, "echo x", Duration::from_secs(1)).await.err.contains("接続も変更もしない"));
        }
        assert_eq!(mock.count(), 0, "{id} に接続した");
        assert_eq!(mock.probe_count(), 0, "{id} に named probe を実行した");
    }
}

struct NoDb;
impl DbAccess for NoDb {
    fn with<T>(&self, _f: impl FnOnce(&Store) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
        panic!("対象外の機体で DB を触った");
    }
}

#[tokio::test]
async fn logs_inventory_actions_network_and_live_refuse_nodes_that_are_not_online() {
    let excluded: Vec<Node> = EXCLUDED.iter().map(|i| node(i)).collect();
    assert!(sync_all(Arc::new(NoDb), &excluded, |_| panic!("結果が届いた")).await.is_empty());
    let mock = Mock::new(ok_reply());
    for n in &excluded {
        let s = sync_node(&NoDb, n).await;
        assert_eq!((s["skipped"].clone(), s["sources"].clone()), (json!(true), json!({})));
        let inv = inventory::inventory_node(n).await;
        assert_eq!((inv["ok"].clone(), inv["skipped"].clone()), (json!(false), json!(true)));
        let a = json!({ "type": "kill-process", "params": { "pid": 99999, "name": "x" } });
        let e = actions::plan(n, &a, Some(&[])).unwrap_err();
        assert!(e.contains("接続も変更もしない"), "{e}");
        assert!(actions::execute_with(mock.as_ref(), n, &a, Some(&[])).await.is_err());
        assert!(tune_core::ai_sessions::run_python(n, "print(1)", Duration::from_secs(1)).await.err.contains("接続も変更もしない"));
        assert!(!tune_core::ai_sessions::enabled(n) && !tune_core::codex_limits::enabled(n));
        assert!(tune_core::live::launch(n, &Opts::default()).is_err());
    }
    assert_eq!(mock.count(), 0);
    let all = config().nodes;
    assert!(network::select(&all, &["old-tower".to_string()]).unwrap_err().contains("接続も変更もしない"));
    let live = Live::new(Arc::new(ProcessRoute), Opts::default(), Box::new(|_| {}));
    let r = live.start(&excluded, &[]);
    assert_eq!(r["started"], json!([]));
    assert_eq!(r["refused"].as_array().unwrap().len(), excluded.len());
    assert!(r["refused"].as_array().unwrap().iter().all(|x| x["reason"] == json!("excluded")));
}

#[tokio::test]
async fn online_unreachable_stays_online_and_the_ledger_is_never_rewritten() {
    let c = config();
    let before = c.nodes.iter().map(|n| n.raw.clone()).collect::<Vec<_>>();
    let mock =
        Mock::new(RunResult { code: Some(255), out: String::new(), err: "ssh: Could not resolve hostname active-mac: Name or service not known\n".into() });
    let r = probe_node_with(mock.as_ref(), c.node("active-mac").unwrap()).await;
    assert_eq!((r["ok"].clone(), r["reason"].clone()), (json!(false), json!("unreachable")));
    assert!(r.get("skipped").is_none());
    assert_eq!(c.nodes.iter().map(|n| n.raw.clone()).collect::<Vec<_>>(), before);
    assert_eq!(c.node("active-mac").unwrap().op_state().state, "online");
    // 異常（fail）として扱い、退役とは別
    let checks = health::node_checks(&c.node("active-mac").unwrap().raw, None, &[], &json!({ "now": 1, "lastError": r["error"] }));
    assert_eq!(checks.iter().find(|x| x.id == "probe").unwrap().status, "fail");
}

#[test]
fn health_of_excluded_nodes_is_one_neutral_check() {
    let cursors = json!([{ "node_id": "old-tower", "source": "win_system", "last_error": "boom", "fail_streak": 9, "last_ok_at": 1 }]);
    for id in EXCLUDED.iter().filter(|i| **i != "typo-node") {
        let checks = health::node_checks(
            &node(id).raw,
            Some(&json!({ "at": 1, "data": {} })),
            &[],
            &json!({ "now": 9e12, "lastError": "ssh: timeout", "cursors": cursors }),
        );
        assert_eq!(checks.len(), 1);
        assert_eq!((checks[0].id.as_str(), checks[0].status.as_str()), ("ledger", "unknown"));
        assert!(checks[0].detail.as_deref().unwrap().contains("対象外"));
    }
    let old = health::node_checks(&node("old-tower").raw, None, &[], &json!({}));
    let d = old[0].detail.as_deref().unwrap();
    assert!(d.contains("退役") && d.contains("2026-09 に解体") && d.contains("2026-09-30"), "{d}");
    let typo = health::node_checks(&node("typo-node").raw, None, &[], &json!({ "lastError": "x" }));
    assert_eq!((typo.len(), typo[0].status.as_str()), (1, "warn"));
}

#[test]
fn target_ledger_separates_target_unobserved_and_excluded_with_reasons() {
    let c = config();
    let led = target_ledger(&c.nodes, |id| match id {
        "active-mac" => Observation { last_ok_at: Some(1_800_000_000_000), last_error: None },
        "explicit-online" => Observation { last_ok_at: None, last_error: Some("ssh: connect to host explicit-online port 22: Operation timed out\n".into()) },
        _ => Observation { last_ok_at: None, last_error: None },
    });
    let row = |id: &str| led["rows"].as_array().unwrap().iter().find(|r| r["node_id"] == id).cloned().unwrap();
    assert_eq!(row("active-mac")["status"], json!("target"));
    assert_eq!(row("explicit-online")["status"], json!("unobserved"));
    assert!(row("explicit-online")["reason"].as_str().unwrap().contains("Operation timed out"));
    assert_eq!(row("explicit-online")["state"], json!("online"));
    assert_eq!(
        (row("old-tower")["status"].clone(), row("old-tower")["reason"].clone(), row("old-tower")["date"].clone()),
        (json!("excluded"), json!("2026-09 に解体"), json!("2026-09-30"))
    );
    assert_eq!(row("someone-else")["reason"], json!("理由の記載なし"));
    assert_eq!(led["counts"], json!({ "target": 1, "unobserved": 1, "excluded": 6, "total": 8 }));
}

/// 運用中の機体が無い台帳でアプリ本体（Engine）を動かしても、接続せず、台帳ファイルも変わらない（本物の ssh は起動できない構成）
#[tokio::test]
async fn engine_with_only_excluded_nodes_probes_nothing_and_leaves_the_ledger_untouched() {
    let dir = tmp("engine");
    let cfg_path = dir.join("nodes.json");
    let only_excluded = json!({ "nodes": [
        { "id": "gone-1", "alias": "gone-1", "os": "windows", "status": "retired", "status_reason": "解体", "status_date": "2026-09-30" },
        { "id": "gone-2", "alias": "gone-2", "os": "macos", "status": "no-ssh" }
    ] });
    std::fs::write(&cfg_path, serde_json::to_string_pretty(&only_excluded).unwrap()).unwrap();
    let before = std::fs::read(&cfg_path).unwrap();
    let e = Engine::open(cfg_path.clone(), dir.join("data"), Arc::new(NoHost)).unwrap();
    let r = e.run_probe(None, false).await;
    assert_eq!(r["done"], json!(0));
    assert_eq!(r["skipped"].as_array().unwrap().len(), 2);
    assert_eq!(r["skipped"][0]["reason"], json!("解体"));
    assert!(e.sync_logs(e.config().nodes).await["results"].as_array().unwrap().is_empty());
    assert_eq!(e.run_inventory(None).await["results"], json!([]));
    e.compute_checks(false, false);
    let led = e.ledger().unwrap();
    assert_eq!(led["counts"], json!({ "target": 0, "unobserved": 0, "excluded": 2, "total": 2 }));
    // 状態は「運用状態」の1件だけ。異常（fail）は出ない
    let checks = e.with_db(|d| d.checks()).unwrap();
    assert!(checks.iter().filter(|c| c["scope"] == "gone-1").all(|c| c["id"] == "ledger" && c["status"] == "unknown"));
    assert!(!checks.iter().any(|c| c["status"] == "fail" && c["scope"] != "_app"));
    assert_eq!(std::fs::read(&cfg_path).unwrap(), before, "台帳が書き換わった");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn monitor_and_helper_transport_refuse_excluded_nodes() {
    let dir = tmp("monitor");
    let cfg_path = dir.join("nodes.json");
    let cfg = json!({"nodes": [{"id": "hub", "alias": "excluded.invalid", "os": "windows", "status": "retired"}]});
    std::fs::write(&cfg_path, cfg.to_string()).unwrap();
    let e = Engine::open(cfg_path, dir.join("data"), Arc::new(NoHost)).unwrap();
    // Stored settings may outlive a node's online status.
    e.with_db(|d| d.set_meta("monitor", &json!({"hub": "hub"}))).unwrap();
    let hub = e.config().node("hub").unwrap().clone();
    let refusal = hub.refusal().unwrap();
    assert_eq!(tune_core::monitor::pull(&e).await.unwrap_err(), refusal);
    assert_eq!(tune_core::helpers::push_agent(&hub, &[]).await.unwrap_err(), refusal);
    let _ = std::fs::remove_dir_all(&dir);
}
