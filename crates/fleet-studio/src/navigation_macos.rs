//! macOS-only handoff. Called on the existing dedicated worker thread.
use std::ffi::CString;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

use objc::runtime::{BOOL, Class, NO, Object, YES};
use objc::{msg_send, sel, sel_impl};

use super::OpenResult;
use super::macos_policy::{self, DirectoryFs, FsError, Workspace};

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

struct LocalDirectories;

impl DirectoryFs for LocalDirectories {
    type Directory = OwnedFd;

    fn root(&mut self) -> Result<Self::Directory, FsError> {
        // SAFETY: the literal is NUL terminated and the flags request only a
        // read-only directory handle. A successful descriptor is owned below.
        let fd = unsafe { libc::open(c"/".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 { Err(FsError::Failed) } else { Ok(unsafe { OwnedFd::from_raw_fd(fd) }) }
    }

    fn child(&mut self, parent: &Self::Directory, name: &str) -> Result<Self::Directory, FsError> {
        let name = CString::new(name).map_err(|_| FsError::Failed)?;
        // SAFETY: parent stays open, name is NUL terminated and policy has
        // restricted it to one component. NOFOLLOW rejects a symlink here.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 { Err(FsError::Failed) } else { Ok(unsafe { OwnedFd::from_raw_fd(fd) }) }
    }

    fn local(&mut self, directory: &Self::Directory) -> Result<bool, FsError> {
        let mut info = MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: the descriptor is open and the output points to sufficient storage.
        if unsafe { libc::fstatfs(directory.as_raw_fd(), info.as_mut_ptr()) } != 0 {
            return Err(FsError::Failed);
        }
        // SAFETY: fstatfs succeeded and initialized the complete structure.
        let info = unsafe { info.assume_init() };
        Ok(info.f_flags & libc::MNT_LOCAL as u32 != 0)
    }

    fn same(&mut self, first: &Self::Directory, second: &Self::Directory, authorized: &impl Fn() -> bool) -> Result<bool, FsError> {
        fn identity(directory: &OwnedFd) -> Result<(libc::dev_t, libc::ino_t), FsError> {
            let mut info = MaybeUninit::<libc::stat>::uninit();
            // SAFETY: the descriptor is open and the output points to sufficient storage.
            if unsafe { libc::fstat(directory.as_raw_fd(), info.as_mut_ptr()) } != 0 {
                return Err(FsError::Failed);
            }
            // SAFETY: fstat succeeded and initialized the complete structure.
            let info = unsafe { info.assume_init() };
            Ok((info.st_dev, info.st_ino))
        }
        let first = identity(first)?;
        if !authorized() {
            return Err(FsError::Revoked);
        }
        Ok(first == identity(second)?)
    }
}

struct AutoreleasePool(*mut Object);

impl AutoreleasePool {
    fn new() -> Option<Self> {
        let class = Class::get("NSAutoreleasePool")?;
        // SAFETY: this class and selector are provided by Foundation on macOS.
        let pool: *mut Object = unsafe { msg_send![class, new] };
        (!pool.is_null()).then_some(Self(pool))
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        // SAFETY: new created this pool on the same worker thread; drain balances it.
        unsafe {
            let _: () = msg_send![self.0, drain];
        }
    }
}

struct MacWorkspace;

impl MacWorkspace {
    fn open(value: &str, directory: bool, authorized: &impl Fn() -> bool) -> OpenResult {
        let Some(_pool) = AutoreleasePool::new() else { return OpenResult::Failed };
        let Some(string_class) = Class::get("NSString") else { return OpenResult::Failed };
        let Some(url_class) = Class::get("NSURL") else { return OpenResult::Failed };
        let Some(workspace_class) = Class::get("NSWorkspace") else { return OpenResult::Failed };
        let Ok(value) = CString::new(value) else { return OpenResult::Failed };
        // SAFETY: the string is valid UTF-8 and NUL terminated for this call;
        // all autoreleased objects remain alive until the pool is drained.
        let string: *mut Object = unsafe { msg_send![string_class, stringWithUTF8String: value.as_ptr()] };
        if string.is_null() {
            return OpenResult::Failed;
        }
        // SAFETY: the selectors accept the NSString above. fileURLWithPath
        // treats the path as data rather than a shell command.
        let url: *mut Object =
            unsafe { if directory { msg_send![url_class, fileURLWithPath: string isDirectory: YES] } else { msg_send![url_class, URLWithString: string] } };
        if url.is_null() {
            return OpenResult::Failed;
        }
        // SAFETY: NSWorkspace and NSURL are provided by AppKit/Foundation and
        // remain alive during this call. BOOL reports OS acceptance only.
        let workspace: *mut Object = unsafe { msg_send![workspace_class, sharedWorkspace] };
        if workspace.is_null() {
            return OpenResult::Failed;
        }
        super::finish_if_authorized(authorized, || {
            let accepted: BOOL = unsafe { msg_send![workspace, openURL: url] };
            accepted != NO
        })
    }
}

impl Workspace for MacWorkspace {
    fn github(&mut self, url: &str, authorized: &impl Fn() -> bool) -> OpenResult {
        Self::open(url, false, authorized)
    }

    fn directory(&mut self, path: &str, authorized: &impl Fn() -> bool) -> OpenResult {
        Self::open(path, true, authorized)
    }
}

pub(super) fn github(value: &str, authorized: &impl Fn() -> bool) -> OpenResult {
    macos_policy::open_github(value, &mut MacWorkspace, authorized)
}

pub(super) fn directory(path: &Path, authorized: &impl Fn() -> bool, current: &impl Fn() -> bool) -> OpenResult {
    let Some(path) = path.to_str() else { return OpenResult::Failed };
    macos_policy::open_directory(path, &mut LocalDirectories, &mut MacWorkspace, authorized, current)
}
