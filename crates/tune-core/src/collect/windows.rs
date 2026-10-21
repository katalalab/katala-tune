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

#[cfg(test)]
type AccountingHook = Box<dyn FnOnce(&Job, &ProcessReceipt) -> io::Result<()> + Send>;

struct Job {
    handle: OwnedHandle,
    processes: Vec<Process>,
    #[cfg(test)]
    after_first_accounting: Option<AccountingHook>,
}
impl Job {
    fn new() -> io::Result<Self> {
        // SAFETY: null name creates a fresh unnamed Job; null attributes prevent inheritance.
        let handle = owned(unsafe { CreateJobObjectW(null(), null()) })?;
        let job = Self {
            handle,
            processes: Vec::new(),
            #[cfg(test)]
            after_first_accounting: None,
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
    fn observe(&mut self, receipt: &mut ProcessReceipt) -> io::Result<()> {
        self.accounting(receipt)?;
        #[cfg(test)]
        if let Some(hook) = self.after_first_accounting.take() {
            hook(self, receipt)?;
        }
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
            // The initial accounting may predate root exit. Only a subsequent
            // sample can establish that descendants remain after that exit.
            self.accounting(receipt)?;
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
        if units.len() == 1 || units.contains(&u16::from(b'=')) {
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
}

fn spawn_owned(context: SpawnContext<'_>, job: &Job, stdio: [&OwnedHandle; 3], spawn_failed: &mut bool) -> io::Result<(Process, OwnedHandle)> {
    let SpawnContext { command, environment: values, deadline } = context;
    let env = environment(command, values)?;
    let executable = user_executable_path(executable(command, values)?)?;
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

pub(super) async fn run(
    command: &std::process::Command,
    environment: &[(OsString, OsString)],
    input: &[u8],
    deadline: std::time::Instant,
    stdout_limit: usize,
    stderr_limit: usize,
) -> BoundedResult {
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
                SpawnContext { command, environment, deadline },
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
        if now >= deadline && receipt.stop == ProcessStop::Completed {
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
    answer.seal_windows_payload(Instant::now() >= deadline);
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accounting_exit_fixture() {
        if std::env::args().any(|arg| arg == "--owned-accounting-exit") {
            std::process::exit(0);
        }
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
            SpawnContext { command: &command, environment: &[], deadline: spawn_deadline },
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
        assert!(receipt.root_exited && receipt.observed_exited);
        assert_eq!(receipt.root_exit_code, Some(0));
        assert_eq!(receipt.total_processes, 1);
        assert!(receipt.job_empty, "post-root-exit accounting must replace the stale active sample");
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
