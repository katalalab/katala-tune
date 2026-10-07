//! 待ち受けるアドレス。既定は 127.0.0.1 と、この機体の Tailscale のアドレスだけ。
//! 全部のアドレス（0.0.0.0・::）では決して待ち受けない（`--listen` で渡されても断る）。

use std::net::IpAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

/// Tailscale が配るアドレスか（IPv4 は 100.64/10 の範囲、IPv6 は fd7a:115c:a1e0::/48）
pub fn is_tailscale(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            o[0] == 100 && (64..=127).contains(&o[1])
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// `--listen` で渡されたアドレスを確かめる
pub fn check_listen(s: &str) -> Result<IpAddr, String> {
    let ip: IpAddr = s.trim().trim_start_matches('[').trim_end_matches(']').parse().map_err(|_| format!("IP アドレスではない: {s}"))?;
    if ip.is_unspecified() {
        return Err(format!("{ip} （全部のアドレス）では待ち受けない。127.0.0.1 か Tailscale のアドレスを指定してください"));
    }
    if ip.is_multicast() {
        return Err(format!("{ip} では待ち受けられない"));
    }
    Ok(ip)
}

fn tailscale_cli() -> Vec<PathBuf> {
    let mut c = vec![PathBuf::from("tailscale")];
    if cfg!(target_os = "macos") {
        c.push(PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale"));
        c.push(PathBuf::from("/opt/homebrew/bin/tailscale"));
        c.push(PathBuf::from("/usr/local/bin/tailscale"));
    } else if cfg!(windows) {
        let pf = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Program Files"));
        c.push(pf.join("Tailscale").join("tailscale.exe"));
    }
    c
}

/// この機体の Tailscale のアドレス（`tailscale ip` の出力のうち、Tailscale の範囲のものだけ）。無ければ空
pub async fn tailscale_addrs() -> Vec<IpAddr> {
    for cli in tailscale_cli() {
        let mut c = tokio::process::Command::new(&cli);
        c.arg("ip").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
        #[cfg(windows)]
        c.creation_flags(0x0800_0000);
        let Ok(child) = c.spawn() else { continue };
        let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await else { continue };
        if !out.status.success() {
            continue;
        }
        let ips: Vec<IpAddr> = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse().ok()).filter(is_tailscale).collect();
        if !ips.is_empty() {
            return ips;
        }
    }
    Vec::new()
}

/// 既定の待ち受け: 127.0.0.1 と Tailscale のアドレス
pub async fn defaults() -> Vec<IpAddr> {
    let mut v = vec![IpAddr::from([127, 0, 0, 1])];
    v.extend(tailscale_addrs().await);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailscale_range() {
        assert!(is_tailscale(&IpAddr::from([100, 64, 0, 1])));
        assert!(is_tailscale(&IpAddr::from([100, 127, 255, 254])));
        assert!(!is_tailscale(&IpAddr::from([100, 128, 0, 1])));
        assert!(!is_tailscale(&IpAddr::from([100, 63, 0, 1])));
        assert!(!is_tailscale(&IpAddr::from([192, 168, 1, 2])));
        assert!(is_tailscale(&IpAddr::from([0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 1])));
        assert!(!is_tailscale(&IpAddr::from([0xfd7a, 0x115c, 0xa1e1, 0, 0, 0, 0, 1])));
    }

    #[test]
    fn never_listen_on_all_addresses() {
        assert!(check_listen("0.0.0.0").is_err());
        assert!(check_listen("::").is_err());
        assert!(check_listen("[::]").is_err());
        assert!(check_listen("224.0.0.1").is_err());
        assert!(check_listen("not-an-ip").is_err());
        assert_eq!(check_listen("127.0.0.1").unwrap(), IpAddr::from([127, 0, 0, 1]));
        assert_eq!(check_listen("[::1]").unwrap(), "::1".parse::<IpAddr>().unwrap());
    }
}
