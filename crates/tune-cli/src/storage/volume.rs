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

#[cfg(target_os = "linux")]
pub(super) fn guest_only() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/osrelease").is_ok_and(|r| r.to_ascii_lowercase().contains("microsoft"))
}

#[cfg(not(target_os = "linux"))]
pub(super) fn guest_only() -> bool {
    false
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
