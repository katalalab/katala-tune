//! Dedicated local first-instance pipe. No connection, accept loop or worker.
#![allow(unsafe_code)]
use super::AdmissionError;
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::null_mut,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{Pipes::*, Threading::*},
};
pub(super) type Claim = OwnedHandle;
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
unsafe fn sid_text(sid: PSID) -> Result<String, AdmissionError> {
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(AdmissionError::Unavailable);
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
fn user() -> Result<String, AdmissionError> {
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(AdmissionError::Unavailable);
        }
        let token = OwnedHandle::from_raw_handle(token);
        let mut size = 0;
        GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut size);
        if size == 0 || size > 65536 {
            return Err(AdmissionError::Unavailable);
        }
        // Pointer-aligned buffer for TOKEN_USER and variable-sized SID.
        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        if GetTokenInformation(token.as_raw_handle(), TokenUser, buffer.as_mut_ptr().cast(), size, &mut size) == 0 {
            return Err(AdmissionError::Unavailable);
        }
        sid_text((*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid)
    }
}

pub(super) fn claim() -> Result<Claim, AdmissionError> {
    unsafe {
        let sid = user()?;
        let name = wide(&format!(r"\\.\pipe\katala-tune-collector-v1-{sid}"));
        let dacl = wide(&format!("O:{sid}D:P(A;;GA;;;{sid})"));
        let mut descriptor = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(dacl.as_ptr(), 1, &mut descriptor, null_mut()) == 0 {
            return Err(AdmissionError::Unavailable);
        }
        let security = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor, bInheritHandle: 0 };
        let pipe = CreateNamedPipeW(
            name.as_ptr(),
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
            return Err(if error == ERROR_ACCESS_DENIED || error == ERROR_PIPE_BUSY { AdmissionError::BusyOrDenied } else { AdmissionError::Unavailable });
        }
        Ok(OwnedHandle::from_raw_handle(pipe))
    }
}
