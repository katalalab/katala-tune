//! 読み取り専用の調査（今の probe と同じ JSON）。probes/ のスクリプトをバイナリに埋め込み、この機体で動かす。
//! tune-core の collect.rs のローカル実行と同じやり方（macOS は python3 の標準入力へ、Windows は一時ファイルの PowerShell）。
//! 引数は受け取らない（要求の args を何も使わないので、外から何かを差し込む余地が無い）。

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

pub const MAC_PROBE: &str = include_str!("../../../probes/mac_probe.py");
/// PS 5.1 のため BOM 付き ASCII。バイトのまま渡す
pub const WIN_PROBE: &[u8] = include_bytes!("../../../probes/win_probe.ps1");
/// Windows の調査には 1 スレッドの計測が入っていないので、python があれば足す（tune-core の collect.rs の BENCH_PY と同じ）
const BENCH_PY: &str = "import time,json,statistics as s\nr=[]\nfor _ in range(5):\n t=time.perf_counter();sum(i*i for i in range(3000000));r.append(round((time.perf_counter()-t)*1000,1))\nprint(json.dumps({'runs_ms':r,'median_ms':s.median(r)}))";

/// Finder・launchd から起動すると PATH が最小なので、python3 が見えるよう足す（collect.rs と同じ）
fn path_env() -> std::ffi::OsString {
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if !cfg!(windows) {
        for p in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
            paths.push(PathBuf::from(p));
        }
        if let Some(h) = std::env::home_dir() {
            paths.push(h.join(".local/bin"));
        }
    }
    std::env::join_paths(paths).unwrap_or_default()
}

struct Out {
    ok: bool,
    out: String,
    err: String,
}

async fn run(cmd: &str, args: &[&str], input: Option<&[u8]>, limit: Duration) -> Out {
    let mut c = Command::new(cmd);
    c.args(args).env("PATH", path_env()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return Out { ok: false, out: String::new(), err: format!("{cmd} を起動できない: {e}") },
    };
    let mut stdin = child.stdin.take();
    let input = input.map(<[u8]>::to_vec).unwrap_or_default();
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let io = async move {
        let w = async {
            if let Some(si) = stdin.as_mut() {
                let _ = si.write_all(&input).await;
                let _ = si.shutdown().await;
            }
            drop(stdin);
        };
        let r = async {
            let mut b = Vec::new();
            if let Some(s) = so.as_mut() {
                let _ = s.read_to_end(&mut b).await;
            }
            b
        };
        let e = async {
            let mut b = Vec::new();
            if let Some(s) = se.as_mut() {
                let _ = s.read_to_end(&mut b).await;
            }
            b
        };
        let ((), o, e) = tokio::join!(w, r, e);
        (o, e)
    };
    match tokio::time::timeout(limit, async { tokio::join!(io, child.wait()) }).await {
        Ok(((o, e), st)) => Out {
            ok: st.map(|s| s.success()).unwrap_or(false),
            out: String::from_utf8_lossy(&o).into_owned(),
            err: String::from_utf8_lossy(&e).chars().rev().take(800).collect::<Vec<_>>().into_iter().rev().collect(),
        },
        Err(_) => Out { ok: false, out: String::new(), err: format!("{}秒で打ち切った", limit.as_secs()) },
    }
}

/// 出力の最後の JSON 行（collect.rs の last_json_line と同じ）
pub fn last_json_line(text: &str) -> Option<Value> {
    text.lines().map(str::trim).filter(|l| l.starts_with('{')).collect::<Vec<_>>().into_iter().rev().find_map(|l| serde_json::from_str(l).ok())
}

/// 調べて、(probe の JSON, かかった秒)
pub async fn probe() -> Result<(Value, f64), String> {
    let started = Instant::now();
    let data = if cfg!(target_os = "macos") {
        let r = run("/usr/bin/env", &["python3", "-"], Some(MAC_PROBE.as_bytes()), Duration::from_secs(90)).await;
        last_json_line(&r.out).ok_or_else(|| if r.err.is_empty() { format!("調査の結果が無い（ok={}）", r.ok) } else { r.err.clone() })?
    } else if cfg!(windows) {
        windows_probe().await?
    } else {
        return Err("この OS の調査はまだ無い（macOS と Windows だけ）".into());
    };
    Ok((data, started.elapsed().as_secs_f64()))
}

async fn windows_probe() -> Result<Value, String> {
    let dir = std::env::temp_dir().join("katala-tune");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let f = dir.join(format!("agent-probe-{}-{}.ps1", std::process::id(), tune_link::now_ms()));
    std::fs::write(&f, WIN_PROBE).map_err(|e| e.to_string())?;
    let path = f.to_string_lossy().into_owned();
    let r = run("powershell.exe", &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", &path], None, Duration::from_secs(120)).await;
    let _ = std::fs::remove_file(&f);
    let mut data = last_json_line(&r.out).ok_or_else(|| if r.err.is_empty() { "調査の結果が無い".to_string() } else { r.err.clone() })?;
    if data.get("bench").is_none_or(Value::is_null) {
        let b = run("python", &["-c", BENCH_PY], None, Duration::from_secs(60)).await;
        if let (Some(bj), Value::Object(m)) = (last_json_line(&b.out), &mut data) {
            m.insert("bench".into(), bj);
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_probes_are_the_repository_ones() {
        assert!(MAC_PROBE.contains("読み取り専用"));
        assert_eq!(&WIN_PROBE[..3], &[0xEF, 0xBB, 0xBF], "PS 5.1 のため BOM 付き");
    }

    #[test]
    fn last_json_line_skips_noise() {
        assert_eq!(last_json_line("noise\n{\"a\":1}\r\n{broken\n"), Some(serde_json::json!({ "a": 1 })));
        assert_eq!(last_json_line("nothing"), None);
    }

    #[tokio::test]
    async fn run_times_out_and_reports_missing_commands() {
        if cfg!(windows) {
            return;
        }
        let r = run("/bin/sh", &["-c", "cat"], Some(b"hello"), Duration::from_secs(5)).await;
        assert!(r.ok && r.out == "hello");
        let t = run("/bin/sh", &["-c", "sleep 5"], None, Duration::from_millis(200)).await;
        assert!(!t.ok && t.err.contains("打ち切った"));
        assert!(!run("/nonexistent/cmd", &[], None, Duration::from_secs(1)).await.ok);
    }
}
