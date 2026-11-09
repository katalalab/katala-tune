//! `tune ssh` の点検を、本物の ssh・ssh-keygen で通しで確かめる。利用者の ~/.ssh/config・鍵・エージェントは使わない
//! （設定は `-F` で一時ディレクトリのものを渡し、IdentityAgent none でエージェントを切る）。接続先は 127.0.0.1 の閉じたポート。
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tune_core::nodes::Node;
use tune_core::ssh_audit::{Opts, audit_all};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn have(cmd: &str) -> bool {
    Command::new(cmd).arg("-V").output().is_ok() || Command::new("which").arg(cmd).output().is_ok_and(|o| o.status.success())
}

fn keygen(dir: &Path, name: &str, kind: &str, bits: &str) -> PathBuf {
    let path = dir.join(name);
    let ok = Command::new("ssh-keygen").args(["-q", "-t", kind, "-b", bits, "-N", "", "-C", "kt-test-comment", "-f"]).arg(&path).status().expect("ssh-keygen");
    assert!(ok.success());
    path
}

fn node(id: &str) -> Node {
    Node::from_value(&json!({ "id": id, "alias": id, "os": "macos" }), "kt-not-this-host")
}

fn codes(v: &Value) -> Vec<&str> {
    v["findings"].as_array().map(|a| a.iter().filter_map(|f| f["code"].as_str()).collect()).unwrap_or_default()
}

#[tokio::test]
async fn audits_config_keys_and_known_hosts_with_real_openssh() {
    if !have("ssh") || !have("ssh-keygen") {
        eprintln!("ssh / ssh-keygen が無いので省略");
        return;
    }
    let dir = std::env::temp_dir().join(format!("kt-ssh-audit-{}-{}", std::process::id(), tune_core::db::now_ms()));
    std::fs::create_dir_all(&dir).expect("scratch");
    let _cleanup = Scratch(dir.clone());
    let good = keygen(&dir, "id_good", "ed25519", "256");
    let weak = keygen(&dir, "id_weak", "rsa", "2048");
    let public_key = std::fs::read_to_string(good.with_extension("pub")).expect("pub");
    let known = dir.join("known_hosts");
    // 公開鍵の中身は何でもよい（登録の有無だけを見る）
    std::fs::write(&known, format!("[127.0.0.1]:1 {}", public_key.split_whitespace().take(2).collect::<Vec<_>>().join(" "))).expect("known_hosts");
    let common = format!(
        "  HostName 127.0.0.1\n  IdentitiesOnly yes\n  IdentityAgent none\n  UserKnownHostsFile \"{}\"\n  GlobalKnownHostsFile \"{}\"\n  StrictHostKeyChecking yes\n  ConnectTimeout 3\n",
        known.display(),
        dir.join("no-global").display()
    );
    let config = dir.join("config");
    std::fs::write(
        &config,
        format!(
            "Host kt-good\n  Port 1\n  IdentityFile \"{}\"\n{common}\nHost kt-weak\n  Port 9\n  IdentityFile \"{}\"\n  ForwardAgent yes\n{common}\nHost kt-nokey\n  Port 1\n  IdentityFile \"{}\"\n{common}",
            good.display(),
            weak.display(),
            dir.join("missing").display()
        ),
    )
    .expect("config");

    let nodes = [node("kt-good"), node("kt-weak"), node("kt-nokey"), node("-oProxyCommand=x")];
    let report = audit_all(&nodes, &Opts { probe: true, config: Some(config) }).await;
    let by_id = |id: &str| report["nodes"].as_array().and_then(|a| a.iter().find(|n| n["node_id"] == id)).cloned().unwrap_or(Value::Null);

    let g = by_id("kt-good");
    assert_eq!(g["ok"], true, "{g}");
    assert_eq!(g["config"]["port"], 1);
    assert_eq!(g["config"]["agent"], "none");
    assert_eq!(g["known_host"], true);
    assert_eq!(g["keys"], json!([{ "source": "file", "type": "ED25519", "bits": 256, "age_days": 0, "in_agent": false }]));
    // 閉じたポートなので接続はできない。理由は unreachable
    assert_eq!(g["probe"]["ok"], false);
    assert_eq!(g["probe"]["reason"], "unreachable");
    assert_eq!(codes(&g), ["probe_unreachable"]);

    let w = by_id("kt-weak");
    assert_eq!(w["known_host"], false);
    assert_eq!(w["keys"][0]["type"], "RSA");
    assert_eq!(codes(&w), ["rsa_short", "unknown_host", "forward_agent", "probe_unreachable"]);

    let n = by_id("kt-nokey");
    assert_eq!(n["keys"], json!([]));
    assert_eq!(codes(&n)[0], "no_key");

    assert_eq!(by_id("-oProxyCommand=x")["ok"], false);

    // 出力に一時ディレクトリのパス・鍵のコメント・指紋が出ない
    let text = report.to_string();
    for leaked in [dir.to_string_lossy().as_ref(), "kt-test-comment", "SHA256:", "127.0.0.1"] {
        assert!(!text.contains(leaked), "{leaked} が出力に含まれる");
    }
}
