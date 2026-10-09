//! AI エージェント（Claude Code・Codex）のセッションの取り込み。各機体で probes/ai_sessions.py を流す（読み取り専用）。
//! - macOS: python3（この機体ならローカル、それ以外は ssh で標準入力へ）
//! - Windows: python3 → python → py の順に、動くものを探して使う（Microsoft Store の案内だけの python は動かないので飛ばす）。
//!   どれも無ければ「python が無い」と状態に出す
//! - 続きの位置（ファイルごとのバイト位置）は DB が覚えて、スクリプトの `KT_STATE = {}` を置き換えて毎回渡す。機体には何も書かない
//! - 1回は 25 秒で区切る（スクリプトの KT_BUDGET_S）。続きは次の回に読む
//! - 続きの位置と一緒に、位置の手前の指紋（出どころの台帳の区間から）を渡す。調査は指紋が合わなければ最初から読み直す
//! - 同じスクリプトの KT_VERIFY に区間を入れると、取り込まずに元ファイルの区間を読み直して指紋だけを返す（検算。[`verify_script`]）
//!
//! 取り出すのは数・名前・時刻・モデル・トークン・PR の URL だけ。会話の本文・ツールの入出力は取り出さない・保存しない。

use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::collect::{self, RunResult, last_json_line, ssh_args};
use crate::js;
use crate::nodes::Node;

pub const AI_PROBE: &str = include_str!("../../../probes/ai_sessions.py");
/// 出どころの台帳に残す調査スクリプトの名前
pub const AI_PROBE_NAME: &str = "probes/ai_sessions.py";
/// 機体に動く python が無いときの印（Windows の ssh で出す）
pub const NO_PYTHON: &str = "@@KATALA_TUNE_NO_PYTHON@@";
/// 既定の取り込みの間隔（分）。日ごとの集計には 30 分の鮮度で足り、各機体のファイルを全部 stat する負荷を抑える。
/// 初回など続きがあるとき（25 秒で区切った）は [`CATCH_UP_MINUTES`] で続きを読む
pub const DEFAULT_AI_MINUTES: i64 = 30;
pub const CATCH_UP_MINUTES: i64 = 2;
const TIMEOUT: Duration = Duration::from_secs(150);

/// スクリプトの中の `NAME = "..."` の値（版の読み取り）
pub fn script_const(src: &str, name: &str) -> Option<String> {
    let line = src.lines().find(|l| l.starts_with(&format!("{name} = \"")))?;
    let rest = &line[name.len() + 4..];
    Some(rest[..rest.find('"')?].to_string())
}

/// 調査スクリプト（続きの位置を埋め込む前のひな形）の SHA-256 と版。出どころの台帳に残す
pub fn probe_meta() -> &'static (String, Option<String>) {
    static META: LazyLock<(String, Option<String>)> = LazyLock::new(|| (crate::db::sha256_hex(AI_PROBE.as_bytes()), script_const(AI_PROBE, "KT_VERSION")));
    &META
}

/// 出どころの台帳に書く、AI の取り込み 1 回の見出し（実行 ID・調査スクリプトの版と SHA-256・Tune の版・開始と終了）
pub fn run_meta(node_id: &str, started_at: i64, finished_at: i64) -> crate::db::RunMeta {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let (sha, ver) = probe_meta();
    crate::db::RunMeta {
        run_id: format!("ai-{started_at}-{}-{node_id}", SEQ.fetch_add(1, Ordering::SeqCst)),
        kind: "ai_sessions".into(),
        node_id: node_id.into(),
        probe: AI_PROBE_NAME.into(),
        probe_version: ver.clone(),
        probe_sha256: sha.clone(),
        tune_version: env!("CARGO_PKG_VERSION").into(),
        started_at,
        finished_at,
    }
}

fn is_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 続きの位置を埋め込んだスクリプト。state は {"files": {鍵: 数}, "anchors": {鍵: [数, 16 進]}}（[`crate::db::Store::ai_state`]）。
/// 数と文字列と配列だけなので、JSON がそのまま Python の辞書として読める（形の違う値は落とす）
pub fn script(state: &Value) -> String {
    let files: Map<String, Value> = state
        .get("files")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, v)| v.is_u64() || v.is_i64())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let anchors: Map<String, Value> = state
        .get("anchors")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, v)| {
            let a = js::arr(Some(v));
            a.len() == 2 && a[0].is_u64() && a[1].as_str().is_some_and(is_sha)
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    // 旧 cursor だけの Claude ファイルを一度だけ全量 replace で読み直す対象（応答 ID の台帳を作るため）
    let replay = state.get("claude_replay").and_then(Value::as_array).cloned().unwrap_or_default();
    let state = json!({ "files": files, "anchors": anchors, "claude_replay": replay }).to_string();
    // 文字列の JSON として埋め込み、機体の Python で json.loads する（鍵に引用符などが入っても壊れない）
    let literal = serde_json::to_string(&state).unwrap_or_else(|_| "\"{}\"".into());
    AI_PROBE.replacen("KT_STATE = {}", &format!("KT_STATE = json.loads({literal})"), 1)
}

/// 検算のスクリプト: 区間（鍵, 開始, 終了）を元ファイルから読み直して指紋だけを返す（取り込まない）。最大 50 区間
pub fn verify_script(items: &[(String, i64, i64)]) -> String {
    let list: Vec<Value> = items.iter().take(50).map(|(k, a, b)| json!([k, a, b])).collect();
    AI_PROBE.replacen("KT_VERIFY = []", &format!("KT_VERIFY = {}", Value::Array(list)), 1)
}

/// Windows（ssh の既定シェルは Git Bash）で動く python を探して実行する。無ければ印を出す
fn windows_remote() -> String {
    format!(
        "for c in python3 python py; do if command -v \"$c\" >/dev/null 2>&1 && \"$c\" -c \"import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)\" >/dev/null 2>&1 </dev/null; then exec \"$c\" -X utf8 -; fi; done; echo {NO_PYTHON}"
    )
}

/// 調査の結果
#[derive(Debug, Clone)]
pub enum Fetched {
    Ok(Value),
    NoPython(String),
    Error(String),
}

fn missing_python(r: &RunResult) -> bool {
    let e = format!("{}\n{}", r.err, r.out);
    r.out.contains(NO_PYTHON)
        || (r.code.is_none() && e.contains("Error: spawn"))
        || r.code == Some(9009)
        || e.contains("Python was not found")
        || e.contains("xcode-select: note: no developer tools")
        || (e.contains("env: python3") && e.contains("No such file"))
}

/// 出力を結果にする
pub fn parse(r: &RunResult) -> Fetched {
    if let Some(v) = last_json_line(&r.out).filter(|v| v.get("sessions").is_some_and(Value::is_array)) {
        return Fetched::Ok(v);
    }
    if missing_python(r) {
        return Fetched::NoPython("python が無い（python 3.8 以上を入れると取り込める）".into());
    }
    let e = if !r.err.trim().is_empty() {
        r.err.clone()
    } else if !r.out.trim().is_empty() {
        r.out.clone()
    } else {
        format!("exit {}", r.code_str())
    };
    Fetched::Error(js::slice16_tail(js::trim(&e), 800))
}

/// 1台を調べる。state は [`script`] と同じ
pub async fn fetch(node: &Node, state: &Value) -> (Fetched, f64) {
    let started = Instant::now();
    let res = run_python(node, &script(state), TIMEOUT).await;
    (parse(&res), started.elapsed().as_secs_f64())
}

/// 検算（[`verify_script`]）。{ verify: [{ file, start, end, state, sha256, lines }] } か、失敗の理由
pub async fn verify(node: &Node, items: &[(String, i64, i64)]) -> Result<Vec<Value>, String> {
    let r = run_python(node, &verify_script(items), TIMEOUT).await;
    if let Some(v) = last_json_line(&r.out).and_then(|v| v.get("verify").and_then(Value::as_array).cloned()) {
        return Ok(v);
    }
    Err(match parse(&r) {
        Fetched::NoPython(m) | Fetched::Error(m) => m,
        Fetched::Ok(_) => "検算の結果が無い".into(),
    })
}

/// python の標準ライブラリだけのスクリプトを、機体で（この機体ならローカル、ほかは ssh で）標準入力に渡して動かす
pub async fn run_python(node: &Node, src: &str, timeout: Duration) -> RunResult {
    let input = Some(src.as_bytes());
    if node.is_mac() {
        if node.local {
            collect::run("/usr/bin/env", &["python3".into(), "-X".into(), "utf8".into(), "-".into()], input, timeout).await
        } else {
            let remote = "command -v python3 >/dev/null && exec python3 -X utf8 - || exec /usr/bin/python3 -X utf8 -";
            collect::run("ssh", &ssh_args(&node.alias, remote), input, timeout).await
        }
    } else if node.local {
        let mut last = RunResult::default();
        for py in ["python3", "python", "py"] {
            let r = collect::run(py, &["-X".into(), "utf8".into(), "-".into()], input, timeout).await;
            if !missing_python(&r) {
                last = r;
                break;
            }
            last = RunResult { out: NO_PYTHON.into(), ..r };
        }
        last
    } else {
        collect::run("ssh", &ssh_args(&node.alias, &windows_remote()), input, timeout).await
    }
}

/// AI セッションを取り込む機体か。台帳の "ai_sessions" が true / false ならそれに従う。
/// 書いていなければ、共用機（shared）は取り込まない（他の人のセッションの cwd や PR を集めないため）。それ以外は取り込む
pub fn enabled(node: &Node) -> bool {
    match node.get("ai_sessions") {
        Some(Value::Bool(b)) => *b,
        _ => !node.shared,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_nodes_are_off_unless_opted_in() {
        let n = |v: Value| crate::nodes::Node::from_value(&v, "other-host");
        assert!(enabled(&n(json!({ "id": "a", "alias": "a", "os": "macos" }))));
        assert!(!enabled(&n(json!({ "id": "a", "alias": "a", "os": "macos", "ai_sessions": false }))));
        assert!(!enabled(&n(json!({ "id": "s", "alias": "s", "os": "windows", "shared": true }))));
        assert!(enabled(&n(json!({ "id": "s", "alias": "s", "os": "windows", "shared": true, "ai_sessions": true }))));
    }

    #[test]
    fn state_is_embedded_as_python_dict() {
        let sha = crate::db::sha256_hex(b"x");
        let state = json!({ "files": { "claude:~-work/a \"b\".jsonl": 12, "bad": "x" },
                            "anchors": { "claude:~-work/a \"b\".jsonl": [12, sha], "c": [1, "NOT-HEX"], "d": [1] },
                            "claude_replay": ["claude:~-work/a \"b\".jsonl"] });
        let s = script(&state);
        assert!(s.contains("KT_STATE = json.loads("));
        assert!(s.contains("\\\"files\\\":{\\\"claude:~-work/a"), "{}", &s[..2000]);
        assert!(s.contains(&sha) && s.contains("claude_replay"));
        assert!(!s.contains("KT_STATE = {}"));
        assert!(!s.contains("\"bad\"") && !s.contains("NOT-HEX"));
        assert!(s.contains("KT_VERIFY = []"), "取り込みのときは検算しない");
        let v = verify_script(&[("codex:2026/r.jsonl".into(), 0, 10)]);
        assert!(v.contains("KT_VERIFY = [[\"codex:2026/r.jsonl\",0,10]]") && v.contains("KT_STATE = {}"));
    }

    #[test]
    fn probe_meta_has_version_and_sha() {
        let (sha, ver) = probe_meta();
        assert_eq!(sha.len(), 64);
        assert!(ver.as_deref().is_some_and(|v| !v.is_empty()));
        assert_eq!(script_const("A = \"1.2\" # x\n", "A").as_deref(), Some("1.2"));
    }

    #[test]
    fn parse_outcomes() {
        let ok = RunResult { code: Some(0), out: "{\"sessions\":[],\"cursors\":{}}".into(), err: String::new() };
        assert!(matches!(parse(&ok), Fetched::Ok(_)));
        let none = RunResult { code: Some(0), out: format!("{NO_PYTHON}\n"), err: String::new() };
        assert!(matches!(parse(&none), Fetched::NoPython(_)));
        let spawn = RunResult { code: None, out: String::new(), err: "Error: spawn python3 No such file".into() };
        assert!(matches!(parse(&spawn), Fetched::NoPython(_)));
        let ssh = RunResult { code: Some(255), out: String::new(), err: "ssh: connect to host x port 22: Operation timed out\n".into() };
        assert!(matches!(parse(&ssh), Fetched::Error(e) if e.starts_with("ssh: connect")));
    }

    #[test]
    fn windows_command_looks_for_a_working_python() {
        let c = windows_remote();
        assert!(c.contains("for c in python3 python py") && c.contains("-X utf8 -") && c.ends_with(NO_PYTHON));
        // PowerShell を通さない（シングルクォートを含めない）
        assert!(!c.contains('\''));
    }

    #[tokio::test]
    async fn probe_runs_locally_on_an_empty_home() {
        if cfg!(windows) || std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        // 架空のホーム（空）で動かすと、何も読まずに終わる
        let home = std::env::temp_dir().join(format!("kt-ai-empty-{}-{}", std::process::id(), crate::db::now_ms()));
        std::fs::create_dir_all(&home).unwrap();
        let mut c = collect::command("python3");
        c.args(["-X", "utf8", "-"]).env("HOME", &home).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
        let mut child = c.spawn().unwrap();
        use tokio::io::AsyncWriteExt;
        let mut si = child.stdin.take().unwrap();
        si.write_all(script(&json!({})).as_bytes()).await.unwrap();
        drop(si);
        let o = child.wait_with_output().await.unwrap();
        let r = RunResult { code: o.status.code(), out: String::from_utf8_lossy(&o.stdout).into(), err: String::new() };
        let Fetched::Ok(v) = parse(&r) else { panic!("{r:?}") };
        assert_eq!((v["files_total"].clone(), v["truncated"].clone()), (json!(0), json!(false)));
        let _ = std::fs::remove_dir_all(&home);
    }
}
