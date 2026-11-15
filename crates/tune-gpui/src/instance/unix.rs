//! macOS: UID-private directory, kernel file lock, peer-UID checked Unix socket.
#![allow(unsafe_code)]
use super::{ACK_TIMEOUT, IO_TIMEOUT};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::PathBuf,
};

pub fn user() -> Result<String, String> {
    Ok(unsafe { libc::geteuid() }.to_string())
}
fn directory() -> Result<PathBuf, String> {
    // Short fixed OS-local path avoids sockaddr_un truncation and temp env aliases.
    #[cfg(not(test))]
    let path = PathBuf::from(format!("/tmp/katala-gpui-{}", user()?));
    #[cfg(test)]
    let path =
        PathBuf::from(format!("/tmp/kgpui-test-{}-{}", user()?, std::env::var("TUNE_IPC_FIXTURE_OWNER").unwrap_or_else(|_| std::process::id().to_string())));
    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.to_string()),
    }
    let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err("activation directory must be owned by current user with mode 0700".into());
    }
    Ok(path)
}
fn socket(namespace: &str) -> Result<PathBuf, String> {
    Ok(directory()?.join(format!("{}.sock", &namespace[..32])))
}
pub enum Claim {
    Primary(Listener),
    Secondary,
}
pub struct Listener {
    socket: UnixListener,
    path: PathBuf,
    _lock: File,
}
pub struct Stream {
    socket: UnixStream,
    deadline: std::time::Instant,
}
impl Stream {
    pub fn read_budget(&mut self, budget: std::time::Duration) {
        self.deadline = std::time::Instant::now() + budget;
    }
}
impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let budget = self.deadline.checked_duration_since(std::time::Instant::now()).ok_or(io::ErrorKind::TimedOut)?;
        self.socket.set_read_timeout(Some(budget))?;
        self.socket.read(bytes)
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.socket.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
pub fn claim(namespace: &str) -> Result<Claim, String> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory()?.join(format!("{namespace}.lock")))
        .map_err(|e| e.to_string())?;
    let meta = lock.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
        return Err("unsafe activation lock".into());
    }
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(Claim::Secondary),
        Err(e) => return Err(e.to_string()),
    }
    let path = socket(namespace)?;
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            use std::os::unix::fs::FileTypeExt;
            if !meta.file_type().is_socket() || meta.uid() != unsafe { libc::geteuid() } {
                return Err("unsafe stale activation endpoint".into());
            }
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let socket = UnixListener::bind(&path).map_err(|e| e.to_string())?;
    socket.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(Claim::Primary(Listener { socket, path, _lock: lock }))
}
fn peer(stream: &UnixStream) -> Result<(), String> {
    let (mut uid, mut gid) = (0, 0);
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 || uid != unsafe { libc::geteuid() } {
        return Err("foreign activation peer".into());
    }
    Ok(())
}
impl Listener {
    pub fn accept(&self) -> Result<Option<Stream>, String> {
        match self.socket.accept() {
            Ok((s, _)) => {
                if peer(&s).is_err() {
                    return Ok(None);
                }
                s.set_nonblocking(false).map_err(|e| e.to_string())?;
                s.set_read_timeout(Some(IO_TIMEOUT)).and_then(|()| s.set_write_timeout(Some(IO_TIMEOUT))).map_err(|e| e.to_string())?;
                Ok(Some(Stream { socket: s, deadline: std::time::Instant::now() + IO_TIMEOUT }))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}
pub fn connect(namespace: &str) -> Result<Stream, String> {
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    let stream = loop {
        match UnixStream::connect(socket(namespace)?) {
            Ok(s) => break s,
            Err(_) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(25)),
            Err(_) => return Err("existing instance did not expose its activation endpoint".into()),
        }
    };
    peer(&stream)?;
    stream.set_read_timeout(Some(ACK_TIMEOUT + IO_TIMEOUT)).and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT))).map_err(|e| e.to_string())?;
    Ok(Stream { socket: stream, deadline: std::time::Instant::now() + ACK_TIMEOUT + IO_TIMEOUT })
}
