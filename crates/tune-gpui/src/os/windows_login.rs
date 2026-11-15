//! Pure command/registry-value boundary, also tested on macOS.
use super::State;
use sha2::{Digest, Sha256};
pub fn identity(executable: &str) -> String {
    let hash: String = Sha256::digest(executable.to_lowercase().as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    format!("KatalaTuneGPUI-{hash}")
}
pub fn command(executable: &str, args: &[&str]) -> Result<String, String> {
    fn quote(value: &str) -> Result<String, String> {
        if value.contains(['\0', '\r', '\n']) {
            return Err("起動引数に制御文字があります".into());
        }
        let mut out = String::from("\"");
        let mut slashes = 0;
        for ch in value.chars() {
            if ch == '\\' {
                slashes += 1;
                continue;
            }
            out.extend(std::iter::repeat_n('\\', if ch == '"' { slashes * 2 + 1 } else { slashes }));
            slashes = 0;
            out.push(ch);
        }
        out.extend(std::iter::repeat_n('\\', slashes * 2));
        out.push('"');
        Ok(out)
    }
    let result = std::iter::once(executable).chain(args.iter().copied()).map(quote).collect::<Result<Vec<_>, _>>()?.join(" ");
    // Documented Run-key command limit. Count UTF-16, not UTF-8 bytes.
    if result.encode_utf16().count() > 260 {
        return Err("Windows Run の起動コマンド上限（260文字）を超えます".into());
    }
    Ok(result)
}
pub fn observed(registered: Option<&str>, expected: &str, approval: Option<&[u8]>) -> State {
    match registered {
        None => State::Disabled,
        Some(value) if value != expected => State::Failed("同じ登録名の起動コマンドが一致しません".into()),
        Some(_) => match approval {
            None => State::Enabled,
            Some(bytes) if bytes.len() == 12 && bytes[..4] == [2, 0, 0, 0] => State::Enabled,
            Some(bytes) if bytes.len() == 12 && bytes[..4] == [3, 0, 0, 0] => State::Denied,
            Some(_) => State::Unknown,
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_launch_preserves_paths_arguments_and_metacharacters() {
        assert_eq!(
            command(r"C:\App Space\preview.exe", &["--data", r"C:\data & fixture\", "--open", "dashboard"]).unwrap(),
            "\"C:\\App Space\\preview.exe\" \"--data\" \"C:\\data & fixture\\\\\" \"--open\" \"dashboard\""
        );
        assert_eq!(command("app.exe", &["a\"b"]).unwrap(), "\"app.exe\" \"a\\\"b\"");
        assert!(command("app.exe", &["bad\0value"]).is_err());
        assert!(command("app.exe", &[&"x".repeat(260)]).is_err());
        assert_ne!(identity(r"C:\qa\app.exe"), identity(r"C:\installed\app.exe"));
    }
    #[test]
    fn registry_requires_matching_command_and_preserves_policy_denial() {
        assert_eq!(observed(None, "expected", None), State::Disabled);
        assert_eq!(observed(Some("expected"), "expected", None), State::Enabled);
        assert_eq!(observed(Some("expected"), "expected", Some(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])), State::Denied);
        assert_eq!(observed(Some("expected"), "expected", Some(&[7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])), State::Unknown);
        assert!(matches!(observed(Some("other"), "expected", None), State::Failed(_)));
    }
}
