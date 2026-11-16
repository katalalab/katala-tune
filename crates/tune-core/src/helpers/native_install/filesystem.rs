use super::{Cause, InstallError, Operation, fault, os, rejected};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};
pub(super) struct RenameFailure {
    pub error: io::Error,
    pub moved: bool,
}
impl From<io::Error> for RenameFailure {
    fn from(error: io::Error) -> Self {
        Self { error, moved: false }
    }
}
#[derive(Clone, Copy)]
pub(super) enum Kind {
    Directory,
    Read,
    Create,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct State {
    pub identity: os::Identity,
    pub size: u64,
    pub links: u64,
    pub regular: bool,
    pub directory: bool,
    pub mode: u32,
    pub times: [i128; 2],
}
impl State {
    pub fn regular(&self, max: u64) -> Result<(), InstallError> {
        if self.regular && self.links == 1 && self.size > 0 && self.size <= max { Ok(()) } else { Err(rejected(Operation::Metadata, Cause::UnsafeFile)) }
    }
}
pub(super) fn io_error(op: Operation, e: io::Error) -> InstallError {
    let cause = match e.kind() {
        io::ErrorKind::NotFound => Cause::Missing,
        io::ErrorKind::PermissionDenied => Cause::PermissionDenied,
        io::ErrorKind::AlreadyExists => Cause::Collision,
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => Cause::IdentityChanged,
        _ => Cause::Io,
    };
    rejected(op, cause)
}
pub(super) fn checked_close(file: File) -> Result<(), InstallError> {
    // Even the injected close failure consumes and closes the real handle once.
    let actual = os::close(file).map_err(|e| io_error(Operation::Close, e));
    actual.and_then(|()| fault(Operation::Close))
}
pub(super) struct Directory {
    pub path: PathBuf,
    chain: Vec<(OsString, File, os::Identity)>,
}
impl Directory {
    pub fn open(path: &Path) -> Result<Self, InstallError> {
        let (root, parts) = os::root(path).map_err(|e| io_error(Operation::Open, e))?;
        let id = os::state(&root).map_err(|e| io_error(Operation::Metadata, e))?.identity;
        let mut out = Self { path: path.to_owned(), chain: vec![(OsString::new(), root, id)] };
        for name in parts {
            out.push(&name)?;
        }
        out.check()?;
        Ok(out)
    }
    fn push(&mut self, name: &OsStr) -> Result<(), InstallError> {
        let before = os::named(self.file(), name).map_err(|e| io_error(Operation::Open, e))?;
        let file = os::open(self.file(), name, Kind::Directory).map_err(|e| io_error(Operation::Open, e))?;
        let after = os::state(&file).map_err(|e| io_error(Operation::Metadata, e))?;
        if !after.directory || after.identity != before.identity {
            return Err(rejected(Operation::Metadata, Cause::UnsafeFile));
        }
        self.chain.push((name.to_owned(), file, after.identity));
        Ok(())
    }
    pub fn child(&mut self, name: &OsStr) -> Result<(), InstallError> {
        self.push(name)?;
        self.path.push(name);
        self.check()
    }
    pub fn identity(&self) -> &os::Identity {
        &self.chain.last().expect("root exists").2
    }
    pub fn file(&self) -> &File {
        &self.chain.last().expect("root exists").1
    }
    pub fn check(&self) -> Result<(), InstallError> {
        for (i, (name, file, id)) in self.chain.iter().enumerate() {
            let held = os::state(file).map_err(|e| io_error(Operation::Metadata, e))?;
            if !held.directory || &held.identity != id {
                return Err(rejected(Operation::Metadata, Cause::IdentityChanged));
            }
            if i > 0 {
                let named = os::named(&self.chain[i - 1].1, name).map_err(|e| io_error(Operation::Metadata, e))?;
                if !named.directory || &named.identity != id {
                    return Err(rejected(Operation::Metadata, Cause::IdentityChanged));
                }
            }
        }
        Ok(())
    }
    pub fn close(self) -> Result<(), InstallError> {
        let mut error = None;
        for (_, file, _) in self.chain.into_iter().rev() {
            if let Err(e) = checked_close(file) {
                error.get_or_insert(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Observed {
    pub state: State,
    pub hash: String,
}
pub(super) struct ReadFile {
    pub observed: Observed,
    pub bytes: Vec<u8>,
}
pub(super) fn read(dir: &Directory, name: &OsStr, maximum: u64, collect: bool) -> Result<ReadFile, InstallError> {
    dir.check()?;
    let before = os::named(dir.file(), name).map_err(|e| io_error(Operation::Open, e))?;
    before.regular(maximum)?;
    let mut file = os::open(dir.file(), name, Kind::Read).map_err(|e| io_error(Operation::Open, e))?;
    let result = (|| {
        let opened = os::state(&file).map_err(|e| io_error(Operation::Metadata, e))?;
        opened.regular(maximum)?;
        if opened != before {
            return Err(rejected(Operation::Metadata, Cause::IdentityChanged));
        }
        let mut hash = Sha256::new();
        let mut bytes = Vec::new();
        let mut count = 0u64;
        let mut chunk = vec![0u8; (1024 * 1024).min(maximum as usize)];
        loop {
            fault(Operation::Read)?;
            let n = file.read(&mut chunk).map_err(|e| io_error(Operation::Read, e))?;
            if n == 0 {
                break;
            }
            count += n as u64;
            if count > opened.size || count > maximum {
                return Err(rejected(Operation::Read, Cause::Limit));
            }
            hash.update(&chunk[..n]);
            if collect {
                bytes.extend_from_slice(&chunk[..n]);
            }
        }
        let after = os::state(&file).map_err(|e| io_error(Operation::Metadata, e))?;
        fault(Operation::ReadIdentity)?;
        let named = os::named(dir.file(), name).map_err(|e| io_error(Operation::Metadata, e))?;
        if count != opened.size || opened != after || after != named {
            return Err(rejected(Operation::ReadIdentity, Cause::IdentityChanged));
        }
        dir.check()?;
        Ok(ReadFile { observed: Observed { state: after, hash: hash.finalize().iter().map(|b| format!("{b:02x}")).collect() }, bytes })
    })();
    let close = checked_close(file);
    match (result, close) {
        (_, Err(e)) | (Err(e), _) => Err(e),
        (Ok(v), Ok(())) => Ok(v),
    }
}
pub(super) fn optional(dir: &Directory, name: &OsStr) -> Result<Option<Observed>, InstallError> {
    dir.check()?;
    match os::named(dir.file(), name) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(Operation::Metadata, e)),
        Ok(_) => read(dir, name, super::artifact::MAX_BINARY, false).map(|v| Some(v.observed)),
    }
}
