//! Query the filesystem containing the actual directory. IDs only have local meaning.
use std::{io, path::Path};

#[cfg(unix)]
pub(super) fn identity(path: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(path.metadata()?.dev())
}

#[cfg(unix)]
pub(super) fn available(path: &Path) -> io::Result<u64> {
    let info = rustix::fs::statvfs(path)?;
    info.f_bavail.checked_mul(info.f_frsize).ok_or_else(|| io::Error::other("capacity overflow"))
}

#[cfg(windows)]
mod win {
    #![allow(unsafe_code)]
    use super::*;
    use std::{os::windows::ffi::OsStrExt, ptr::null_mut};
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeInformationW, GetVolumePathNameW};

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain([0]).collect()
    }

    pub(in super::super) fn identity(path: &Path) -> io::Result<u64> {
        let mut root = vec![0u16; 32768];
        let mut serial = 0;
        // SAFETY: the input is NUL terminated, root has the advertised writable length,
        // and the root returned on success is NUL terminated.
        unsafe {
            if GetVolumePathNameW(wide(path).as_ptr(), root.as_mut_ptr(), root.len() as _) == 0
                || GetVolumeInformationW(root.as_ptr(), null_mut(), 0, &mut serial, null_mut(), null_mut(), null_mut(), 0) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(serial.into())
    }

    pub(in super::super) fn available(path: &Path) -> io::Result<u64> {
        let mut bytes = 0;
        // SAFETY: NUL-terminated directory and one valid writable u64; other outputs are optional.
        if unsafe { GetDiskFreeSpaceExW(wide(path).as_ptr(), &mut bytes, null_mut(), null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(bytes)
    }
}

#[cfg(windows)]
pub(super) use win::{available, identity};

// macOS admission uses the same important-usage capacity as the existing Mac probe.
// Raw free space sampled during the run stays a single comparable series.
#[cfg(target_os = "macos")]
pub(super) fn admission(path: &Path) -> Option<u64> {
    let script = r#"ObjC.import("Foundation"); function run(a) { let v=Ref(), e=Ref(); if (!$.NSURL.fileURLWithPath(a[0]).getResourceValueForKeyError(v,$.NSURLVolumeAvailableCapacityForImportantUsageKey,e)) throw Error("unavailable"); return String(v[0].longLongValue); }"#;
    let result = std::process::Command::new("/usr/bin/osascript").args(["-l", "JavaScript", "-e", script, "--"]).arg(path).output().ok()?;
    result.status.success().then(|| std::str::from_utf8(&result.stdout).ok()?.trim().parse().ok()).flatten()
}

#[cfg(any(target_os = "linux", test))]
fn classify_linux(release: Option<&str>, wsl_hint: bool) -> Option<&'static str> {
    let release = release.map(str::trim).filter(|s| !s.is_empty());
    if wsl_hint
        || release.is_some_and(|r| {
            let kernel = r.to_ascii_lowercase();
            kernel.contains("microsoft") || kernel.contains("wsl")
        })
    {
        Some("guest_capacity_only")
    } else if release.is_none() {
        Some("environment_unknown")
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
pub(super) fn environment_hold() -> Option<&'static str> {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok();
    let hint = ["WSL_DISTRO_NAME", "WSL_INTEROP"].iter().any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()))
        || Path::new("/run/WSL").is_dir()
        || Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists();
    classify_linux(release.as_deref(), hint)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn environment_hold() -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::classify_linux;

    #[test]
    fn wsl_and_unidentified_environments_hold_admission() {
        assert_eq!(classify_linux(Some("6.1-standard-Microsoft"), false), Some("guest_capacity_only"));
        assert_eq!(classify_linux(Some("6.8-custom"), true), Some("guest_capacity_only"));
        assert_eq!(classify_linux(None, true), Some("guest_capacity_only"));
        assert_eq!(classify_linux(None, false), Some("environment_unknown"));
        assert_eq!(classify_linux(Some("\n"), false), Some("environment_unknown"));
        assert_eq!(classify_linux(Some("6.8-generic"), false), None);
    }
}

pub(super) fn link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}
