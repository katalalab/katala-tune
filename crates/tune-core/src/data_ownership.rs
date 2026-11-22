//! Cooperative ownership of an Engine data directory, not collector ownership.
//! Raw Store callers and older applications do not participate. Namespace
//! replacement by a hostile same-user process is outside this contract.
use std::{
    fmt,
    fs::{self, File, Metadata, OpenOptions, TryLockError},
    io,
    path::{Path, PathBuf},
};

const LOCK_FILE: &str = ".katala-tune-engine.lock";

#[derive(Debug)]
pub(crate) enum ClaimError {
    Busy,
    UnsafePath(&'static str),
    Io(io::Error),
}
impl From<io::Error> for ClaimError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl fmt::Display for ClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("Engine data ownership busy: another participating Engine holds this data directory"),
            Self::UnsafePath(kind) => write!(f, "Engine data ownership unsafe path: {kind} must be a regular file with one link"),
            // Do not disclose private paths from filesystem errors.
            Self::Io(error) => write!(f, "Engine data ownership I/O error: {:?} (OS code {:?})", error.kind(), error.raw_os_error()),
        }
    }
}

pub(crate) struct DataClaim {
    data: PathBuf,
    // Never unlink: all participating openers must lock the same persistent file.
    _lock: File,
}
impl DataClaim {
    pub(crate) fn acquire(data: &Path) -> Result<Self, ClaimError> {
        let data = resolve_path(data)?;
        fs::create_dir_all(&data)?;
        let data = resolve_path(&data)?;
        let path = data.join(LOCK_FILE);
        validate_path(&path, "lock")?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let lock = options.open(&path)?;
        validate_file(&lock, "lock")?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(ClaimError::Busy),
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        let claim = Self { data, _lock: lock };
        // SQLite can also write these sidecars. Refuse detectable aliases before
        // ensure_config, schema initialization, migration or legacy import.
        for name in [crate::db::DB_FILE, "katala-tune.db-wal", "katala-tune.db-shm", "katala-tune.db-journal"] {
            let path = claim.data.join(name);
            if validate_path(&path, "database")? {
                validate_file(&File::open(path)?, "database")?;
            }
        }
        Ok(claim)
    }
    pub(crate) fn data(&self) -> &Path {
        &self.data
    }
}

fn validate_path(path: &Path, kind: &'static str) -> Result<bool, ClaimError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            validate_metadata(&meta, kind)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
fn validate_metadata(meta: &Metadata, kind: &'static str) -> Result<(), ClaimError> {
    if !meta.is_file() {
        return Err(ClaimError::UnsafePath(kind));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err(ClaimError::UnsafePath(kind));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(ClaimError::UnsafePath(kind));
        }
    }
    Ok(())
}
fn validate_file(file: &File, kind: &'static str) -> Result<(), ClaimError> {
    validate_metadata(&file.metadata()?, kind)?;
    #[cfg(windows)]
    if windows_link_count(file)? != 1 {
        return Err(ClaimError::UnsafePath(kind));
    }
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_link_count(file: &File) -> io::Result<u32> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: the borrowed File keeps this handle valid; the API initializes
    // the entire output structure on success, checked before assume_init.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.assume_init() }.nNumberOfLinks)
}

/// Canonicalize existing ancestors too, so missing and later-created paths
/// have the same identity. Resolve symlinks before `..`; never create state.
pub fn resolve_path(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut result = PathBuf::new();
    for part in absolute.components() {
        match part {
            std::path::Component::Prefix(_) => {
                // A Windows prefix such as \\?\C: is not a complete root yet.
                // Wait for RootDir before any filesystem query/canonicalize.
                result.push(part.as_os_str());
                continue;
            }
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            part => result.push(part.as_os_str()),
        }
        // Missing tails are valid; other filesystem errors must fail closed.
        if result.try_exists()? {
            result = result.canonicalize()?;
        }
    }
    Ok(result)
}
