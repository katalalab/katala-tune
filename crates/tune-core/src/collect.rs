//! 各機体で調査スクリプトを走らせ、snapshot を返す。読み取り専用（lib/collect.js と同じやり方）。
//! macOS: この機体ならローカル実行、それ以外は ssh で python3 の標準入力へ渡す。
//! Windows: ssh（既定シェルは Git Bash）で ~/.katala-tune/ に置いて PowerShell 5.1 で実行。この機体が Windows ならローカルで実行。
//! 機体で動くのは probes/ の読めるスクリプトだけ（ビルド時に埋め込む）。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::db::now_ms;
use crate::js;
use crate::nodes::Node;

pub const MAC_PROBE: &str = include_str!("../../../probes/mac_probe.py");
pub const MAC_LOGS: &str = include_str!("../../../probes/mac_logs.py");
/// PS 5.1 のため BOM 付き ASCII。バイトのまま渡す
pub const WIN_PROBE: &[u8] = include_bytes!("../../../probes/win_probe.ps1");
pub const WIN_LOGS: &[u8] = include_bytes!("../../../probes/win_logs.ps1");

pub const SSH_OPTS: [&str; 11] =
    ["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=10", "-o", "ControlMaster=no", "-o", "ControlPath=none"];
pub const BENCH_PY: &str = "import time,json,statistics as s\nr=[]\nfor _ in range(5):\n t=time.perf_counter();sum(i*i for i in range(3000000));r.append(round((time.perf_counter()-t)*1000,1))\nprint(json.dumps({'runs_ms':r,'median_ms':s.median(r)}))";
pub const BENCH_MARK: &str = "@@KATALA_TUNE_BENCH@@";

/// Finder・スタートメニューから起動すると PATH が最小なので、ssh・op-agent・python が見えるよう足す（main.js と同じ）
pub static PATH: LazyLock<std::ffi::OsString> = LazyLock::new(|| {
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        paths.push(root.join("System32").join("OpenSSH"));
    } else {
        let home = crate::nodes::home();
        for p in ["/opt/homebrew/bin", "/usr/local/bin"] {
            paths.push(PathBuf::from(p));
        }
        paths.push(home.join(".local/bin"));
        paths.push(PathBuf::from("/usr/bin"));
        paths.push(PathBuf::from("/bin"));
    }
    std::env::join_paths(paths).unwrap_or_default()
});

/// 子プロセスを作る（PATH を足し、Windows ではコンソール窓を出さない）
pub fn command(cmd: &str) -> Command {
    let mut c = Command::new(cmd);
    c.env("PATH", &*PATH);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}

#[derive(Clone, Debug, Default)]
pub struct RunResult {
    /// 終了コード（シグナルで終わった・起動できなかったときは None。JS の null）
    pub code: Option<i32>,
    pub out: String,
    pub err: String,
}

impl RunResult {
    pub fn code_str(&self) -> String {
        self.code.map_or_else(|| "null".into(), |c| c.to_string())
    }
}

/// 日本語版 Windows のコマンド（powercfg など）は CP932 で出力する。UTF-8 として読めなければ Shift_JIS で読む
pub fn decode(buf: &[u8]) -> String {
    match std::str::from_utf8(buf) {
        // TextDecoder('utf-8') と同じく先頭の BOM は落とす
        Ok(s) => s.strip_prefix('\u{FEFF}').unwrap_or(s).to_string(),
        Err(_) => encoding_rs::SHIFT_JIS.decode_without_bom_handling(buf).0.into_owned(),
    }
}

/// コマンドを実行し、標準入力に input を渡す。timeout を過ぎたら強制終了して `timeout <ms>ms` を err に足す
pub async fn run(cmd: &str, args: &[String], input: Option<&[u8]>, timeout: Duration) -> RunResult {
    let mut c = command(cmd);
    c.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return RunResult { code: None, out: String::new(), err: format!("Error: spawn {cmd} {e}") },
    };
    let mut stdin = child.stdin.take();
    let input = input.map(<[u8]>::to_vec).unwrap_or_default();
    let writer = tokio::spawn(async move {
        if let Some(si) = stdin.as_mut() {
            let _ = si.write_all(&input).await;
            let _ = si.shutdown().await;
        }
        drop(stdin);
    });
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let read_out = tokio::spawn(async move {
        let mut b = Vec::new();
        if let Some(s) = so.as_mut() {
            let _ = s.read_to_end(&mut b).await;
        }
        b
    });
    let read_err = tokio::spawn(async move {
        let mut b = Vec::new();
        if let Some(s) = se.as_mut() {
            let _ = s.read_to_end(&mut b).await;
        }
        b
    });
    let mut extra = String::new();
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(st) => st.ok(),
        Err(_) => {
            let _ = child.start_kill();
            extra.push_str(&format!("\ntimeout {}ms", timeout.as_millis()));
            child.wait().await.ok()
        }
    };
    let _ = writer.await;
    // 孫プロセスがパイプを持ち続けても止まらないよう、読み終わりを待つのは少しだけ
    let grace = Duration::from_secs(3);
    let out = tokio::time::timeout(grace, read_out).await.ok().and_then(Result::ok).unwrap_or_default();
    let err = tokio::time::timeout(grace, read_err).await.ok().and_then(Result::ok).unwrap_or_default();
    RunResult { code: status.and_then(|s| s.code()), out: decode(&out), err: decode(&err) + &extra }
}

/// 出力の最後の JSON 行（`{` で始まる行を後ろから読めるまで）
pub fn last_json_line(text: &str) -> Option<Value> {
    let lines: Vec<&str> = text.split('\n').map(|l| js::trim(l.strip_suffix('\r').unwrap_or(l))).filter(|l| l.starts_with('{')).collect();
    lines.iter().rev().find_map(|l| serde_json::from_str(l).ok())
}

/// この機体（アプリを動かしている機体）でのコマンド実行
pub mod local {
    use super::*;

    /// PowerShell スクリプトを一時ファイルに置いて実行する（Windows のローカル実行）
    pub async fn powershell_file(script: &[u8], params: &str, timeout: Duration) -> RunResult {
        let dir = std::env::temp_dir().join("katala-tune");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return RunResult { code: None, out: String::new(), err: e.to_string() };
        }
        let f = match write_unique(&dir, script) {
            Ok(f) => f,
            Err(e) => return RunResult { code: None, out: String::new(), err: e.to_string() },
        };
        let mut args: Vec<String> = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"].iter().map(|s| s.to_string()).collect();
        args.push(f.to_string_lossy().into_owned());
        args.extend(params.split(' ').filter(|s| !s.is_empty()).map(str::to_string));
        let r = run("powershell.exe", &args, None, timeout).await;
        let _ = std::fs::remove_file(&f);
        r
    }

    /// 一時ファイルを、同時に走る他の実行と重ならない名前で新しく作る（分析とログ取り込みは並行して動く）
    pub(crate) fn write_unique(dir: &std::path::Path, script: &[u8]) -> std::io::Result<PathBuf> {
        use std::io::Write;
        static SEQ: AtomicU64 = AtomicU64::new(0);
        for _ in 0..16 {
            let f = dir.join(format!("p-{}-{}-{}.ps1", std::process::id(), now_ms(), SEQ.fetch_add(1, Ordering::Relaxed)));
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&f) {
                Ok(mut h) => {
                    h.write_all(script)?;
                    return Ok(f);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "一時ファイルの名前を作れない"))
    }

    pub async fn python(code: &str, timeout: Duration) -> RunResult {
        if cfg!(windows) {
            run("python", &["-c".into(), code.into()], None, timeout).await
        } else {
            run("/usr/bin/env", &["python3".into(), "-c".into(), code.into()], None, timeout).await
        }
    }

    /// ローカルの短いスクリプト（actions 用）。macOS は sh、Windows は PowerShell
    pub async fn script(text: &str, timeout: Duration) -> RunResult {
        if cfg!(windows) {
            run("powershell.exe", &["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), text.into()], None, timeout).await
        } else {
            run("/bin/sh", &["-c".into(), text.into()], None, timeout).await
        }
    }
}

pub fn ssh_args(alias: &str, remote: &str) -> Vec<String> {
    let mut a: Vec<String> = SSH_OPTS.iter().map(|s| s.to_string()).collect();
    a.push(alias.into());
    a.push(remote.into());
    a
}

fn mac_probe_args(benchmark: bool) -> Vec<String> {
    let mut args = vec!["python3".into(), "-".into()];
    if !benchmark {
        args.push("--skip-benchmark".into());
    }
    args
}

fn mac_ssh_command(benchmark: bool) -> String {
    let suffix = if benchmark { "" } else { " --skip-benchmark" };
    format!("command -v python3 >/dev/null && exec python3 -{suffix} || exec /usr/bin/python3 -{suffix}")
}

fn windows_ssh_command(benchmark: bool) -> String {
    let bench = BENCH_PY.replace('\'', "\"");
    let mut parts = vec![
        "mkdir -p ~/.katala-tune && cat > ~/.katala-tune/probe.ps1 &&".to_string(),
        "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"$(cygpath -w ~/.katala-tune/probe.ps1)\";".to_string(),
    ];
    if benchmark {
        parts.extend([format!("echo {BENCH_MARK};"), format!("PY=$(command -v python3 || command -v python); [ -n \"$PY\" ] && \"$PY\" -c '{bench}'")]);
    }
    parts.join(" ")
}

/// 1台を調べる。戻り値は `{ node_id, ok, data | error, wall_s, at }`（collect.js と同じ形）
pub async fn probe_node(node: &Node) -> Value {
    let started = Instant::now();
    let benchmark = node.get("benchmark") != Some(&Value::Bool(false));
    let res = if node.is_mac() {
        if node.local {
            run("/usr/bin/env", &mac_probe_args(benchmark), Some(MAC_PROBE.as_bytes()), Duration::from_secs(90)).await
        } else {
            run("ssh", &ssh_args(&node.alias, &mac_ssh_command(benchmark)), Some(MAC_PROBE.as_bytes()), Duration::from_secs(90)).await
        }
    } else if node.local {
        let p = local::powershell_file(WIN_PROBE, "", Duration::from_secs(120)).await;
        if benchmark {
            let b = local::python(BENCH_PY, Duration::from_secs(60)).await;
            RunResult { out: format!("{}\n{BENCH_MARK}\n{}", p.out, b.out), ..p }
        } else {
            p
        }
    } else {
        run("ssh", &ssh_args(&node.alias, &windows_ssh_command(benchmark)), Some(WIN_PROBE), Duration::from_secs(120)).await
    };
    let wall_s = started.elapsed().as_secs_f64();
    let (main, bench) = match res.out.split_once(BENCH_MARK) {
        Some((m, b)) => (m, Some(b)),
        None => (res.out.as_str(), None),
    };
    let Some(mut data) = last_json_line(main) else {
        let e = if !res.err.is_empty() {
            res.err.clone()
        } else if !res.out.is_empty() {
            res.out.clone()
        } else {
            format!("exit {}", res.code_str())
        };
        return json!({ "node_id": node.id, "ok": false, "error": js::slice16_tail(js::trim(&e), 800), "wall_s": wall_s, "at": now_ms() });
    };
    if let (Some(b), Value::Object(m)) = (bench, &mut data)
        && !js::truthy(m.get("bench"))
        && let Some(bj) = last_json_line(b)
    {
        m.insert("bench".into(), bj);
    }
    if !benchmark {
        data["bench"] = Value::Null;
        data["benchmark_skipped"] = Value::Bool(true);
    }
    json!({ "node_id": node.id, "ok": true, "data": data, "wall_s": wall_s, "at": now_ms() })
}

/// 全機体を並列に調べる。on_result は機体ごとに終わった順で呼ぶ。戻り値は台帳の順
pub async fn probe_all<F>(nodes: &[Node], on_result: F) -> Vec<Value>
where
    F: Fn(&Value) + Send + Sync + 'static,
{
    let on_result = std::sync::Arc::new(on_result);
    let mut set = tokio::task::JoinSet::new();
    for (i, n) in nodes.iter().cloned().enumerate() {
        let cb = on_result.clone();
        set.spawn(async move {
            let r = probe_node(&n).await;
            cb(&r);
            (i, r)
        });
    }
    let mut out: Vec<Option<Value>> = vec![None; nodes.len()];
    while let Some(j) = set.join_next().await {
        if let Ok((i, r)) = j {
            out[i] = Some(r);
        }
    }
    out.into_iter()
        .enumerate()
        .map(|(i, r)| r.unwrap_or_else(|| json!({ "node_id": nodes[i].id, "ok": false, "error": "調査の途中で内部エラー", "at": now_ms() })))
        .collect()
}

/// 機体でコマンドを実行する（actions 用）。ローカルならその OS のシェル、リモートは ssh
pub async fn exec_on(node: &Node, script: &str, timeout: Duration) -> RunResult {
    if node.local { local::script(script, timeout).await } else { run("ssh", &ssh_args(&node.alias, script), None, timeout).await }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_falls_back_to_cp932() {
        // 「電源」を CP932 で
        assert_eq!(decode(&[0x93, 0x64, 0x8C, 0xB9]), "電源");
        assert_eq!(decode("\u{FEFF}ok".as_bytes()), "ok");
    }

    #[test]
    fn last_json_line_skips_noise() {
        assert_eq!(last_json_line("noise\n{\"a\":1}\r\n{broken\n"), Some(json!({ "a": 1 })));
        assert_eq!(last_json_line("nothing"), None);
    }

    #[test]
    fn temp_scripts_never_collide() {
        let dir = std::env::temp_dir().join(format!("kt-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = local::write_unique(&dir, b"a").unwrap();
        let b = local::write_unique(&dir, b"b").unwrap();
        assert_ne!(a, b);
        assert_eq!((std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap()), (b"a".to_vec(), b"b".to_vec()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_benchmark_never_sends_fixed_computation() {
        let enabled = windows_ssh_command(true);
        assert!(enabled.contains(BENCH_MARK));
        assert!(enabled.contains("3000000"));
        let disabled = windows_ssh_command(false);
        assert!(disabled.contains("probe.ps1"));
        assert!(!disabled.contains(BENCH_MARK));
        assert!(!disabled.contains("3000000"));
        assert!(!disabled.contains("command -v python"));
        assert_eq!(mac_probe_args(false), vec!["python3", "-", "--skip-benchmark"]);
        assert_eq!(mac_ssh_command(false).matches("--skip-benchmark").count(), 2);
        assert!(!mac_ssh_command(true).contains("--skip-benchmark"));
    }

    #[test]
    fn probes_are_embedded() {
        assert!(MAC_PROBE.contains("def "));
        assert_eq!(&WIN_PROBE[..3], &[0xEF, 0xBB, 0xBF], "PS 5.1 のため BOM 付き");
    }

    #[test]
    fn ssh_does_not_reuse_controlpersist_master() {
        assert!(SSH_OPTS.contains(&"-T"));
        assert!(SSH_OPTS.contains(&"ControlMaster=no"));
        assert!(SSH_OPTS.contains(&"ControlPath=none"));
    }

    #[tokio::test]
    async fn run_passes_stdin_and_times_out() {
        if cfg!(windows) {
            return;
        }
        let r = run("/bin/sh", &["-c".into(), "cat".into()], Some(b"hello"), Duration::from_secs(5)).await;
        assert_eq!((r.code, r.out.as_str()), (Some(0), "hello"));
        let t = run("/bin/sh", &["-c".into(), "sleep 5".into()], None, Duration::from_millis(200)).await;
        assert_eq!(t.code, None);
        assert!(t.err.ends_with("timeout 200ms"));
        let missing = run("/nonexistent/cmd", &[], None, Duration::from_secs(1)).await;
        assert_eq!(missing.code, None);
        assert!(!missing.err.is_empty());
    }
}
