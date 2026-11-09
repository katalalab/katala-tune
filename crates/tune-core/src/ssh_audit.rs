//! SSH の点検（docs/roadmap.md の「SSH」）: この機体の SSH クライアントから台帳の機体へ向けた設定・鍵・known_hosts を、
//! 読み取り専用で確かめる。`ssh -G` で解決後の設定、`ssh-keygen -l` で公開鍵の種類と長さ、`ssh-add -l` でエージェントの鍵、
//! `ssh-keygen -F` で known_hosts の登録を見る。秘密鍵は開かない（種類は隣の .pub から読む）。
//! 出力には接続先の名前・ユーザー名・鍵のパス・指紋・鍵のコメントを入れない。設定・鍵・known_hosts・DB を変えない。
//! probe のときだけ BatchMode で接続して所要時間と失敗の種類を測る（リモートでは exit するだけ）。
//! 保存はしない（`tune ssh` から呼ぶ）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use crate::collect::{self, BoundedEnvironment, RunResult};
use crate::db::now_ms;
use crate::nodes::{self, Node};

const LOCAL_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const OUTPUT_LIMIT: usize = 256 * 1024;
const OLD_KEY_DAYS: i64 = 3 * 365;
const RSA_MIN_BITS: u32 = 3072;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opts {
    /// BatchMode で実際に接続して測る
    pub probe: bool,
    /// `ssh -F`（テスト用。None なら利用者の ~/.ssh/config）
    pub config: Option<PathBuf>,
}

/// `ssh -G` の解決結果のうち、点検に使う項目
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    pub hostname: String,
    pub port: u16,
    pub host_key_alias: Option<String>,
    pub identity_files: Vec<String>,
    pub identities_only: bool,
    pub identity_agent: Option<String>,
    pub strict: String,
    pub known_hosts_files: Vec<String>,
    pub proxy: bool,
    pub forward_agent: bool,
}

/// `ssh -G` の出力（1 行に「小文字の項目名 値」）を読む。複数行の項目（identityfile）は順に集める
pub fn parse_resolved(text: &str) -> Resolved {
    let mut r = Resolved { port: 22, strict: "ask".into(), ..Resolved::default() };
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let Some((key, value)) = line.split_once(' ') else { continue };
        let value = value.trim();
        match key {
            "hostname" => r.hostname = value.into(),
            "port" => r.port = value.parse().unwrap_or(22),
            "hostkeyalias" => r.host_key_alias = Some(value.into()),
            "identityfile" => r.identity_files.push(value.into()),
            "identitiesonly" => r.identities_only = value == "yes",
            "identityagent" => r.identity_agent = Some(value.into()),
            "stricthostkeychecking" => r.strict = value.into(),
            "userknownhostsfile" | "globalknownhostsfile" => r.known_hosts_files.extend(value.split_whitespace().map(String::from)),
            "proxyjump" | "proxycommand" => r.proxy |= value != "none",
            "forwardagent" => r.forward_agent = value != "no",
            _ => {}
        }
    }
    r
}

/// `ssh-keygen -l` と `ssh-add -l` の 1 行（`256 SHA256:… コメント (ED25519)`）。指紋は照合にだけ使い、出力しない
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    pub kind: String,
    pub bits: u32,
    pub fingerprint: String,
}

pub fn parse_key_line(line: &str) -> Option<KeyInfo> {
    let mut words = line.split_whitespace();
    let bits = words.next()?.parse().ok()?;
    let fingerprint = words.next().filter(|f| f.contains(':'))?.to_string();
    let kind = words.last()?.strip_prefix('(')?.strip_suffix(')')?.to_string();
    Some(KeyInfo { kind, bits, fingerprint })
}

/// エージェントの鍵の一覧。reachable = false はつながらない・無効（IdentityAgent none）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agent {
    pub reachable: bool,
    pub keys: Vec<KeyInfo>,
}

/// `ssh-add -l` の結果を読む。終了 0 は一覧、1 は鍵が無い（つながっている）、それ以外はつながらない
pub fn parse_agent(code: Option<i32>, out: &str) -> Agent {
    match code {
        Some(0) => Agent { reachable: true, keys: out.lines().filter_map(parse_key_line).collect() },
        Some(1) => Agent { reachable: true, keys: Vec::new() },
        _ => Agent::default(),
    }
}

/// 鍵ファイル 1 つ。kind が None なら .pub が無い・読めない（秘密鍵は開かないので種類は分からない）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileKey {
    pub key: Option<KeyInfo>,
    pub age_days: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentUse {
    /// SSH_AUTH_SOCK（既定）
    Default,
    /// IdentityAgent で指定
    Configured,
    /// IdentityAgent none
    Disabled,
}

impl AgentUse {
    fn of(r: &Resolved) -> AgentUse {
        match r.identity_agent.as_deref() {
            None | Some("SSH_AUTH_SOCK") => AgentUse::Default,
            Some("none") => AgentUse::Disabled,
            Some(_) => AgentUse::Configured,
        }
    }
    fn label(self) -> &'static str {
        match self {
            AgentUse::Default => "default",
            AgentUse::Configured => "configured",
            AgentUse::Disabled => "none",
        }
    }
}

/// 1 台分の観測（所見の材料）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    pub resolved: Resolved,
    pub agent_use: AgentUse,
    pub agent: Agent,
    pub files: Vec<FileKey>,
    /// None は確かめられなかった（ssh-keygen を起動できない など）
    pub known_host: Option<bool>,
    /// probe のときだけ。Some(None) は成功、Some(Some(種類)) は失敗
    pub probe: Option<Option<&'static str>>,
}

fn finding(level: &str, code: &str, text: String) -> Value {
    json!({ "level": level, "code": code, "text": text })
}

/// 観測から所見を作る（純粋な関数。テストは合成の観測で行う）
pub fn analyze(o: &Observed) -> Vec<Value> {
    let mut out = Vec::new();
    let agent_keys: &[KeyInfo] = if o.agent_use == AgentUse::Disabled { &[] } else { &o.agent.keys };
    let file_keys: Vec<&KeyInfo> = o.files.iter().filter_map(|f| f.key.as_ref()).collect();
    // IdentitiesOnly では、鍵ファイルと一致するエージェントの鍵だけが使われる
    let usable_agent = agent_keys.iter().filter(|k| !o.resolved.identities_only || file_keys.iter().any(|f| f.fingerprint == k.fingerprint)).count();
    if o.files.is_empty() && usable_agent == 0 {
        out.push(finding("warn", "no_key", "使える鍵が無い（鍵ファイルもエージェントの鍵も見つからない）".into()));
    }
    if o.agent_use == AgentUse::Configured && !o.agent.reachable {
        out.push(finding("warn", "agent_unreachable", "IdentityAgent で指定したエージェントにつながらない".into()));
    }
    let mut seen = HashSet::new();
    for k in file_keys.iter().copied().chain(agent_keys.iter()) {
        if !seen.insert(&k.fingerprint) {
            continue;
        }
        match k.kind.as_str() {
            "DSA" => out.push(finding("crit", "dsa_key", "DSA の鍵（OpenSSH 10 では使えない）".into())),
            "RSA" if k.bits < RSA_MIN_BITS => out.push(finding("warn", "rsa_short", format!("RSA {} ビットの鍵（{RSA_MIN_BITS} ビット未満）", k.bits))),
            "ED25519" | "ED25519-SK" => {}
            other => out.push(finding("info", "not_ed25519", format!("{other} {} ビットの鍵（Ed25519 を推奨）", k.bits))),
        }
    }
    if let Some(days) = o.files.iter().filter_map(|f| f.age_days).max().filter(|d| *d > OLD_KEY_DAYS) {
        out.push(finding("info", "old_key", format!("{} 年以上前に作られた鍵ファイル", days / 365)));
    }
    match (o.known_host, o.resolved.strict.as_str()) {
        (Some(false), "false") => {}
        (Some(false), "accept-new") => out.push(finding("info", "unknown_host", "known_hosts に無い（初回の接続で自動登録される）".into())),
        (Some(false), _) => out.push(finding("warn", "unknown_host", "known_hosts に無い（BatchMode の接続はホスト鍵の確認で止まる）".into())),
        _ => {}
    }
    if o.resolved.strict == "false" {
        out.push(finding("warn", "strict_off", "StrictHostKeyChecking no（ホスト鍵を確かめない）".into()));
    }
    if o.resolved.forward_agent {
        out.push(finding("warn", "forward_agent", "ForwardAgent（接続先からエージェントの鍵を使える）".into()));
    }
    if let Some(Some(kind)) = o.probe {
        let level = if kind == "auth" { "crit" } else { "warn" };
        out.push(finding(level, &format!("probe_{kind}"), collect::reason_text(kind).into()));
    }
    out
}

/// 出力用の形（名前・パス・指紋・コメントを含めない）
pub fn public(o: &Observed, probe_ms: Option<u64>) -> Value {
    let in_agent = |k: &KeyInfo| o.agent.keys.iter().any(|a| a.fingerprint == k.fingerprint);
    let file_fps: Vec<&str> = o.files.iter().filter_map(|f| f.key.as_ref().map(|k| k.fingerprint.as_str())).collect();
    let mut keys: Vec<Value> = o
        .files
        .iter()
        .map(|f| {
            json!({
                "source": "file",
                "type": f.key.as_ref().map(|k| k.kind.clone()),
                "bits": f.key.as_ref().map(|k| k.bits),
                "age_days": f.age_days,
                "in_agent": f.key.as_ref().is_some_and(in_agent),
            })
        })
        .collect();
    if o.agent_use != AgentUse::Disabled {
        keys.extend(
            o.agent.keys.iter().filter(|k| !file_fps.contains(&k.fingerprint.as_str())).map(|k| json!({ "source": "agent", "type": k.kind, "bits": k.bits })),
        );
    }
    let mut v = json!({
        "config": {
            "port": o.resolved.port,
            "proxy": o.resolved.proxy,
            "identities_only": o.resolved.identities_only,
            "strict_host_key_checking": o.resolved.strict,
            "forward_agent": o.resolved.forward_agent,
            "agent": o.agent_use.label(),
        },
        "agent": { "reachable": o.agent.reachable, "keys": o.agent.keys.len() },
        "keys": keys,
        "known_host": o.known_host,
        "findings": analyze(o),
    });
    if let Some(result) = o.probe {
        v["probe"] = json!({ "ok": result.is_none(), "ms": probe_ms, "reason": result });
    }
    v
}

/// ssh に渡す名前として安全か（オプションと取り違えない・空白や制御文字を含まない）
fn safe_name(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn config_args(opts: &Opts) -> Vec<String> {
    opts.config.as_ref().map(|p| vec!["-F".to_string(), p.to_string_lossy().into_owned()]).unwrap_or_default()
}

async fn run_local(cmd: &str, args: &[String], env: &[(&str, &str)], remove: &[&str]) -> RunResult {
    let r = collect::run_bounded_with_env_clean(cmd, args, &[], LOCAL_TIMEOUT, OUTPUT_LIMIT, OUTPUT_LIMIT, BoundedEnvironment { set: env, remove }).await;
    RunResult { code: if r.timed_out || r.spawn_failed { None } else { r.code }, out: collect::decode(&r.stdout), err: collect::decode(&r.stderr) }
}

async fn key_of(pub_path: &Path) -> Option<KeyInfo> {
    if !pub_path.is_file() {
        return None;
    }
    let r = run_local("ssh-keygen", &["-l".into(), "-f".into(), pub_path.to_string_lossy().into_owned()], &[], &[]).await;
    if r.code == Some(0) { r.out.lines().find_map(parse_key_line) } else { None }
}

fn age_days(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(modified).map_or(0, |d| (d.as_secs() / 86_400) as i64))
}

async fn agent_for(r: &Resolved) -> Agent {
    let socket = match AgentUse::of(r) {
        AgentUse::Disabled => return Agent::default(),
        AgentUse::Default => None,
        AgentUse::Configured => {
            let raw = r.identity_agent.as_deref().unwrap_or_default();
            // `IdentityAgent $VAR` は環境変数の値を使う
            match raw.strip_prefix('$') {
                Some(var) => match std::env::var(var.trim_matches(|c| c == '{' || c == '}')) {
                    Ok(v) => Some(v),
                    Err(_) => return Agent::default(),
                },
                None => Some(nodes::expand(raw.trim_matches('"'))),
            }
        }
    };
    let r = match &socket {
        Some(s) => run_local("ssh-add", &["-l".into()], &[("SSH_AUTH_SOCK", s)], &[]).await,
        None => run_local("ssh-add", &["-l".into()], &[], &[]).await,
    };
    parse_agent(r.code, &r.out)
}

async fn known_host(r: &Resolved) -> Option<bool> {
    let name = r.host_key_alias.as_deref().unwrap_or(&r.hostname);
    if !safe_name(name) {
        return None;
    }
    let target = if r.port == 22 { name.to_string() } else { format!("[{name}]:{}", r.port) };
    let mut checked = false;
    for file in &r.known_hosts_files {
        let path = PathBuf::from(nodes::expand(file));
        if !path.is_file() {
            continue;
        }
        let res = run_local("ssh-keygen", &["-F".into(), target.clone(), "-f".into(), path.to_string_lossy().into_owned()], &[], &[]).await;
        match res.code {
            Some(0) => return Some(true),
            Some(_) => checked = true,
            None => {}
        }
    }
    // known_hosts が 1 つも無いのは「登録が無い」と同じ
    if checked || r.known_hosts_files.iter().all(|f| !Path::new(&nodes::expand(f)).is_file()) { Some(false) } else { None }
}

async fn probe(alias: &str, opts: &Opts) -> (Option<&'static str>, u64) {
    let mut args: Vec<String> = collect::SSH_OPTS.iter().map(|s| s.to_string()).collect();
    args.extend(config_args(opts));
    args.push(alias.into());
    args.push("exit 0".into());
    let t = Instant::now();
    let r = collect::run("ssh", &args, None, PROBE_TIMEOUT).await;
    let ms = t.elapsed().as_millis() as u64;
    if r.code == Some(0) { (None, ms) } else { (Some(collect::classify_failure(&r)), ms) }
}

/// 1 台を点検する。戻り値は `{ node_id, ok, ... | error }`
pub async fn audit_node(node: &Node, opts: &Opts) -> Value {
    if !safe_name(&node.alias) {
        return json!({ "node_id": node.id, "ok": false, "error": "台帳の alias が SSH の Host 名として使えない" });
    }
    let mut args = config_args(opts);
    args.extend(["-G".to_string(), node.alias.clone()]);
    let res = run_local("ssh", &args, &[], &[]).await;
    if res.code != Some(0) {
        return json!({ "node_id": node.id, "ok": false, "error": "ssh -G で設定を解決できない" });
    }
    let resolved = parse_resolved(&res.out);
    let mut files = Vec::new();
    for f in &resolved.identity_files {
        let path = PathBuf::from(nodes::expand(f));
        if !path.is_file() {
            continue;
        }
        let mut pub_path = path.clone().into_os_string();
        pub_path.push(".pub");
        files.push(FileKey { key: key_of(Path::new(&pub_path)).await, age_days: age_days(&path) });
    }
    let (agent, known) = tokio::join!(agent_for(&resolved), known_host(&resolved));
    let (probe_result, probe_ms) = if opts.probe {
        let (r, ms) = probe(&node.alias, opts).await;
        (Some(r), Some(ms))
    } else {
        (None, None)
    };
    let observed = Observed { agent_use: AgentUse::of(&resolved), resolved, agent, files, known_host: known, probe: probe_result };
    let mut v = public(&observed, probe_ms);
    v["node_id"] = json!(node.id);
    v["ok"] = json!(true);
    v
}

/// 台帳の対象（この機体を除く）を並べて点検する
pub async fn audit_all(nodes: &[Node], opts: &Opts) -> Value {
    let (local, remote): (Vec<&Node>, Vec<&Node>) = nodes.iter().partition(|n| n.local);
    let mut set = tokio::task::JoinSet::new();
    for (i, n) in remote.iter().enumerate() {
        let (n, opts) = ((*n).clone(), opts.clone());
        set.spawn(async move { (i, audit_node(&n, &opts).await) });
    }
    let mut out: Vec<(usize, Value)> = Vec::new();
    while let Some(r) = set.join_next().await {
        if let Ok(x) = r {
            out.push(x);
        }
    }
    out.sort_by_key(|(i, _)| *i);
    json!({
        "at": now_ms(),
        "probe": opts.probe,
        "nodes": out.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
        "local_nodes": local.iter().map(|n| &n.id).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(kind: &str, bits: u32, fp: &str) -> KeyInfo {
        KeyInfo { kind: kind.into(), bits, fingerprint: fp.into() }
    }

    fn observed() -> Observed {
        Observed {
            resolved: Resolved { hostname: "h".into(), port: 22, strict: "ask".into(), ..Resolved::default() },
            agent_use: AgentUse::Default,
            agent: Agent { reachable: true, keys: vec![key("ED25519", 256, "SHA256:a")] },
            files: vec![FileKey { key: Some(key("ED25519", 256, "SHA256:a")), age_days: Some(10) }],
            known_host: Some(true),
            probe: None,
        }
    }

    fn codes(o: &Observed) -> Vec<String> {
        analyze(o).iter().map(|f| f["code"].as_str().unwrap_or_default().to_string()).collect()
    }

    #[test]
    fn parses_ssh_g_output() {
        let text = "user someone\r\nhostname 192.0.2.10\nport 2222\nidentityfile ~/.ssh/id_ed25519\nidentityfile ~/.ssh/id_rsa\n\
                    identitiesonly yes\nidentityagent ~/agent.sock\nstricthostkeychecking accept-new\n\
                    userknownhostsfile ~/.ssh/known_hosts ~/.ssh/known_hosts2\nglobalknownhostsfile /etc/ssh/ssh_known_hosts\n\
                    proxyjump bastion\nforwardagent no\nhostkeyalias alias-key\n";
        let r = parse_resolved(text);
        assert_eq!(r.hostname, "192.0.2.10");
        assert_eq!(r.port, 2222);
        assert_eq!(r.identity_files, ["~/.ssh/id_ed25519", "~/.ssh/id_rsa"]);
        assert!(r.identities_only && r.proxy && !r.forward_agent);
        assert_eq!(r.identity_agent.as_deref(), Some("~/agent.sock"));
        assert_eq!(r.strict, "accept-new");
        assert_eq!(r.known_hosts_files, ["~/.ssh/known_hosts", "~/.ssh/known_hosts2", "/etc/ssh/ssh_known_hosts"]);
        assert_eq!(r.host_key_alias.as_deref(), Some("alias-key"));
        let defaults = parse_resolved("hostname h\nproxycommand none\nforwardagent yes\n");
        assert_eq!((defaults.port, defaults.strict.as_str(), defaults.proxy, defaults.forward_agent), (22, "ask", false, true));
    }

    #[test]
    fn parses_key_lines_and_agent_states() {
        assert_eq!(parse_key_line("256 SHA256:abc comment with spaces (ED25519)"), Some(key("ED25519", 256, "SHA256:abc")));
        assert_eq!(parse_key_line("2048 SHA256:def no comment (RSA)"), Some(key("RSA", 2048, "SHA256:def")));
        assert_eq!(parse_key_line("The agent has no identities."), None);
        assert_eq!(parse_key_line("256 notafingerprint x (ED25519)"), None);
        let a = parse_agent(Some(0), "256 SHA256:a c (ED25519)\n3072 SHA256:b c (RSA)\n");
        assert!(a.reachable);
        assert_eq!(a.keys.len(), 2);
        assert_eq!(parse_agent(Some(1), "The agent has no identities.\n"), Agent { reachable: true, keys: vec![] });
        assert_eq!(parse_agent(Some(2), ""), Agent::default());
        assert_eq!(parse_agent(None, ""), Agent::default());
    }

    #[test]
    fn healthy_setup_has_no_findings() {
        assert!(analyze(&observed()).is_empty());
    }

    #[test]
    fn key_findings() {
        let mut o = observed();
        o.files = vec![
            FileKey { key: Some(key("RSA", 2048, "SHA256:r")), age_days: Some(4 * 365) },
            FileKey { key: Some(key("DSA", 1024, "SHA256:d")), age_days: None },
            FileKey { key: Some(key("ECDSA", 256, "SHA256:e")), age_days: Some(1) },
        ];
        // 同じ鍵がエージェントにもあっても 1 回だけ数える
        o.agent.keys.push(key("RSA", 2048, "SHA256:r"));
        assert_eq!(codes(&o), ["rsa_short", "dsa_key", "not_ed25519", "old_key"].map(String::from));
        let mut long = observed();
        long.files = vec![FileKey { key: Some(key("RSA", 4096, "SHA256:r")), age_days: Some(1) }];
        long.agent.keys.clear();
        assert_eq!(codes(&long), ["not_ed25519"]);
    }

    #[test]
    fn missing_keys_respect_identities_only_and_disabled_agent() {
        let mut o = observed();
        o.files.clear();
        assert!(codes(&o).is_empty(), "エージェントの鍵が使える");
        o.resolved.identities_only = true;
        assert_eq!(codes(&o), ["no_key"], "IdentitiesOnly ではファイルに無いエージェントの鍵は使われない");
        o.resolved.identities_only = false;
        o.agent_use = AgentUse::Disabled;
        assert_eq!(codes(&o), ["no_key"]);
        let mut unreachable = observed();
        unreachable.agent_use = AgentUse::Configured;
        unreachable.agent = Agent::default();
        assert_eq!(codes(&unreachable), ["agent_unreachable"]);
        // .pub が無い鍵ファイルも鍵として数える（種類は分からない）
        let mut no_pub = observed();
        no_pub.agent = Agent { reachable: true, keys: vec![] };
        no_pub.files = vec![FileKey { key: None, age_days: Some(1) }];
        assert!(codes(&no_pub).is_empty());
    }

    #[test]
    fn host_key_findings() {
        let mut o = observed();
        o.known_host = Some(false);
        assert_eq!(codes(&o), ["unknown_host"]);
        assert_eq!(analyze(&o)[0]["level"], "warn");
        o.resolved.strict = "accept-new".into();
        assert_eq!(analyze(&o)[0]["level"], "info");
        o.resolved.strict = "false".into();
        assert_eq!(codes(&o), ["strict_off"]);
        o.known_host = None;
        o.resolved.strict = "ask".into();
        assert!(codes(&o).is_empty(), "確かめられなかったときは所見にしない");
        o.resolved.forward_agent = true;
        assert_eq!(codes(&o), ["forward_agent"]);
    }

    #[test]
    fn probe_findings() {
        let mut o = observed();
        o.probe = Some(None);
        assert!(codes(&o).is_empty());
        o.probe = Some(Some("auth"));
        let f = analyze(&o);
        assert_eq!((f[0]["code"].as_str(), f[0]["level"].as_str()), (Some("probe_auth"), Some("crit")));
        o.probe = Some(Some("unreachable"));
        assert_eq!(analyze(&o)[0]["level"], "warn");
    }

    #[test]
    fn public_output_has_no_names_paths_or_fingerprints() {
        let mut o = observed();
        o.resolved.hostname = "secret-host.example".into();
        o.resolved.identity_files = vec!["~/.ssh/id_secret".into()];
        o.agent.keys.push(key("RSA", 4096, "SHA256:agentonly"));
        o.probe = Some(Some("timeout"));
        let v = public(&o, Some(1234));
        let text = v.to_string();
        for leaked in ["secret-host", "id_secret", "SHA256:"] {
            assert!(!text.contains(leaked), "{leaked} が出力に含まれる: {text}");
        }
        assert_eq!(v["keys"].as_array().map(Vec::len), Some(2));
        assert_eq!(v["keys"][0]["in_agent"], true);
        assert_eq!(v["keys"][1]["source"], "agent");
        assert_eq!(v["probe"], json!({ "ok": false, "ms": 1234, "reason": "timeout" }));
    }

    #[test]
    fn unsafe_names_are_refused() {
        assert!(safe_name("my-host"));
        for bad in ["", "-oProxyCommand=x", "a b", "a\nb"] {
            assert!(!safe_name(bad), "{bad:?}");
        }
    }
}
