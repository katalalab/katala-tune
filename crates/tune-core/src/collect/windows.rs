//! Owned Windows process boundary. No PID-based termination or shared Job mutation.
#![allow(unsafe_code)] // Win32 ownership/identity calls only; each boundary documents its pointers and lifetimes.

use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_PARAMETER, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Globalization::{CSTR_EQUAL, CSTR_LESS_THAN, CompareStringOrdinal},
    Security::{
        Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom},
        GetTokenInformation, SECURITY_ATTRIBUTES, SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, GetFullPathNameW, OPEN_EXISTING},
    System::{
        JobObjects::{
            CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
            SetInformationJobObject, TerminateJobObject,
        },
        Pipes::GetNamedPipeClientProcessId,
        RemoteDesktop::ProcessIdToSessionId,
        SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW},
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
            GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess, GetProcessTimes, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
            OpenProcess, OpenProcessToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, PROCESS_QUERY_INFORMATION,
            ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

use super::windows_pin::PinnedWindowsExecutable;
use super::windows_quiescence::{CLEANUP, Decision as QuiescenceDecision, Observation as QuiescenceObservation, Quiescence};
use super::{BoundedResult, ProcessIdentity, ProcessReceipt, ProcessStop, WindowsMetadataExitPolicy};

const POLL: Duration = Duration::from_millis(10);
const MAX_ACTIVE: usize = 256;
const MAX_OBSERVED: usize = 4096;
const SYNCHRONIZE_PROCESS: u32 = 0x0010_0000;
const GENERIC_READ_ACCESS: u32 = 0x8000_0000;
const GENERIC_WRITE_ACCESS: u32 = 0x4000_0000;

#[cfg(test)]
#[path = "windows_diagnostics.rs"]
mod diagnostics;

fn os_error() -> io::Error {
    io::Error::last_os_error()
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidInput)
}
fn check(ok: i32) -> io::Result<()> {
    if ok == 0 { Err(os_error()) } else { Ok(()) }
}
fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut units: Vec<_> = value.encode_wide().collect();
    if units.contains(&0) {
        return Err(invalid());
    }
    units.push(0);
    Ok(units)
}
fn owned(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        return Err(os_error());
    }
    // SAFETY: callers transfer a fresh owned Win32 handle, never a pseudo/borrowed handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

fn nonce() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    // SAFETY: writable fixed buffer; null provider is required with SYSTEM_PREFERRED_RNG.
    let status = unsafe { BCryptGenRandom(null_mut(), bytes.as_mut_ptr(), bytes.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status < 0 {
        return Err(io::Error::from(io::ErrorKind::Other));
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

struct LocalAllocation(*mut std::ffi::c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: allocation came from a documented LocalAlloc-family security conversion API.
        unsafe { LocalFree(self.0) };
    }
}

fn pipe_security() -> io::Result<LocalAllocation> {
    let mut raw = null_mut();
    // SAFETY: current process pseudo-handle is borrowed; output token requests query access only.
    check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) })?;
    let token = owned(raw)?;
    #[repr(C)]
    struct UserBuffer {
        user: TOKEN_USER,
        sid: [u8; SECURITY_MAX_SID_SIZE as usize],
    }
    let mut buffer = UserBuffer { user: TOKEN_USER::default(), sid: [0; SECURITY_MAX_SID_SIZE as usize] };
    let mut used = 0;
    // SAFETY: aligned TOKEN_USER buffer includes the maximum SID storage; token remains live.
    check(unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, std::ptr::from_mut(&mut buffer).cast(), size_of::<UserBuffer>() as u32, &mut used) })?;
    if used < size_of::<TOKEN_USER>() as u32 || used > size_of::<UserBuffer>() as u32 || buffer.user.User.Sid.is_null() {
        return Err(invalid());
    }
    let mut text = null_mut();
    // SAFETY: successful TokenUser provides a valid SID within the live buffer.
    check(unsafe { ConvertSidToStringSidW(buffer.user.User.Sid, &mut text) })?;
    let text_owner = LocalAllocation(text.cast());
    let mut sid = String::new();
    for offset in 0..256 {
        // SAFETY: API returned NUL-terminated SID text; stop at its first NUL.
        let unit = unsafe { *text.add(offset) };
        if unit == 0 {
            break;
        }
        if unit != u16::from(b'S') && unit != u16::from(b'-') && !(u16::from(b'0')..=u16::from(b'9')).contains(&unit) {
            return Err(invalid());
        }
        sid.push(char::from_u32(u32::from(unit)).ok_or_else(invalid)?);
    }
    drop(text_owner);
    if !sid.starts_with("S-") || sid.len() >= 255 {
        return Err(invalid());
    }
    // Protected DACL: only the current token's user and SYSTEM may access the private pipe.
    let sddl = wide(OsStr::new(&format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})")))?;
    let mut descriptor = null_mut();
    // SAFETY: NUL-terminated SDDL and valid output pointer; LocalFree owns the resulting allocation.
    check(unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut descriptor, null_mut()) })?;
    Ok(LocalAllocation(descriptor))
}

struct Pipe {
    server: NamedPipeServer,
    client: OwnedHandle,
}
fn pipe(session: &str, label: &str, to_child: bool, security: &LocalAllocation) -> io::Result<Pipe> {
    let name = OsString::from(format!(r"\\.\pipe\katala-tune-{session}-{label}"));
    let attrs = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: security.0, bInheritHandle: 0 };
    let mut options = ServerOptions::new();
    options.access_inbound(!to_child).access_outbound(to_child).first_pipe_instance(true).max_instances(1).reject_remote_clients(true);
    // SAFETY: security descriptor and aligned attributes remain live throughout pipe creation. Parent server is non-inheritable.
    let server = unsafe { options.create_with_security_attributes_raw(&name, std::ptr::from_ref(&attrs).cast_mut().cast()) }?;
    let name = wide(&name)?;
    let child_attrs = SECURITY_ATTRIBUTES { bInheritHandle: 1, ..attrs };
    // SAFETY: our private, single-instance server already exists. Only this synchronous client endpoint is inheritable.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            if to_child { GENERIC_READ_ACCESS } else { GENERIC_WRITE_ACCESS },
            0,
            &child_attrs,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    let client = owned(raw)?;
    let mut client_pid = 0;
    // SAFETY: server is live; connected client was opened by this process before any child is created.
    check(unsafe { GetNamedPipeClientProcessId(server.as_raw_handle(), &mut client_pid) })?;
    // SAFETY: no arguments or ownership transfer.
    if client_pid != unsafe { GetCurrentProcessId() } {
        return Err(invalid());
    }
    Ok(Pipe { server, client })
}

struct Attributes {
    storage: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn new(jobs: &[HANDLE; 1], handles: &[HANDLE; 3]) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: sizing call with null list; Windows writes the required allocation size.
        let first = unsafe { InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut bytes) };
        if first != 0 || os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) || bytes == 0 || bytes > 65536 {
            return Err(invalid());
        }
        let mut result = Self { storage: vec![0; bytes.div_ceil(size_of::<usize>())], initialized: false };
        // SAFETY: usize-aligned storage has at least the API-requested byte size.
        check(unsafe { InitializeProcThreadAttributeList(result.raw(), 2, 0, &mut bytes) })?;
        result.initialized = true;
        // SAFETY: attribute value arrays are retained by spawn_owned until this list is dropped.
        let update = unsafe {
            check(UpdateProcThreadAttribute(
                result.raw(),
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                jobs.as_ptr().cast(),
                size_of::<HANDLE>(),
                null_mut(),
                null(),
            ))
            .and_then(|()| {
                check(UpdateProcThreadAttribute(
                    result.raw(),
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_ptr().cast(),
                    size_of_val(handles),
                    null_mut(),
                    null(),
                ))
            })
        };
        update?;
        Ok(result)
    }
    fn raw(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: attribute list was initialized in the aligned live storage; values still outlive this destructor.
        if self.initialized {
            unsafe { DeleteProcThreadAttributeList(self.raw()) };
        }
    }
}

struct Process {
    handle: OwnedHandle,
    identity: ProcessIdentity,
}
fn identity(handle: &OwnedHandle, pid: u32) -> io::Result<ProcessIdentity> {
    identity_raw(handle.as_raw_handle(), pid)
}
pub(super) fn current_process_identity() -> io::Result<ProcessIdentity> {
    // SAFETY: borrow our current-process pseudo-handle for read-only identity;
    // it is never closed, and the process remains live throughout this query.
    identity_raw(unsafe { GetCurrentProcess() }, unsafe { GetCurrentProcessId() })
}
fn identity_raw(handle: HANDLE, pid: u32) -> io::Result<ProcessIdentity> {
    let mut created = Default::default();
    let mut exit = Default::default();
    let mut kernel = Default::default();
    let mut user = Default::default();
    // SAFETY: retained handle has query rights; all FILETIME outputs are valid writable values.
    check(unsafe { GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) })?;
    let created = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    let mut session = 0;
    // SAFETY: retained process object and query rights preserve identity while session is queried.
    check(unsafe { ProcessIdToSessionId(pid, &mut session) })?;
    if pid == 0 || created == 0 {
        return Err(invalid());
    }
    Ok(ProcessIdentity { pid, created_filetime: created.to_string(), os_session: session })
}
fn exited(handle: &OwnedHandle) -> io::Result<bool> {
    // SAFETY: owned process handle remains live; zero timeout never blocks.
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(os_error()),
    }
}

#[cfg(test)]
type AccountingHook = Box<dyn FnOnce(&Job, &ProcessReceipt) -> io::Result<()> + Send>;
#[cfg(test)]
type PidListHook = Box<dyn FnOnce(&Job, &[u32]) -> io::Result<()> + Send>;

fn complete_pid_list(assigned: u32, count: u32, pids: &[usize]) -> io::Result<Vec<u32>> {
    if assigned != count || count as usize > MAX_ACTIVE || count as usize > pids.len() {
        return Err(invalid());
    }
    let mut result = Vec::with_capacity(count as usize);
    for value in &pids[..count as usize] {
        let pid = u32::try_from(*value).map_err(|_| invalid())?;
        if pid == 0 || result.contains(&pid) {
            return Err(invalid());
        }
        result.push(pid);
    }
    Ok(result)
}

struct Job {
    handle: OwnedHandle,
    processes: Vec<Process>,
    root_exit_at: Option<std::time::Instant>,
    #[cfg(test)]
    after_first_accounting: Option<AccountingHook>,
    #[cfg(test)]
    after_pid_list: Option<PidListHook>,
    #[cfg(test)]
    injected_open_error: Option<(u32, i32)>,
}
impl Job {
    fn new() -> io::Result<Self> {
        // SAFETY: null name creates a fresh unnamed Job; null attributes prevent inheritance.
        let handle = owned(unsafe { CreateJobObjectW(null(), null()) })?;
        let job = Self {
            handle,
            processes: Vec::new(),
            root_exit_at: None,
            #[cfg(test)]
            after_first_accounting: None,
            #[cfg(test)]
            after_pid_list: None,
            #[cfg(test)]
            injected_open_error: None,
        };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: exact documented structure and size; only the newly owned Job is modified.
        check(unsafe {
            SetInformationJobObject(
                job.handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                size_of_val(&limits) as u32,
            )
        })?;
        Ok(job)
    }
    fn terminate(&self) -> io::Result<()> {
        // SAFETY: terminate only this new Job, never a parent/shared Job or a PID.
        check(unsafe { TerminateJobObject(self.handle.as_raw_handle(), 124) })
    }
    fn accounting(&self, receipt: &mut ProcessReceipt) -> io::Result<()> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: explicit owned Job and correctly sized aligned output structure; never use null hJob.
        check(unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut accounting).cast(),
                size_of_val(&accounting) as u32,
                null_mut(),
            )
        })?;
        receipt.total_processes = accounting.TotalProcesses;
        receipt.job_empty = accounting.ActiveProcesses == 0;
        Ok(())
    }
    fn process_ids(&self) -> io::Result<Vec<u32>> {
        #[repr(C)]
        struct Pids {
            assigned: u32,
            count: u32,
            pids: [usize; MAX_ACTIVE],
        }
        let mut pids = Pids { assigned: 0, count: 0, pids: [0; MAX_ACTIVE] };
        // SAFETY: flexible-array output has a fixed cap with correct pointer-size alignment.
        check(unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle(),
                JobObjectBasicProcessIdList,
                std::ptr::from_mut(&mut pids).cast(),
                size_of_val(&pids) as u32,
                null_mut(),
            )
        })?;
        complete_pid_list(pids.assigned, pids.count, &pids.pids)
    }
    fn observe(&mut self, receipt: &mut ProcessReceipt) -> io::Result<()> {
        self.accounting(receipt)?;
        #[cfg(test)]
        if let Some(hook) = self.after_first_accounting.take() {
            hook(self, receipt)?;
        }
        let pids = self.process_ids()?;
        #[cfg(test)]
        if let Some(hook) = self.after_pid_list.take() {
            hook(self, &pids)?;
        }
        for pid in pids {
            if self.processes.iter().any(|p| p.identity.pid == pid) {
                continue;
            }
            if self.processes.len() >= MAX_OBSERVED {
                return Err(invalid());
            }
            let handle = match self.open_candidate(pid) {
                Ok(handle) => handle,
                Err(error) => {
                    // A snapshot PID can exit before OpenProcess. Only the specific
                    // disappearance error plus a complete fresh query of this owned
                    // Job permits skipping it. No identity/exit is fabricated; actual
                    // Job emptiness and retained handles still gate completion later.
                    if error.raw_os_error() != Some(ERROR_INVALID_PARAMETER as i32) || self.process_ids()?.contains(&pid) {
                        return Err(error);
                    }
                    continue;
                }
            };
            let mut member = 0;
            // SAFETY: recheck membership on the opened object, closing enumeration/PID reuse races.
            check(unsafe { IsProcessInJob(handle.as_raw_handle(), self.handle.as_raw_handle(), &mut member) })?;
            if member == 0 {
                return Err(invalid());
            }
            let identity = identity(&handle, pid)?;
            receipt.observed.push(identity.clone());
            self.processes.push(Process { handle, identity });
        }
        let root = self.processes.first().ok_or_else(invalid)?;
        let root_observed_at = Instant::now().into_std();
        receipt.root_exited = exited(&root.handle)?;
        if receipt.root_exited {
            // Use the start of the successful zero-wait query conservatively:
            // scheduling after it must not move either completion clock later.
            self.root_exit_at.get_or_insert(root_observed_at);
            let mut code = 0;
            // SAFETY: retained root has signaled; 259 is a valid exit value, never a liveness test.
            check(unsafe { GetExitCodeProcess(root.handle.as_raw_handle(), &mut code) })?;
            receipt.root_exit_code = Some(code as i32);
            // The initial accounting may predate root exit. Only a subsequent
            // sample can establish that descendants remain after that exit.
            self.accounting(receipt)?;
        }
        // Sample retained members after root exit; an earlier nonsignaled root
        // must not be mistaken for a descendant that remains after root exit.
        receipt.observed_exited = !self.processes.is_empty();
        for process in &self.processes {
            receipt.observed_exited &= exited(&process.handle)?;
        }
        Ok(())
    }
    fn open_candidate(&mut self, pid: u32) -> io::Result<OwnedHandle> {
        #[cfg(test)]
        if let Some((expected, error)) = self.injected_open_error.take() {
            assert_eq!(pid, expected, "fault injection targets only the captured fixture member");
            return Err(io::Error::from_raw_os_error(error));
        }
        // SAFETY: observation only; retain query/synchronize handle, no terminate rights requested.
        owned(unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | SYNCHRONIZE_PROCESS, 0, pid) })
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.terminate(); // cancellation fallback, plus KILL_ON_JOB_CLOSE on the sole non-inherited handle.
    }
}

#[cfg(test)]
fn diagnostic_members(job: &Job) -> Vec<diagnostics::Member> {
    use windows_sys::Win32::{Foundation::WAIT_FAILED, System::Threading::QueryFullProcessImageNameW};
    let fixture = std::env::current_exe().ok();
    let fixture_basename = fixture.as_deref().and_then(Path::file_name).and_then(OsStr::to_str);
    job.processes
        .iter()
        .take(diagnostics::MEMBER_LIMIT)
        .enumerate()
        .map(|(ordinal, process)| {
            let handle = process.handle.as_raw_handle();
            let mut in_job = 0;
            // SAFETY: read-only query of the same retained process and owned Job, never a PID lookup.
            let member_ok = unsafe { IsProcessInJob(handle, job.handle.as_raw_handle(), &mut in_job) } != 0;
            let membership_error = (!member_ok).then(|| os_error().raw_os_error()).flatten();
            // SAFETY: zero wait on the exact retained handle; exit code is not used as liveness evidence.
            let wait = unsafe { WaitForSingleObject(handle, 0) };
            let wait_error = (wait == WAIT_FAILED).then(|| os_error().raw_os_error()).flatten();
            let mut code = 0;
            // SAFETY: same query-capable retained handle and valid writable output.
            let code_ok = unsafe { GetExitCodeProcess(handle, &mut code) } != 0;
            let exit_error = (!code_ok).then(|| os_error().raw_os_error()).flatten();
            let mut image = vec![0u16; 32768];
            let mut image_len = image.len() as u32;
            // SAFETY: bounded UTF-16 output, same retained query-capable process handle.
            let image_ok = unsafe { QueryFullProcessImageNameW(handle, 0, image.as_mut_ptr(), &mut image_len) } != 0;
            let image_error = (!image_ok).then(|| os_error().raw_os_error()).flatten();
            // Paths and arbitrary basenames stay local and are discarded into a closed enum.
            let image = (image_ok && (image_len as usize) < image.len()).then(|| PathBuf::from(OsString::from_wide(&image[..image_len as usize])));
            let basename = image.as_deref().and_then(Path::file_name).and_then(OsStr::to_str);
            diagnostics::Member {
                ordinal,
                primary_root: ordinal == 0,
                in_job: member_ok.then_some(in_job != 0),
                membership_error,
                wait,
                wait_error,
                exit_code: code_ok.then_some(code),
                exit_error,
                image: diagnostics::image_kind(basename, fixture_basename),
                image_error,
            }
        })
        .collect()
}

#[cfg(test)]
struct DiagnosticTrace {
    started: Instant,
    invocation: u64,
    first_stop: bool,
}

#[cfg(test)]
impl DiagnosticTrace {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self { started: Instant::now(), invocation: NEXT.fetch_add(1, Ordering::Relaxed), first_stop: false }
    }
    fn record_stop(&mut self, job: &Job, receipt: &ProcessReceipt, requested: bool) {
        if receipt.stop != ProcessStop::Completed && !self.first_stop {
            self.first_stop = true;
            self.emit(diagnostics::Event::FirstStop, job, receipt, requested);
        }
    }
    fn emit(&self, event: diagnostics::Event, job: &Job, receipt: &ProcessReceipt, requested: bool) {
        let members = diagnostic_members(job);
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: diagnostic-only observation of the same owned Job into a correctly sized output.
        let ok = unsafe {
            QueryInformationJobObject(
                job.handle.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut accounting).cast(),
                size_of_val(&accounting) as u32,
                null_mut(),
            )
        } != 0;
        let error = (!ok).then(|| os_error().raw_os_error()).flatten();
        let line = diagnostics::Snapshot {
            event,
            invocation: self.invocation,
            elapsed_ms: self.started.elapsed().as_millis(),
            receipt: diagnostics::ReceiptState {
                stop: match receipt.stop {
                    ProcessStop::SetupFailed => diagnostics::Stop::SetupFailed,
                    ProcessStop::SpawnFailed => diagnostics::Stop::SpawnFailed,
                    ProcessStop::Completed => diagnostics::Stop::Completed,
                    ProcessStop::Timeout => diagnostics::Stop::Timeout,
                    ProcessStop::OutputLimit => diagnostics::Stop::OutputLimit,
                    ProcessStop::LingeringDescendants => diagnostics::Stop::LingeringDescendants,
                    ProcessStop::IoFailure => diagnostics::Stop::IoFailure,
                    ProcessStop::CleanupUnconfirmed => diagnostics::Stop::CleanupUnconfirmed,
                },
                root_exited: receipt.root_exited,
                job_empty: receipt.job_empty,
                observed_exited: receipt.observed_exited,
                stdout_eof: receipt.stdout_eof,
                stderr_eof: receipt.stderr_eof,
                writer_finished: receipt.writer_finished,
                termination_requested: requested,
            },
            accounting: diagnostics::Accounting { active: ok.then_some(accounting.ActiveProcesses), total: ok.then_some(accounting.TotalProcesses), error },
            retained_count: job.processes.len(),
            members: &members,
        }
        .line();
        eprintln!("{line}");
    }
}

fn quote(arg: &OsStr, output: &mut Vec<u16>) -> io::Result<()> {
    output.push(u16::from(b'"'));
    let mut slashes = 0;
    for unit in arg.encode_wide() {
        if unit == 0 {
            return Err(invalid());
        }
        if unit == u16::from(b'\\') {
            slashes += 1;
            continue;
        }
        output.extend(std::iter::repeat_n(u16::from(b'\\'), if unit == u16::from(b'"') { slashes * 2 + 1 } else { slashes }));
        output.push(unit);
        slashes = 0;
    }
    output.extend(std::iter::repeat_n(u16::from(b'\\'), slashes * 2));
    output.push(u16::from(b'"'));
    Ok(())
}
fn command_line(executable: &Path, command: &std::process::Command) -> io::Result<Vec<u16>> {
    let mut output = Vec::new();
    quote(executable.as_os_str(), &mut output)?;
    for arg in command.get_args() {
        output.push(u16::from(b' '));
        quote(arg, &mut output)?;
    }
    if output.len() >= 32767 {
        return Err(invalid());
    }
    output.push(0);
    Ok(output)
}
fn compare_names(left: &OsStr, right: &OsStr) -> Ordering {
    let left: Vec<_> = left.encode_wide().collect();
    let right: Vec<_> = right.encode_wide().collect();
    // SAFETY: read-only UTF-16 slices and explicit lengths; environment names are bounded by Windows.
    match unsafe { CompareStringOrdinal(left.as_ptr(), left.len() as i32, right.as_ptr(), right.len() as i32, 1) } {
        CSTR_EQUAL => Ordering::Equal,
        CSTR_LESS_THAN => Ordering::Less,
        _ => Ordering::Greater,
    }
}
fn environment(command: &std::process::Command, entries: &[(OsString, OsString)]) -> io::Result<Vec<u16>> {
    // Command does not expose its env_clear state. A separate complete map is the
    // sole authority; reject edits rather than silently ignoring/merging them.
    if command.get_envs().next().is_some() {
        return Err(invalid());
    }
    let mut vars: Vec<_> = entries.iter().collect();
    for (name, value) in &vars {
        if name.len() > 1024 * 1024 || value.len() > 1024 * 1024 {
            return Err(invalid());
        }
        let units = wide(name)?;
        // GetEnvironmentStringsW includes the documented per-drive current
        // directory keys (=C:). Preserve that native state in a complete block;
        // ordinary names containing '=' and malformed pseudo-keys stay invalid.
        let drive_directory = units.len() == 4
            && units[0] == u16::from(b'=')
            && ((u16::from(b'A')..=u16::from(b'Z')).contains(&units[1]) || (u16::from(b'a')..=u16::from(b'z')).contains(&units[1]))
            && units[2] == u16::from(b':');
        if units.len() == 1 || (units.contains(&u16::from(b'=')) && !drive_directory) {
            return Err(invalid());
        }
        wide(value)?;
    }
    vars.sort_by(|(a, _), (b, _)| compare_names(a, b));
    if vars.windows(2).any(|pair| compare_names(&pair[0].0, &pair[1].0) == Ordering::Equal) {
        return Err(invalid());
    }
    let mut output = Vec::new();
    for (key, value) in vars {
        let mut key = wide(key)?;
        key.pop();
        let value = wide(value)?;
        output.extend(key);
        output.push(u16::from(b'='));
        output.extend(value);
        if output.len() > 1024 * 1024 {
            return Err(invalid());
        }
    }
    if output.is_empty() {
        output.push(0);
    }
    output.push(0);
    Ok(output)
}
fn path_entries(path: &OsStr) -> impl Iterator<Item = PathBuf> + '_ {
    // Match Rust Command: an empty PATH entry must not introduce a cwd executable search.
    std::env::split_paths(path).filter(|entry| !entry.as_os_str().is_empty())
}

fn executable(command: &std::process::Command, environment: &[(OsString, OsString)]) -> io::Result<PathBuf> {
    let program = Path::new(command.get_program());
    let extension = program.extension().unwrap_or_default().to_string_lossy();
    if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
        return Err(invalid());
    }
    let name = if extension.is_empty() {
        let mut n = program.as_os_str().to_owned();
        n.push(".exe");
        PathBuf::from(n)
    } else {
        program.to_owned()
    };
    if name.components().count() > 1 || name.is_absolute() {
        return name.canonicalize().and_then(|p| if p.is_file() { Ok(p) } else { Err(invalid()) });
    }
    let child_path = environment.iter().find(|(k, _)| compare_names(k, OsStr::new("PATH")) == Ordering::Equal).map(|(_, v)| v);
    let mut paths = Vec::new();
    if let Some(path) = child_path {
        paths.extend(path_entries(path));
    }
    if let Some(parent) = std::env::current_exe()?.parent() {
        paths.push(parent.to_owned());
    }
    for system in [true, false] {
        let mut buffer = [0u16; 32768];
        // SAFETY: writable bounded UTF-16 output; these APIs only read system locations.
        let count = unsafe {
            if system { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } else { GetWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) }
        };
        if count == 0 || count as usize >= buffer.len() {
            return Err(os_error());
        }
        use std::os::windows::ffi::OsStringExt;
        paths.push(PathBuf::from(OsString::from_wide(&buffer[..count as usize])));
    }
    paths.into_iter().map(|p| p.join(&name)).find(|p| p.is_file()).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?.canonicalize()
}

// Match the short-path compatibility rule used by Rust Command. A verbatim path
// cannot simply lose its prefix: Win32 normalization could select a different file.
fn user_executable_path(path: PathBuf) -> io::Result<PathBuf> {
    let original = wide(path.as_os_str())?;
    if original.len() > 260 {
        return Ok(path);
    }
    let prefix: Vec<_> = r"\\?\".encode_utf16().collect();
    let unc_prefix: Vec<_> = r"\\?\UNC\".encode_utf16().collect();
    let candidate = if original.starts_with(&unc_prefix) {
        let mut units = vec![u16::from(b'\\'); 2];
        units.extend_from_slice(&original[unc_prefix.len()..]);
        units
    } else if original.starts_with(&prefix)
        && original.len() >= 8
        && ((u16::from(b'A')..=u16::from(b'Z')).contains(&original[4]) || (u16::from(b'a')..=u16::from(b'z')).contains(&original[4]))
        && original[5] == u16::from(b':')
        && original[6] == u16::from(b'\\')
    {
        original[prefix.len()..].to_vec()
    } else {
        return Ok(path);
    };
    let mut normalized = [0u16; 261];
    // SAFETY: candidate is an absolute, NUL-terminated UTF-16 drive/UNC path;
    // the output is writable and bounded. This lexical query does not open the path.
    let count = unsafe { GetFullPathNameW(candidate.as_ptr(), normalized.len() as u32, normalized.as_mut_ptr(), null_mut()) } as usize;
    if count == 0 {
        return Err(os_error());
    }
    if count < normalized.len() && normalized[..count] == candidate[..candidate.len() - 1] {
        Ok(PathBuf::from(OsString::from_wide(&candidate[..candidate.len() - 1])))
    } else {
        Ok(path)
    }
}

struct SpawnContext<'a> {
    command: &'a std::process::Command,
    environment: &'a [(OsString, OsString)],
    deadline: Instant,
    pin: Option<&'a PinnedWindowsExecutable>,
}

fn spawn_owned(context: SpawnContext<'_>, job: &Job, stdio: [&OwnedHandle; 3], spawn_failed: &mut bool) -> io::Result<(Process, OwnedHandle)> {
    let SpawnContext { command, environment: values, deadline, pin } = context;
    let env = environment(command, values)?;
    let executable = if let Some(pin) = pin {
        if command.get_program() != pin.executable().as_os_str() {
            return Err(invalid());
        }
        // The GUID remains the caller's authority. This fixed DOS locator was
        // derived once from its held leaf and must still resolve to that file ID.
        // It is used for both lpApplicationName and argv[0], with no retry.
        pin.launch_path()?.to_path_buf()
    } else {
        user_executable_path(executable(command, values)?)?
    };
    let application = wide(executable.as_os_str())?;
    let mut argv = command_line(&executable, command)?;
    let cwd = command.get_current_dir().map(|p| wide(p.as_os_str())).transpose()?;
    // Value arrays are declared before Attributes and remain alive until its destructor.
    let jobs = [job.handle.as_raw_handle()];
    let handles = stdio.map(AsRawHandle::as_raw_handle);
    let mut attributes = Attributes::new(&jobs, &handles)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.raw();
    let mut created = PROCESS_INFORMATION::default();
    if Instant::now() >= deadline {
        return Err(io::Error::from(io::ErrorKind::TimedOut));
    }
    // SAFETY: all buffers/attribute values survive CreateProcess and attribute deletion. Only three explicit handles inherit.
    // JOB_LIST assigns this fresh Job before any child instruction; suspended creation additionally permits identity-before-run.
    let spawned = unsafe {
        CreateProcessW(
            application.as_ptr(),
            argv.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            env.as_ptr().cast(),
            cwd.as_ref().map_or(null(), |p| p.as_ptr()),
            &startup.StartupInfo,
            &mut created,
        )
    };
    if spawned == 0 {
        // Preserve GetLastError before any test-only formatting or IO changes it.
        let error = os_error();
        #[cfg(test)]
        eprintln!("owned_create_process_error_code={}", error.raw_os_error().unwrap_or(-1));
        *spawn_failed = true;
        return Err(error);
    }
    let handle = owned(created.hProcess)?;
    let thread = owned(created.hThread)?;
    if let Some(pin) = pin {
        // The actual atomically assigned root is still suspended. Never resume
        // an image that does not match the retained, deny-write/delete lease.
        pin.verify_process(handle.as_raw_handle())?;
    }
    let identity = identity(&handle, created.dwProcessId)?;
    Ok((Process { handle, identity }, thread))
}

#[derive(Default)]
struct PipeOutput {
    bytes: Vec<u8>,
    eof: bool,
    limited: bool,
    failed: bool,
}
async fn drain(mut pipe: NamedPipeServer, limit: usize, signal: mpsc::Sender<()>) -> PipeOutput {
    let mut output = PipeOutput::default();
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) => {
                output.eof = true;
                break;
            }
            Ok(count) => {
                let room = limit.saturating_sub(output.bytes.len());
                output.bytes.extend_from_slice(&chunk[..count.min(room)]);
                if count > room && !output.limited {
                    output.limited = true;
                    let _ = signal.try_send(());
                }
            }
            Err(_) => {
                output.failed = true;
                break;
            }
        }
    }
    output
}
struct Tasks {
    writer: Option<JoinHandle<io::Result<()>>>,
    stdout: Option<JoinHandle<PipeOutput>>,
    stderr: Option<JoinHandle<PipeOutput>>,
}
impl Drop for Tasks {
    fn drop(&mut self) {
        if let Some(task) = &self.writer {
            task.abort();
        }
        if let Some(task) = &self.stdout {
            task.abort();
        }
        if let Some(task) = &self.stderr {
            task.abort();
        }
    }
}

// A diagnostic snapshot predicate, not an immediate lifecycle verdict.
#[cfg(test)]
fn lingering_descendants(receipt: &ProcessReceipt) -> bool {
    receipt.root_exited && !receipt.job_empty && !receipt.observed_exited
}

#[derive(Default)]
struct CleanupState {
    deadline: Option<Instant>,
    termination_requested: bool,
}

struct CleanupStep {
    terminate: bool,
    expired: bool,
}

impl CleanupState {
    fn advance(&mut self, receipt: &mut ProcessReceipt, now: Instant) -> CleanupStep {
        if self.deadline.is_none() && (receipt.stop != ProcessStop::Completed || receipt.root_exited) {
            self.deadline = Some(now + CLEANUP);
        }
        let expired = self.deadline.is_some_and(|end| now >= end);
        if expired && receipt.stop == ProcessStop::Completed {
            receipt.stop = ProcessStop::CleanupUnconfirmed;
        }
        // Decide after every stop mutation, including expiry. Record the one-shot
        // request before the caller attempts termination; Drop remains a fallback.
        let terminate = receipt.stop != ProcessStop::Completed && !self.termination_requested;
        if terminate {
            self.termination_requested = true;
        }
        CleanupStep { terminate, expired }
    }
}

pub(super) async fn run(
    command: &std::process::Command,
    environment: &[(OsString, OsString)],
    input: &[u8],
    deadline: std::time::Instant,
    stdout_limit: usize,
    stderr_limit: usize,
) -> BoundedResult {
    run_inner(command, environment, input, deadline, stdout_limit, stderr_limit, None).await.0
}

// No constructor, serde, Clone or conversion from public diagnostic data exists.
// Retention is intentional even after exit: only consuming/dropping completion
// releases the empty Job, every observed process handle, and the pinned image.
struct WindowsOsExitWitness {
    _job: Job,
    pin: Arc<PinnedWindowsExecutable>,
    input_sha256: String,
    stdout_sha256: String,
    artifact_sha256: String,
    deadline: std::time::Instant,
}

/// Move-only successful OS observation, bound to exact request/response bytes.
/// This local capability cannot be reconstructed from a wire receipt or JSON.
///
/// ```compile_fail
/// use tune_core::collect::{OwnedWindowsCompletion, ProcessReceipt};
/// let diagnostic = ProcessReceipt::default();
/// let _: OwnedWindowsCompletion = diagnostic.into();
/// ```
pub struct OwnedWindowsCompletion {
    answer: BoundedResult,
    witness: WindowsOsExitWitness,
}

impl OwnedWindowsCompletion {
    /// Read-only OS diagnostics. These public facts are not a capability issuer.
    pub fn diagnostic(&self) -> &BoundedResult {
        &self.answer
    }

    /// Consume this invocation once, admitting only its exact input and image.
    /// Response hashing and admission must also finish before the original deadline.
    #[allow(clippy::result_large_err)] // Keep failure diagnostics identical to the bounded transport result.
    pub fn into_verified_output(mut self, expected_input: &[u8], expected_artifact: &str) -> Result<Vec<u8>, BoundedResult> {
        let matches = self.witness.input_sha256 == sha256(expected_input)
            && self.witness.stdout_sha256 == sha256(&self.answer.stdout)
            && self.witness.artifact_sha256 == expected_artifact
            && self.witness.pin.sha256() == expected_artifact;
        let expired = std::time::Instant::now() >= self.witness.deadline;
        if !matches || expired {
            return Err(withhold(self.answer, expired));
        }
        Ok(std::mem::take(&mut self.answer.stdout))
    }
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn withhold(mut answer: BoundedResult, expired: bool) -> BoundedResult {
    if expired {
        answer.timed_out = true;
        if let Some(receipt) = answer.process_receipt.as_mut().filter(|receipt| receipt.stop == ProcessStop::Completed) {
            receipt.stop = ProcessStop::Timeout;
        }
    }
    answer.code = None;
    answer.stdout.clear();
    answer.stderr.clear();
    answer
}

#[allow(clippy::result_large_err)] // The public owned API deliberately returns the existing complete diagnostic type.
pub(super) async fn run_owned(
    pin: PinnedWindowsExecutable,
    command: &std::process::Command,
    environment: &[(OsString, OsString)],
    input: &[u8],
    deadline: std::time::Instant,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<OwnedWindowsCompletion, BoundedResult> {
    run_owned_shared(Arc::new(pin), command, environment, input, deadline, stdout_limit, stderr_limit, WindowsMetadataExitPolicy::ZeroOnly).await
}

fn metadata_exit_allowed(answer: &BoundedResult, policy: WindowsMetadataExitPolicy) -> bool {
    answer.code == Some(0)
        || (matches!(policy, WindowsMetadataExitPolicy::MissingOrigin) && answer.code == Some(1) && answer.stdout.is_empty() && answer.stderr.is_empty())
}

#[allow(clippy::too_many_arguments, clippy::result_large_err)] // One shared lease and a narrow metadata policy added to the unchanged bounded transport contract.
pub(super) async fn run_owned_shared(
    pin: Arc<PinnedWindowsExecutable>,
    command: &std::process::Command,
    environment: &[(OsString, OsString)],
    input: &[u8],
    deadline: std::time::Instant,
    stdout_limit: usize,
    stderr_limit: usize,
    exit_policy: WindowsMetadataExitPolicy,
) -> Result<OwnedWindowsCompletion, BoundedResult> {
    if deadline > pin.deadline() {
        // A caller may shorten the lease's original budget, never restart or
        // extend it between pinning and actual process creation.
        return Err(BoundedResult { process_receipt: Some(ProcessReceipt::default()), ..Default::default() });
    }
    let (answer, job) = run_inner(command, environment, input, deadline, stdout_limit, stderr_limit, Some(&pin)).await;
    #[cfg(test)]
    let missing_origin_policy = matches!(&exit_policy, WindowsMetadataExitPolicy::MissingOrigin);
    if !metadata_exit_allowed(&answer, exit_policy)
        || answer.timed_out
        || answer.output_limited
        || answer.spawn_failed
        || !answer.process_receipt.as_ref().is_some_and(|receipt| receipt.stop == ProcessStop::Completed && receipt.cleanup_confirmed())
    {
        #[cfg(test)]
        eprintln!(
            "owned_admission_rejected missing_origin={} code={:?} timeout={} limited={} spawn={} stdout_len={} stderr_len={} stop={:?} cleanup={}",
            missing_origin_policy,
            answer.code,
            answer.timed_out,
            answer.output_limited,
            answer.spawn_failed,
            answer.stdout.len(),
            answer.stderr.len(),
            answer.process_receipt.as_ref().map(|receipt| receipt.stop),
            answer.process_receipt.as_ref().is_some_and(ProcessReceipt::cleanup_confirmed)
        );
        return Err(withhold(answer, std::time::Instant::now() >= deadline));
    }
    let Some(job) = job else {
        return Err(withhold(answer, false));
    };
    let input_sha256 = sha256(input);
    let stdout_sha256 = sha256(&answer.stdout);
    let artifact_sha256 = pin.sha256().to_owned();
    // Hashing and the final actual completion admission share the caller's
    // absolute budget. Neither a public receipt nor a caller marker mints this.
    if std::time::Instant::now() >= deadline {
        return Err(withhold(answer, true));
    }
    Ok(OwnedWindowsCompletion { answer, witness: WindowsOsExitWitness { _job: job, pin, input_sha256, stdout_sha256, artifact_sha256, deadline } })
}

async fn run_inner(
    command: &std::process::Command,
    environment: &[(OsString, OsString)],
    input: &[u8],
    deadline: std::time::Instant,
    stdout_limit: usize,
    stderr_limit: usize,
    pin: Option<&PinnedWindowsExecutable>,
) -> (BoundedResult, Option<Job>) {
    let mut answer = BoundedResult::default();
    let mut receipt = ProcessReceipt::default();
    let deadline = Instant::from_std(deadline);
    let setup = {
        let setup = async {
            if Instant::now() >= deadline {
                answer.timed_out = true;
                receipt.stop = ProcessStop::Timeout;
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            receipt.owner_session = nonce()?;
            // SAFETY: current process pseudo-handle is borrowed for read-only identity, never closed.
            receipt.owner = Some(identity_raw(unsafe { GetCurrentProcess() }, unsafe { GetCurrentProcessId() })?);
            let (input_pipe, stdout, stderr) = {
                let security = pipe_security()?;
                (
                    pipe(&receipt.owner_session, "stdin", true, &security)?,
                    pipe(&receipt.owner_session, "stdout", false, &security)?,
                    pipe(&receipt.owner_session, "stderr", false, &security)?,
                )
            };
            input_pipe.server.connect().await?;
            stdout.server.connect().await?;
            stderr.server.connect().await?;
            let mut job = Job::new()?;
            let spawn = spawn_owned(
                SpawnContext { command, environment, deadline, pin },
                &job,
                [&input_pipe.client, &stdout.client, &stderr.client],
                &mut answer.spawn_failed,
            );
            if let Err(error) = &spawn {
                answer.spawn_failed |= error.kind() == io::ErrorKind::NotFound;
                if error.kind() == io::ErrorKind::TimedOut {
                    answer.timed_out = true;
                    receipt.stop = ProcessStop::Timeout;
                }
            }
            let (root, thread) = spawn?;
            receipt.root = Some(root.identity.clone());
            receipt.observed.push(root.identity.clone());
            job.processes.push(root);
            // Close every parent copy of child-side endpoints, before any user code can run.
            let Pipe { server: input_pipe, client } = input_pipe;
            drop(client);
            let Pipe { server: stdout, client } = stdout;
            drop(client);
            let Pipe { server: stderr, client } = stderr;
            drop(client);
            if Instant::now() >= deadline {
                answer.timed_out = true;
                receipt.stop = ProcessStop::Timeout;
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            // SAFETY: retained primary-thread handle belongs to our atomically Job-assigned suspended child.
            if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
                return Err(os_error());
            }
            drop(thread);
            Ok::<_, io::Error>((job, input_pipe, stdout, stderr))
        };
        match tokio::time::timeout_at(deadline, setup).await {
            Ok(result) => result.ok(),
            Err(_) => {
                answer.timed_out = true;
                receipt.stop = ProcessStop::Timeout;
                None
            }
        }
    };
    let Some((mut job, mut input_pipe, stdout, stderr)) = setup else {
        if answer.spawn_failed {
            receipt.stop = ProcessStop::SpawnFailed;
        }
        answer.process_receipt = Some(receipt);
        return (answer, None);
    };
    receipt.stop = ProcessStop::Completed;
    let input = input.to_vec();
    let writer = tokio::spawn(async move {
        let result = input_pipe.write_all(&input).await;
        // NamedPipeServer::shutdown is not a half-close. Drop the server to deliver EOF after queued writes.
        drop(input_pipe);
        result
    });
    let (signal, mut overflow) = mpsc::channel(2);
    let mut tasks = Tasks {
        writer: Some(writer),
        stdout: Some(tokio::spawn(drain(stdout, stdout_limit, signal.clone()))),
        stderr: Some(tokio::spawn(drain(stderr, stderr_limit, signal))),
    };
    let mut cleanup = CleanupState::default();
    let mut quiescence = Quiescence::new(deadline.into_std());
    #[cfg(test)]
    let mut diagnostic = DiagnosticTrace::new();
    loop {
        if overflow.try_recv().is_ok() {
            answer.output_limited = true;
            if receipt.stop == ProcessStop::Completed {
                receipt.stop = ProcessStop::OutputLimit;
            }
        }
        if tasks.writer.as_ref().is_some_and(JoinHandle::is_finished) {
            let result = tasks.writer.take().expect("present writer").await;
            receipt.writer_finished = true;
            if !matches!(result, Ok(Ok(()))) && receipt.stop == ProcessStop::Completed {
                receipt.stop = ProcessStop::IoFailure;
            }
        }
        for error in [false, true] {
            let task = if error { &mut tasks.stderr } else { &mut tasks.stdout };
            if task.as_ref().is_some_and(JoinHandle::is_finished) {
                match task.take().expect("present reader").await {
                    Ok(output) => {
                        answer.output_limited |= output.limited;
                        if output.limited && receipt.stop == ProcessStop::Completed {
                            receipt.stop = ProcessStop::OutputLimit;
                        }
                        if output.failed && receipt.stop == ProcessStop::Completed {
                            receipt.stop = ProcessStop::IoFailure;
                        }
                        if error {
                            receipt.stderr_eof = output.eof;
                            answer.stderr = output.bytes;
                        } else {
                            receipt.stdout_eof = output.eof;
                            answer.stdout = output.bytes;
                        }
                    }
                    Err(_) => {
                        if receipt.stop == ProcessStop::Completed {
                            receipt.stop = ProcessStop::IoFailure;
                        }
                    }
                }
            }
        }
        if let Err(_error) = job.observe(&mut receipt) {
            #[cfg(test)]
            eprintln!("owned_observe_error_code={}", _error.raw_os_error().unwrap_or(-1));
            receipt.job_empty = false;
            receipt.observed_exited = false;
            if receipt.stop == ProcessStop::Completed {
                receipt.stop = ProcessStop::IoFailure;
            }
        }
        let now = Instant::now();
        let decision = quiescence.observe(
            QuiescenceObservation {
                root_exit_at: job.root_exit_at,
                process_quiet: receipt.root_exited && receipt.job_empty && receipt.observed_exited,
                failed: receipt.stop != ProcessStop::Completed || cleanup.termination_requested,
                cleanup_deadline: cleanup.deadline.map(Instant::into_std),
            },
            now.into_std(),
        );
        if let Some(end) = quiescence.cleanup_deadline().map(Instant::from_std) {
            cleanup.deadline = Some(cleanup.deadline.map_or(end, |earlier| earlier.min(end)));
        }
        if receipt.stop == ProcessStop::Completed {
            match decision {
                QuiescenceDecision::Timeout => {
                    answer.timed_out = true;
                    receipt.stop = ProcessStop::Timeout;
                }
                QuiescenceDecision::Lingering => receipt.stop = ProcessStop::LingeringDescendants,
                QuiescenceDecision::CleanupExpired => receipt.stop = ProcessStop::CleanupUnconfirmed,
                QuiescenceDecision::Stopped => receipt.stop = ProcessStop::IoFailure,
                QuiescenceDecision::Observe | QuiescenceDecision::Quiescent => {}
            }
        }
        #[cfg(test)]
        diagnostic.record_stop(&job, &receipt, cleanup.termination_requested);
        let step = cleanup.advance(&mut receipt, now);
        #[cfg(test)]
        diagnostic.record_stop(&job, &receipt, cleanup.termination_requested);
        if step.terminate {
            #[cfg(test)]
            diagnostic.emit(diagnostics::Event::TerminateRequested, &job, &receipt, cleanup.termination_requested);
            let _ = job.terminate();
        }
        if receipt.cleanup_confirmed() {
            #[cfg(test)]
            diagnostic.emit(diagnostics::Event::CleanupConfirmed, &job, &receipt, cleanup.termination_requested);
            break;
        }
        if step.expired {
            #[cfg(test)]
            diagnostic.emit(diagnostics::Event::CleanupExpired, &job, &receipt, cleanup.termination_requested);
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    answer.code = receipt.root_exit_code;
    answer.process_receipt = Some(receipt);
    answer.seal_windows_payload(Instant::now() >= deadline);
    (answer, Some(job))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_resume_diagnostics_are_isolated_and_keep_the_strict_lifecycle() {
        let fixture = format!("{}::real_resume_diagnostic_standalone_fixture", module_path!().split_once("::").unwrap().1);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &fixture, "--nocapture", "--", "--owned-resume-diagnostic"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= end {
                let _ = child.kill();
                let _ = child.wait();
                panic!("isolated real-resume diagnostic exceeded its finite deadline");
            }
            std::thread::sleep(POLL);
        }
        assert!(child.wait().unwrap().success(), "isolated real-resume diagnostic failed; closed metadata is on stderr");
    }

    #[test]
    #[allow(clippy::zombie_processes)] // The owned Job must terminate the deliberate live descendant.
    fn diagnostic_exit_fixture() {
        if std::env::args().any(|arg| arg == "--owned-diagnostic-sleep") {
            std::thread::sleep(Duration::from_secs(30));
            std::process::exit(0);
        }
        if std::env::args().any(|arg| arg == "--owned-diagnostic-linger") {
            let fixture = format!("{}::diagnostic_exit_fixture", module_path!().split_once("::").unwrap().1);
            let _child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &fixture, "--nocapture", "--", "--owned-diagnostic-sleep"])
                .spawn()
                .unwrap();
            std::process::exit(0);
        }
        if std::env::args().any(|arg| arg == "--owned-diagnostic-exit") {
            std::process::exit(0);
        }
    }

    #[test]
    fn real_resume_diagnostic_standalone_fixture() {
        if !std::env::args().any(|arg| arg == "--owned-resume-diagnostic") {
            return;
        }
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for (flag, expected) in [("--owned-diagnostic-exit", ProcessStop::Completed), ("--owned-diagnostic-linger", ProcessStop::LingeringDescendants)] {
            let fixture = format!("{}::diagnostic_exit_fixture", module_path!().split_once("::").unwrap().1);
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", &fixture, "--nocapture", "--", flag]);
            let environment: Vec<_> =
                ["SystemRoot", "WINDIR", "TEMP", "TMP"].into_iter().filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(key), value))).collect();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let (answer, job) = runtime.block_on(run_inner(&command, &environment, b"", deadline, 65536, 65536, None));
            let receipt = answer.process_receipt.as_ref().expect("actual guard receipt");
            let job = job.as_ref().expect("actual guard owns its Job");
            let members = diagnostic_members(job);
            assert!(!members.is_empty());
            assert!(members[0].primary_root && members[0].wait == WAIT_OBJECT_0);
            assert!(members.iter().all(|member| member.wait_error.is_none()));
            assert_eq!(receipt.stop, expected, "unexpected lifecycle outcome; closed metadata is on stderr");
            assert!(receipt.cleanup_confirmed());
            assert!(std::time::Instant::now() < deadline);
            assert_eq!(answer.code, if expected == ProcessStop::Completed { Some(0) } else { None });
        }
    }

    #[test]
    fn metadata_exit_one_is_only_the_empty_missing_origin_case() {
        // Policy truth table only: these diagnostics cannot create a witness.
        for (code, stdout, stderr, zero_only, missing_origin) in [
            (Some(0), b"value".as_slice(), b"".as_slice(), true, true),
            (Some(1), b"", b"", false, true),
            (Some(1), b"value", b"", false, false),
            (Some(1), b"", b"error", false, false),
            (Some(2), b"", b"", false, false),
            (None, b"", b"", false, false),
        ] {
            let result = BoundedResult { code, stdout: stdout.to_vec(), stderr: stderr.to_vec(), ..Default::default() };
            assert_eq!(metadata_exit_allowed(&result, WindowsMetadataExitPolicy::ZeroOnly), zero_only);
            assert_eq!(metadata_exit_allowed(&result, WindowsMetadataExitPolicy::MissingOrigin), missing_origin);
        }
    }

    #[test]
    fn shared_metadata_keeps_one_lease_and_rejects_another_same_bytes_program() {
        // Only the isolated exact-filter child creates native inheritable pipe
        // clients. The parallel libtest parent uses ordinary std spawn only.
        let fixture = format!("{}::shared_metadata_standalone_fixture", module_path!().split_once("::").unwrap().1);
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", &fixture, "--nocapture", "--", "--shared-pinned-metadata"]);
        command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::inherit());
        let mut child = command.spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("isolated shared metadata fixture exceeded its finite deadline");
            }
            std::thread::sleep(POLL);
        }
        assert!(child.wait().unwrap().success(), "isolated shared metadata fixture must succeed");
    }

    #[test]
    fn shared_metadata_standalone_fixture() {
        if !std::env::args().any(|arg| arg == "--shared-pinned-metadata") {
            return;
        }
        // Diagnostic only. Both variants remain suspended, verify the held
        // image, and terminate their actual retained handle. No witness is issued.
        async fn suspended_probes(
            pin: &PinnedWindowsExecutable,
            command: &std::process::Command,
            values: &[(OsString, OsString)],
            deadline: std::time::Instant,
            dos_application: &Path,
            cases: [&'static str; 2],
        ) -> io::Result<()> {
            let session = nonce()?;
            let security = pipe_security()?;
            let input = pipe(&session, "stdin", true, &security)?;
            let stdout = pipe(&session, "stdout", false, &security)?;
            let stderr = pipe(&session, "stderr", false, &security)?;
            input.server.connect().await?;
            stdout.server.connect().await?;
            stderr.server.connect().await?;
            let job = Job::new()?;
            let jobs = [job.handle.as_raw_handle()];
            let handles = [input.client.as_raw_handle(), stdout.client.as_raw_handle(), stderr.client.as_raw_handle()];
            let mut attributes = Attributes::new(&jobs, &handles)?;
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = handles[0];
            startup.StartupInfo.hStdOutput = handles[1];
            startup.StartupInfo.hStdError = handles[2];
            startup.lpAttributeList = attributes.raw();
            let original_argv = command_line(pin.executable(), command)?;
            let env = environment(command, values)?;
            let cwd = command.get_current_dir().map(|path| wide(path.as_os_str())).transpose()?;
            // Reuse these exact Job, stdio handles, attribute list and environment
            // for the two one-parameter variants of each pinned image.
            for (case, application, null_line) in [(cases[0], pin.executable(), true), (cases[1], dos_application, false)] {
                let application = wide(application.as_os_str())?;
                let mut argv = original_argv.clone();
                let mut created = PROCESS_INFORMATION::default();
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::from(io::ErrorKind::TimedOut));
                }
                // SAFETY: all buffers, attribute arrays and handles remain live.
                // JOB_LIST is unchanged; the primary thread is NEVER resumed.
                let ok = unsafe {
                    CreateProcessW(
                        application.as_ptr(),
                        if null_line { null_mut() } else { argv.as_mut_ptr() },
                        null(),
                        null(),
                        1,
                        CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                        env.as_ptr().cast(),
                        cwd.as_ref().map_or(null(), |path| path.as_ptr()),
                        &startup.StartupInfo,
                        &mut created,
                    )
                };
                if ok == 0 {
                    let error = os_error();
                    eprintln!("shared_suspended_case={case} error={} verified=false termination_ack=false job_empty=false", error.raw_os_error().unwrap_or(-1));
                    continue;
                }
                let root = owned(created.hProcess)?;
                let thread = owned(created.hThread)?;
                let verified = pin.verify_process(root.as_raw_handle()).is_ok();
                // SAFETY: terminate only this freshly created retained suspended
                // root, regardless of image verification. Never resume either case.
                let requested = unsafe { windows_sys::Win32::System::Threading::TerminateProcess(root.as_raw_handle(), 0) } != 0;
                let cleanup_end = deadline.min(std::time::Instant::now() + CLEANUP);
                let mut receipt = ProcessReceipt::default();
                let (mut root_ack, mut empty) = (false, false);
                while std::time::Instant::now() < cleanup_end {
                    if exited(&root).unwrap_or(false) {
                        let mut code = 0;
                        // SAFETY: query the retained root only after actual signal.
                        root_ack = requested && unsafe { GetExitCodeProcess(root.as_raw_handle(), &mut code) } != 0 && code == 0;
                    }
                    empty = job.accounting(&mut receipt).is_ok() && receipt.job_empty;
                    if root_ack && empty {
                        break;
                    }
                    tokio::time::sleep(POLL).await;
                }
                eprintln!("shared_suspended_case={case} error=0 verified={verified} termination_ack={root_ack} job_empty={empty}");
                // Keep both handles through finite observation. If cleanup is
                // unknown, stop and let the owned Job destructor request cleanup.
                drop(thread);
                drop(root);
                if !root_ack || !empty {
                    break;
                }
            }
            Ok(())
        }
        let end = std::time::Instant::now() + Duration::from_secs(45);
        let mut buffer = [0u16; 32768];
        // SAFETY: fixed writable UTF-16 buffer for a read-only OS location query.
        let count = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        assert!(count > 0 && count < buffer.len());
        let program = PathBuf::from(OsString::from_wide(&buffer[..count])).join("cmd.exe").canonicalize().unwrap();
        let image = std::fs::read(&program).unwrap();
        let artifact = sha256(&image);
        let pin = Arc::new(PinnedWindowsExecutable::open(&program, &artifact, end).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let source = crate::topology::adapters::native_reader::windows::WindowsSourceIo::default();
        let git_environment = crate::topology::adapters::native_reader::environment(&source).unwrap();
        let cwd = std::env::current_dir().unwrap();
        let report = |case: &'static str, volume_match: bool, result: &Result<OwnedWindowsCompletion, BoundedResult>| {
            let diagnostic = match result {
                Ok(completion) => completion.diagnostic(),
                Err(result) => result,
            };
            eprintln!(
                "shared_pinned_case={case} system_volume_match={volume_match} code={:?} root_exit_code={:?} root_present={} timeout={} limit={} spawn={} stop={:?} cleanup={}",
                diagnostic.code,
                diagnostic.process_receipt.as_ref().and_then(|receipt| receipt.root_exit_code),
                diagnostic.process_receipt.as_ref().is_some_and(|receipt| receipt.root.is_some()),
                diagnostic.timed_out,
                diagnostic.output_limited,
                diagnostic.spawn_failed,
                diagnostic.process_receipt.as_ref().map(|receipt| receipt.stop),
                diagnostic.process_receipt.as_ref().is_some_and(ProcessReceipt::cleanup_confirmed)
            );
        };
        let same_volume = |candidate: &PinnedWindowsExecutable| candidate.executable().components().next() == pin.executable().components().next();
        struct TemporaryDirectory(PathBuf);
        impl Drop for TemporaryDirectory {
            fn drop(&mut self) {
                // Exclusively created below. Every later-declared lease is
                // dropped before this cleanup, including during unwinding.
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        // The copied-image control deliberately shares the original artifact's
        // volume. TEMP/TMP may be redirected to another drive; they do not
        // establish where the executable under test resides.
        let own_program = std::env::current_exe().unwrap();
        let directory =
            own_program.parent().expect("test executable directory").join(format!("shared-pin-fixture-{}-{}", std::process::id(), crate::db::now_ms()));
        std::fs::create_dir(&directory).unwrap();
        let temporary = TemporaryDirectory(directory);
        let own_image = std::fs::read(&own_program).unwrap();
        let own_artifact = sha256(&own_image);
        let own_pin = Arc::new(PinnedWindowsExecutable::open(&own_program, &own_artifact, end).unwrap());
        let exit_fixture = format!("{}::accounting_exit_fixture", module_path!().split_once("::").unwrap().1);
        let exit_args = ["--exact", &exit_fixture, "--nocapture", "--", "--owned-accounting-exit"];
        let mut own_command = std::process::Command::new(own_pin.executable());
        own_command.current_dir(&cwd).args(exit_args);
        let own_result =
            runtime.block_on(run_owned_shared(Arc::clone(&own_pin), &own_command, &git_environment, b"", end, 8192, 8192, WindowsMetadataExitPolicy::ZeroOnly));
        report("own_rust", same_volume(&own_pin), &own_result);
        let own_passed = own_result.is_ok_and(|completion| completion.into_verified_output(b"", &own_artifact).is_ok());
        let copied_program = temporary.0.join("rust-control.exe");
        let copy_pin = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&copied_program)
            .and_then(|mut file| std::io::Write::write_all(&mut file, &own_image))
            .and_then(|()| PinnedWindowsExecutable::open(&copied_program, &own_artifact, end))
            .ok()
            .map(Arc::new);
        let copy_passed = if let Some(copied) = &copy_pin {
            let mut command = std::process::Command::new(copied.executable());
            command.current_dir(&cwd).args(exit_args);
            let result =
                runtime.block_on(run_owned_shared(Arc::clone(copied), &command, &git_environment, b"", end, 8192, 8192, WindowsMetadataExitPolicy::ZeroOnly));
            let volume_match = same_volume(copied);
            report("copied_rust", volume_match, &result);
            let consumed = result.is_ok_and(|completion| completion.into_verified_output(b"", &own_artifact).is_ok());
            let original_volume_match = copied.executable().components().next() == own_pin.executable().components().next();
            original_volume_match && consumed
        } else {
            eprintln!("shared_pinned_case=copied_rust_setup_failed");
            false
        };
        // Fixture-only PATH discovery. Product execution still receives one
        // explicit pinned program and the native collector's exact nine-key map.
        let git_program = std::env::split_paths(&std::env::var_os("PATH").expect("fixture Git PATH"))
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.join("git.exe"))
            .find(|path| path.is_file())
            .expect("real fixture Git")
            .canonicalize()
            .unwrap();
        let git_artifact = sha256(&std::fs::read(&git_program).unwrap());
        let git_pin = Arc::new(PinnedWindowsExecutable::open(&git_program, &git_artifact, end).unwrap());
        let mut git_command = std::process::Command::new(git_pin.executable());
        git_command.current_dir(&cwd).args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=NUL",
            "rev-parse",
            "--absolute-git-dir",
        ]);
        let git_result =
            runtime.block_on(run_owned_shared(Arc::clone(&git_pin), &git_command, &git_environment, b"", end, 8192, 8192, WindowsMetadataExitPolicy::ZeroOnly));
        report("actual_git", same_volume(&git_pin), &git_result);
        let git_passed = git_result.is_ok_and(|completion| completion.into_verified_output(b"", &git_artifact).is_ok_and(|bytes| !bytes.is_empty()));
        let git_dos = user_executable_path(git_program.clone()).unwrap();
        let cmd_dos = user_executable_path(program.clone()).unwrap();
        let mut probe_cmd = std::process::Command::new(pin.executable());
        probe_cmd.current_dir(&cwd).args(["/D", "/C", "exit", "0"]);
        for (held, command, values, dos, cases) in [
            (git_pin.as_ref(), &git_command, git_environment.as_slice(), git_dos.as_path(), ["git_guid_null", "git_dos_application"]),
            (pin.as_ref(), &probe_cmd, git_environment.as_slice(), cmd_dos.as_path(), ["cmd_guid_null", "cmd_dos_application"]),
        ] {
            if let Err(error) = runtime.block_on(suspended_probes(held, command, values, end, dos, cases)) {
                for case in cases {
                    eprintln!("shared_suspended_case={case} error={} verified=false termination_ack=false job_empty=false", error.raw_os_error().unwrap_or(-1));
                }
            }
        }
        // cmd /C uses shell-specific parsing rather than this backend's CRT
        // argv contract. Test metadata policies with actual Git metadata instead.
        let repository = temporary.0.join("metadata");
        let mut setup = std::process::Command::new(git_pin.executable());
        setup.current_dir(&temporary.0).args(["--no-optional-locks", "-c", "core.hooksPath=NUL", "init", "--quiet", "--template="]).arg(&repository);
        let setup_result =
            runtime.block_on(run_owned_shared(Arc::clone(&git_pin), &setup, &git_environment, b"", end, 8192, 8192, WindowsMetadataExitPolicy::ZeroOnly));
        report("metadata_fixture_setup", true, &setup_result);
        assert!(
            setup_result.is_ok_and(|completion| completion.into_verified_output(b"", &git_artifact).is_ok()),
            "exclusive no-origin Git fixture setup must complete and consume"
        );
        let mut metadata_passed = true;
        for (arguments, policy, empty) in [
            (vec!["rev-parse", "--absolute-git-dir"], WindowsMetadataExitPolicy::ZeroOnly, false),
            (vec!["rev-parse", "--absolute-git-dir"], WindowsMetadataExitPolicy::ZeroOnly, false),
            (vec!["config", "--local", "--get", "remote.origin.url"], WindowsMetadataExitPolicy::MissingOrigin, true),
        ] {
            let mut command = std::process::Command::new(git_pin.executable());
            command.current_dir(&repository).args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.hooksPath=NUL"]).args(arguments);
            let result = runtime.block_on(run_owned_shared(Arc::clone(&git_pin), &command, &git_environment, b"", end, 8192, 8192, policy));
            report(if empty { "metadata_origin_one" } else { "metadata_zero" }, true, &result);
            let passed = result.is_ok_and(|completion| {
                let retained = Arc::ptr_eq(&completion.witness.pin, &git_pin) && Arc::strong_count(&git_pin) == 2;
                let cleanup = completion.diagnostic().process_receipt.as_ref().is_some_and(ProcessReceipt::cleanup_confirmed);
                let expected_code = completion.diagnostic().code == Some(if empty { 1 } else { 0 });
                let consumed = completion.into_verified_output(b"", &git_artifact).is_ok_and(|bytes| bytes.is_empty() == empty);
                retained && cleanup && expected_code && consumed
            });
            metadata_passed &= passed && Arc::strong_count(&git_pin) == 1;
        }
        let mut missing = std::process::Command::new(git_pin.executable());
        missing.current_dir(&repository).args(["--no-optional-locks", "-c", "core.hooksPath=NUL", "config", "--local", "--get", "remote.origin.url"]);
        let missing_result =
            runtime.block_on(run_owned_shared(Arc::clone(&git_pin), &missing, &git_environment, b"", end, 8192, 8192, WindowsMetadataExitPolicy::ZeroOnly));
        report("metadata_one_zero_only", true, &missing_result);
        let missing_rejected = missing_result.is_err_and(|failed| {
            failed.code.is_none()
                && failed.stdout.is_empty()
                && failed.stderr.is_empty()
                && failed
                    .process_receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.root_exit_code == Some(1) && receipt.stop == ProcessStop::Completed && receipt.cleanup_confirmed())
        });
        assert_eq!(Arc::strong_count(&git_pin), 1);

        let alternate = temporary.0.join("same-bytes.exe");
        std::fs::write(&alternate, &image).unwrap();
        let other = PinnedWindowsExecutable::open(&alternate, &artifact, end).unwrap();
        assert_eq!(pin.sha256(), other.sha256());
        assert_ne!(pin.executable(), other.executable());
        let mut command = std::process::Command::new(other.executable());
        command.current_dir(&cwd);
        let failed = runtime
            .block_on(run_owned_shared(Arc::clone(&pin), &command, &git_environment, b"", end, 8192, 8192, WindowsMetadataExitPolicy::ZeroOnly))
            .err()
            .expect("same image bytes at another executable path cannot replace the retained artifact");
        assert_eq!(failed.code, None);
        assert!(failed.stdout.is_empty() && failed.stderr.is_empty());
        let receipt = failed.process_receipt.unwrap();
        assert_eq!(receipt.stop, ProcessStop::SetupFailed);
        assert!(receipt.root.is_none() && receipt.observed.is_empty(), "reject the different program before process creation");
        assert_eq!(Arc::strong_count(&pin), 1);
        assert!(own_passed, "same-parent pinned original Rust control must complete and consume");
        assert!(copy_passed, "same-bytes Rust control must complete on the original artifact volume and consume");
        assert!(metadata_passed, "all shared metadata completion, exit-policy and lease assertions remain required");
        assert!(missing_rejected, "zero-only policy still requires a real completed exit-one rejection");
        assert!(git_passed, "actual pinned Git metadata must succeed as well as the no-origin fixture");
        drop(other);
        drop(copy_pin);
        drop(own_pin);
        drop(git_pin);
        drop(runtime);
        drop(pin);
        drop(temporary);
        std::process::exit(0);
    }

    // Synthetic decision-table state only: these tests issue no OS/ownership witness.
    fn policy_receipt() -> ProcessReceipt {
        let owner = ProcessIdentity { pid: 1, created_filetime: "1".into(), os_session: 1 };
        let root = ProcessIdentity { pid: 2, created_filetime: "2".into(), os_session: 1 };
        ProcessReceipt {
            owner_session: "a".repeat(32),
            owner: Some(owner),
            root: Some(root.clone()),
            observed: vec![root],
            total_processes: 1,
            stop: ProcessStop::Completed,
            root_exited: true,
            root_exit_code: Some(0),
            job_empty: false,
            observed_exited: true,
            stdout_eof: true,
            stderr_eof: true,
            writer_finished: true,
        }
    }

    fn assert_unknown_payload_is_sealed(receipt: &ProcessReceipt) {
        assert!(!receipt.cleanup_confirmed());
        let mut result = BoundedResult {
            code: Some(0),
            stdout: b"parser".to_vec(),
            stderr: b"diagnostic".to_vec(),
            process_receipt: Some(receipt.clone()),
            ..Default::default()
        };
        result.seal_windows_payload(false);
        assert_eq!(result.code, None);
        assert!(result.stdout.is_empty() && result.stderr.is_empty());
    }

    #[test]
    fn classifier_requires_a_nonsignaled_member_after_root_exit() {
        for (root_exited, job_empty, observed_exited, expected) in
            [(true, false, true, false), (true, false, false, true), (false, false, false, false), (true, true, false, false)]
        {
            let mut receipt = policy_receipt();
            receipt.root_exited = root_exited;
            receipt.job_empty = job_empty;
            receipt.observed_exited = observed_exited;
            assert_eq!(lingering_descendants(&receipt), expected, "root={root_exited} empty={job_empty} observed={observed_exited}");
        }
    }

    #[test]
    fn pending_cleanup_then_observe_failure_requests_termination_once() {
        let start = Instant::now();
        let mut state = CleanupState::default();
        let mut receipt = policy_receipt();
        assert!(!state.advance(&mut receipt, start).terminate);
        let fixed = state.deadline;
        receipt.stop = ProcessStop::IoFailure;
        let first = state.advance(&mut receipt, start + POLL);
        let repeated = state.advance(&mut receipt, start + POLL + POLL);
        assert_eq!(state.deadline, fixed);
        assert_eq!(fixed, Some(start + CLEANUP));
        assert!(!first.expired && !repeated.expired);
        assert_unknown_payload_is_sealed(&receipt);
        assert_eq!(usize::from(first.terminate) + usize::from(repeated.terminate), 1, "failure after pending cleanup must request once");
        assert!(state.termination_requested);
    }

    #[test]
    fn pending_cleanup_expiry_requests_termination_once_before_break() {
        let start = Instant::now();
        let mut state = CleanupState::default();
        let mut receipt = policy_receipt();
        assert!(!state.advance(&mut receipt, start).terminate);
        let fixed = state.deadline;
        let first = state.advance(&mut receipt, start + CLEANUP);
        let repeated = state.advance(&mut receipt, start + CLEANUP + POLL);
        assert_eq!(state.deadline, fixed);
        assert_eq!(fixed, Some(start + CLEANUP));
        assert!(first.expired && repeated.expired);
        assert_eq!(receipt.stop, ProcessStop::CleanupUnconfirmed);
        assert_unknown_payload_is_sealed(&receipt);
        assert_eq!(usize::from(first.terminate) + usize::from(repeated.terminate), 1, "expiry must request termination before returning break");
        assert!(state.termination_requested);
    }

    #[test]
    fn accounting_exit_fixture() {
        if std::env::args().any(|arg| arg == "--owned-accounting-exit") {
            std::process::exit(0);
        }
    }

    fn suspended_observation_member(job: &Job, deadline: Instant) -> (Process, OwnedHandle) {
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        let fixture = format!("{}::accounting_exit_fixture", module_path!().split_once("::").unwrap().1);
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", &fixture, "--nocapture", "--", "--owned-accounting-exit"]);
        let name = wide(OsStr::new("NUL")).unwrap();
        let security = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: null_mut(), bInheritHandle: 1 };
        let stdio: Vec<_> = (0..3)
            .map(|_| {
                // SAFETY: only fixture-owned inheritable NUL handles; no application IO or file payload.
                owned(unsafe {
                    CreateFileW(
                        name.as_ptr(),
                        GENERIC_READ_ACCESS | GENERIC_WRITE_ACCESS,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        &security,
                        OPEN_EXISTING,
                        FILE_ATTRIBUTE_NORMAL,
                        null_mut(),
                    )
                })
                .unwrap()
            })
            .collect();
        let mut failed = false;
        let member =
            spawn_owned(SpawnContext { command: &command, environment: &[], deadline, pin: None }, job, [&stdio[0], &stdio[1], &stdio[2]], &mut failed)
                .unwrap();
        assert!(!failed);
        member
    }

    fn fixture_pid_list(assigned: u32, count: u32, values: &[usize]) -> io::Result<Option<Vec<u32>>> {
        complete_pid_list(assigned, count, values).map(Some)
    }

    fn fixture_job_pids(job: &Job) -> io::Result<Option<Vec<u32>>> {
        #[repr(C)]
        struct Pids {
            assigned: u32,
            count: u32,
            values: [usize; MAX_ACTIVE],
        }
        let mut pids = Pids { assigned: 0, count: 0, values: [0; MAX_ACTIVE] };
        // SAFETY: independent native observation of this fixture's retained Job, fixed aligned output.
        check(unsafe {
            QueryInformationJobObject(
                job.handle.as_raw_handle(),
                JobObjectBasicProcessIdList,
                std::ptr::from_mut(&mut pids).cast(),
                size_of_val(&pids) as u32,
                null_mut(),
            )
        })?;
        fixture_pid_list(pids.assigned, pids.count, &pids.values)
    }

    fn wait_fixture_root_only(root_pid: u32, deadline: Instant, mut query: impl FnMut() -> io::Result<Option<Vec<u32>>>) -> io::Result<()> {
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            if query()?.is_some_and(|pids| pids == [root_pid]) {
                return Ok(());
            }
            std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn remove_fixture_member_after_snapshot(
        job: &mut Job,
        root_pid: u32,
        member: Process,
        member_thread: OwnedHandle,
        deadline: Instant,
    ) -> Arc<std::sync::atomic::AtomicU8> {
        remove_fixture_member_with_query(job, root_pid, member, member_thread, deadline, fixture_job_pids)
    }

    fn remove_fixture_member_with_query(
        job: &mut Job,
        root_pid: u32,
        member: Process,
        member_thread: OwnedHandle,
        deadline: Instant,
        mut query: impl FnMut(&Job) -> io::Result<Option<Vec<u32>>> + Send + 'static,
    ) -> Arc<std::sync::atomic::AtomicU8> {
        use std::sync::atomic::{AtomicU8, Ordering};
        // Closed test diagnostics: 0=not entered, 1=hook, 2=terminate,
        // 3=retained member signaled/absence pending, 4=complete root-only.
        let progress = Arc::new(AtomicU8::new(0));
        let observed = Arc::clone(&progress);
        let member_pid = member.identity.pid;
        job.after_pid_list = Some(Box::new(move |job, snapshot| {
            observed.store(1, Ordering::Relaxed);
            assert!(snapshot.contains(&root_pid) && snapshot.contains(&member_pid));
            // SAFETY: terminate only the exact suspended member owned by this fixture; never a PID lookup.
            check(unsafe { windows_sys::Win32::System::Threading::TerminateProcess(member.handle.as_raw_handle(), 0) })?;
            observed.store(2, Ordering::Relaxed);
            let wait_ms = deadline.saturating_duration_since(Instant::now()).as_millis().min(u128::from(u32::MAX)) as u32;
            // SAFETY: retained handle of this fixture member; bounded by the original absolute deadline.
            if unsafe { WaitForSingleObject(member.handle.as_raw_handle(), wait_ms) } != WAIT_OBJECT_0 || Instant::now() >= deadline {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            drop(member_thread);
            drop(member);
            observed.store(3, Ordering::Relaxed);
            wait_fixture_root_only(root_pid, deadline, || query(job))?;
            observed.store(4, Ordering::Relaxed);
            Ok(())
        }));
        progress
    }

    #[test]
    fn exited_member_between_pid_snapshot_and_open_does_not_invent_an_identity() {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut job = Job::new().unwrap();
        let (root, root_thread) = suspended_observation_member(&job, deadline);
        let (member, member_thread) = suspended_observation_member(&job, deadline);
        let root_pid = root.identity.pid;
        let member_pid = member.identity.pid;
        let mut receipt =
            ProcessReceipt { root: Some(root.identity.clone()), observed: vec![root.identity.clone()], stop: ProcessStop::Completed, ..Default::default() };
        job.processes.push(root);
        let _progress = remove_fixture_member_after_snapshot(&mut job, root_pid, member, member_thread, deadline);
        // Fault injection is explicit: object lifetime/other observers can keep an exited PID openable.
        // Actual member exit and Job absence above are real, not mocked.
        job.injected_open_error = Some((member_pid, ERROR_INVALID_PARAMETER as i32));
        job.observe(&mut receipt).expect("a disappeared member needs fresh Job evidence, not an invented identity");
        assert!(job.injected_open_error.is_none(), "one error must be consumed for the captured member");
        assert_eq!(job.processes.len(), 1);
        assert_eq!(receipt.observed.len(), 1);
        assert_eq!(receipt.observed[0].pid, root_pid);
        assert_eq!(receipt.total_processes, 2);
        assert!(!receipt.root_exited && !receipt.job_empty && !receipt.observed_exited);
        assert!(!receipt.cleanup_confirmed(), "an active root/Job must never acquire a completion witness");
        job.terminate().unwrap();
        let root = &job.processes[0];
        let wait_ms = deadline.saturating_duration_since(Instant::now()).as_millis().min(u128::from(u32::MAX)) as u32;
        // SAFETY: wait only for the retained root of the terminated owned Job.
        assert_eq!(unsafe { WaitForSingleObject(root.handle.as_raw_handle(), wait_ms) }, WAIT_OBJECT_0);
        while Instant::now() < deadline {
            job.observe(&mut receipt).unwrap();
            if receipt.root_exited && receipt.job_empty && receipt.observed_exited {
                break;
            }
            std::thread::sleep(POLL);
        }
        assert!(Instant::now() < deadline && receipt.root_exited && receipt.job_empty && receipt.observed_exited);
        assert!(!receipt.cleanup_confirmed(), "exit observation cannot manufacture missing pipe/owner evidence");
        drop(root_thread);
    }

    #[test]
    fn member_open_error_does_not_hide_a_live_pid_or_access_denial() {
        use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
        for (disappear, error) in [(false, ERROR_INVALID_PARAMETER), (false, ERROR_ACCESS_DENIED), (true, ERROR_ACCESS_DENIED)] {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut job = Job::new().unwrap();
            let (root, root_thread) = suspended_observation_member(&job, deadline);
            let (member, member_thread) = suspended_observation_member(&job, deadline);
            let root_pid = root.identity.pid;
            let member_pid = member.identity.pid;
            let mut receipt =
                ProcessReceipt { root: Some(root.identity.clone()), observed: vec![root.identity.clone()], stop: ProcessStop::Completed, ..Default::default() };
            job.processes.push(root);
            let (retained, progress) = if disappear {
                let progress = remove_fixture_member_after_snapshot(&mut job, root_pid, member, member_thread, deadline);
                (None, Some(progress))
            } else {
                (Some((member, member_thread)), None)
            };
            job.injected_open_error = Some((member_pid, error as i32));
            let failed = job.observe(&mut receipt).expect_err("only a confirmed-gone invalid-parameter candidate can be skipped");
            assert_eq!(
                failed.raw_os_error(),
                Some(error as i32),
                "disappear={disappear} expected={error} kind={:?} injection_pending={} hook_pending={} hook_stage={}",
                failed.kind(),
                job.injected_open_error.is_some(),
                job.after_pid_list.is_some(),
                progress.as_ref().map_or(0, |value| value.load(std::sync::atomic::Ordering::Relaxed)),
            );
            assert!(job.injected_open_error.is_none());
            assert!(!exited(&job.processes[0].handle).unwrap());
            assert_eq!(receipt.observed.len(), 1);
            assert_eq!(job.processes.len(), 1);
            assert!(!receipt.cleanup_confirmed());
            job.terminate().unwrap();
            for handle in std::iter::once(&job.processes[0].handle).chain(retained.iter().map(|(member, _)| &member.handle)) {
                let wait_ms = deadline.saturating_duration_since(Instant::now()).as_millis().min(u128::from(u32::MAX)) as u32;
                // SAFETY: only retained fixture handles from this terminated owned Job, under the original deadline.
                assert_eq!(unsafe { WaitForSingleObject(handle.as_raw_handle(), wait_ms) }, WAIT_OBJECT_0);
            }
            assert!(Instant::now() < deadline);
            drop(root_thread);
        }
    }

    #[test]
    fn fixture_partial_pid_snapshot_does_not_replace_an_injected_access_denial() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut job = Job::new().unwrap();
        let (root, root_thread) = suspended_observation_member(&job, deadline);
        let (member, member_thread) = suspended_observation_member(&job, deadline);
        let root_pid = root.identity.pid;
        let member_pid = member.identity.pid;
        let mut receipt =
            ProcessReceipt { root: Some(root.identity.clone()), observed: vec![root.identity.clone()], stop: ProcessStop::Completed, ..Default::default() };
        job.processes.push(root);
        let injected = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&injected);
        let progress = remove_fixture_member_with_query(&mut job, root_pid, member, member_thread, deadline, move |job| {
            if !seen.swap(true, Ordering::Relaxed) {
                // The actual owned member has exited. One incomplete snapshot
                // must remain unknown, not abort this fixture's precondition.
                fixture_pid_list(2, 1, &[root_pid as usize])
            } else {
                fixture_job_pids(job)
            }
        });
        job.injected_open_error = Some((member_pid, ERROR_ACCESS_DENIED as i32));
        let failed = job.observe(&mut receipt).expect_err("access denial must still be rejected");
        assert!(injected.load(Ordering::Relaxed), "controlled partial query must actually be reached");
        assert_eq!(failed.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32), "partial fixture query must not replace injected denial");
        assert_eq!(progress.load(Ordering::Relaxed), 4);
        assert!(job.injected_open_error.is_none());
        assert_eq!(job.processes.len(), 1);
        assert_eq!(receipt.observed.len(), 1);
        assert!(!exited(&job.processes[0].handle).unwrap());
        assert!(!receipt.cleanup_confirmed());
        job.terminate().unwrap();
        let wait_ms = deadline.saturating_duration_since(Instant::now()).as_millis().min(u128::from(u32::MAX)) as u32;
        // SAFETY: exact retained root owned by this fixture and original deadline.
        assert_eq!(unsafe { WaitForSingleObject(job.processes[0].handle.as_raw_handle(), wait_ms) }, WAIT_OBJECT_0);
        drop(root_thread);
    }

    #[test]
    fn fixture_root_only_result_after_the_original_deadline_is_rejected() {
        let deadline = Instant::now() + Duration::from_millis(20);
        let mut queried = false;
        let result = wait_fixture_root_only(17, deadline, || {
            queried = true;
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(Some(vec![17]))
        });
        assert!(queried, "late-result fixture must actually issue its query");
        assert_eq!(result.expect_err("late root-only result must not be admitted").kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn incomplete_or_malformed_pid_lists_cannot_prove_absence() {
        assert_eq!(complete_pid_list(2, 2, &[17, 23]).unwrap(), vec![17, 23]);
        assert!(complete_pid_list(2, 1, &[17]).is_err(), "missing candidate in a partial list is not absence");
        assert!(complete_pid_list(1, 2, &[17, 23]).is_err());
        assert!(complete_pid_list(2, 2, &[17]).is_err());
        assert!(complete_pid_list(0, 0, &[]).unwrap().is_empty());
        assert!(complete_pid_list(1, 1, &[0]).is_err());
        assert!(complete_pid_list(2, 2, &[17, 17]).is_err());
        assert!(complete_pid_list(1, 1, &[usize::MAX]).is_err());
        assert!(complete_pid_list(MAX_ACTIVE as u32 + 1, MAX_ACTIVE as u32 + 1, &[17; MAX_ACTIVE + 1]).is_err());
    }

    #[test]
    fn root_exit_between_accounting_and_root_query_refreshes_the_job_sample() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        let fixture_name = format!("{}::accounting_exit_fixture", module_path!().split_once("::").unwrap().1);
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", &fixture_name, "--nocapture", "--", "--owned-accounting-exit"]);
        let name = wide(OsStr::new("NUL")).unwrap();
        let security = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: null_mut(), bInheritHandle: 1 };
        let stdio: Vec<_> = (0..3)
            .map(|_| {
                // SAFETY: fixed NUL device, owned inheritable stdio handles, no pipe or filesystem payload.
                owned(unsafe {
                    CreateFileW(
                        name.as_ptr(),
                        GENERIC_READ_ACCESS | GENERIC_WRITE_ACCESS,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        &security,
                        OPEN_EXISTING,
                        FILE_ATTRIBUTE_NORMAL,
                        null_mut(),
                    )
                })
                .unwrap()
            })
            .collect();
        let mut job = Job::new().unwrap();
        let mut spawn_failed = false;
        let spawn_deadline = Instant::now() + Duration::from_secs(5);
        let (root, thread) = spawn_owned(
            SpawnContext { command: &command, environment: &[], deadline: spawn_deadline, pin: None },
            &job,
            [&stdio[0], &stdio[1], &stdio[2]],
            &mut spawn_failed,
        )
        .unwrap();
        let mut receipt =
            ProcessReceipt { root: Some(root.identity.clone()), observed: vec![root.identity.clone()], stop: ProcessStop::Completed, ..Default::default() };
        job.processes.push(root);
        let old_order_would_report_lingering = Arc::new(AtomicBool::new(false));
        let observed_old_order = Arc::clone(&old_order_would_report_lingering);
        let hook_root_wait = Arc::new(AtomicU32::new(u32::MAX));
        let observed_hook_wait = Arc::clone(&hook_root_wait);
        job.after_first_accounting = Some(Box::new(move |job, first| {
            assert!(!first.job_empty, "suspended real root must be active at the first sample");
            assert_eq!(first.total_processes, 1);
            let primary_thread = thread; // Keep the suspended primary-thread handle until root exit is confirmed.
            let root = &job.processes[0];
            if Instant::now() >= spawn_deadline {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            // SAFETY: terminate only the retained process created suspended in this test's fresh Job.
            // Its primary thread is never resumed; this isolates accounting order from executed fixture code.
            check(unsafe { windows_sys::Win32::System::Threading::TerminateProcess(root.handle.as_raw_handle(), 0) })?;
            let wait_ms = spawn_deadline.saturating_duration_since(Instant::now()).as_millis().min(u128::from(u32::MAX)) as u32;
            // SAFETY: finite wait on our retained root, bounded by the original absolute spawn deadline.
            let wait = unsafe { WaitForSingleObject(root.handle.as_raw_handle(), wait_ms) };
            observed_hook_wait.store(wait, Ordering::SeqCst);
            if wait != WAIT_OBJECT_0 || Instant::now() >= spawn_deadline {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            // The old decision combines this actual root-exit query with the earlier active sample.
            observed_old_order.store(exited(&root.handle)? && !first.job_empty, Ordering::SeqCst);
            drop(primary_thread);
            Ok(())
        }));
        job.observe(&mut receipt).unwrap();
        let diagnostic_started = Instant::now();
        let final_observation_deadline = spawn_deadline.min(diagnostic_started + CLEANUP);
        assert!(!lingering_descendants(&receipt), "an exited observed root alone is not a live descendant: {receipt:?}");
        let diagnostic_deadline = spawn_deadline.min(diagnostic_started + Duration::from_millis(1000));
        // Diagnostics are separate later observations; never overwrite the receipt under test.
        let retained: Vec<_> = job
            .processes
            .iter()
            .enumerate()
            .map(|(index, process)| {
                // SAFETY: zero wait only on a retained observer handle from this owned Job.
                let wait = unsafe { WaitForSingleObject(process.handle.as_raw_handle(), 0) };
                let wait_error = if wait == windows_sys::Win32::Foundation::WAIT_FAILED { os_error().raw_os_error() } else { None };
                let mut image = vec![0u16; 32768];
                let mut image_len = image.len() as u32;
                // SAFETY: retained query-capable process handle and bounded writable UTF-16 buffer.
                let image_ok = unsafe {
                    windows_sys::Win32::System::Threading::QueryFullProcessImageNameW(process.handle.as_raw_handle(), 0, image.as_mut_ptr(), &mut image_len)
                };
                let image_error = if image_ok == 0 { os_error().raw_os_error() } else { None };
                // Never place the full image path in diagnostic output, including on errors.
                let image_basename = if image_ok != 0 && (image_len as usize) < image.len() {
                    Path::new(&OsString::from_wide(&image[..image_len as usize])).file_name().map(OsStr::to_owned)
                } else {
                    None
                };
                let mut code = 0;
                // SAFETY: read-only exit query on that same retained handle; wait remains the liveness evidence.
                let code_ok = unsafe { GetExitCodeProcess(process.handle.as_raw_handle(), &mut code) };
                let code_error = if code_ok == 0 { os_error().raw_os_error() } else { None };
                (process.identity.clone(), index == 0, wait, wait_error, image_basename, image_error, (code_ok != 0).then_some(code as i32), code_error)
            })
            .collect();
        let mut current_accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: read-only query on the owned Job into a correctly sized aligned local output.
        let accounting_ok = unsafe {
            QueryInformationJobObject(
                job.handle.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut current_accounting).cast(),
                size_of_val(&current_accounting) as u32,
                null_mut(),
            )
        };
        let accounting_error = if accounting_ok == 0 { os_error().raw_os_error() } else { None };
        let mut current_root_code = 0;
        // SAFETY: query the same retained root handle; code alone is not a liveness test.
        let root_code_ok = unsafe { GetExitCodeProcess(job.processes[0].handle.as_raw_handle(), &mut current_root_code) };
        let root_code_error = if root_code_ok == 0 { os_error().raw_os_error() } else { None };
        eprintln!(
            "accounting race metadata: fixture={fixture_name} receipt={receipt:?} observed_count={} retained_count={} retained_identity_is_root_wait_error_basename_image_error_exit_code_error={retained:?} hook_root_wait={} raw_accounting_ok={accounting_ok} raw_accounting_error={accounting_error:?} raw_active={} raw_total={} root_code_ok={root_code_ok} root_code_error={root_code_error:?} current_root_code={}",
            receipt.observed.len(),
            job.processes.len(),
            hook_root_wait.load(Ordering::SeqCst),
            current_accounting.ActiveProcesses,
            current_accounting.TotalProcesses,
            current_root_code as i32
        );
        type RetainedWaitSample = (ProcessIdentity, bool, u32, Option<i32>, Option<i32>, Option<i32>);
        #[derive(Clone)]
        struct AccountingSample {
            waits: Vec<RetainedWaitSample>,
            active: Option<u32>,
            total: Option<u32>,
            query_error: Option<i32>,
            elapsed_ms: u128,
        }
        let first_sample = AccountingSample {
            waits: retained
                .iter()
                .map(|(identity, is_root, wait, error, _, _, code, code_error)| (identity.clone(), *is_root, *wait, *error, *code, *code_error))
                .collect(),
            active: (accounting_ok != 0).then_some(current_accounting.ActiveProcesses),
            total: (accounting_ok != 0).then_some(current_accounting.TotalProcesses),
            query_error: accounting_error,
            elapsed_ms: diagnostic_started.elapsed().as_millis(),
        };
        let sample = || {
            let waits = job
                .processes
                .iter()
                .enumerate()
                .map(|(index, process)| {
                    // SAFETY: zero wait on the same retained observer handles; no acquisition or close.
                    let wait = unsafe { WaitForSingleObject(process.handle.as_raw_handle(), 0) };
                    let error = if wait == windows_sys::Win32::Foundation::WAIT_FAILED { os_error().raw_os_error() } else { None };
                    let mut code = 0;
                    // SAFETY: read-only exit query on the same retained handle, never used instead of its wait result.
                    let ok = unsafe { GetExitCodeProcess(process.handle.as_raw_handle(), &mut code) };
                    let code_error = if ok == 0 { os_error().raw_os_error() } else { None };
                    (process.identity.clone(), index == 0, wait, error, (ok != 0).then_some(code as i32), code_error)
                })
                .collect();
            let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            // SAFETY: separate local output for a read-only query of the same owned Job.
            let ok = unsafe {
                QueryInformationJobObject(
                    job.handle.as_raw_handle(),
                    JobObjectBasicAccountingInformation,
                    std::ptr::from_mut(&mut accounting).cast(),
                    size_of_val(&accounting) as u32,
                    null_mut(),
                )
            };
            let query_error = if ok == 0 { os_error().raw_os_error() } else { None };
            AccountingSample {
                waits,
                active: (ok != 0).then_some(accounting.ActiveProcesses),
                total: (ok != 0).then_some(accounting.TotalProcesses),
                query_error,
                elapsed_ms: diagnostic_started.elapsed().as_millis(),
            }
        };
        let mut last_sample = first_sample.clone();
        let mut snapshots = vec![first_sample];
        let milestones = [10, 50, 100, 500, 1000];
        let mut milestone = 0;
        while Instant::now() < diagnostic_deadline
            && !(last_sample.active == Some(0) && last_sample.waits.iter().all(|(_, _, wait, _, _, _)| *wait == WAIT_OBJECT_0))
        {
            let remaining = diagnostic_deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
            if Instant::now() >= diagnostic_deadline {
                break;
            }
            last_sample = sample();
            if milestone < milestones.len() && last_sample.elapsed_ms >= milestones[milestone] {
                snapshots.push(last_sample.clone());
                while milestone < milestones.len() && last_sample.elapsed_ms >= milestones[milestone] {
                    milestone += 1;
                }
            }
        }
        snapshots.push(last_sample);
        for (index, snapshot) in snapshots.iter().enumerate() {
            eprintln!(
                "accounting bounded metadata: sample={index} waits_identity_is_root_wait_error_exit_code_error={:?} active={:?} total={:?} query_error={:?} elapsed_ms={}",
                snapshot.waits, snapshot.active, snapshot.total, snapshot.query_error, snapshot.elapsed_ms
            );
        }
        eprintln!("accounting bounded observation_elapsed_ms={}", diagnostic_started.elapsed().as_millis());
        assert!(old_order_would_report_lingering.load(Ordering::SeqCst), "the real interleaving must reproduce the old false-positive predicate");
        let mut final_receipt = receipt.clone();
        while Instant::now() < final_observation_deadline {
            job.observe(&mut final_receipt).unwrap();
            assert!(!lingering_descendants(&final_receipt), "no actual observed live descendant is expected");
            if final_receipt.root_exited && final_receipt.observed_exited && final_receipt.job_empty {
                break;
            }
            let remaining = final_observation_deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(remaining.min(POLL));
        }
        assert!(Instant::now() < final_observation_deadline, "final OS evidence must be observed within the original fixed deadlines");
        assert!(final_receipt.root_exited && final_receipt.observed_exited);
        assert_eq!(final_receipt.root_exit_code, Some(0));
        assert_eq!(final_receipt.total_processes, 1);
        assert!(final_receipt.job_empty, "post-root-exit accounting must eventually report actual zero within the fixed deadlines");
        assert!(!spawn_failed);
    }

    #[test]
    fn compatible_short_executable_paths_drop_only_a_safe_verbatim_prefix() {
        for (original, expected) in [
            (r"\\?\C:\fixtures\tool.exe", r"C:\fixtures\tool.exe"),
            (r"\\?\UNC\example.invalid\share\tool.exe", r"\\example.invalid\share\tool.exe"),
            (r"C:\fixtures\tool.exe", r"C:\fixtures\tool.exe"),
        ] {
            assert_eq!(user_executable_path(PathBuf::from(original)).unwrap(), PathBuf::from(expected));
        }
    }

    #[test]
    fn executable_path_normalization_preserves_distinct_names_and_namespaces() {
        for original in [
            r"\\?\C:\fixtures\tool.exe.",
            r"\\?\C:\fixtures\tool.exe ",
            r"\\?\C:\fixtures\..\tool.exe",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\tool.exe",
        ] {
            let path = PathBuf::from(original);
            assert_eq!(user_executable_path(path.clone()).unwrap(), path);
        }
    }

    #[test]
    fn executable_path_prefix_conversion_respects_the_utf16_length_boundary() {
        let prefix = r"\\?\C:\";
        let at_limit = PathBuf::from(format!("{prefix}{}", "x".repeat(252)));
        assert_eq!(wide(at_limit.as_os_str()).unwrap().len(), 260);
        assert_eq!(user_executable_path(at_limit).unwrap(), PathBuf::from(format!("C:\\{}", "x".repeat(252))));
        let over_limit = PathBuf::from(format!("{prefix}{}", "x".repeat(253)));
        assert_eq!(user_executable_path(over_limit.clone()).unwrap(), over_limit);
    }

    #[test]
    fn complete_environment_is_explicit_sorted_and_double_terminated() {
        let mut command = std::process::Command::new("example.exe");
        command.env_clear();
        assert_eq!(environment(&command, &[]).unwrap(), vec![0, 0]);
        let entries = [(OsString::from("b"), OsString::from("second")), (OsString::from("A"), OsString::from("first"))];
        let expected: Vec<_> = "A=first\0b=second\0\0".encode_utf16().collect();
        assert_eq!(environment(&command, &entries).unwrap(), expected);
    }

    #[test]
    fn empty_path_entries_never_add_the_working_directory() {
        let paths: Vec<_> = path_entries(OsStr::new(r";C:\trusted;;D:\tools;")).collect();
        assert_eq!(paths, vec![PathBuf::from(r"C:\trusted"), PathBuf::from(r"D:\tools")]);
        assert_eq!(path_entries(OsStr::new(";;")).count(), 0);
    }
}
