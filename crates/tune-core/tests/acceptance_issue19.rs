// Issue #19 の受入試験（mock SSH・一時ディレクトリ・偽の対象だけ。実機の ssh・launchd・タスクスケジューラは呼ばない）。
// Electron 版の同じ条件は test/acceptance-issue19.test.js。結果の記録は docs/acceptance/issue-19.md
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tune_core::actions;
use tune_core::collect::{RunFuture, RunResult, Runner, classify_failure, probe_all_with, reason_text};
use tune_core::engine::{Confirm, Engine, NoHost};
use tune_core::nodes::Node;

fn tmp(tag: &str) -> PathBuf {
    // 並列に走るテストで名前が重ならないよう、通し番号を付ける（時刻だけでは macOS の刻みが粗く重なりうる）
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("kt-accept19-{tag}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn node_json(id: &str) -> Value {
    json!({ "id": id, "alias": format!("mock-{id}"), "os": "windows" })
}

fn node(id: &str) -> Node {
    Node::from_value(&node_json(id), "not-this-host")
}

fn res(code: Option<i32>, out: &str, err: &str) -> RunResult {
    RunResult { code, out: out.into(), err: err.into() }
}

// ---- 偽の ssh（呼び出しを数え、台本どおりの結果を返す。本物の子プロセスは作らない） ----
struct MockSsh {
    script: HashMap<String, RunResult>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl Runner for MockSsh {
    fn run<'a>(&'a self, cmd: &'a str, args: &'a [String], _input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
        Box::pin(async move {
            assert_eq!(cmd, "ssh", "偽の実行口は ssh 以外を受け付けない");
            self.calls.lock().unwrap().push(args.to_vec());
            let alias = args.iter().find(|a| a.starts_with("mock-")).cloned().unwrap_or_default();
            self.script.get(&alias).cloned().unwrap_or_else(|| res(None, "", &format!("台本に無い host: {alias}")))
        })
    }
}

// ---- 条件1: read-only probe が host 別の成功・timeout・unreachable・認証失敗を分ける ----
#[tokio::test]
async fn probe_separates_success_timeout_unreachable_and_auth_per_host() {
    let script: HashMap<String, RunResult> = [
        ("mock-ok", res(Some(0), "{\"probe\":\"windows\",\"cpu_busy\":3}\n", "")),
        ("mock-slow", res(None, "", "\ntimeout 120000ms")),
        ("mock-gone", res(Some(255), "", "ssh: Could not resolve hostname mock-gone: Name or service not known\n")),
        ("mock-down", res(Some(255), "", "ssh: connect to host mock-down port 22: Operation timed out\n")),
        ("mock-key", res(Some(255), "", "user@mock-key: Permission denied (publickey).\n")),
        ("mock-broken", res(Some(1), "not json\n", "boom")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let ssh = Arc::new(MockSsh { script, calls: Mutex::new(Vec::new()) });
    let ids = ["ok", "slow", "gone", "down", "key", "broken"];
    let nodes: Vec<Node> = ids.iter().map(|i| node(i)).collect();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen2 = seen.clone();
    let results = probe_all_with(ssh.clone(), &nodes, move |r| seen2.lock().unwrap().push(r["node_id"].as_str().unwrap().to_string())).await;

    // 戻り値は台帳の順
    assert_eq!(results.iter().map(|r| r["node_id"].as_str().unwrap()).collect::<Vec<_>>(), ids);
    assert_eq!(results[0]["ok"], json!(true));
    assert!(results[0].get("reason").is_none() && results[0].get("reason_text").is_none());
    let reasons: Vec<(&Value, &Value)> = results[1..].iter().map(|r| (&r["ok"], &r["reason"])).collect();
    assert_eq!(
        reasons,
        vec![
            (&json!(false), &json!("timeout")),
            (&json!(false), &json!("unreachable")),
            (&json!(false), &json!("unreachable")),
            (&json!(false), &json!("auth")),
            (&json!(false), &json!("error")),
        ]
    );
    // 画面に出す文言は種類ごとに違う
    let texts: std::collections::HashSet<&str> = [1, 2, 4, 5].iter().map(|&i| results[i]["reason_text"].as_str().unwrap()).collect();
    assert_eq!(texts.len(), 4);
    assert_eq!(results[1]["reason_text"], json!(reason_text("timeout")));
    // 元のエラー文は消さない（既存の画面・状態が使う）
    assert!(results[2]["error"].as_str().unwrap().contains("Could not resolve hostname"));
    assert_eq!(seen.lock().unwrap().len(), 6, "host ごとに結果が1回ずつ届く");
    let calls = ssh.calls.lock().unwrap();
    assert_eq!(calls.len(), 6);
    // すべて偽の ssh を通り、BatchMode（パスワードを聞かない）・接続の打ち切りが付く
    for a in calls.iter() {
        assert!(a.iter().any(|x| x == "BatchMode=yes") && a.iter().any(|x| x == "ConnectTimeout=10"));
    }
}

#[test]
fn failure_classification_table_matches_electron() {
    let table = [
        (res(None, "", "\ntimeout 90000ms"), "timeout"),
        (res(Some(1), "", "\ntimeout 35000ms"), "timeout"),
        (res(Some(255), "", "Connection closed by remote host\ntimeout 35000ms"), "timeout"),
        (res(Some(255), "", "ssh: connect to host x port 22: Connection refused"), "unreachable"),
        (res(Some(255), "", "ssh: connect to host x port 22: No route to host"), "unreachable"),
        (res(Some(255), "", "kex_exchange_identification: Connection closed by remote host"), "unreachable"),
        (res(Some(255), "", "Host key verification failed."), "auth"),
        (res(Some(255), "", "Connection closed by authenticating user x port 22 [preauth]\nPermission denied"), "auth"),
        (res(None, "", "Error: spawn ssh ENOENT"), "error"),
        (res(Some(3), "", ""), "error"),
    ];
    for (r, kind) in table {
        assert_eq!(classify_failure(&r), kind, "{r:?}");
    }
}

// ---- 偽の対象: 一時ディレクトリの「タスク」。state ファイルが実体 ----
struct FakeTaskHost {
    dir: PathBuf,
    calls: AtomicUsize,
    fail_next: AtomicBool,
}

impl FakeTaskHost {
    fn state_file(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{name}.state"))
    }
    fn state(&self, name: &str) -> String {
        std::fs::read_to_string(Self::state_file(&self.dir, name)).unwrap()
    }
}

impl Runner for FakeTaskHost {
    fn run<'a>(&'a self, cmd: &'a str, args: &'a [String], _input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
        Box::pin(async move {
            assert_eq!(cmd, "ssh", "偽の実行口は ssh 以外を受け付けない");
            self.calls.fetch_add(1, Ordering::SeqCst);
            let command = args.last().unwrap();
            let verb = if command.contains("Disable-ScheduledTask") {
                "Disabled"
            } else if command.contains("Enable-ScheduledTask") {
                "Ready"
            } else {
                return res(Some(1), "", "unsupported");
            };
            let name = command.split("-TaskName \"").nth(1).and_then(|s| s.split('"').next()).unwrap_or("");
            let file = Self::state_file(&self.dir, name);
            if !file.exists() {
                return res(Some(1), "", "no such task");
            }
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return res(Some(1), "", "access denied");
            }
            std::fs::write(&file, verb).unwrap();
            res(Some(0), &format!("{verb}\n"), "")
        })
    }
}

/// 永久に返らない実行器（結果を書く前にアプリが止まった状態を作る）
struct Hung(AtomicUsize);
impl Runner for Hung {
    fn run<'a>(&'a self, _cmd: &'a str, _args: &'a [String], _input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
}

struct Setup {
    dir: PathBuf,
    ledger: PathBuf,
    engine: Arc<Engine>,
    host: Arc<FakeTaskHost>,
}

fn write_ledger(path: &Path, nodes: &[Value], protect: &[&str]) {
    std::fs::write(path, json!({ "protect": protect, "nodes": nodes }).to_string()).unwrap();
}

fn setup(nodes: &[Value], protect: &[&str]) -> Setup {
    let dir = tmp("run");
    let ledger = dir.join("nodes.json");
    write_ledger(&ledger, nodes, protect);
    let engine = Engine::open(ledger.clone(), dir.join("data"), Arc::new(NoHost)).unwrap();
    let host = Arc::new(FakeTaskHost { dir: dir.clone(), calls: AtomicUsize::new(0), fail_next: AtomicBool::new(false) });
    std::fs::write(FakeTaskHost::state_file(&dir, "Backup"), "Ready").unwrap();
    Setup { dir, ledger, engine, host }
}

fn approve(yes: bool, on_confirm: impl FnOnce() + Send + 'static) -> Confirm {
    Box::new(move |_t, _b| {
        Box::pin(async move {
            on_confirm();
            yes
        })
    })
}

fn disable() -> Value {
    json!({ "type": "task-disable", "params": { "path": "\\Katala\\", "name": "Backup" } })
}

fn actions_of(e: &Engine) -> Vec<Value> {
    e.with_db(|d| d.actions(100)).unwrap()
}

// ---- 条件2: 承認なしでは変更コマンドを実行しない ----
#[tokio::test]
async fn nothing_runs_without_approval() {
    let s = setup(&[node_json("w")], &[]);
    let r = s.engine.confirm_and_run_with(s.host.as_ref(), "w", &disable(), "w: タスクを止める", None, approve(false, || {})).await.unwrap();
    assert_eq!(r["cancelled"], json!(true));
    assert_eq!(s.host.calls.load(Ordering::SeqCst), 0);
    assert_eq!(s.host.state("Backup"), "Ready");
    assert!(actions_of(&s.engine).is_empty());
}

#[tokio::test]
async fn refused_before_confirmation_never_reaches_the_runner() {
    let nodes = [node_json("w"), json!({ "id": "shared", "alias": "mock-shared", "os": "windows", "shared": true })];
    let s = setup(&nodes, &["Backup"]);
    let asked = Arc::new(AtomicUsize::new(0));
    let kill = json!({ "type": "kill-process", "params": { "pid": 4242, "name": "Backup" } });
    for (id, action) in [("w", json!({ "type": "format-disk", "params": {} })), ("shared", disable()), ("w", kill), ("nobody", disable())] {
        let asked = asked.clone();
        let r = s
            .engine
            .confirm_and_run_with(
                s.host.as_ref(),
                id,
                &action,
                "t",
                None,
                approve(true, move || {
                    asked.fetch_add(1, Ordering::SeqCst);
                }),
            )
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["refused"].is_string(), "{r}");
    }
    assert_eq!(asked.load(Ordering::SeqCst), 0, "確認ダイアログすら出さない");
    assert_eq!(s.host.calls.load(Ordering::SeqCst), 0);
    // 台帳が読めないときも実行しない
    std::fs::write(&s.ledger, "{broken").unwrap();
    let r = s.engine.confirm_and_run_with(s.host.as_ref(), "w", &disable(), "t", None, approve(true, || {})).await.unwrap();
    assert!(r["refused"].as_str().unwrap().contains("台帳を読めない"));
    assert_eq!(s.host.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn execute_called_directly_does_not_reach_the_runner_for_shared_or_unread_protect_list() {
    let host = FakeTaskHost { dir: tmp("direct"), calls: AtomicUsize::new(0), fail_next: AtomicBool::new(false) };
    let shared = Node::from_value(&json!({ "id": "s", "alias": "mock-s", "os": "windows", "shared": true }), "x");
    assert!(actions::execute_with(&host, &shared, &disable(), Some(&[])).await.unwrap_err().contains("共用機"));
    assert!(actions::execute_with(&host, &node("w"), &disable(), None).await.unwrap_err().contains("保護リスト"));
    assert_eq!(host.calls.load(Ordering::SeqCst), 0);
}

// ---- 条件3: 承認後も対象同一性が合わなければ実行を止める ----
#[tokio::test]
async fn ledger_change_during_confirmation_stops_execution_and_records_reason() {
    let cases: Vec<(&str, Vec<Value>, &str)> = vec![
        ("alias", vec![json!({ "id": "w", "alias": "mock-other", "os": "windows" })], "接続先"),
        ("os", vec![json!({ "id": "w", "alias": "mock-w", "os": "macos" })], "接続先"),
        ("local", vec![json!({ "id": "w", "alias": "mock-w", "os": "windows", "local_hostname": tune_core::nodes::local_host() })], "接続先"),
        ("removed", vec![], "台帳に無い"),
        ("shared", vec![json!({ "id": "w", "alias": "mock-w", "os": "windows", "shared": true })], "共用機"),
    ];
    for (name, changed, expected) in cases {
        let s = setup(&[node_json("w")], &[]);
        let ledger = s.ledger.clone();
        let r = s
            .engine
            .confirm_and_run_with(s.host.as_ref(), "w", &disable(), "w: タスクを止める", None, approve(true, move || write_ledger(&ledger, &changed, &[])))
            .await
            .unwrap();
        assert_eq!(r["ok"], json!(false), "{name}");
        assert!(r["refused"].as_str().unwrap().contains(expected), "{name}: {r}");
        assert_eq!(s.host.calls.load(Ordering::SeqCst), 0, "{name}: 実行器は呼ばれない");
        assert_eq!(s.host.state("Backup"), "Ready", "{name}");
        let log = actions_of(&s.engine);
        assert_eq!(log.len(), 1, "{name}");
        assert_eq!((log[0]["ok"].clone(), log[0]["state"].clone()), (json!(false), json!("failed")), "{name}");
        assert_eq!((log[0]["node_id"].as_str(), log[0]["type"].as_str()), (Some("w"), Some("task-disable")));
        assert!(log[0]["output"].as_str().unwrap().starts_with("中止（実行していない）: "), "{name}");
        assert_eq!(log[0]["undo"], Value::Null);
    }
}

#[tokio::test]
async fn protect_list_or_broken_ledger_after_approval_stops_execution() {
    let s = setup(&[node_json("w")], &[]);
    let ledger = s.ledger.clone();
    let kill = json!({ "type": "kill-process", "params": { "pid": 4242, "name": "Slow" } });
    let r = s
        .engine
        .confirm_and_run_with(s.host.as_ref(), "w", &kill, "t", None, approve(true, move || write_ledger(&ledger, &[node_json("w")], &["Slow"])))
        .await
        .unwrap();
    assert!(r["refused"].as_str().unwrap().contains("保護対象"), "{r}");
    let s2 = setup(&[node_json("w")], &[]);
    let ledger = s2.ledger.clone();
    let r2 = s2
        .engine
        .confirm_and_run_with(s2.host.as_ref(), "w", &disable(), "t", None, approve(true, move || std::fs::write(&ledger, "{broken").unwrap()))
        .await
        .unwrap();
    assert!(r2["refused"].as_str().unwrap().contains("台帳を読めない"), "{r2}");
    for x in [&s, &s2] {
        assert_eq!(x.host.calls.load(Ordering::SeqCst), 0);
        assert!(actions_of(&x.engine)[0]["output"].as_str().unwrap().starts_with("中止（実行していない）"));
    }
}

#[tokio::test]
async fn unchanged_identity_still_runs() {
    let s = setup(&[node_json("w")], &[]);
    let ledger = s.ledger.clone();
    let rewritten = json!({ "id": "w", "alias": "mock-w", "os": "windows", "note": "説明を足しただけ" });
    let r = s
        .engine
        .confirm_and_run_with(s.host.as_ref(), "w", &disable(), "t", None, approve(true, move || write_ledger(&ledger, &[rewritten], &[])))
        .await
        .unwrap();
    assert_eq!(r["ok"], json!(true));
    assert_eq!(s.host.calls.load(Ordering::SeqCst), 1);
}

// ---- 条件4: 成功・失敗・rollback を実行記録へ。偽の対象で可逆操作を実際に動かし、前後状態と照合する ----
#[tokio::test]
async fn reversible_action_on_fake_target_records_before_after_and_rollback() {
    let s = setup(&[node_json("w")], &[]);
    assert_eq!(s.host.state("Backup"), "Ready"); // 実行前
    let r = s.engine.confirm_and_run_with(s.host.as_ref(), "w", &disable(), "w: タスクを止める", None, approve(true, || {})).await.unwrap();
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["refresh_required"], json!(true));
    assert_eq!(s.host.state("Backup"), "Disabled"); // 実行後
    let log = actions_of(&s.engine);
    let row = &log[0];
    assert_eq!(
        (row["state"].as_str(), row["ok"].as_bool(), row["node_id"].as_str(), row["type"].as_str()),
        (Some("ok"), Some(true), Some("w"), Some("task-disable"))
    );
    assert_eq!(row["params"], json!({ "path": "\\Katala\\", "name": "Backup" })); // 対象 ID
    assert!(row["output"].as_str().unwrap().contains("Disabled")); // 実行後の状態
    assert_eq!(row["undo"], json!({ "type": "task-enable", "params": { "path": "\\Katala\\", "name": "Backup" } })); // 実行前の状態へ戻す操作
    let id = row["id"].as_str().unwrap().to_string();

    let u = s.engine.undo_with(s.host.as_ref(), &id, approve(true, || {})).await.unwrap();
    assert_eq!(u["ok"], json!(true));
    assert_eq!(s.host.state("Backup"), "Ready"); // 実行前と同じ
    let log = actions_of(&s.engine);
    assert_eq!(log.len(), 2);
    let undo_row = log.iter().find(|a| a["undo_of"] == json!(id)).unwrap();
    assert_eq!((undo_row["state"].as_str(), undo_row["type"].as_str(), undo_row["node_id"].as_str()), (Some("ok"), Some("task-enable"), Some("w")));
    assert!(undo_row["output"].as_str().unwrap().contains("Ready"));
    let again = s.engine.undo_with(s.host.as_ref(), &id, approve(true, || {})).await.unwrap();
    assert!(again["refused"].as_str().unwrap().contains("すでに元に戻した"));
    assert_eq!(s.host.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn failed_action_is_recorded_as_failed_without_undo() {
    let s = setup(&[node_json("w")], &[]);
    s.host.fail_next.store(true, Ordering::SeqCst);
    let r = s.engine.confirm_and_run_with(s.host.as_ref(), "w", &disable(), "w: タスクを止める", None, approve(true, || {})).await.unwrap();
    assert_eq!(r["ok"], json!(false));
    assert_eq!(r["refresh_required"], json!(true));
    let row = &actions_of(&s.engine)[0];
    assert_eq!((row["state"].as_str(), row["ok"].as_bool(), &row["undo"]), (Some("failed"), Some(false), &Value::Null));
    assert!(row["output"].as_str().unwrap().contains("失敗（exit 1）"));
    assert_eq!(s.host.state("Backup"), "Ready");
}

#[tokio::test]
async fn failed_rollback_is_recorded_and_can_be_retried() {
    let s = setup(&[node_json("w")], &[]);
    s.engine.confirm_and_run_with(s.host.as_ref(), "w", &disable(), "w: タスクを止める", None, approve(true, || {})).await.unwrap();
    let orig = actions_of(&s.engine)[0]["id"].as_str().unwrap().to_string();
    s.host.fail_next.store(true, Ordering::SeqCst);
    let u = s.engine.undo_with(s.host.as_ref(), &orig, approve(true, || {})).await.unwrap();
    assert_eq!(u["ok"], json!(false));
    assert_eq!(s.host.state("Backup"), "Disabled"); // 戻っていない
    let log = actions_of(&s.engine);
    let undo_row = log.iter().find(|a| a["undo_of"] == json!(orig)).unwrap();
    assert_eq!((undo_row["state"].as_str(), undo_row["ok"].as_bool()), (Some("failed"), Some(false)));
    // 失敗した戻しは「戻し済み」と見なされず、やり直せる
    let again = s.engine.undo_with(s.host.as_ref(), &orig, approve(true, || {})).await.unwrap();
    assert_eq!(again["ok"], json!(true));
    assert_eq!(s.host.state("Backup"), "Ready");
}

// ---- 条件5: アプリ再起動後に未完了操作を成功扱いしない ----
#[tokio::test]
async fn interrupted_action_is_incomplete_after_restart_not_success() {
    let s = setup(&[node_json("w")], &[]);
    let hung = Arc::new(Hung(AtomicUsize::new(0)));
    let (engine, runner) = (s.engine.clone(), hung.clone());
    let task =
        tokio::spawn(async move { engine.confirm_and_run_with(runner.as_ref(), "w", &disable(), "w: タスクを止める", None, approve(true, || {})).await });
    for _ in 0..200 {
        if hung.0.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(hung.0.load(Ordering::SeqCst), 1, "実行器まで進んだ（結果は返らない）");
    assert_eq!(actions_of(&s.engine).len(), 1, "実行の前に記録が書かれている");
    // アプリが止まる
    task.abort();
    let _ = task.await;
    drop(s.engine);

    // 再起動: 同じ DB を開き直す
    let reopened = Engine::open(s.ledger.clone(), s.dir.join("data"), Arc::new(NoHost)).unwrap();
    let log = actions_of(&reopened);
    assert_eq!(log.len(), 1);
    let row = &log[0];
    assert_eq!(row["state"], json!("incomplete"));
    assert_eq!(row["ok"], json!(false), "成功扱いにしない");
    assert_eq!(row["undo"], Value::Null);
    assert!(row["output"].as_str().unwrap().contains("結果は未確認"));
    let count = |st: &str| log.iter().filter(|a| a["state"] == json!(st)).count();
    assert_eq!((count("ok"), count("failed"), count("incomplete")), (0, 0, 1));
    // 状態が分からない記録は元に戻さない
    let id = row["id"].as_str().unwrap().to_string();
    let u = reopened.undo_with(s.host.as_ref(), &id, approve(true, || {})).await.unwrap();
    assert!(u["refused"].as_str().unwrap().contains("元に戻せる記録が無い"));
    assert_eq!(s.host.calls.load(Ordering::SeqCst), 0);
    // 未完了の記録にだけ結果を書ける。書いた結果は上書きしない
    assert!(reopened.with_db(|d| d.finish_action(&id, true, "x", None)).unwrap());
    assert!(!reopened.with_db(|d| d.finish_action(&id, false, "y", None)).unwrap());
    assert_eq!(actions_of(&reopened)[0]["state"], json!("ok"));
}

// ---- 条件3 の機体側: 生成されるプロセス終了スクリプトは、名前・起動時刻・負荷が合わなければ kill に進まない ----
// 試験が自分で起こした子プロセス（sleep）にだけ、ローカルの sh で実行する。どの経路でも kill に届かないことを確かめる
#[cfg(unix)]
#[tokio::test]
async fn kill_script_stops_before_kill_when_target_identity_or_load_differs() {
    let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let pid = i64::from(child.id());
    let ps = |field: &str| {
        let o = std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", field]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let lstart = ps("lstart=");
    let mac = Node::from_value(&json!({ "id": "m", "alias": "mock-m", "os": "macos" }), "x");
    let alive = || std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().unwrap().success();
    let cases = [
        // (名前, 起動時刻, 負荷のしきい値, 期待する終了コード)
        ("Slowproc", None, 50, 3),
        ("sleep", Some("Mon Jan  1 00:00:00 2001".to_string()), 50, 3),
        ("sleep", Some(lstart.clone()), 50, 4),
    ];
    for (name, start, min_cpu, expect) in cases {
        let mut params = json!({ "pid": pid, "name": name, "min_cpu": min_cpu });
        if let Some(st) = start {
            params["start"] = json!(st);
        }
        let p = actions::plan(&mac, &json!({ "type": "kill-process", "params": params }), Some(&[])).unwrap();
        let r = tune_core::collect::run("/bin/sh", &["-c".into(), p.script.clone()], None, Duration::from_secs(10)).await;
        assert_eq!(r.code, Some(expect), "{name}: {r:?}");
        assert!(alive(), "{name}: 対象は生きている");
    }
    let _ = child.kill();
    let _ = child.wait();
}

// ---- レビュー指摘: この機体（local）も渡された Runner を通る。本物のローカル実行に迂回しない ----
/// 呼び出しの種類を数えるだけの実行器（本物は何も起こさない）
struct CountLocal(Mutex<Vec<String>>);
impl Runner for CountLocal {
    fn run<'a>(&'a self, cmd: &'a str, _args: &'a [String], _input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
        Box::pin(async move {
            self.0.lock().unwrap().push(cmd.to_string());
            res(Some(0), "{\"probe\":\"windows\"}\n", "")
        })
    }
}

fn local_node() -> Node {
    Node::from_value(&json!({ "id": "here", "alias": "mock-here", "os": "windows", "local_hostname": "this-host" }), "this-host")
}

#[tokio::test]
async fn local_node_execution_and_probe_go_through_the_given_runner() {
    let n = local_node();
    assert!(n.local);
    let r = CountLocal(Mutex::new(Vec::new()));
    let out = actions::execute_with(&r, &n, &disable(), Some(&[])).await.unwrap();
    assert!(out.ok);
    assert_eq!(r.0.lock().unwrap().len(), 1, "ローカルの変更も渡された実行器を通る");
    let p = tune_core::collect::probe_node_with(&r, &n).await;
    assert_eq!(p["ok"], json!(true), "{p}");
    assert!(r.0.lock().unwrap().len() >= 3, "ローカルの調査（調査・ベンチ）も渡された実行器を通る");
}

// ---- レビュー指摘: 結果の書き込みが「未更新」を返したら成功扱いにしない ----
/// 実行中に、同じ記録へ別の手で結果を書く実行器
struct FinishesEarly(Arc<Engine>);
impl Runner for FinishesEarly {
    fn run<'a>(&'a self, _cmd: &'a str, _args: &'a [String], _input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
        Box::pin(async move {
            let id = actions_of(&self.0)[0]["id"].as_str().unwrap().to_string();
            assert!(self.0.with_db(|d| d.finish_action(&id, false, "別の手", None)).unwrap());
            res(Some(0), "Disabled\n", "")
        })
    }
}

#[tokio::test]
async fn result_write_that_updates_nothing_is_not_reported_as_success() {
    let s = setup(&[node_json("w")], &[]);
    let runner = FinishesEarly(s.engine.clone());
    let r = s.engine.confirm_and_run_with(&runner, "w", &disable(), "t", None, approve(true, || {})).await;
    let msg = r.expect_err("未更新は成功として返さない");
    assert!(msg.contains("実行記録を書けなかった"), "{msg}");
}

// ---- レビュー指摘: 内部エラーで落ちた host にも reason・reason_text が付く ----
struct Panics;
impl Runner for Panics {
    fn run<'a>(&'a self, _cmd: &'a str, _args: &'a [String], _input: Option<&'a [u8]>, _timeout: Duration) -> RunFuture<'a> {
        panic!("試験用の内部エラー")
    }
}

#[tokio::test]
async fn internal_error_result_has_reason_like_other_failures() {
    let results = probe_all_with(Arc::new(Panics), &[node("boom")], |_| {}).await;
    assert_eq!(results[0]["ok"], json!(false));
    assert_eq!(results[0]["reason"], json!("error"));
    assert_eq!(results[0]["reason_text"], json!(reason_text("error")));
}
