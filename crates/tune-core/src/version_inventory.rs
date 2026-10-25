//! Read-only CLI identity and fleet compatibility audit. No config/DB initialization.
use crate::{collect, db::now_ms, nodes::Node, specs};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Read, sync::OnceLock, time::Duration};

pub const SCHEMA: &str = "katala_cli_version.v1";

fn executable_hash() -> Option<String> {
    let mut file = std::fs::File::open(std::env::current_exe().ok()?).ok()?;
    if file.metadata().ok()?.len() > 512 * 1024 * 1024 {
        return None;
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Some(hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn local() -> Value {
    static LOCAL: OnceLock<Value> = OnceLock::new();
    LOCAL
        .get_or_init(|| {
            json!({
                "schema":SCHEMA,"version":env!("CARGO_PKG_VERSION"),"cli_version":env!("CARGO_PKG_VERSION"),
                "platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,
                "specs_schema":specs::SCHEMA,"executable_sha256":executable_hash(),
                "commands":["version --json","cli-info --json","specs-local","specs","specs --saved","versions"]
            })
        })
        .clone()
}

pub fn validate(v: &Value, platform: &str) -> Result<Value, String> {
    if v["schema"] != SCHEMA || v["platform"] != platform || !matches!(platform, "macos" | "windows") {
        return Err("CLI バージョン形式または機体の OS が一致しません。対象の tune CLI を更新してください".into());
    }
    let version =
        v["version"].as_str().filter(|s| s.len() <= 80 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))).ok_or("invalid CLI version")?;
    let core = version.split(['-', '+']).next().unwrap_or("");
    let numbers: Vec<_> = core.split('.').collect();
    if numbers.len() != 3 || numbers.iter().any(|n| n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit())) {
        return Err("invalid CLI version".into());
    }
    let architecture = v["architecture"].as_str().filter(|s| matches!(*s, "x86_64" | "aarch64" | "x86" | "arm")).ok_or("invalid CLI architecture")?;
    let hash = v["executable_sha256"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("CLI 実行ファイルの SHA-256 を取得できません")?;
    if v["specs_schema"] != specs::SCHEMA {
        return Err("対象 CLI は機体仕様の形式に対応していません".into());
    }
    Ok(
        json!({"schema":SCHEMA,"version":version,"cli_version":version,"platform":platform,"architecture":architecture,"specs_schema":specs::SCHEMA,"executable_sha256":hash.to_ascii_lowercase()}),
    )
}

async fn inspect(node: Node) -> Value {
    let expected = env!("CARGO_PKG_VERSION");
    if let Err(error) = specs::select(std::slice::from_ref(&node), std::slice::from_ref(&node.id)) {
        return json!({"node_id":node.id,"ok":false,"refused":true,"error":error,"expected_version":expected});
    }
    let platform = if node.is_mac() { "macos" } else { "windows" };
    let report = if node.local {
        validate(&local(), platform)
    } else {
        // Fixed remote entry point; the alias is validated before use.
        let remote = if node.is_mac() {
            "if [ -x \"$HOME/.katala-tune/bin/tune\" ]; then exec \"$HOME/.katala-tune/bin/tune\" version --json; else exec tune version --json; fi"
        } else {
            "if [ -x \"$HOME/.katala-tune/bin/tune.exe\" ]; then exec \"$HOME/.katala-tune/bin/tune.exe\" version --json; else exec tune.exe version --json; fi"
        };
        let mut args = collect::ssh_args(&node.alias, remote);
        args.splice(
            0..0,
            ["-o", "StrictHostKeyChecking=yes", "-o", "UpdateHostKeys=no", "-o", "ClearAllForwardings=yes", "-o", "PermitLocalCommand=no"].map(str::to_string),
        );
        let result = collect::run_bounded("ssh", &args, &[], Duration::from_secs(25), 16384, 4096).await;
        if result.code != Some(0) || result.timed_out || result.output_limited {
            Err("CLI の版を取得できません。SSH 接続と version --json 対応版の配置を確認してください".into())
        } else {
            serde_json::from_slice::<Value>(&result.stdout).map_err(|_| "CLI が有効な JSON を返しませんでした".into()).and_then(|v| validate(&v, platform))
        }
    };
    match report {
        Ok(report) => json!({"node_id":node.id,"ok":true,"matches_expected":report["version"]==expected,"expected_version":expected,"report":report}),
        Err(error) => json!({"node_id":node.id,"ok":false,"error":error,"expected_version":expected}),
    }
}

pub async fn audit(nodes: Vec<Node>) -> Value {
    let mut reports = Vec::new();
    // Bound concurrency and retain ledger order for readable diffs/JSON exports.
    for batch in nodes.chunks(4) {
        let mut tasks = Vec::new();
        for node in batch {
            tasks.push(tokio::spawn(inspect(node.clone())));
        }
        for task in tasks {
            reports.push(task.await.unwrap_or_else(|_| json!({"ok":false,"error":"version task failed"})));
        }
    }
    let matched = reports.iter().filter(|r| r["ok"] == true && r["matches_expected"] == true).count();
    let mismatched = reports.iter().filter(|r| r["ok"] == true && r["matches_expected"] != true).count();
    let failed = reports.iter().filter(|r| r["ok"] != true).count();
    json!({"schema":"katala_fleet_versions.v1","at":now_ms(),"expected_version":env!("CARGO_PKG_VERSION"),"ok":failed==0 && mismatched==0,"nodes":reports,"counts":{"total":reports.len(),"matched":matched,"mismatched":mismatched,"failed":failed}})
}
