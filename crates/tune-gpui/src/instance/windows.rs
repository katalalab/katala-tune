//! Windows: one first-instance named pipe, current-token SID DACL, local clients only.
//! No account database lookup; querying our own access token is sufficient.
#![allow(unsafe_code)]
use super::{ACK_TIMEOUT, IO_TIMEOUT};
use std::{
    io::{self, Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{null, null_mut},
    sync::Arc,
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{Pipes::*, Threading::*},
};
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
fn err() -> String {
    io::Error::last_os_error().to_string()
}
unsafe fn sid_text(sid: PSID) -> Result<String, String> {
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(err());
    }
    let mut length = 0;
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    let result = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    unsafe {
        LocalFree(text.cast());
    }
    Ok(result)
}
pub fn user() -> Result<String, String> {
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(err());
        }
        let token = OwnedHandle::from_raw_handle(token);
        let mut size = 0;
        GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut size);
        if size == 0 || size > 65536 {
            return Err("invalid token user size".into());
        }
        // Pointer-aligned buffer for TOKEN_USER and variable-sized SID.
        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        if GetTokenInformation(token.as_raw_handle(), TokenUser, buffer.as_mut_ptr().cast(), size, &mut size) == 0 {
            return Err(err());
        }
        sid_text((*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid)
    }
}
fn name(namespace: &str) -> Vec<u16> {
    wide(&format!(r"\\.\pipe\katala-gpui-{namespace}"))
}
pub enum Claim {
    Primary(Listener),
    Secondary,
}
pub struct Listener {
    pipe: Arc<OwnedHandle>,
}
pub struct Stream {
    pipe: Arc<OwnedHandle>,
    server: bool,
    deadline: Instant,
}
impl Stream {
    pub fn read_budget(&mut self, budget: std::time::Duration) {
        self.deadline = Instant::now() + budget;
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        if self.server {
            unsafe {
                DisconnectNamedPipe(self.pipe.as_raw_handle());
            }
        }
    }
}
pub fn claim(namespace: &str) -> Result<Claim, String> {
    unsafe {
        let mut descriptor = null_mut();
        let sid = user()?;
        let dacl = wide(&format!("O:{sid}D:P(A;;GA;;;{sid})"));
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(dacl.as_ptr(), 1, &mut descriptor, null_mut()) == 0 {
            return Err(err());
        }
        let security = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor, bInheritHandle: 0 };
        let pipe = CreateNamedPipeW(
            name(namespace).as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            500,
            &security,
        );
        let error = GetLastError();
        LocalFree(descriptor);
        if pipe == INVALID_HANDLE_VALUE {
            return if error == ERROR_ACCESS_DENIED || error == ERROR_PIPE_BUSY {
                Ok(Claim::Secondary)
            } else {
                Err(io::Error::from_raw_os_error(error as i32).to_string())
            };
        }
        Ok(Claim::Primary(Listener { pipe: Arc::new(OwnedHandle::from_raw_handle(pipe)) }))
    }
}
struct Connect<'a>(&'a OwnedHandle);
impl super::pipe_accept::Api for Connect<'_> {
    const CONNECTED: u32 = ERROR_PIPE_CONNECTED;
    const LISTENING: u32 = ERROR_PIPE_LISTENING;
    const NO_DATA: u32 = ERROR_NO_DATA;
    fn connect(&self) -> (bool, u32) {
        let ready = unsafe { ConnectNamedPipe(self.0.as_raw_handle(), null_mut()) } != 0;
        (ready, if ready { 0 } else { unsafe { GetLastError() } })
    }
    fn disconnect(&self) {
        unsafe {
            DisconnectNamedPipe(self.0.as_raw_handle());
        }
    }
}
impl Listener {
    pub fn accept(&self) -> Result<Option<Stream>, String> {
        if super::pipe_accept::poll(&Connect(&self.pipe)).map_err(|e| io::Error::from_raw_os_error(e as i32).to_string())? {
            Ok(Some(Stream { pipe: self.pipe.clone(), server: true, deadline: Instant::now() + IO_TIMEOUT }))
        } else {
            Ok(None)
        }
    }
}
pub fn connect(namespace: &str) -> Result<Stream, String> {
    unsafe {
        let deadline = Instant::now() + IO_TIMEOUT;
        let pipe = loop {
            let pipe = CreateFileW(
                name(namespace).as_ptr(),
                GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
                0,
                null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            );
            if pipe != INVALID_HANDLE_VALUE {
                break OwnedHandle::from_raw_handle(pipe);
            }
            if Instant::now() >= deadline {
                return Err("existing instance activation pipe is unavailable".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        };
        let (mut owner, mut descriptor) = (null_mut(), null_mut());
        let result = GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        );
        if result != ERROR_SUCCESS {
            return Err("cannot verify activation pipe owner".into());
        }
        let owner = sid_text(owner);
        LocalFree(descriptor);
        if owner? != user()? {
            return Err("foreign activation pipe owner".into());
        }
        let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
        if SetNamedPipeHandleState(pipe.as_raw_handle(), &mode, null(), null()) == 0 {
            return Err(err());
        }
        Ok(Stream { pipe: Arc::new(pipe), server: false, deadline: Instant::now() + ACK_TIMEOUT + IO_TIMEOUT })
    }
}
impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut count = 0;
            let ok = unsafe { ReadFile(self.pipe.as_raw_handle(), bytes.as_mut_ptr(), bytes.len() as u32, &mut count, null_mut()) };
            if ok != 0 && count > 0 {
                return Ok(count as usize);
            }
            let code = unsafe { GetLastError() };
            if ok == 0 && code != ERROR_NO_DATA {
                return Err(io::Error::from_raw_os_error(code as i32));
            }
            if Instant::now() >= self.deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut count = 0;
        if unsafe { WriteFile(self.pipe.as_raw_handle(), bytes.as_ptr(), bytes.len() as u32, &mut count, null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // PIPE_NOWAIT prevents a malicious reader from blocking the worker.
        if count == 0 && !bytes.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(count as usize)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
