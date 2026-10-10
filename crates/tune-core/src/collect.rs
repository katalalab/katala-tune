//! 各機体で調査スクリプトを走らせ、snapshot を返す。読み取り専用（lib/collect.js と同じやり方）。
//! macOS: この機体ならローカル実行、それ以外は ssh で python3 の標準入力へ渡す。
//! Windows: ssh（既定シェルは Git Bash）で ~/.katala-tune/ に置いて PowerShell 5.1 で実行。この機体が Windows ならローカルで実行。
//! 機体で動くのは probes/ の読めるスクリプトだけ（ビルド時に埋め込む）。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::db::now_ms;
use crate::js;
use crate::nodes::Node;

pub const MAC_PROBE: &str = include_str!("../../../probes/mac_probe.py");
pub const MAC_LOGS: &str = include_str!("../../../probes/mac_logs.py");
/// PS 5.1 のため BOM 付き ASCII。バイトのまま渡す
pub const WIN_PROBE: &[u8] = include_bytes!("../../../probes/win_probe.ps1");
pub const WIN_LOGS: &[u8] = include_bytes!("../../../probes/win_logs.ps1");
/// ログオンの記録（win_security）。win_logs.ps1 と合わせると取り込みのコマンド行（8191 バイト）に収まらないので別にする
pub const WIN_LOGONS: &[u8] = include_bytes!("../../../probes/win_logons.ps1");

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
    // Git Bash を既定シェルにする機体では、標準 OpenSSH と Git 同梱版で
    // 非対話パイプの挙動が異なる。GUI と CLI で同じ実体を選ぶ。
    #[cfg(windows)]
    let cmd = if cmd == "ssh" {
        windows_ssh_program(std::env::var_os("ProgramFiles").as_deref(), std::env::var_os("SystemRoot").as_deref(), |p| p.is_file())
    } else {
        cmd.into()
    };
    let mut c = Command::new(cmd);
    c.env("PATH", &*PATH);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}

#[cfg(any(windows, test))]
fn windows_ssh_program(
    program_files: Option<&std::ffi::OsStr>,
    system_root: Option<&std::ffi::OsStr>,
    exists: impl Fn(&std::path::Path) -> bool,
) -> std::ffi::OsString {
    if let Some(dir) = program_files {
        let git = PathBuf::from(dir).join("Git").join("usr").join("bin").join("ssh.exe");
        if exists(&git) {
            return git.into_os_string();
        }
    }
    let native = system_root.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows")).join("System32").join("OpenSSH").join("ssh.exe");
    if exists(&native) { native.into_os_string() } else { "ssh".into() }
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
    let (out, read_out) = read_into_buffer(child.stdout.take());
    let (err, read_err) = read_into_buffer(child.stderr.take());
    let mut extra = String::new();
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(st) => st.ok(),
        Err(_) => {
            let _ = child.start_kill();
            extra.push_str(&format!("\ntimeout {}ms", timeout.as_millis()));
            child.wait().await.ok()
        }
    };
    // 孫プロセスがパイプを握り続けても止まらないよう、子が終わってから待つのは PIPE_GRACE まで。
    // 過ぎたら読み取り（と書き込み）を止め、それまでに読んだ分を返す
    let deadline = tokio::time::Instant::now() + PIPE_GRACE;
    for mut task in [writer, read_out, read_err] {
        if tokio::time::timeout_at(deadline, &mut task).await.is_err() {
            task.abort();
            let _ = task.await;
        }
    }
    let out = std::mem::take(&mut *lock_buf(&out));
    let err = std::mem::take(&mut *lock_buf(&err));
    RunResult { code: status.and_then(|s| s.code()), out: decode(&out), err: decode(&err) + &extra }
}

pub type RunFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = RunResult> + Send + 'a>>;

/// 子プロセスの実行口。本物は [`System`]、テストは偽の ssh に差し替える（呼び出しを数え、台本どおりの結果を返す）
///
/// この機体（local）での実行も同じ口を通す。既定の実装は `run` に `local:script` などの名前で渡すので、
/// 偽の実行器は本物のローカル実行に迂回されず、呼び出しを数えられる。[`System`] だけが本物のローカル実行をする
pub trait Runner: Send + Sync {
    fn run<'a>(&'a self, cmd: &'a str, args: &'a [String], input: Option<&'a [u8]>, timeout: Duration) -> RunFuture<'a>;

    /// ローカルの短いスクリプト（actions 用）
    fn local_script<'a>(&'a self, text: &'a str, timeout: Duration) -> RunFuture<'a> {
        Box::pin(async move { self.run("local:script", &[text.to_string()], None, timeout).await })
    }
    /// ローカルで PowerShell スクリプトを実行する（Windows のローカル調査）
    fn local_powershell_file<'a>(&'a self, script: &'a [u8], params: &'a str, timeout: Duration) -> RunFuture<'a> {
        Box::pin(async move { self.run("local:powershell", &[params.to_string()], Some(script), timeout).await })
    }
    /// ローカルで python を実行する（Windows のローカル調査のベンチ）
    fn local_python<'a>(&'a self, code: &'a str, timeout: Duration) -> RunFuture<'a> {
        Box::pin(async move { self.run("local:python", &[code.to_string()], None, timeout).await })
    }
}

/// 本物の子プロセスを起こす
pub struct System;

impl Runner for System {
    fn run<'a>(&'a self, cmd: &'a str, args: &'a [String], input: Option<&'a [u8]>, timeout: Duration) -> RunFuture<'a> {
        Box::pin(run(cmd, args, input, timeout))
    }
    fn local_script<'a>(&'a self, text: &'a str, timeout: Duration) -> RunFuture<'a> {
        Box::pin(local::script(text, timeout))
    }
    fn local_powershell_file<'a>(&'a self, script: &'a [u8], params: &'a str, timeout: Duration) -> RunFuture<'a> {
        Box::pin(local::powershell_file(script, params, timeout))
    }
    fn local_python<'a>(&'a self, code: &'a str, timeout: Duration) -> RunFuture<'a> {
        Box::pin(local::python(code, timeout))
    }
}

/// 失敗の理由を、画面と判定が使える種類に分ける（lib/collect.js の classifyFailure と同じ）。
/// timeout: 接続はできたが、こちらの打ち切り（run が足す `timeout <ms>ms`）まで終わらなかった。
/// unreachable: ssh が相手に届かない（名前が引けない・拒否・経路なし・接続の時間切れ）。auth: 鍵・ホスト鍵で入れない。error: それ以外
pub fn classify_failure(res: &RunResult) -> &'static str {
    static TIMEOUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"timeout \d+ms\s*$").expect("TIMEOUT"));
    static AUTH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)Permission denied|Host key verification failed|Authentication failed|Too many authentication failures").expect("AUTH")
    });
    static UNREACHABLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)Could not resolve hostname|Name or service not known|Connection refused|No route to host|Network is unreachable|Connection timed out|Operation timed out|Host is down|Connection reset|Connection closed by|kex_exchange_identification|banner exchange",
        )
        .expect("UNREACHABLE")
    });
    let text = format!("{}\n{}", res.err, res.out);
    if TIMEOUT.is_match(&res.err) {
        "timeout"
    } else if AUTH.is_match(&text) {
        "auth"
    } else if UNREACHABLE.is_match(&text) {
        "unreachable"
    } else {
        "error"
    }
}

/// 失敗の種類の、画面に出す文言
pub fn reason_text(kind: &str) -> &'static str {
    match kind {
        "timeout" => "時間切れ（接続後に応答が返らなかった）",
        "unreachable" => "接続できない（電源・ネットワーク・Host 名）",
        "auth" => "認証できない（鍵・ホスト鍵）",
        _ => "調査の失敗",
    }
}

/// 子が終わった後、孫プロセスが標準出力・標準エラーを握っていても待つ時間。
/// lib/collect.js が 'exit' から 'close' を待つ 500ms と揃える（同じコマンドなら Electron 版と Tauri 版で同じ出力を返す）。
/// 子が書いた分はパイプに残っていて終了の直後に読み切れるので、ここで切れるのは孫が後から書く分だけ
const PIPE_GRACE: Duration = Duration::from_millis(500);

type SharedBuf = Arc<Mutex<Vec<u8>>>;

fn lock_buf(b: &SharedBuf) -> std::sync::MutexGuard<'_, Vec<u8>> {
    b.lock().unwrap_or_else(PoisonError::into_inner)
}

/// パイプを読み終わるまで共有のバッファへ貯めるタスク。止めても、それまでに読んだ分はバッファに残る
fn read_into_buffer<R: AsyncRead + Unpin + Send + 'static>(pipe: Option<R>) -> (SharedBuf, JoinHandle<()>) {
    let buf = SharedBuf::default();
    let b = buf.clone();
    let task = tokio::spawn(async move {
        let Some(mut p) = pipe else { return };
        let mut chunk = vec![0u8; 16 * 1024];
        while let Ok(n) = p.read(&mut chunk).await
            && n > 0
        {
            lock_buf(&b).extend_from_slice(&chunk[..n]);
        }
    });
    (buf, task)
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

/// 調査スクリプトへの印。benchmark = false（台帳の "benchmark": false）でベンチマークを省き、
/// network = false（台帳の "network": false）でネットワークとセキュリティ（netsec）を集めない（lib/collect.js と同じ）
fn mac_probe_flags(benchmark: bool, network: bool) -> Vec<String> {
    let mut f = Vec::new();
    if !benchmark {
        f.push("--skip-benchmark".to_string());
    }
    if !network {
        f.push("nonet".to_string());
    }
    f
}

fn mac_probe_args(benchmark: bool, network: bool) -> Vec<String> {
    let mut args = vec!["python3".to_string(), "-".to_string()];
    args.extend(mac_probe_flags(benchmark, network));
    args
}

fn mac_ssh_command(benchmark: bool, network: bool) -> String {
    let suffix: String = mac_probe_flags(benchmark, network).iter().map(|a| format!(" {a}")).collect();
    format!("command -v python3 >/dev/null && exec python3 -{suffix} || exec /usr/bin/python3 -{suffix}")
}

fn windows_ssh_command(benchmark: bool, network: bool) -> String {
    let bench = BENCH_PY.replace('\'', "\"");
    let mut parts = vec![
        "mkdir -p ~/.katala-tune && cat > ~/.katala-tune/probe.ps1 &&".to_string(),
        format!(
            "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"$(cygpath -w ~/.katala-tune/probe.ps1)\"{};",
            if network { "" } else { " -NoNetwork" }
        ),
    ];
    if benchmark {
        parts.extend([format!("echo {BENCH_MARK};"), format!("PY=$(command -v python3 || command -v python); [ -n \"$PY\" ] && \"$PY\" -c '{bench}'")]);
    }
    parts.join(" ")
}

/// 1台を調べる。戻り値は `{ node_id, ok, data | error, wall_s, at }`（collect.js と同じ形）。失敗には reason・reason_text が付く
pub async fn probe_node(node: &Node) -> Value {
    probe_node_with(&System, node).await
}

/// `probe_node` の子プロセスの実行口を差し替えられる版（テストは偽の ssh を渡す）
pub async fn probe_node_with(runner: &dyn Runner, node: &Node) -> Value {
    let started = Instant::now();
    let network = crate::netsec::enabled(&node.raw);
    let benchmark = node.get("benchmark") != Some(&Value::Bool(false));
    let res = if node.is_mac() {
        if node.local {
            runner.run("/usr/bin/env", &mac_probe_args(benchmark, network), Some(MAC_PROBE.as_bytes()), Duration::from_secs(90)).await
        } else {
            runner.run("ssh", &ssh_args(&node.alias, &mac_ssh_command(benchmark, network)), Some(MAC_PROBE.as_bytes()), Duration::from_secs(90)).await
        }
    } else if node.local {
        let p = runner.local_powershell_file(WIN_PROBE, if network { "" } else { "-NoNetwork" }, Duration::from_secs(120)).await;
        if benchmark {
            let b = runner.local_python(BENCH_PY, Duration::from_secs(60)).await;
            RunResult { out: format!("{}\n{BENCH_MARK}\n{}", p.out, b.out), ..p }
        } else {
            p
        }
    } else {
        runner.run("ssh", &ssh_args(&node.alias, &windows_ssh_command(benchmark, network)), Some(WIN_PROBE), Duration::from_secs(120)).await
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
        let reason = classify_failure(&res);
        return json!({ "node_id": node.id, "ok": false, "reason": reason, "reason_text": reason_text(reason), "error": js::slice16_tail(js::trim(&e), 800), "wall_s": wall_s, "at": now_ms() });
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
    if !crate::netsec::peers_enabled(&node.raw) {
        crate::netsec::drop_peers(&mut data);
    }
    json!({ "node_id": node.id, "ok": true, "data": data, "wall_s": wall_s, "at": now_ms() })
}

/// 全機体を並列に調べる。on_result は機体ごとに終わった順で呼ぶ。戻り値は台帳の順
pub async fn probe_all<F>(nodes: &[Node], on_result: F) -> Vec<Value>
where
    F: Fn(&Value) + Send + Sync + 'static,
{
    probe_all_with(std::sync::Arc::new(System), nodes, on_result).await
}

/// `probe_all` の子プロセスの実行口を差し替えられる版
pub async fn probe_all_with<F>(runner: std::sync::Arc<dyn Runner>, nodes: &[Node], on_result: F) -> Vec<Value>
where
    F: Fn(&Value) + Send + Sync + 'static,
{
    let on_result = std::sync::Arc::new(on_result);
    let mut set = tokio::task::JoinSet::new();
    for (i, n) in nodes.iter().cloned().enumerate() {
        let cb = on_result.clone();
        let runner = runner.clone();
        set.spawn(async move {
            let r = probe_node_with(runner.as_ref(), &n).await;
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
        .map(|(i, r)| {
            r.unwrap_or_else(|| {
                json!({ "node_id": nodes[i].id, "ok": false, "reason": "error", "reason_text": reason_text("error"), "error": "調査の途中で内部エラー", "at": now_ms() })
            })
        })
        .collect()
}

/// 機体でコマンドを実行する（actions 用）。ローカルならその OS のシェル、リモートは ssh
pub async fn exec_on(node: &Node, script: &str, timeout: Duration) -> RunResult {
    exec_on_with(&System, node, script, timeout).await
}

/// `exec_on` の子プロセスの実行口を差し替えられる版（変更操作のテストは偽の実行器を渡す）
pub async fn exec_on_with(runner: &dyn Runner, node: &Node, script: &str, timeout: Duration) -> RunResult {
    if node.local { runner.local_script(script, timeout).await } else { runner.run("ssh", &ssh_args(&node.alias, script), None, timeout).await }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_ssh_prefers_git_independently_of_parent_path() {
        let programs = PathBuf::from("programs");
        let root = PathBuf::from("system");
        let git = programs.join("Git/usr/bin/ssh.exe");
        let native = root.join("System32/OpenSSH/ssh.exe");
        let selected = windows_ssh_program(Some(programs.as_os_str()), Some(root.as_os_str()), |p| p == git || p == native);
        assert_eq!(PathBuf::from(selected), git);
    }

    #[test]
    fn windows_ssh_falls_back_without_installing_or_changing_config() {
        let programs = PathBuf::from("programs");
        let root = PathBuf::from("system");
        let native = root.join("System32/OpenSSH/ssh.exe");
        assert_eq!(PathBuf::from(windows_ssh_program(Some(programs.as_os_str()), Some(root.as_os_str()), |p| p == native)), native);
        assert_eq!(windows_ssh_program(None, Some(root.as_os_str()), |_| false), std::ffi::OsString::from("ssh"));
    }

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
        let enabled = windows_ssh_command(true, true);
        assert!(enabled.contains(BENCH_MARK));
        assert!(enabled.contains("3000000"));
        let disabled = windows_ssh_command(false, true);
        assert!(disabled.contains("probe.ps1"));
        assert!(!disabled.contains(BENCH_MARK));
        assert!(!disabled.contains("3000000"));
        assert!(!disabled.contains("command -v python"));
        assert_eq!(mac_probe_args(false, true), vec!["python3", "-", "--skip-benchmark"]);
        assert_eq!(mac_ssh_command(false, true).matches("--skip-benchmark").count(), 2);
        assert!(!mac_ssh_command(true, true).contains("--skip-benchmark"));
    }

    #[test]
    fn network_false_skips_netsec_independently_of_benchmark() {
        assert_eq!(mac_probe_args(true, false), vec!["python3", "-", "nonet"]);
        assert_eq!(mac_probe_args(false, false), vec!["python3", "-", "--skip-benchmark", "nonet"]);
        assert_eq!(mac_ssh_command(false, false).matches(" --skip-benchmark nonet").count(), 2);
        assert!(!mac_ssh_command(true, true).contains("nonet"));
        assert!(windows_ssh_command(true, false).contains("probe.ps1)\" -NoNetwork;"));
        assert!(windows_ssh_command(true, false).contains(BENCH_MARK));
        assert!(!windows_ssh_command(false, true).contains("-NoNetwork"));
    }

    #[test]
    fn probes_are_embedded() {
        assert!(MAC_PROBE.contains("def "));
        assert_eq!(&WIN_PROBE[..3], &[0xEF, 0xBB, 0xBF], "PS 5.1 のため BOM 付き");
        assert_eq!(&WIN_LOGONS[..3], &[0xEF, 0xBB, 0xBF], "PS 5.1 のため BOM 付き");
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

    #[tokio::test]
    async fn run_keeps_output_when_a_grandchild_holds_the_pipes() {
        // 子はすぐ終わるが、孫が標準エラー（と標準入力）のパイプを数秒握り続ける
        #[cfg(windows)]
        let (cmd, args) = ("cmd.exe", vec!["/C".to_string(), "echo first& >&2 echo oops& start /b ping -n 9 127.0.0.1 >nul".to_string()]);
        #[cfg(not(windows))]
        let (cmd, args) = ("/bin/sh", vec!["-c".to_string(), "echo first; echo oops >&2; (sleep 5 &)".to_string()]);
        let started = Instant::now();
        let r = run(cmd, &args, None, Duration::from_secs(10)).await;
        // Windows の cmd は改行が CRLF なので、前後の空白を除いて比べる
        assert_eq!((r.code, r.out.trim(), r.err.trim()), (Some(0), "first", "oops"), "それまでに読んだ分は捨てない");
        assert!(started.elapsed() < Duration::from_secs(4), "孫の終わりを待たない: {:?}", started.elapsed());
    }
}
