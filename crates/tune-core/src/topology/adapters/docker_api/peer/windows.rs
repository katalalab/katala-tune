//! Read-only identity checks on an already connected, explicitly selected pipe.
#![allow(unsafe_code)] // Win32 query handles only; never terminate or alter the peer.
use super::{Policy, Stream};
use std::{
    mem::size_of,
    os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::{FILETIME, LocalFree, WAIT_TIMEOUT},
    Security::{Authorization::ConvertSidToStringSidW, GetTokenInformation, SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER, TokenUser},
    System::{
        Pipes::{GetNamedPipeServerProcessId, GetNamedPipeServerSessionId},
        RemoteDesktop::ProcessIdToSessionId,
        Threading::{GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, WaitForSingleObject},
    },
};
pub(in crate::topology::adapters::docker_api) struct Guard {
    process: OwnedHandle,
    created: u64,
    session: u32,
    pid: u32,
    pipe: OwnedHandle,
    owner_sid: String,
    image_path_sha256: String,
}
fn handle(pid: u32) -> Result<OwnedHandle, &'static str> {
    // SAFETY: query/synchronize rights only, no inheritance; valid returned handle becomes owned.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | 0x0010_0000, 0, pid) };
    if raw.is_null() {
        return Err("peer process unavailable");
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}
fn identity(process: &OwnedHandle, pid: u32) -> Result<(u64, u32), &'static str> {
    let mut created = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let mut session = 0;
    // SAFETY: retained query handle and valid output buffers.
    if unsafe { GetProcessTimes(process.as_raw_handle(), &mut created, &mut exit, &mut kernel, &mut user) } == 0
        || unsafe { ProcessIdToSessionId(pid, &mut session) } == 0
    {
        return Err("peer identity unavailable");
    }
    let created = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    if created == 0 {
        return Err("peer identity unavailable");
    }
    Ok((created, session))
}
fn owner(process: &OwnedHandle) -> Result<String, &'static str> {
    let mut raw = null_mut();
    // SAFETY: process remains live and the returned query token is independently owned.
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err("peer owner unavailable");
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    #[repr(C)]
    struct Buffer {
        user: TOKEN_USER,
        sid: [u8; SECURITY_MAX_SID_SIZE as usize],
    }
    let mut buffer = Buffer { user: TOKEN_USER::default(), sid: [0; SECURITY_MAX_SID_SIZE as usize] };
    let mut used = 0;
    // SAFETY: aligned buffer includes the documented maximum SID space.
    if unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, std::ptr::from_mut(&mut buffer).cast(), size_of::<Buffer>() as u32, &mut used) } == 0
        || used < size_of::<TOKEN_USER>() as u32
        || used > size_of::<Buffer>() as u32
        || buffer.user.User.Sid.is_null()
    {
        return Err("peer owner unavailable");
    }
    let mut text = null_mut();
    // SAFETY: valid SID in live TokenUser buffer, API allocates a terminated SID string.
    if unsafe { ConvertSidToStringSidW(buffer.user.User.Sid, &mut text) } == 0 {
        return Err("peer owner unavailable");
    }
    struct Allocation(*mut u16);
    impl Drop for Allocation {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0.cast()) };
        }
    }
    let allocation = Allocation(text);
    let mut sid = String::new();
    for offset in 0..256 {
        // SAFETY: documented SID string is terminated and at most 256 UTF-16 units.
        let unit = unsafe { *allocation.0.add(offset) };
        if unit == 0 {
            return if sid.starts_with("S-") { Ok(sid) } else { Err("peer owner unavailable") };
        }
        if unit > 127 || !(char::from_u32(u32::from(unit)).is_some_and(|c| c.is_ascii_digit() || c == 'S' || c == '-')) {
            return Err("peer owner unavailable");
        }
        sid.push(char::from_u32(u32::from(unit)).ok_or("peer owner unavailable")?);
    }
    Err("peer owner unavailable")
}
fn image_path_digest(process: &OwnedHandle) -> Result<String, &'static str> {
    let mut image = [0u16; 32768];
    let mut length = image.len() as u32;
    // SAFETY: fixed bounded output buffer, retained query handle.
    if unsafe { QueryFullProcessImageNameW(process.as_raw_handle(), 0, image.as_mut_ptr(), &mut length) } == 0 || length == 0 || length as usize >= image.len()
    {
        return Err("peer image unavailable");
    }
    let path = String::from_utf16(&image[..length as usize]).map_err(|_| "peer image unavailable")?;
    Ok(crate::db::sha256_hex(path.to_lowercase().as_bytes()))
}
impl Guard {
    pub(in crate::topology::adapters::docker_api) fn epoch_digest(&self) -> String {
        crate::db::sha256_hex(format!("{}:{}:{}:{}:{}", self.pid, self.created, self.session, self.owner_sid, self.image_path_sha256).as_bytes())
    }
    pub(in crate::topology::adapters::docker_api) fn check(&self) -> Result<(), &'static str> {
        let mut pid = 0;
        let mut session = 0;
        // SAFETY: retained duplicate refers to the same connected pipe object.
        if unsafe { GetNamedPipeServerProcessId(self.pipe.as_raw_handle(), &mut pid) } == 0
            || unsafe { GetNamedPipeServerSessionId(self.pipe.as_raw_handle(), &mut session) } == 0
            || pid != self.pid
            || session != self.session
        {
            return Err("pipe peer changed");
        }
        // SAFETY: retained synchronize handle; zero wait never blocks.
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } != WAIT_TIMEOUT
            || identity(&self.process, self.pid)? != (self.created, self.session)
            || owner(&self.process)? != self.owner_sid
            || image_path_digest(&self.process)? != self.image_path_sha256
        {
            return Err("peer process changed");
        }
        Ok(())
    }
}
pub(super) fn connect(policy: &Policy) -> Result<(Stream, Guard), &'static str> {
    let Policy::Windows { endpoint, pid, created_filetime, session, owner_sid, image_path_sha256 } = policy else {
        return Err("unsupported explicit IPC policy");
    };
    let name = endpoint
        .strip_prefix("npipe:////./pipe/")
        .filter(|name| !name.is_empty() && name.len() <= 128 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)))
        .ok_or("invalid local pipe policy")?;
    let process = handle(*pid)?;
    let (created, actual_session) = identity(&process, *pid)?;
    if created_filetime.parse::<u64>().ok() != Some(created)
        || actual_session != *session
        || owner(&process)? != *owner_sid
        || image_path_digest(&process)? != *image_path_sha256
    {
        return Err("pipe peer policy mismatch");
    }
    // Pin the approved process object before connecting, then match the connected pipe to it.
    if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } != WAIT_TIMEOUT {
        return Err("peer process exited");
    }
    let stream = tokio::net::windows::named_pipe::ClientOptions::new().open(format!(r"\\.\pipe\{name}")).map_err(|_| "local connection unavailable")?;
    let mut server_pid = 0;
    let mut server_session = 0;
    // SAFETY: connected pipe handle remains owned throughout both queries.
    if unsafe { GetNamedPipeServerProcessId(stream.as_raw_handle(), &mut server_pid) } == 0
        || unsafe { GetNamedPipeServerSessionId(stream.as_raw_handle(), &mut server_session) } == 0
        || server_pid != *pid
        || server_session != *session
        || *pid == 0
    {
        return Err("pipe peer mismatch");
    }
    // Re-read the same pipe identity after obtaining the process handle to close PID reuse races.
    if unsafe { GetNamedPipeServerProcessId(stream.as_raw_handle(), &mut server_pid) } == 0 || server_pid != *pid {
        return Err("pipe peer changed");
    }
    // SAFETY: borrow the live pipe only long enough to obtain an independently owned duplicate.
    let pipe = unsafe { BorrowedHandle::borrow_raw(stream.as_raw_handle()) }.try_clone_to_owned().map_err(|_| "peer handle unavailable")?;
    let guard = Guard { process, created, session: *session, pid: *pid, pipe, owner_sid: owner_sid.clone(), image_path_sha256: image_path_sha256.clone() };
    guard.check()?;
    Ok((stream, guard))
}
#[cfg(test)]
pub(in crate::topology::adapters::docker_api) fn fixture_policy(endpoint: String) -> Policy {
    let pid = std::process::id();
    let process = handle(pid).unwrap();
    let (created, session) = identity(&process, pid).unwrap();
    Policy::Windows {
        endpoint,
        pid,
        created_filetime: created.to_string(),
        session,
        owner_sid: owner(&process).unwrap(),
        image_path_sha256: image_path_digest(&process).unwrap(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn real_named_pipe_matches_retained_peer_and_rejects_each_policy_mismatch() {
        for (index, field) in ["match", "pid", "created_filetime", "session", "owner_sid", "image_path_sha256"].iter().enumerate() {
            let name = format!("docker-api-peer-{}-{}-{index}", std::process::id(), crate::db::now_ms());
            let server = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(format!(r"\\.\pipe\{name}"))
                .unwrap();
            let mut value = serde_json::to_value(fixture_policy(format!("npipe:////./pipe/{name}"))).unwrap();
            match *field {
                "pid" => value[field] = serde_json::json!(std::process::id().wrapping_add(1)),
                "created_filetime" => value[field] = serde_json::json!("0"),
                "session" => value[field] = serde_json::json!(u32::MAX),
                "owner_sid" => value[field] = serde_json::json!("S-1-0-0"),
                "image_path_sha256" => value[field] = serde_json::json!("0".repeat(64)),
                _ => {}
            }
            let policy = serde_json::from_value(value).unwrap();
            if *field == "match" {
                let (client, guard) = connect(&policy).unwrap();
                server.connect().await.unwrap();
                guard.check().unwrap();
                drop(client);
                drop(guard);
            } else {
                assert!(connect(&policy).is_err(), "wrong {field} must be rejected");
            }
            drop(server);
        }
    }
    #[tokio::test]
    async fn real_pipe_server_exit_is_rejected_through_retained_process_handle() {
        use tokio::io::AsyncReadExt;
        let name = format!("docker-api-exit-{}-{}", std::process::id(), crate::db::now_ms());
        let program = std::path::PathBuf::from(std::env::var_os("SystemRoot").expect("native Windows system root"))
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let script = format!(
            "$p=[IO.Pipes.NamedPipeServerStream]::new('{name}',[IO.Pipes.PipeDirection]::InOut,1,[IO.Pipes.PipeTransmissionMode]::Byte,[IO.Pipes.PipeOptions]::Asynchronous);[Console]::Write(\"READY`n\");$p.WaitForConnection();$null=$p.ReadByte();$p.Dispose()"
        );
        let mut child = tokio::process::Command::new(program)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("owned fixture child");
        let result = async {
            let mut stdout = child.stdout.take().ok_or("fixture stdout absent")?;
            let mut ready = [0u8; 6];
            tokio::time::timeout(std::time::Duration::from_secs(10), stdout.read_exact(&mut ready))
                .await
                .map_err(|_| "fixture readiness timeout")?
                .map_err(|_| "fixture readiness failed")?;
            if &ready != b"READY\n" {
                return Err("fixture readiness invalid");
            }
            let pid = child.id().ok_or("fixture child identity absent")?;
            let process = handle(pid)?;
            let (created, session) = identity(&process, pid)?;
            let policy = Policy::Windows {
                endpoint: format!("npipe:////./pipe/{name}"),
                pid,
                created_filetime: created.to_string(),
                session,
                owner_sid: owner(&process)?,
                image_path_sha256: image_path_digest(&process)?,
            };
            let (client, guard) = connect(&policy)?;
            child.start_kill().map_err(|_| "owned fixture termination failed")?;
            tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
                .await
                .map_err(|_| "owned fixture wait timed out")?
                .map_err(|_| "owned fixture wait failed")?;
            let refused = guard.check().is_err();
            drop(client);
            drop(guard);
            if !refused {
                return Err("exited server peer was accepted");
            }
            Ok::<_, &'static str>(())
        }
        .await;
        let _ = child.start_kill();
        let reaped = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
        assert!(reaped.is_ok(), "owned fixture child must be reaped");
        result.unwrap();
    }
}
