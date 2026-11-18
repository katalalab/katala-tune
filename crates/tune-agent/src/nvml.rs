//! Read the installed NVIDIA driver once per request. The parent owns the worker and its deadline.
#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_void};
use std::io::{Read, Write};
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};

type Device = *mut c_void;
type Status = i32;
type Shutdown = unsafe extern "C" fn() -> Status;
type Scalar = unsafe extern "C" fn(Device, *mut u32) -> Status;
type Selector = unsafe extern "C" fn(Device, u32, *mut u32) -> Status;
type Text = unsafe extern "C" fn(Device, *mut c_char, u32) -> Status;

#[repr(C)]
#[derive(Default)]
struct Memory {
    version: u32,
    total: u64,
    reserved: u64,
    free: u64,
    used: u64,
}

struct Library(HMODULE);
impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: owned reference returned by LoadLibraryExW.
        unsafe { FreeLibrary(self.0) };
    }
}
struct Session(Shutdown);
impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: one successful initialization; the library outlives the session.
        unsafe { (self.0)() };
    }
}

fn field<T: Default>(read: impl FnOnce(&mut T) -> Status) -> Option<T> {
    let mut value = T::default();
    (read(&mut value) == 0).then_some(value)
}

fn string(read: impl FnOnce(*mut c_char, u32) -> Status) -> Option<String> {
    let mut bytes = [0u8; 256];
    if read(bytes.as_mut_ptr().cast(), bytes.len() as u32) != 0 {
        return None;
    }
    let text = CStr::from_bytes_until_nul(&bytes).ok()?.to_str().ok()?;
    (!text.contains([',', '\r', '\n'])).then(|| text.to_owned())
}

fn csv(value: Option<impl ToString>) -> String {
    value.map_or_else(|| "N/A".into(), |v| v.to_string())
}

pub fn query() -> Option<()> {
    // Wait for the parent to assign our kill-on-close job before entering the driver.
    let mut stdin = std::io::stdin().lock();
    let mut request = [0u8; 1];
    if stdin.read(&mut request).ok()? == 0 {
        return Some(());
    }
    let name: Vec<u16> = "nvml.dll".encode_utf16().chain([0]).collect();
    // SAFETY: NUL-terminated name, system32-only library and dependency lookup.
    let module = unsafe { LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
    if module.is_null() {
        return None;
    }
    let dll = Library(module);
    // Signatures and output layouts below follow NVIDIA's nvml.h C ABI.
    macro_rules! symbol {
        ($name:literal, $ty:ty) => {{
            // SAFETY: the symbol name matches the stated prototype, and dll remains loaded.
            let proc = unsafe { GetProcAddress(dll.0, concat!($name, "\0").as_ptr()) }?;
            unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(proc) }
        }};
    }
    let init = symbol!("nvmlInit_v2", Shutdown);
    let shutdown = symbol!("nvmlShutdown", Shutdown);
    let count = symbol!("nvmlDeviceGetCount_v2", unsafe extern "C" fn(*mut u32) -> Status);
    let handle = symbol!("nvmlDeviceGetHandleByIndex_v2", unsafe extern "C" fn(u32, *mut Device) -> Status);
    let version = symbol!("nvmlSystemGetDriverVersion", unsafe extern "C" fn(*mut c_char, u32) -> Status);
    let name = symbol!("nvmlDeviceGetName", Text);
    let uuid = symbol!("nvmlDeviceGetUUID", Text);
    let mem = symbol!("nvmlDeviceGetMemoryInfo_v2", unsafe extern "C" fn(Device, *mut Memory) -> Status);
    let util = symbol!("nvmlDeviceGetUtilizationRates", unsafe extern "C" fn(Device, *mut [u32; 2]) -> Status);
    let temperature = symbol!("nvmlDeviceGetTemperature", Selector);
    let clock = symbol!("nvmlDeviceGetClockInfo", Selector);
    let power = symbol!("nvmlDeviceGetPowerUsage", Scalar);
    let limit = symbol!("nvmlDeviceGetPowerManagementLimit", Scalar);
    let range = symbol!("nvmlDeviceGetPowerManagementLimitConstraints", unsafe extern "C" fn(Device, *mut u32, *mut u32) -> Status);
    let pstate = symbol!("nvmlDeviceGetPerformanceState", Scalar);
    // SAFETY: every output points to its matching local type or sized byte buffer.
    // Handles are used only while the initialized library is loaded.
    unsafe {
        if init() != 0 {
            return None;
        }
        let _session = Session(shutdown);
        let mut out = std::io::stdout().lock();
        loop {
            if request != [1] {
                return None;
            }
            let count = field(|v| count(v))?.min(16);
            if count == 0 {
                return None;
            }
            let driver = string(|p, n| version(p, n));
            let mut rows = Vec::with_capacity(count as usize);
            for index in 0..count {
                let device = field(|v| handle(index, v))?;
                let mut memory = Memory { version: (2 << 24) | std::mem::size_of::<Memory>() as u32, ..Memory::default() };
                if mem(device, &mut memory) != 0 {
                    return None; // Let nvidia-smi try its older query on this driver.
                }
                let mut lo = 0;
                let mut hi = 0;
                let limits = range(device, &mut lo, &mut hi) == 0;
                let watts = |milliwatts: u32| f64::from(milliwatts) / 1000.0;
                let row = [
                    index.to_string(),
                    csv(string(|p, n| name(device, p, n))),
                    csv(field(|v| util(device, v)).map(|v| v[0])),
                    memory.used.div_ceil(1 << 20).to_string(),
                    memory.total.div_ceil(1 << 20).to_string(),
                    memory.free.div_ceil(1 << 20).to_string(),
                    csv(field(|v| temperature(device, 0, v))),
                    csv(field(|v| power(device, v)).map(watts)),
                    csv(field(|v| clock(device, 0, v))),
                    csv(field(|v| clock(device, 2, v))),
                    csv(string(|p, n| uuid(device, p, n))),
                    csv(field(|v| pstate(device, v)).filter(|v| *v <= 15).map(|v| format!("P{v}"))),
                    csv(field(|v| limit(device, v)).map(watts)),
                    csv(limits.then(|| watts(lo))),
                    csv(limits.then(|| watts(hi))),
                    csv(field(|v| clock(device, 1, v))),
                    csv(driver.as_ref()),
                    "N/A".into(), // Reserved CSV slot, not exposed by parse_gpu_query.
                ];
                rows.push(row.join(","));
            }
            serde_json::to_writer(&mut out, &rows.join("\n")).ok()?;
            out.write_all(b"\n").and_then(|_| out.flush()).ok()?;
            if stdin.read(&mut request).ok()? == 0 {
                break;
            }
        }
    }
    Some(())
}
