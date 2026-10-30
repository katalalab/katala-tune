//! Credentials are queried on the connected stream, never from an unrelated PID lookup.
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub(super) enum Policy {
    Unix { endpoint: String, uid: u32, gid: u32, socket_uid: u32, socket_gid: u32 },
    Windows { endpoint: String, pid: u32, created_filetime: String, session: u32, owner_sid: String, image_path_sha256: String },
}

#[cfg(unix)]
pub(super) type Stream = tokio::net::UnixStream;
#[cfg(windows)]
pub(super) type Stream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use windows::Guard;
#[cfg(all(test, windows))]
pub(super) use windows::fixture_policy;

#[cfg(unix)]
pub(super) struct Guard {
    path: std::path::PathBuf,
    identity: (u64, u64, u32, u32),
    peer: tokio::net::UnixStream,
    credentials: tokio::net::unix::UCred,
}
#[cfg(unix)]
fn socket_identity(path: &std::path::Path) -> Result<(u64, u64, u32, u32), &'static str> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let m = std::fs::symlink_metadata(path).map_err(|_| "socket identity unavailable")?;
    if !m.file_type().is_socket() {
        return Err("socket identity unavailable");
    }
    Ok((m.dev(), m.ino(), m.uid(), m.gid()))
}
#[cfg(unix)]
impl Guard {
    pub(super) fn epoch_digest(&self) -> String {
        crate::db::sha256_hex(
            format!("{}:{}:{}:{}:{}", self.identity.0, self.identity.1, self.credentials.uid(), self.credentials.gid(), self.credentials.pid().unwrap_or(0))
                .as_bytes(),
        )
    }
    pub(super) fn check(&self) -> Result<(), &'static str> {
        (socket_identity(&self.path)? == self.identity && self.peer.peer_cred().map_err(|_| "peer credentials unavailable")? == self.credentials)
            .then_some(())
            .ok_or("socket identity changed")
    }
}
#[cfg(unix)]
async fn connect_unix(path: &std::path::Path, uid: u32, gid: u32, socket_uid: u32, socket_gid: u32) -> Result<(Stream, Guard), &'static str> {
    let identity = socket_identity(path)?;
    if (identity.2, identity.3) != (socket_uid, socket_gid) {
        return Err("socket owner mismatch");
    }
    let stream = Stream::connect(path).await.map_err(|_| "local connection unavailable")?;
    let cred = stream.peer_cred().map_err(|_| "peer credentials unavailable")?;
    if cred.uid() != uid || cred.gid() != gid || cred.pid().is_none_or(|pid| pid <= 0) {
        return Err("peer credentials mismatch");
    }
    let standard = stream.into_std().map_err(|_| "peer handle unavailable")?;
    let copy = standard.try_clone().map_err(|_| "peer handle unavailable")?;
    let stream = Stream::from_std(standard).map_err(|_| "peer handle unavailable")?;
    let guard = Guard { path: path.to_owned(), identity, peer: Stream::from_std(copy).map_err(|_| "peer handle unavailable")?, credentials: cred };
    guard.check()?;
    Ok((stream, guard))
}
pub(super) async fn connect(policy: &Policy) -> Result<(Stream, Guard), &'static str> {
    match policy {
        #[cfg(unix)]
        Policy::Unix { endpoint, uid, gid, socket_uid, socket_gid }
            if endpoint.starts_with("unix:///") && !endpoint.starts_with("unix:////") && endpoint.len() <= 4096 && !endpoint.chars().any(char::is_control) =>
        {
            connect_unix(std::path::Path::new(&endpoint[7..]), *uid, *gid, *socket_uid, *socket_gid).await
        }
        #[cfg(windows)]
        Policy::Windows { .. } => windows::connect(policy),
        _ => Err("unsupported explicit IPC policy"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    #[tokio::test]
    async fn real_unix_peer_matches_on_connected_stream_and_rejects_wrong_policy() {
        let path = std::env::temp_dir().join(format!("docker-peer-{}-{}.sock", std::process::id(), crate::db::now_ms()));
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let stream =
            connect_unix(&path, metadata.uid(), metadata.gid(), metadata.uid(), metadata.gid()).await.expect("real peer must match explicit uid/gid policy");
        drop(stream);
        assert!(connect_unix(&path, metadata.uid().wrapping_add(1), metadata.gid(), metadata.uid(), metadata.gid()).await.is_err());
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }
    #[tokio::test]
    async fn unix_replaced_socket_symlink_and_unavailable_identity_are_rejected() {
        let path = std::env::temp_dir().join(format!("docker-peer-replaced-{}-{}.sock", std::process::id(), crate::db::now_ms()));
        let saved = path.with_extension("saved");
        let alias = path.with_extension("alias");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let m = std::fs::symlink_metadata(&path).unwrap();
        let (stream, guard) = connect_unix(&path, m.uid(), m.gid(), m.uid(), m.gid()).await.unwrap();
        std::fs::rename(&path, &saved).unwrap();
        let replacement = tokio::net::UnixListener::bind(&path).unwrap();
        assert!(guard.check().is_err());
        std::os::unix::fs::symlink(&saved, &alias).unwrap();
        assert!(connect_unix(&alias, m.uid(), m.gid(), m.uid(), m.gid()).await.is_err());
        assert!(connect_unix(&path, m.uid(), m.gid(), m.uid().wrapping_add(1), m.gid()).await.is_err());
        drop(stream);
        drop(guard);
        drop(listener);
        drop(replacement);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(saved).unwrap();
        std::fs::remove_file(alias).unwrap();
        assert!(connect_unix(&path, m.uid(), m.gid(), m.uid(), m.gid()).await.is_err());
    }
}
