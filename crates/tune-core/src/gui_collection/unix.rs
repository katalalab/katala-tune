//! Fixed OS-local namespace, independent of HOME/TMPDIR/config/data/executable.
#![allow(unsafe_code)]
use super::AdmissionError;
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
};
pub(super) type Claim = File;

fn open_claim(directory: &Path, uid: u32) -> Result<Claim, AdmissionError> {
    match std::fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(AdmissionError::Unavailable),
    }
    let meta = std::fs::symlink_metadata(directory).map_err(|_| AdmissionError::Unavailable)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(AdmissionError::UnsafeStorage);
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        // NONBLOCK also ensures an unsafe FIFO cannot stall admission before metadata.
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("collection.lock"))
        .map_err(|_| AdmissionError::Unavailable)?;
    let meta = lock.metadata().map_err(|_| AdmissionError::Unavailable)?;
    if !meta.is_file() || meta.uid() != uid || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
        return Err(AdmissionError::UnsafeStorage);
    }
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => Err(AdmissionError::Busy),
        Err(_) => Err(AdmissionError::Unavailable),
    }
}
pub(super) fn claim() -> Result<Claim, AdmissionError> {
    let uid = unsafe { libc::geteuid() };
    open_claim(Path::new(&format!("/tmp/katala-tune-collector-v1-{uid}")), uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn unsafe_storage_is_refused_without_repair() {
        let uid = unsafe { libc::geteuid() };
        let root = std::env::temp_dir().join(format!("collector-path-check-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let directory = root.join("claim");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(open_claim(&directory, uid).unwrap_err(), AdmissionError::UnsafeStorage);
        assert!(!directory.join("collection.lock").exists());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias = root.join("directory-alias");
        symlink(&directory, &alias).unwrap();
        assert_eq!(open_claim(&alias, uid).unwrap_err(), AdmissionError::UnsafeStorage);
        let file = directory.join("collection.lock");
        let target = root.join("target");
        std::fs::write(&target, "unchanged").unwrap();
        symlink(&target, &file).unwrap();
        assert!(open_claim(&directory, uid).is_err());
        std::fs::remove_file(&file).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&target, &file).unwrap();
        assert_eq!(open_claim(&directory, uid).unwrap_err(), AdmissionError::UnsafeStorage);
        std::fs::remove_file(&file).unwrap();
        std::fs::write(&file, "restricted").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(open_claim(&directory, uid).is_err(), "actual access/mode failure must be bounded");
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(open_claim(&directory, uid).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "unchanged");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
