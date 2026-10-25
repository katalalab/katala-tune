//! アプリに同梱した道具（`tune` CLI・Windows の `tune-agent` サンプラー）を、アプリと同じ版に保つ。
//!
//! - この機体: 起動のたびに、アプリの隣にある道具を `~/.katala-tune/bin` へ写す（中身が違うときだけ）。
//!   Windows の既存 tune.exe は承認済みハッシュを保つため自動で上書きせず、artifact と台帳を明示的に更新する。
//!   ローカルのサンプラーはアプリと同じ版に保ち、Windows CLI の版は承認した更新に従う
//! - ほかの Windows の機体: ハブ（常時監視）が、サンプラーの hello に入った版が古い・無い機体へ、同梱のサンプラーを
//!   SSH の標準入力で送り、SHA-256 を確かめてから入れ替える（`~/.katala-tune/bin` だけ。probe.ps1 を置くのと同じ場所）
//!
//! 動いているサンプラーがあっても置き換えられるよう、古い物は名前を変えて退け、使われていない古い物は消す。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::collect;
use crate::db::{now_ms, sha256_hex};
use crate::nodes::Node;

/// この版（アプリ・CLI・サンプラーはワークスペースで同じ版）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

/// 同梱道具の保存先。検証用に `KATALA_TUNE_DATA_DIR` を明示したときは
/// その隔離 data directory 配下へ置き、通常運用は従来の `~/.katala-tune/bin` を使う。
fn bin_dir_for(data_dir: Option<&Path>) -> PathBuf {
    data_dir.map(|path| path.join("bin")).unwrap_or_else(|| crate::nodes::home().join(".katala-tune").join("bin"))
}

pub fn bin_dir() -> PathBuf {
    let isolated = std::env::var_os("KATALA_TUNE_DATA_DIR").filter(|value| !value.is_empty()).map(PathBuf::from);
    bin_dir_for(isolated.as_deref())
}

/// from（アプリの隣）にある道具のうち、to と中身が違うものを写す。戻り値は道具ごとの結果
pub fn sync(from: &Path, to: &Path, names: &[&str]) -> Vec<Value> {
    sync_with_policy(from, to, names, cfg!(windows))
}

fn sync_with_policy(from: &Path, to: &Path, names: &[&str], preserve_windows_helper: bool) -> Vec<Value> {
    names
        .iter()
        .filter_map(|n| {
            let src = from.join(exe(n));
            let bytes = std::fs::read(&src).ok()?;
            let dst = to.join(exe(n));
            let hash = sha256_hex(&bytes);
            if std::fs::read(&dst).ok().is_some_and(|b| sha256_hex(&b) == hash) {
                return Some(json!({ "name": n, "sha256": hash, "changed": false }));
            }
            // The ledger approves exact Windows helper bytes. App updates must not
            // invalidate that approval; replacing tune.exe requires a coordinated
            // artifact + ledger rollout (docs/windows-helper-approval.md).
            if preserve_windows_helper && *n == "tune" && dst.exists() {
                return Some(json!({ "name": n, "sha256": std::fs::read(&dst).ok().map(|bytes| sha256_hex(&bytes)), "bundled_sha256": hash, "changed": false, "approval_required": true,
                    "error": "existing Windows helper retained; deploy the verified artifact and ledger record together" }));
            }
            Some(match replace(to, &dst, &bytes, &hash) {
                Ok(()) => json!({ "name": n, "sha256": hash, "changed": true }),
                Err(e) => json!({ "name": n, "sha256": hash, "changed": false, "error": e }),
            })
        })
        .collect()
}

fn replace(dir: &Path, dst: &Path, bytes: &[u8], hash: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = dst.file_name().and_then(|n| n.to_str()).ok_or("名前が無い")?;
    let new = dir.join(format!("{name}.new"));
    std::fs::write(&new, bytes).map_err(|e| e.to_string())?;
    if std::fs::read(&new).map(|b| sha256_hex(&b)).ok().as_deref() != Some(hash) {
        let _ = std::fs::remove_file(&new);
        return Err("書いた物の SHA-256 が合わない".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    if dst.exists() {
        // 動いている実行ファイルも名前は変えられる（Windows）。古い物は使われていなければ消す
        std::fs::rename(dst, dir.join(format!("{name}.old-{}", now_ms()))).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&new, dst).map_err(|e| e.to_string())?;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(&format!("{name}.old-")) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// a が b より古い版か（SemVer の主・副・修正を数で比べ、同じならプレリリースの方が古い）
pub fn older(a: &str, b: &str) -> bool {
    let split = |v: &str| {
        let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p.to_string())));
        let nums: Vec<u64> = core.split('.').map(|x| x.parse().unwrap_or(0)).collect();
        (nums, pre)
    };
    let ((an, ap), (bn, bp)) = (split(a), split(b));
    if an != bn {
        return an < bn;
    }
    match (ap, bp) {
        (Some(_), None) => true,
        (Some(x), Some(y)) => x < y,
        _ => false,
    }
}

/// 機体のサンプラーを入れ替えるべきか（Windows のほかの機体で、hello の版が古い・ネイティブが無い）
pub fn agent_outdated(node: &Node, brief: Option<&Value>) -> bool {
    if !node.is_active() || !node.is_windows() || node.local {
        return false;
    }
    let Some(b) = brief else { return false };
    match (b["impl"].as_str(), b["agent"].as_str()) {
        (Some("rust"), Some(v)) => older(v, VERSION),
        (Some(_), _) => true, // スクリプト（ネイティブが無い）か、版の無い古い物
        (None, _) => false,   // まだ hello を受け取っていない
    }
}

/// サンプラーを SSH の標準入力で送り、SHA-256 を確かめてから入れ替える（既定シェルは Git Bash。collect.rs と同じ）
pub async fn push_agent(node: &Node, bytes: &[u8]) -> Result<String, String> {
    if let Some(err) = node.refusal() {
        return Err(err);
    }
    let hash = sha256_hex(bytes);
    let stamp = now_ms();
    let remote = format!(
        "mkdir -p ~/.katala-tune/bin && cd ~/.katala-tune/bin && cat > tune-agent.exe.new && [ \"$(sha256sum tune-agent.exe.new | cut -c1-64)\" = {hash} ] \
         && {{ [ ! -e tune-agent.exe ] || mv -f tune-agent.exe tune-agent.exe.old-{stamp}; }} && mv -f tune-agent.exe.new tune-agent.exe \
         && {{ rm -f tune-agent.exe.old-* 2>/dev/null; true; }} && echo pushed-{hash}"
    );
    let r = collect::run("ssh", &collect::ssh_args(&node.alias, &remote), Some(bytes), Duration::from_secs(120)).await;
    if r.out.contains(&format!("pushed-{hash}")) {
        Ok(hash)
    } else {
        let e = r.err.trim();
        Err(format!(
            "入れ替えられなかった（exit {}）{}",
            r.code_str(),
            if e.is_empty() { String::new() } else { format!(": {}", e.lines().last().unwrap_or(e)) }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_dir_uses_isolated_data_namespace_only_when_requested() {
        assert_eq!(bin_dir_for(Some(Path::new("/private/qa/data"))), PathBuf::from("/private/qa/data/bin"));
        assert_eq!(bin_dir_for(None), crate::nodes::home().join(".katala-tune").join("bin"));
    }

    #[test]
    fn versions_compare_like_semver() {
        assert!(older("0.5.0-power.7", "0.6.0"));
        assert!(older("0.6.0-rt.1", "0.6.0"), "同じ版ならプレリリースが古い");
        assert!(older("0.6.0", "0.6.1"));
        assert!(older("0.9.0", "0.10.0"), "数として比べる");
        assert!(!older("0.6.0", "0.6.0"));
        assert!(!older("0.6.1", "0.6.0"), "新しい物は戻さない");
        assert!(!older("0.6.0", "0.6.0-rt.1"));
    }

    #[test]
    fn outdated_agents_are_only_remote_windows() {
        let n = |os: &str, local: bool| {
            let mut x = Node::from_value(&json!({"id": "x", "alias": "x", "os": os}), "h");
            x.local = local;
            x
        };
        let rust = |v: &str| json!({"impl": "rust", "agent": v});
        assert!(agent_outdated(&n("windows", false), Some(&rust("0.0.1"))));
        assert!(agent_outdated(&n("windows", false), Some(&json!({"impl": "script"}))), "ネイティブが無ければ入れる");
        assert!(!agent_outdated(&n("windows", false), Some(&rust(VERSION))));
        assert!(!agent_outdated(&n("windows", false), Some(&json!({"impl": null}))), "hello の前は判断しない");
        assert!(!agent_outdated(&n("windows", true), Some(&rust("0.0.1"))), "この機体は sync で揃える");
        assert!(!agent_outdated(&n("macos", false), Some(&json!({"impl": "script"}))));
        assert!(!agent_outdated(&n("windows", false), None));
    }

    #[test]
    fn excluded_agents_are_never_updated() {
        for status in ["retired", "no-ssh", "paused", "out-of-scope", "invalid"] {
            let node = Node::from_value(&json!({"id": "x", "alias": "x", "os": "windows", "status": status}), "h");
            assert!(!agent_outdated(&node, Some(&json!({"impl": "rust", "agent": "0.0.1"}))), "{status}");
            assert!(!agent_outdated(&node, Some(&json!({"impl": "script"}))), "{status}");
        }
    }

    #[test]
    fn sync_copies_only_changed_tools_and_clears_old_copies() {
        let base = std::env::temp_dir().join(format!("kt-helpers-{}-{}", std::process::id(), now_ms()));
        let (from, to) = (base.join("app"), base.join("bin"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join(exe("tune")), b"v1").unwrap();
        let r = sync_with_policy(&from, &to, &["tune", "tune-agent"], false);
        assert_eq!(r.len(), 1, "無い道具は飛ばす");
        assert_eq!(r[0]["changed"], true);
        assert_eq!(std::fs::read(to.join(exe("tune"))).unwrap(), b"v1");
        assert_eq!(sync_with_policy(&from, &to, &["tune"], false)[0]["changed"], false, "同じなら写さない");
        std::fs::write(from.join(exe("tune")), b"v2").unwrap();
        assert_eq!(sync_with_policy(&from, &to, &["tune"], false)[0]["changed"], true);
        assert_eq!(std::fs::read(to.join(exe("tune"))).unwrap(), b"v2");
        let left: Vec<String> = std::fs::read_dir(&to).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, [exe("tune")], "古い物・途中の物は残さない");
        let _ = std::fs::remove_dir_all(&base);
    }
    #[test]
    fn windows_app_sync_retains_an_existing_approved_helper() {
        let base = std::env::temp_dir().join(format!("kt-helper-pin-{}-{}", std::process::id(), now_ms()));
        let (from, to) = (base.join("app"), base.join("bin"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join(exe("tune")), b"new-app-helper").unwrap();
        std::fs::write(to.join(exe("tune")), b"approved-helper").unwrap();
        std::fs::write(from.join(exe("tune-agent")), b"agent").unwrap();
        let results = sync_with_policy(&from, &to, &["tune", "tune-agent"], true);
        assert_eq!(results[0]["changed"], false);
        assert_eq!(results[0]["approval_required"], true);
        assert_eq!(results[0]["sha256"], sha256_hex(b"approved-helper"));
        assert_eq!(results[0]["bundled_sha256"], sha256_hex(b"new-app-helper"));
        assert_eq!(std::fs::read(to.join(exe("tune"))).unwrap(), b"approved-helper");
        assert_eq!(std::fs::read(to.join(exe("tune-agent"))).unwrap(), b"agent");
        std::fs::remove_dir_all(base).unwrap();
    }
}
