use super::*;
use serde_json::json;
use std::path::Path;

/// Inert structural bytes and self-declared raw-v2 metadata. Never native success.
pub(crate) fn prepared(root: &Path, version: &str) -> PreparedNativeSampler {
    let source = root.join("synthetic-source");
    std::fs::create_dir_all(&source).unwrap();
    let bytes = crate::helpers::synthetic_sampler_bytes();
    let files: Vec<_> = [("katala-tune-gpui", "gui"), ("tune", "diagnostic-cli"), ("tune-agent", "native-sampler")]
        .into_iter()
        .map(|(name, role)| {
            let file = crate::helpers::exe(name);
            std::fs::write(source.join(&file), &bytes).unwrap();
            json!({"file":file,"role":role,"bytes":bytes.len(),"sha256":crate::db::sha256_hex(&bytes)})
        })
        .collect();
    let (platform, architecture, target) = if cfg!(windows) { ("win32", "x64", "x86_64-pc-windows-msvc") } else { ("darwin", "arm64", "aarch64-apple-darwin") };
    let manifest = json!({"schema":"katala_gpui_preview.v2","version":version,"revision":"a".repeat(40),"sourceTree":"b".repeat(40),"platform":platform,"architecture":architecture,"build":{"profile":"release","target":target},"ci":{"repository":"fixture/project","workflowRef":"fixture/project/.github/workflows/gpui-preview.yml@refs/heads/main","workflowSha":"c".repeat(40),"eventName":"workflow_dispatch","runId":"123","runAttempt":"1"},"files":files});
    std::fs::write(source.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
    super::super::native_install::prepare_native_sampler(&source).unwrap()
}
pub(crate) fn architecture() -> Option<RemoteArchitecture> {
    Some(if cfg!(windows) { RemoteArchitecture::X64 } else { RemoteArchitecture::Arm64 })
}
#[test]
fn cpu_declaration_rejects_other_supported_cpus() {
    let mut pe = super::super::binary::fixtures::pe();
    assert!(super::super::binary::declared_target(&pe, "x86_64-pc-windows-msvc"));
    pe[132..134].copy_from_slice(&0xaa64u16.to_le_bytes());
    assert!(!super::super::binary::declared_target(&pe, "x86_64-pc-windows-msvc"));
    let mut mac = super::super::binary::fixtures::macho();
    assert!(super::super::binary::declared_target(&mac, "aarch64-apple-darwin"));
    mac[4..8].copy_from_slice(&0x01000007u32.to_le_bytes());
    assert!(!super::super::binary::declared_target(&mac, "aarch64-apple-darwin"));
    for wide in [false, true] {
        assert!(super::super::binary::declared_target(&super::super::binary::fixtures::fat(wide), "aarch64-apple-darwin"));
    }
    assert!(!super::super::binary::declared_target(&mac, "unknown"));
}
