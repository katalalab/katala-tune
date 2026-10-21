//! Read-only executable lease for a finite standalone Windows invocation.
//! A matching hash is artifact evidence, not permission to execute an artifact.
#![allow(unsafe_code)] // Read-only Win32/NT file and process queries; no file creation or settings writes.

use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read},
    mem::size_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    time::Instant,
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{FILE_OPEN, FILE_OPEN_NO_RECALL, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile},
    },
    Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE, OBJ_DONT_REPARSE, UNICODE_STRING},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_TRAVERSE, FILE_TYPE_DISK, FileIdInfo,
            GetFileInformationByHandleEx, GetFileType, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW, OPEN_EXISTING, VOLUME_NAME_GUID,
            VOLUME_NAME_NT,
        },
        System::{
            IO::IO_STATUS_BLOCK,
            Threading::{PROCESS_NAME_NATIVE, QueryFullProcessImageNameW},
        },
    },
};

const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
const SYNCHRONIZE: u32 = 0x0010_0000;
const GENERIC_READ: u32 = 0x8000_0000;

fn rejected() -> io::Error {
    io::Error::from(io::ErrorKind::PermissionDenied)
}
fn before(deadline: Instant) -> io::Result<()> {
    if Instant::now() < deadline { Ok(()) } else { Err(io::Error::from(io::ErrorKind::TimedOut)) }
}
fn wide(value: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
    let mut units: Vec<_> = value.encode_wide().collect();
    if units.is_empty() || units.len() >= 32760 || units.contains(&0) {
        return Err(rejected());
    }
    units.push(0);
    Ok(units)
}

/// Only unambiguous local drive paths are accepted at the approval boundary.
fn components(path: &Path) -> io::Result<(String, Vec<String>)> {
    let text = path.to_str().ok_or_else(rejected)?;
    if text.len() > 32760 {
        return Err(rejected());
    }
    let text = text.strip_prefix(r"\\?\").unwrap_or(text).replace('\\', "/");
    if text.len() < 7 || !text.as_bytes()[0].is_ascii_alphabetic() || &text.as_bytes()[1..3] != b":/" || !text.to_ascii_lowercase().ends_with(".exe") {
        return Err(rejected());
    }
    let mut parts = Vec::new();
    for part in text[3..].split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.is_empty()
            || matches!(part, "." | "..")
            || part.ends_with([' ', '.'])
            || part.chars().any(|ch| ch.is_control() || "<>:\"|?*".contains(ch))
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "COM¹" | "COM²" | "COM³" | "LPT¹" | "LPT²" | "LPT³")
            || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && matches!(stem.as_bytes()[3], b'1'..=b'9'))
            || parts.len() >= 128
        {
            return Err(rejected());
        }
        parts.push(part.to_owned());
    }
    Ok((format!("{}:\\", &text[..1]), parts))
}

#[derive(Eq, PartialEq)]
struct FileIdentity {
    volume: u64,
    file: [u8; 16],
}
fn file_identity(file: &File) -> io::Result<FileIdentity> {
    let mut id = FILE_ID_INFO::default();
    // SAFETY: held live file handle and correctly sized writable FILE_ID_INFO.
    if unsafe { GetFileInformationByHandleEx(file.as_raw_handle(), FileIdInfo, (&mut id as *mut FILE_ID_INFO).cast(), size_of::<FILE_ID_INFO>() as u32) } == 0 {
        return Err(rejected());
    }
    Ok(FileIdentity { volume: id.VolumeSerialNumber, file: id.FileId.Identifier })
}
fn final_path(file: &File, flags: u32) -> io::Result<Vec<u16>> {
    let mut units = vec![0; 32768];
    // SAFETY: held live file handle and bounded writable UTF-16 buffer.
    let count = unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), units.as_mut_ptr(), units.len() as u32, flags) } as usize;
    if count == 0 || count >= units.len() {
        return Err(rejected());
    }
    units.truncate(count);
    Ok(units)
}
fn checked_file(handle: HANDLE, directory: bool) -> io::Result<File> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(rejected());
    }
    // SAFETY: caller transfers one successfully opened, non-inheritable handle.
    let file = File::from(unsafe { OwnedHandle::from_raw_handle(handle) });
    use std::os::windows::fs::MetadataExt;
    let metadata = file.metadata()?;
    // SAFETY: the owned handle is live throughout this read-only type query.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
    {
        return Err(rejected());
    }
    Ok(file)
}
fn open_child(parent: &File, name: &str, directory: bool) -> io::Result<File> {
    let mut name: Vec<_> = name.encode_utf16().collect();
    if name.is_empty() || name.len() > 32760 {
        return Err(rejected());
    }
    let bytes = (name.len() * 2) as u16;
    let mut unicode = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: name.as_mut_ptr() };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &mut unicode,
        Attributes: OBJ_DONT_REPARSE,
        SecurityDescriptor: null_mut(),
        SecurityQualityOfService: null_mut(),
    };
    let mut status = IO_STATUS_BLOCK::default();
    let mut handle = null_mut();
    // SAFETY: one validated relative name, retained parent, live buffers and output.
    // FILE_OPEN only: this never creates a file. No component is reopened through a path.
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            if directory { FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE } else { GENERIC_READ | SYNCHRONIZE },
            &attributes,
            &mut status,
            null(),
            0,
            FILE_SHARE_READ,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_OPEN_NO_RECALL | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
            0,
        )
    };
    if result < 0 {
        return Err(rejected());
    }
    checked_file(handle, directory)
}
struct HeldPath {
    file: File,
    identity: FileIdentity,
    native: Vec<u16>,
}
impl HeldPath {
    fn new(file: File) -> io::Result<Self> {
        Ok(Self { identity: file_identity(&file)?, native: final_path(&file, VOLUME_NAME_NT)?, file })
    }
    fn unchanged(&self, directory: bool) -> io::Result<()> {
        use std::os::windows::fs::MetadataExt;
        let attributes = self.file.metadata()?.file_attributes();
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (attributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
            || file_identity(&self.file)? != self.identity
            || final_path(&self.file, VOLUME_NAME_NT)? != self.native
        {
            return Err(rejected());
        }
        Ok(())
    }
}

/// Non-serializable, non-cloneable lease. Callers must obtain the expected hash
/// from their separate approved artifact record, never from the child response.
/// Local NTFS only; unavailable volume GUIDs and unsupported launches fail closed.
pub struct PinnedWindowsExecutable {
    executable: PathBuf,
    sha256: String,
    chain: Vec<HeldPath>,
    deadline: Instant,
}
impl PinnedWindowsExecutable {
    pub fn open(program: &Path, expected_sha256: &str, deadline: Instant) -> io::Result<Self> {
        before(deadline)?;
        if expected_sha256.len() != 64 || !expected_sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(rejected());
        }
        let (root, parts) = components(program)?;
        let root = wide(std::ffi::OsStr::new(&root))?;
        // SAFETY: NUL-terminated local drive root and no inherited/security/template handle.
        let root = checked_file(
            unsafe {
                CreateFileW(
                    root.as_ptr(),
                    FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
                    FILE_SHARE_READ,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL,
                    null_mut(),
                )
            },
            true,
        )?;
        let mut filesystem = [0u16; 32];
        // SAFETY: live directory handle, optional outputs null, bounded filesystem-name output.
        if unsafe {
            GetVolumeInformationByHandleW(
                root.as_raw_handle(),
                null_mut(),
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        } == 0
            || filesystem[..5] != [b'N' as u16, b'T' as u16, b'F' as u16, b'S' as u16, 0]
        {
            return Err(rejected());
        }
        let mut chain = vec![HeldPath::new(root)?];
        for (index, part) in parts.iter().enumerate() {
            before(deadline)?;
            let file = open_child(&chain.last().ok_or_else(rejected)?.file, part, index + 1 < parts.len())?;
            chain.push(HeldPath::new(file)?);
        }
        let image = &mut chain.last_mut().ok_or_else(rejected)?.file;
        let size = image.metadata()?.len();
        if size == 0 || size > MAX_IMAGE_BYTES {
            return Err(rejected());
        }
        let mut hash = Sha256::new();
        let mut actual = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            before(deadline)?;
            let count = image.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            actual = actual.saturating_add(count as u64);
            if actual > size || actual > MAX_IMAGE_BYTES {
                return Err(rejected());
            }
            hash.update(&buffer[..count]);
        }
        let actual_sha256: String = hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
        if actual != size || actual_sha256 != expected_sha256 {
            return Err(rejected());
        }
        let executable = PathBuf::from(std::ffi::OsString::from_wide(&final_path(image, VOLUME_NAME_GUID)?));
        // Do not fall back to the original drive alias if GUID resolution is unavailable.
        if !executable.as_os_str().to_string_lossy().starts_with(r"\\?\Volume{") {
            return Err(rejected());
        }
        let pin = Self { executable, sha256: expected_sha256.to_owned(), chain, deadline };
        pin.unchanged()?;
        Ok(pin)
    }
    pub fn executable(&self) -> &Path {
        &self.executable
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    fn unchanged(&self) -> io::Result<()> {
        before(self.deadline)?;
        for (index, held) in self.chain.iter().enumerate() {
            held.unchanged(index + 1 < self.chain.len())?;
        }
        let path = wide(self.executable.as_os_str())?;
        // SAFETY: immutable volume-GUID path, no inherited/template handle. This
        // extra ID check is backed by the retained no-reparse chain, not a new lease.
        let resolved = checked_file(
            unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL,
                    null_mut(),
                )
            },
            false,
        )?;
        if file_identity(&resolved)? != self.chain.last().ok_or_else(rejected)?.identity {
            return Err(rejected());
        }
        before(self.deadline)?;
        Ok(())
    }
    pub(crate) fn verify_process(&self, process: HANDLE) -> io::Result<()> {
        self.unchanged()?;
        let mut image = vec![0u16; 32768];
        let mut length = image.len() as u32;
        // SAFETY: backend retains the suspended root process handle; bounded writable buffer.
        if unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_NATIVE, image.as_mut_ptr(), &mut length) } == 0 || length as usize >= image.len() {
            return Err(rejected());
        }
        image.truncate(length as usize);
        // Native image name is tied to the immutable retained chain and leaf file ID.
        // Case-sensitive equality intentionally fails closed on ambiguous case aliases.
        if image != self.chain.last().ok_or_else(rejected)?.native {
            return Err(rejected());
        }
        self.unchanged()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pin_path_rejects_aliases_and_accepts_unicode_without_loss() {
        for bad in [
            "reader.exe",
            "C:reader.exe",
            "//server/share/reader.exe",
            "C:/a/../reader.exe",
            "C:/a./reader.exe",
            "C:/a /reader.exe",
            "C:/CON/reader.exe",
            "C:/a/reader.exe:stream",
            "C://reader.exe",
            "C:/a/reader.cmd",
        ] {
            assert!(components(Path::new(bad)).is_err(), "{bad}");
        }
        let (_, parts) = components(Path::new("C:/fixture/試験😀/reader.exe")).unwrap();
        assert_eq!(parts, ["fixture", "試験😀", "reader.exe"]);
    }

    #[test]
    fn same_image_bytes_at_another_file_cannot_attest_the_running_image() {
        let source = std::env::current_exe().unwrap();
        let bytes = std::fs::read(&source).unwrap();
        let digest: String = Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect();
        let end = Instant::now() + std::time::Duration::from_secs(15);
        let current = PinnedWindowsExecutable::open(&source, &digest, end).unwrap();
        // SAFETY: read-only query of the borrowed current process pseudo-handle.
        let process = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() };
        current.verify_process(process).unwrap();
        let root = std::env::temp_dir().join(format!("pin-image-fixture-{}-{}", std::process::id(), crate::db::now_ms()));
        std::fs::create_dir(&root).unwrap();
        let copied = root.join("reader.exe");
        std::fs::write(&copied, bytes).unwrap();
        let other = PinnedWindowsExecutable::open(&copied, &digest, end).unwrap();
        assert!(other.verify_process(process).is_err());
        drop(other);
        drop(current);
        std::fs::remove_dir_all(root).unwrap();
    }
}
