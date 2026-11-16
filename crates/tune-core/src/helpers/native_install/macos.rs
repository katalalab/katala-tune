//! Every descendant lookup is relative to a retained directory descriptor.
#![allow(unsafe_code)]
use super::filesystem::{Kind, RenameFailure, State};
use std::{
    ffi::{CString, OsStr, OsString},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, IntoRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    device: u64,
    inode: u64,
}
fn c(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}
fn file(fd: i32) -> io::Result<File> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful fresh descriptor transferred exactly once.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
pub(super) fn root(path: &Path) -> io::Result<(File, Vec<OsString>)> {
    if !path.is_absolute() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(p) => parts.push(p.to_owned()),
            _ => return Err(io::ErrorKind::InvalidInput.into()),
        }
    }
    // SAFETY: static root path, readonly directory and close-on-exec descriptor.
    Ok((file(unsafe { libc::open(c"/".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) })?, parts))
}
pub(super) fn open(parent: &File, name: &OsStr, kind: Kind) -> io::Result<File> {
    let name = c(name)?;
    let access = match kind {
        Kind::Directory => libc::O_RDONLY | libc::O_DIRECTORY,
        Kind::Read => libc::O_RDONLY,
        Kind::Create => libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
    };
    // SAFETY: retained parent and one NUL-free component. O_NOFOLLOW rejects links,
    // O_NONBLOCK prevents FIFO/device waits before regular-file validation.
    file(unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), access | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC, 0o600) })
}
pub(super) fn state(file: &File) -> io::Result<State> {
    let m = file.metadata()?;
    Ok(State {
        identity: Identity { device: m.dev(), inode: m.ino() },
        size: m.len(),
        links: m.nlink(),
        regular: m.is_file(),
        directory: m.is_dir(),
        mode: m.mode(),
        times: [m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128, m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128],
    })
}
pub(super) fn named(parent: &File, name: &OsStr) -> io::Result<State> {
    let name = c(name)?;
    let mut s = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: live parent, valid C component, fully initialized stat on success.
    if unsafe { libc::fstatat(parent.as_raw_fd(), name.as_ptr(), s.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatat initialized the complete stat.
    let m = unsafe { s.assume_init() };
    Ok(State {
        identity: Identity { device: m.st_dev as u64, inode: m.st_ino },
        size: m.st_size as u64,
        links: m.st_nlink as u64,
        regular: m.st_mode & libc::S_IFMT == libc::S_IFREG,
        directory: m.st_mode & libc::S_IFMT == libc::S_IFDIR,
        mode: m.st_mode as u32,
        times: [m.st_mtime as i128 * 1_000_000_000 + m.st_mtime_nsec as i128, m.st_ctime as i128 * 1_000_000_000 + m.st_ctime_nsec as i128],
    })
}
pub(super) fn close(file: File) -> io::Result<()> {
    let fd = file.into_raw_fd();
    // SAFETY: consume ownership once. Never retry POSIX close (including EINTR),
    // because another thread might already have reused this descriptor number.
    if unsafe { libc::close(fd) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}
pub(super) fn make_executable(file: &File) -> io::Result<()> {
    // SAFETY: permission change applies only to the exclusively created stage.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o755) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}
pub(super) fn executable(s: &State) -> bool {
    s.mode & 0o111 == 0o111 && s.mode & 0o6022 == 0
}
pub(super) fn mkdir(parent: &File, name: &OsStr) -> io::Result<()> {
    let name = c(name)?;
    // SAFETY: live retained parent and single component, no recursive creation.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o755) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}
pub(super) fn rename(parent: &File, from: &OsStr, to: &OsStr, id: &Identity) -> Result<(), RenameFailure> {
    rename_inner(parent, from, to, id).map_err(Into::into)
}
fn rename_inner(parent: &File, from: &OsStr, to: &OsStr, id: &Identity) -> io::Result<()> {
    if named(parent, from)?.identity != *id {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let from = c(from)?;
    let to = c(to)?;
    // SAFETY: both names are in the retained directory; RENAME_EXCL atomically
    // refuses an occupied destination. Same-user concurrent namespace mutation
    // cannot be completely excluded by descriptor-relative POSIX operations.
    if unsafe { libc::renameatx_np(parent.as_raw_fd(), from.as_ptr(), parent.as_raw_fd(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
pub(super) fn remove(parent: &File, name: &OsStr, id: &Identity) -> io::Result<()> {
    let s = named(parent, name)?;
    if s.identity != *id || !s.regular || s.links != 1 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let name = c(name)?;
    // SAFETY: only an identity-checked owned regular leaf, never a directory.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}
