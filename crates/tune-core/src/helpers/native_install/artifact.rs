use super::{Cause, InstallError, Operation, rejected};
use serde::Deserialize;

pub(super) const MAX_BINARY: u64 = 256 * 1024 * 1024;
pub(super) const MAX_MANIFEST: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Manifest {
    pub schema: String,
    pub version: String,
    pub revision: String,
    pub source_tree: String,
    pub platform: String,
    pub architecture: String,
    pub build: Build,
    pub ci: Ci,
    pub files: Vec<Entry>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Build {
    pub profile: String,
    pub target: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Ci {
    pub repository: String,
    pub workflow_ref: String,
    pub workflow_sha: String,
    pub event_name: String,
    pub run_id: String,
    pub run_attempt: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub file: String,
    pub role: String,
    pub bytes: u64,
    pub sha256: String,
}

pub(super) fn tuple() -> Result<(&'static str, &'static str, &'static str), InstallError> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(("darwin", "arm64", "aarch64-apple-darwin"))
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Ok(("win32", "x64", "x86_64-pc-windows-msvc"))
    } else {
        Err(rejected(Operation::Platform, Cause::UnsupportedPlatform))
    }
}
fn field(value: &str, pattern: &str) -> bool {
    value.len() <= 512 && !value.contains(['\r', '\n']) && regex::Regex::new(pattern).expect("constant schema regex").is_match(value)
}
pub(super) fn parse(raw: &[u8]) -> Result<Manifest, InstallError> {
    let invalid = || rejected(Operation::Manifest, Cause::InvalidArtifact);
    // Typed serde structs reject duplicate fields, unknown fields, noninteger sizes
    // and invalid UTF-8, including in metadata never used for installation.
    let m: Manifest = serde_json::from_slice(raw).map_err(|_| invalid())?;
    let (platform, architecture, target) = tuple()?;
    let sha = r"^[a-f0-9]{40}$";
    let semver = r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-((?:0|[1-9][0-9]*|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9][0-9]*|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$";
    let ci = &m.ci;
    if m.schema != "katala_gpui_preview.v2"
        || !field(&m.version, semver)
        || !field(&m.revision, sha)
        || !field(&m.source_tree, sha)
        || m.platform != platform
        || m.architecture != architecture
        || m.build.profile != "release"
        || m.build.target != target
        || !field(&ci.repository, r"^[A-Za-z0-9_-][A-Za-z0-9_.-]*/[A-Za-z0-9_-][A-Za-z0-9_.-]*$")
        || !field(
            &ci.workflow_ref,
            r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/\.github/workflows/gpui-preview\.yml@refs/(?:heads/[A-Za-z0-9][A-Za-z0-9._/-]*|pull/[1-9][0-9]*/merge)$",
        )
        || !ci.workflow_ref.starts_with(&format!("{}/", ci.repository))
        || !field(&ci.workflow_sha, sha)
        || !matches!(ci.event_name.as_str(), "pull_request" | "workflow_dispatch")
        || (ci.event_name == "pull_request") != ci.workflow_ref.contains("@refs/pull/")
        || !field(&ci.run_id, r"^[1-9][0-9]{0,31}$")
        || !field(&ci.run_attempt, r"^[1-9][0-9]{0,31}$")
        || m.files.len() != 3
    {
        return Err(invalid());
    }
    for (name, role) in [("katala-tune-gpui", "gui"), ("tune", "diagnostic-cli"), ("tune-agent", "native-sampler")] {
        let entries: Vec<_> = m.files.iter().filter(|e| e.file == crate::helpers::exe(name) && e.role == role).collect();
        if entries.len() != 1 || entries[0].bytes == 0 || entries[0].bytes > MAX_BINARY || !field(&entries[0].sha256, r"^[a-f0-9]{64}$") {
            return Err(invalid());
        }
    }
    Ok(m)
}
