//! 所見を AI エージェント（Claude Code・Codex の CLI）へ渡す。
//! プロンプトは画面の文字列を使わず、DB の最新の分析結果から所見を読み直して作る。
//! CLI は操作卓（このアプリを動かしている機体）の新しいターミナルで、読み取り寄りの権限（Claude は plan、Codex は read-only）で対話として開く。
//! 自動で直す（headless の）実行はしない。変更は、操作者がターミナルで AI とやり取りして承認したものだけになる。
//! 共用機・運用対象でない機体の所見は渡さない（外部の AI に機体の情報が届くため）。

use std::path::Path;

use serde_json::{Value, json};

use crate::db::{now_ms, sha256_hex};
use crate::engine::Engine;
use crate::js;
use crate::nodes::Node;

/// 渡す先
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

/// CLI に最初に渡す一文。プロンプト本体は同じフォルダの PROMPT.md に置く（引数の引用符・改行・文字コードの問題を避ける）
const KICKOFF: &str = "Read PROMPT.md in this folder and follow it.";
pub const PROMPT_FILE: &str = "PROMPT.md";

impl Agent {
    pub fn parse(s: &str) -> Option<Agent> {
        match s {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
        }
    }

    /// プロンプトが届く先（確認ダイアログ用）
    pub fn vendor(self) -> &'static str {
        match self {
            Agent::Claude => "Anthropic",
            Agent::Codex => "OpenAI",
        }
    }

    /// 開くときの権限の説明（確認ダイアログ用）
    pub fn mode(self) -> &'static str {
        match self {
            Agent::Claude => "plan モード（読み取りと計画だけ。変更は承認してから）",
            Agent::Codex => "read-only の sandbox（コマンドの実行は承認してから）",
        }
    }

    /// CLI の引数（実行ファイル名を除く）。固定の値だけで、所見の文字列は入れない
    pub fn args(self) -> &'static [&'static str] {
        match self {
            Agent::Claude => &["--permission-mode", "plan", KICKOFF],
            Agent::Codex => &["--sandbox", "read-only", "--ask-for-approval", "on-request", KICKOFF],
        }
    }

    fn command_line(self, quote: fn(&str) -> String) -> String {
        let args = self.args().iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
        format!("{} {args}", self.key())
    }
}

/// 最新の分析結果（`Engine::last` の形）から機体の所見を探す。分析に失敗している・所見が無いときは理由を返す
pub fn find(last: &[Value], node_id: &str, finding_id: &str) -> Result<(Value, Value), String> {
    let r = last.iter().find(|r| r.get("node_id").and_then(Value::as_str) == Some(node_id)).ok_or_else(|| format!("{node_id} の分析結果が無い"))?;
    if r.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(format!("{node_id} の最新の分析は失敗している"));
    }
    let f = r
        .get("findings")
        .and_then(Value::as_array)
        .and_then(|fs| fs.iter().find(|f| f.get("id").and_then(Value::as_str) == Some(finding_id)))
        .ok_or_else(|| "この所見は最新の分析に無い（分析し直して消えた可能性がある）".to_string())?;
    Ok((r.clone(), f.clone()))
}

/// 渡してよい機体か。共用機と運用対象でない機体は断る
pub fn refusal(node: &Node) -> Option<String> {
    if node.shared {
        return Some(format!("{} は共用機なので、所見を外部の AI に渡さない", node.id));
    }
    node.refusal()
}

fn severity_label(s: &str) -> &str {
    match s {
        "critical" => "重大",
        "warn" => "注意",
        "info" => "提案",
        other => other,
    }
}

/// プロンプト本文（Markdown）。機体の情報と所見と、進め方の約束
pub fn prompt(node: &Node, protect: &[String], result: &Value, finding: &Value, now: i64) -> String {
    let s = |k: &str| js::string(finding.get(k));
    let fid = s("id");
    let mut out = String::from("# Katala Tune からの調査依頼\n\n");
    out += "Katala Tune（手元の機体を分析するデスクトップアプリ）が、次の所見を出しました。原因を調べて、対処案を出してください。\n\n";

    out += "## 対象の機体\n\n";
    out += &format!("- 機体: {}（OS: {}）\n", node.id, node.os);
    if node.local {
        out += "- 接続: このターミナルを開いた機体そのもの\n";
    } else {
        out += &format!("- 接続: `ssh {}` で入れます（~/.ssh/config の Host 名）\n", node.alias);
        if node.is_windows() {
            out += "- Windows の ssh 既定シェルが Git Bash の場合があります。PowerShell は `powershell -NoProfile -Command ...` で呼んでください\n";
        }
    }
    if let Some(at) = result.get("at").and_then(Value::as_i64) {
        let min = ((now - at).max(0) as f64 / 60_000.0).round() as i64;
        out += &format!("- 観測: 約 {min} 分前の分析\n");
    }
    out += "\n";

    out += "## 所見\n\n";
    out += &format!("- 見出し: {}\n", s("title"));
    out += &format!("- 重要度: {}\n", severity_label(&s("severity")));
    if !s("category").is_empty() {
        out += &format!("- 分類: {}\n", s("category"));
    }
    out += &format!("- 所見 ID: `{fid}`\n\n");
    if !s("detail").is_empty() {
        out += &format!("### 詳細\n\n{}\n\n", s("detail"));
    }
    if !s("advice").is_empty() {
        out += &format!("### アプリの提案\n\n{}\n\n", s("advice"));
    }
    let cmds: Vec<String> = finding.get("commands").and_then(Value::as_array).map(|a| a.iter().map(|c| js::string(Some(c))).collect()).unwrap_or_default();
    if !cmds.is_empty() {
        out += "### アプリが示したコマンド（未実行）\n\n```\n";
        out += &cmds.join("\n");
        out += "\n```\n\n";
    }

    out += "## 進め方\n\n";
    out += "1. 所見は分析した時点のものです。まず読み取りだけで、今もそうなっているかを確かめてください。\n";
    out += "2. 原因の仮説と根拠、対処案（期待する効果・リスク・元に戻す方法）を示してください。\n";
    out += "3. 設定・プロセス・ファイルを変える操作は、内容を示して私の承認を得てから実行してください。\n";
    if !protect.is_empty() {
        out += &format!("4. 次のプロセスは保護対象です。終了・変更を提案しないでください: {}\n", protect.join(", "));
    }
    out += "\n終わったら、Katala Tune で「この機体を分析し直す」を押して、所見が消えたかを確かめるよう伝えてください。\n\n";
    out += &format!("<!-- katala-tune handoff: {}/{fid} -->\n", node.id);
    out
}

/// ファイル名に使える形（英数・`-`・`_` だけ。ほかは `_`）
fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(48).collect()
}

/// 起動用のスクリプト（ファイル名と中身）。Windows は cmd、macOS は Terminal で開く .command
pub fn launcher(agent: Agent, windows: bool) -> (&'static str, String) {
    let bin = agent.key();
    let missing = format!("{bin} command not found. Install {} or use the copy button in Katala Tune instead.", agent.label());
    if windows {
        let line = agent.command_line(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.to_string() });
        ("launch.cmd", format!("@echo off\r\ncd /d \"%~dp0\"\r\nwhere {bin} >nul 2>nul || (echo {missing} & exit /b 1)\r\n{line}\r\n"))
    } else {
        let line = agent.command_line(|a| if a.contains(' ') { format!("'{a}'") } else { a.to_string() });
        (
            "launch.command",
            format!(
                "#!/bin/zsh -l\ncd \"$(dirname \"$0\")\" || exit 1\nif ! command -v {bin} >/dev/null 2>&1; then echo \"{missing}\"; exit 1; fi\nexec {line}\n"
            ),
        )
    }
}

/// 新しいターミナルで起動スクリプトを開くコマンド（実行ファイルと引数）
pub fn terminal(windows: bool, dir: &Path, script: &str) -> (String, Vec<String>) {
    if windows {
        // 作業パスは shell の文字列へ入れず、current_dir で渡す。
        // start に新しいコンソールの標準入出力を用意させ、GUI の null / ログを CLI に継がない。
        let args = ["/D", "/V:OFF", "/C", "start", "Katala Tune AI", "cmd.exe", "/D", "/V:OFF", "/K", script].map(str::to_string).to_vec();
        ("cmd.exe".into(), args)
    } else {
        ("open".into(), vec!["-a".into(), "Terminal".into(), dir.join(script).to_string_lossy().into_owned()])
    }
}

/// 渡す準備ができた所見（確認ダイアログの前に作る）
pub struct Prepared {
    pub node: Node,
    pub finding_id: String,
    pub title: String,
    pub prompt: String,
}

impl Engine {
    /// 台帳を読み直し、最新の分析結果から所見を探してプロンプトを作る
    pub fn handoff_prepare(&self, node_id: &str, finding_id: &str) -> Result<Prepared, String> {
        let cfg = self.reload_config();
        let node = cfg.node(node_id).cloned().ok_or("台帳に無い機体")?;
        if let Some(r) = refusal(&node) {
            return Err(r);
        }
        let last = self.last()?;
        let (result, finding) = find(&last, node_id, finding_id)?;
        let prompt = prompt(&node, &cfg.protect, &result, &finding, now_ms());
        Ok(Prepared { node, finding_id: finding_id.to_string(), title: js::string(finding.get("title")), prompt })
    }

    /// 確認中の取消も、承認されたプロンプトと同じ識別情報で記録する。
    pub fn handoff_cancel(&self, p: &Prepared, agent: Agent) -> Value {
        self.handoff_record(p, agent, false, "取消（開いていない）".into(), None);
        json!({ "ok": false, "cancelled": true })
    }

    fn handoff_record(&self, p: &Prepared, agent: Agent, ok: bool, output: String, dir: Option<&Path>) {
        let entry = json!({
            "id": format!("{}-handoff-{}", now_ms(), &sha256_hex(output.as_bytes())[..6]), "at": now_ms(), "node_id": p.node.id,
            "type": "ai-handoff",
            "params": { "agent": agent.key(), "finding_id": p.finding_id, "prompt_sha256": sha256_hex(p.prompt.as_bytes()), "dir": dir.map(|d| d.to_string_lossy()) },
            "label": format!("{} に渡す（{}）", agent.label(), p.title), "ok": ok, "output": output, "undo": null, "undo_of": null,
        });
        let _ = self.with_db(|d| d.add_action(&entry));
    }

    /// 承認の後: 台帳をもう一度読み、接続先が変わっていなければ PROMPT.md と起動スクリプトを書いてターミナルを開く。
    /// 結果（開いた・開けなかった・中止）は実行記録に残す
    pub fn handoff_launch(&self, p: &Prepared, agent: Agent) -> Value {
        self.handoff_launch_with(p, agent, open)
    }

    fn handoff_launch_with(&self, p: &Prepared, agent: Agent, launch: impl FnOnce(&Path, &str, Agent) -> Result<(), String>) -> Value {
        let record = |ok, output, dir| self.handoff_record(p, agent, ok, output, dir);
        let refused = |m: String| {
            record(false, format!("中止（開いていない）: {m}"), None);
            json!({ "ok": false, "refused": m })
        };
        let cfg = self.reload_config();
        let Some(node) = cfg.node(&p.node.id) else { return refused("台帳に無い機体".into()) };
        if let Some(r) = refusal(node) {
            return refused(r);
        }
        if (&node.alias, &node.os, node.local) != (&p.node.alias, &p.node.os, p.node.local) {
            return refused("確認のあいだに台帳の接続先（alias・OS・この機体かどうか）が変わった。もう一度押してください".into());
        }
        let dir = self.data_dir().join("handoff").join(format!("{}-{}-{}", now_ms(), safe(&p.node.id), agent.key()));
        match launch(&dir, &p.prompt, agent) {
            Ok(()) => {
                record(true, format!("{} を新しいターミナルで開いた（{}）", agent.label(), dir.display()), Some(&dir));
                json!({ "ok": true, "dir": dir })
            }
            Err(e) => {
                record(false, format!("開けなかった: {e}"), Some(&dir));
                json!({ "ok": false, "refused": format!("開けなかった: {e}") })
            }
        }
    }
}

fn open(dir: &Path, prompt: &str, agent: Agent) -> Result<(), String> {
    let windows = cfg!(windows);
    std::fs::create_dir_all(dir).map_err(|e| format!("フォルダを作れない: {e}"))?;
    std::fs::write(dir.join(PROMPT_FILE), prompt).map_err(|e| format!("{PROMPT_FILE} を書けない: {e}"))?;
    let (script, body) = launcher(agent, windows);
    let path = dir.join(script);
    std::fs::write(&path, body).map_err(|e| format!("{script} を書けない: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("{script} を実行可能にできない: {e}"))?;
    }
    open_terminal(dir, script)
}

fn open_terminal(dir: &Path, script: &str) -> Result<(), String> {
    let (bin, args) = terminal(cfg!(windows), dir, script);
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // 外側は start を呼ぶだけ。パスを引数にせず OS の作業ディレクトリで指定する。
        cmd.current_dir(dir).creation_flags(0x0800_0000);
    }
    let status = cmd.status().map_err(|e| format!("ターミナルを開けない: {e}"))?;
    if !status.success() {
        return Err(format!("ターミナルを開けない（{status}）"));
    }
    Ok(())
}

#[cfg(test)]
mod regression;

#[cfg(test)]
mod tests {
    use super::*;

    fn node(v: Value) -> Node {
        Node::from_value(&v, "this-host")
    }

    fn last() -> Vec<Value> {
        vec![
            json!({ "node_id": "a", "ok": true, "at": 1_000_000, "findings": [
                { "id": "mem-high", "severity": "warn", "category": "memory", "title": "メモリが逼迫", "detail": "空き 3%", "advice": "大口を確かめる", "commands": ["Get-Process | Sort WS"] }
            ] }),
            json!({ "node_id": "b", "ok": false, "error": "timeout" }),
        ]
    }

    #[test]
    fn finds_only_current_findings() {
        let (r, f) = find(&last(), "a", "mem-high").unwrap();
        assert_eq!(r["node_id"], "a");
        assert_eq!(f["title"], "メモリが逼迫");
        assert!(find(&last(), "a", "gone").unwrap_err().contains("最新の分析に無い"));
        assert!(find(&last(), "b", "x").unwrap_err().contains("失敗"));
        assert!(find(&last(), "z", "x").unwrap_err().contains("分析結果が無い"));
    }

    #[test]
    fn refuses_shared_and_inactive_nodes() {
        assert!(refusal(&node(json!({ "id": "a", "alias": "a", "os": "windows" }))).is_none());
        assert!(refusal(&node(json!({ "id": "f", "alias": "f", "os": "windows", "shared": true }))).unwrap().contains("共用機"));
        assert!(refusal(&node(json!({ "id": "p", "alias": "p", "os": "macos", "status": "paused" }))).is_some());
    }

    #[test]
    fn prompt_carries_finding_route_and_rules() {
        let n = node(json!({ "id": "a", "alias": "gpu-a", "os": "windows" }));
        let (r, f) = find(&last(), "a", "mem-high").unwrap();
        let p = prompt(&n, &["python.exe".into()], &r, &f, 1_000_000 + 5 * 60_000);
        for want in [
            "`ssh gpu-a`",
            "OS: windows",
            "約 5 分前",
            "メモリが逼迫",
            "重要度: 注意",
            "空き 3%",
            "大口を確かめる",
            "Get-Process | Sort WS",
            "承認を得てから",
            "python.exe",
            "katala-tune handoff: a/mem-high",
        ] {
            assert!(p.contains(want), "{want} が無い:\n{p}");
        }
        let local = node(json!({ "id": "m", "alias": "m", "os": "macos", "local_hostname": "this-host" }));
        let p = prompt(&local, &[], &r, &f, 1_000_000);
        assert!(p.contains("ターミナルを開いた機体そのもの") && !p.contains("ssh") && !p.contains("保護対象"));
    }

    #[test]
    fn launchers_use_read_only_modes_and_fixed_args() {
        let (name, body) = launcher(Agent::Claude, true);
        assert_eq!(name, "launch.cmd");
        assert!(body.contains("claude --permission-mode plan \"Read PROMPT.md in this folder and follow it.\""), "{body}");
        let (name, body) = launcher(Agent::Codex, false);
        assert_eq!(name, "launch.command");
        assert!(body.starts_with("#!/bin/zsh -l\n"));
        assert!(body.contains("exec codex --sandbox read-only --ask-for-approval on-request 'Read PROMPT.md in this folder and follow it.'"), "{body}");
        for agent in [Agent::Claude, Agent::Codex] {
            for w in [true, false] {
                let (_, b) = launcher(agent, w);
                assert!(!b.contains("dangerously") && !b.contains("bypass") && !b.contains("full-access"));
            }
        }
    }

    #[test]
    fn terminal_opens_a_new_window_in_the_handoff_dir() {
        let dir = Path::new("D:/data/handoff/1-a-claude");
        let (bin, args) = terminal(true, dir, "launch.cmd");
        assert_eq!(bin, "cmd.exe");
        assert_eq!(args, ["/D", "/V:OFF", "/C", "start", "Katala Tune AI", "cmd.exe", "/D", "/V:OFF", "/K", "launch.cmd"]);
        let (bin, args) = terminal(false, dir, "launch.command");
        assert_eq!((bin.as_str(), args[1].as_str()), ("open", "Terminal"));
        assert!(args[2].ends_with("launch.command"));
    }

    #[test]
    fn agent_names_are_a_closed_set() {
        assert_eq!(Agent::parse("claude"), Some(Agent::Claude));
        assert_eq!(Agent::parse("codex"), Some(Agent::Codex));
        assert_eq!(Agent::parse("bash"), None);
        assert_eq!(safe("gpu pc/../x"), "gpu_pc____x");
    }

    #[test]
    fn cancellation_is_audited_without_creating_a_launcher_and_changed_routes_are_refused() {
        use crate::engine::NoHost;
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("kt-handoff-cancel-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("nodes.json");
        std::fs::write(&config, r#"{"nodes":[{"id":"a","alias":"a","os":"macos"}]}"#).unwrap();
        let e = Engine::open(config.clone(), dir.join("data"), Arc::new(NoHost)).unwrap();
        let p = Prepared {
            node: node(json!({"id":"a","alias":"a","os":"macos"})),
            finding_id: "cpu-busy".into(),
            title: "CPU".into(),
            prompt: "Reviewed text".into(),
        };
        assert_eq!(e.handoff_cancel(&p, Agent::Claude)["cancelled"], true);
        let actions = e.with_db(|db| db.actions(10)).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["ok"], false);
        assert_eq!(actions[0]["params"]["agent"], "claude");
        assert_eq!(actions[0]["params"]["finding_id"], "cpu-busy");
        assert_eq!(actions[0]["params"]["prompt_sha256"], sha256_hex(b"Reviewed text"));
        assert!(actions[0]["params"]["dir"].is_null());
        assert!(!dir.join("data/handoff").exists());
        for replacement in [
            r#"{"nodes":[{"id":"a","alias":"changed","os":"macos"}]}"#,
            r#"{"nodes":[{"id":"a","alias":"a","os":"macos","shared":true}]}"#,
            r#"{"nodes":[{"id":"a","alias":"a","os":"macos","status":"paused"}]}"#,
            r#"{"nodes":[]}"#,
        ] {
            std::fs::write(&config, replacement).unwrap();
            let result = e.handoff_launch(&p, Agent::Codex);
            assert_eq!(result["ok"], false, "{result}");
            assert!(result["refused"].is_string());
            assert!(!dir.join("data/handoff").exists());
        }
        drop(e);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_terminal_keeps_console_stdio_and_literal_working_paths() {
        for suffix in ["simple", "space & 日本語 %KT_QA_UNSET% !"] {
            let dir = std::env::temp_dir().join(format!("kt-handoff-{}-{}-{suffix}", std::process::id(), now_ms()));
            let dir = std::path::PathBuf::from(dir.to_string_lossy().replace('\\', "/"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("console-stdio-fixture"), "").unwrap();
            let exe = std::env::current_exe().unwrap().to_string_lossy().replace('%', "%%");
            std::fs::write(
                dir.join("launch.cmd"),
                format!("@echo off\r\necho ran>launcher-ran.txt\r\n\"{exe}\" --exact handoff::tests::windows_console_stdio_child --nocapture\r\nexit\r\n"),
            )
            .unwrap();
            open_terminal(&dir, "launch.cmd").unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !dir.join("receipt.txt").exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            assert_eq!(std::fs::read_to_string(dir.join("receipt.txt")).unwrap(), "launched\r\n");
            // receipt は子の test が書く。cmd が batch のハンドルを閉じるまで待つ。
            loop {
                match std::fs::remove_dir_all(&dir) {
                    Ok(()) => break,
                    Err(_) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(50)),
                    Err(e) => panic!("fixture cleanup: {e}"),
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_console_stdio_child() {
        use std::io::IsTerminal;
        if !Path::new("console-stdio-fixture").exists() {
            return;
        }
        std::fs::write(
            "stdio.json",
            json!({"stdin":std::io::stdin().is_terminal(), "stdout":std::io::stdout().is_terminal(), "stderr":std::io::stderr().is_terminal()}).to_string(),
        )
        .unwrap();
        assert!(std::io::stdin().is_terminal());
        assert!(std::io::stdout().is_terminal());
        assert!(std::io::stderr().is_terminal());
        std::fs::write("receipt.txt", "launched\r\n").unwrap();
    }
}
