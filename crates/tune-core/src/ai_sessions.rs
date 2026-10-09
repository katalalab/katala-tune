//! AI エージェント（Claude Code・Codex）のセッションの取り込み。各機体で probes/ai_sessions.py を流す（読み取り専用）。
//! - macOS: python3（この機体ならローカル、それ以外は ssh で標準入力へ）
//! - Windows: python3 → python → py の順に、動くものを探して使う（Microsoft Store の案内だけの python は動かないので飛ばす）。
//!   どれも無ければ「python が無い」と状態に出す
//! - 続きの位置（ファイルごとのバイト位置）は DB が覚えて、スクリプトの `KT_STATE = {}` を置き換えて毎回渡す。機体には何も書かない
//! - 1回は 25 秒で区切る（スクリプトの KT_BUDGET_S）。続きは次の回に読む
//!
//! 取り出すのは数・名前・時刻・モデル・トークン・PR の URL だけ。会話の本文・ツールの入出力は取り出さない・保存しない。

use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::collect::{self, RunResult, last_json_line, ssh_args};
use crate::js;
use crate::nodes::Node;

pub const AI_PROBE: &str = include_str!("../../../probes/ai_sessions.py");
/// 機体に動く python が無いときの印（Windows の ssh で出す）
pub const NO_PYTHON: &str = "@@KATALA_TUNE_NO_PYTHON@@";
/// 既定の取り込みの間隔（分）。日ごとの集計には 30 分の鮮度で足り、各機体のファイルを全部 stat する負荷を抑える。
/// 初回など続きがあるとき（25 秒で区切った）は [`CATCH_UP_MINUTES`] で続きを読む
pub const DEFAULT_AI_MINUTES: i64 = 30;
pub const CATCH_UP_MINUTES: i64 = 2;
const TIMEOUT: Duration = Duration::from_secs(150);

/// 続きの位置と、旧cursorの一度限りのClaude replay対象を埋め込む。
pub fn script(files: &Map<String, Value>, claude_replay: &Value) -> String {
    let clean: Map<String, Value> = files.iter().filter(|(_, v)| v.is_u64() || v.is_i64()).map(|(k, v)| (k.clone(), v.clone())).collect();
    let replay = claude_replay.as_array().cloned().unwrap_or_default();
    let state = json!({ "files": clean, "claude_replay": replay }).to_string();
    let literal = serde_json::to_string(&state).unwrap_or_else(|_| "\"{}\"".into());
    AI_PROBE.replacen("KT_STATE = {}", &format!("KT_STATE = json.loads({literal})"), 1)
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

/// 1台を調べる
pub async fn fetch(node: &Node, files: &Map<String, Value>, claude_replay: &Value) -> (Fetched, f64) {
    let started = Instant::now();
    let src = script(files, claude_replay);
    let input = Some(src.as_bytes());
    let res = if node.is_mac() {
        if node.local {
            collect::run("/usr/bin/env", &["python3".into(), "-X".into(), "utf8".into(), "-".into()], input, TIMEOUT).await
        } else {
            let remote = "command -v python3 >/dev/null && exec python3 -X utf8 - || exec /usr/bin/python3 -X utf8 -";
            collect::run("ssh", &ssh_args(&node.alias, remote), input, TIMEOUT).await
        }
    } else if node.local {
        let mut last = RunResult::default();
        for py in ["python3", "python", "py"] {
            let r = collect::run(py, &["-X".into(), "utf8".into(), "-".into()], input, TIMEOUT).await;
            if !missing_python(&r) {
                last = r;
                break;
            }
            last = RunResult { out: NO_PYTHON.into(), ..r };
        }
        last
    } else {
        collect::run("ssh", &ssh_args(&node.alias, &windows_remote()), input, TIMEOUT).await
    };
    (parse(&res), started.elapsed().as_secs_f64())
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
        let mut m = Map::new();
        m.insert("claude:~-work/a \"b\".jsonl".into(), json!(12));
        m.insert("bad".into(), json!("x"));
        let s = script(&m, &json!(["claude:~-work/a \"b\".jsonl"]));
        assert!(s.contains("KT_STATE = json.loads("));
        assert!(s.contains("\\\"files\\\":{\\\"claude:~-work/a"));
        assert!(!s.contains("KT_STATE = {}"));
        assert!(!s.contains("\"bad\""));
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
        si.write_all(script(&Map::new(), &json!({})).as_bytes()).await.unwrap();
        drop(si);
        let o = child.wait_with_output().await.unwrap();
        let r = RunResult { code: o.status.code(), out: String::from_utf8_lossy(&o.stdout).into(), err: String::new() };
        let Fetched::Ok(v) = parse(&r) else { panic!("{r:?}") };
        assert_eq!((v["files_total"].clone(), v["truncated"].clone()), (json!(0), json!(false)));
        let _ = std::fs::remove_dir_all(&home);
    }
}
