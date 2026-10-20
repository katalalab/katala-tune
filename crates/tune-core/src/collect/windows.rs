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
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT},
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

use super::{BoundedResult, ProcessIdentity, ProcessReceipt, ProcessStop};

const CLEANUP: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(10);
const MAX_ACTIVE: usize = 256;
const MAX_OBSERVED: usize = 4096;
const SYNCHRONIZE_PROCESS: u32 = 0x0010_0000;
const GENERIC_READ_ACCESS: u32 = 0x8000_0000;
const GENERIC_WRITE_ACCESS: u32 = 0x4000_0000;

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

struct Job {
    handle: OwnedHandle,
    processes: Vec<Process>,
}
impl Job {
    fn new() -> io::Result<Self> {
        // SAFETY: null name creates a fresh unnamed Job; null attributes prevent inheritance.
        let handle = owned(unsafe { CreateJobObjectW(null(), null()) })?;
        let job = Self { handle, processes: Vec::new() };
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
    fn observe(&mut self, receipt: &mut ProcessReceipt) -> io::Result<()> {
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
        if pids.count as usize > MAX_ACTIVE || pids.assigned as usize > MAX_ACTIVE {
            return Err(invalid());
        }
        for pid in &pids.pids[..pids.count as usize] {
            let pid = u32::try_from(*pid).map_err(|_| invalid())?;
            if self.processes.iter().any(|p| p.identity.pid == pid) {
                continue;
            }
            if self.processes.len() >= MAX_OBSERVED {
                return Err(invalid());
            }
            // SAFETY: observation only; retain query/synchronize handle, no terminate rights requested.
            let handle = owned(unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | SYNCHRONIZE_PROCESS, 0, pid) })?;
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
        receipt.observed_exited = !self.processes.is_empty();
        for process in &self.processes {
            receipt.observed_exited &= exited(&process.handle)?;
        }
        let root = self.processes.first().ok_or_else(invalid)?;
        receipt.root_exited = exited(&root.handle)?;
        if receipt.root_exited {
            let mut code = 0;
            // SAFETY: retained root has signaled; 259 is a valid exit value, never a liveness test.
            check(unsafe { GetExitCodeProcess(root.handle.as_raw_handle(), &mut code) })?;
            receipt.root_exit_code = Some(code as i32);
        }
        Ok(())
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.terminate(); // cancellation fallback, plus KILL_ON_JOB_CLOSE on the sole non-inherited handle.
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
fn environment(command: &std::process::Command) -> io::Result<Vec<u16>> {
    let mut vars: Vec<_> = std::env::vars_os().collect();
    for (name, value) in command.get_envs() {
        let units = wide(name)?;
        if units.len() == 1 || units.contains(&u16::from(b'=')) {
            return Err(invalid());
        }
        vars.retain(|(key, _)| compare_names(key, name) != Ordering::Equal);
        if let Some(value) = value {
            vars.push((name.to_owned(), value.to_owned()));
        }
    }
    vars.sort_by(|(a, _), (b, _)| compare_names(a, b));
    let mut output = Vec::new();
    for (key, value) in vars {
        let mut key = wide(&key)?;
        key.pop();
        let value = wide(&value)?;
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

fn executable(command: &std::process::Command) -> io::Result<PathBuf> {
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
    let child_path = command.get_envs().find(|(k, _)| compare_names(k, OsStr::new("PATH")) == Ordering::Equal).and_then(|(_, v)| v);
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
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(path_entries(&path));
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

fn spawn_owned(
    command: &std::process::Command,
    job: &Job,
    input: &Pipe,
    out: &Pipe,
    err: &Pipe,
    spawn_failed: &mut bool,
    deadline: Instant,
) -> io::Result<(Process, OwnedHandle)> {
    let executable = user_executable_path(executable(command)?)?;
    let application = wide(executable.as_os_str())?;
    let mut argv = command_line(&executable, command)?;
    let env = environment(command)?;
    let cwd = command.get_current_dir().map(|p| wide(p.as_os_str())).transpose()?;
    // Value arrays are declared before Attributes and remain alive until its destructor.
    let jobs = [job.handle.as_raw_handle()];
    let handles = [input.client.as_raw_handle(), out.client.as_raw_handle(), err.client.as_raw_handle()];
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
        *spawn_failed = true;
        return Err(os_error());
    }
    let handle = owned(created.hProcess)?;
    let thread = owned(created.hThread)?;
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

pub(super) async fn run(command: &std::process::Command, input: &[u8], timeout: Duration, stdout_limit: usize, stderr_limit: usize) -> BoundedResult {
    let mut answer = BoundedResult::default();
    let mut receipt = ProcessReceipt::default();
    let deadline = Instant::now().checked_add(timeout);
    let setup = {
        let setup = async {
            if deadline.is_none_or(|end| Instant::now() >= end) {
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
            let spawn = spawn_owned(command, &job, &input_pipe, &stdout, &stderr, &mut answer.spawn_failed, deadline.expect("validated deadline"));
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
            if Instant::now() >= deadline.expect("validated deadline") {
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
        match deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, setup).await {
                Ok(result) => result.ok(),
                Err(_) => {
                    answer.timed_out = true;
                    receipt.stop = ProcessStop::Timeout;
                    None
                }
            },
            None => None,
        }
    };
    let Some((mut job, mut input_pipe, stdout, stderr)) = setup else {
        if answer.spawn_failed {
            receipt.stop = ProcessStop::SpawnFailed;
        }
        answer.process_receipt = Some(receipt);
        return answer;
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
    let mut cleanup_deadline = None;
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
        if job.observe(&mut receipt).is_err() {
            receipt.job_empty = false;
            receipt.observed_exited = false;
            if receipt.stop == ProcessStop::Completed {
                receipt.stop = ProcessStop::IoFailure;
            }
        }
        if receipt.root_exited && !receipt.job_empty && receipt.stop == ProcessStop::Completed {
            receipt.stop = ProcessStop::LingeringDescendants;
        }
        let now = Instant::now();
        // All evidence must have been observed before the fixed execution deadline, even during pipe cleanup.
        if now >= deadline.expect("validated deadline") && receipt.stop == ProcessStop::Completed {
            answer.timed_out = true;
            receipt.stop = ProcessStop::Timeout;
        }
        if receipt.cleanup_confirmed() {
            break;
        }
        if cleanup_deadline.is_none() && (receipt.stop != ProcessStop::Completed || receipt.root_exited) {
            cleanup_deadline = Some(now + CLEANUP);
            if receipt.stop != ProcessStop::Completed {
                let _ = job.terminate();
            }
        }
        if cleanup_deadline.is_some_and(|end| now >= end) {
            if receipt.stop == ProcessStop::Completed {
                receipt.stop = ProcessStop::CleanupUnconfirmed;
            }
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    answer.code = receipt.root_exit_code;
    answer.process_receipt = Some(receipt);
    answer.seal_windows_payload(Instant::now() >= deadline.expect("validated deadline"));
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn empty_path_entries_never_add_the_working_directory() {
        let paths: Vec<_> = path_entries(OsStr::new(r";C:\trusted;;D:\tools;")).collect();
        assert_eq!(paths, vec![PathBuf::from(r"C:\trusted"), PathBuf::from(r"D:\tools")]);
        assert_eq!(path_entries(OsStr::new(";;")).count(), 0);
    }
}

#[cfg(test)]
mod native_regression_tests {
    use super::*;
    use std::io::{Read, Write};
    const FIXTURE: &str = "KATALA_BOUNDED_LIB_FIXTURE";

    #[test]
    fn stdio_fixture() {
        if std::env::var(FIXTURE).as_deref() != Ok("stdio") {
            return;
        }
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input).unwrap();
        std::io::stdout().write_all(&input).unwrap();
        std::io::stderr().write_all(b"fixture-stderr").unwrap();
        std::process::exit(0);
    }

    #[tokio::test]
    async fn output_pipe_observes_eof_after_its_only_client_closes() {
        let session = nonce().unwrap();
        let Pipe { mut server, client } = {
            let security = pipe_security().unwrap();
            pipe(&session, "eof-test", false, &security).unwrap()
        };
        server.connect().await.unwrap();
        drop(client);
        let mut byte = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(3), server.read(&mut byte)).await;
        let count = read.as_ref().ok().and_then(|r| r.as_ref().ok()).copied();
        let error_code = read.as_ref().ok().and_then(|r| r.as_ref().err()).and_then(io::Error::raw_os_error);
        assert_eq!(count, Some(0), "pipe metadata: timeout={} os_error={error_code:?}", read.is_err());
    }

    #[tokio::test]
    async fn product_guard_runs_a_native_rust_stdio_fixture() {
        let name = format!("{}::stdio_fixture", module_path!().split_once("::").unwrap().1);
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", &name, "--nocapture"]).env(FIXTURE, "stdio");
        let result = run(&command, b"fixture-input", Duration::from_secs(5), 65536, 65536).await;
        assert_eq!(
            result.code,
            Some(0),
            "guard metadata: timeout={} output_limit={} spawn_failed={} receipt={:?}",
            result.timed_out,
            result.output_limited,
            result.spawn_failed,
            result.process_receipt
        );
        assert!(result.stdout.ends_with(b"fixture-input"));
        assert_eq!(result.stderr, b"fixture-stderr");
    }

    async fn stdio_control(program: &Path) -> (Option<i32>, bool) {
        let mut command = tokio::process::Command::new(program);
        command
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "[Console]::Out.Write('guard-marker')"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let Ok(child) = command.spawn() else {
            return (None, false);
        };
        match tokio::time::timeout(Duration::from_secs(10), child.wait_with_output()).await {
            Ok(Ok(output)) => (output.status.code(), false),
            Ok(Err(_)) => (None, false),
            Err(_) => (None, true),
        }
    }

    #[tokio::test]
    async fn product_guard_preserves_simple_windows_powershell_startup() {
        let mut buffer = [0u16; 32768];
        // SAFETY: writable bounded buffer for the OS Windows-directory query; no environment-derived path is logged.
        let count = unsafe { GetWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
        assert!(count > 0 && (count as usize) < buffer.len());
        use std::os::windows::ffi::OsStringExt;
        let normal =
            PathBuf::from(OsString::from_wide(&buffer[..count as usize])).join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe");
        let canonical = normal.canonicalize().unwrap();
        let normal_control = stdio_control(&normal).await;
        let canonical_control = stdio_control(&canonical).await;
        eprintln!("PowerShell startup control metadata: normal={normal_control:?} canonical={canonical_control:?}");
        assert_eq!(normal_control, (Some(0), false));
        let mut command = std::process::Command::new(normal);
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "[Console]::Out.Write('guard-marker')"]);
        let result = run(&command, b"", Duration::from_secs(10), 65536, 65536).await;
        assert_eq!(
            result.code,
            Some(0),
            "guard metadata: timeout={} output_limit={} spawn_failed={} receipt={:?}",
            result.timed_out,
            result.output_limited,
            result.spawn_failed,
            result.process_receipt
        );
        assert_eq!(result.stdout, b"guard-marker");
    }
}
