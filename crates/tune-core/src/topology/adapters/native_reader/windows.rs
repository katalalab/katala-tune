//! Readonly parent-relative Windows opens. No process or admission authority.
#![allow(unsafe_code)] // OS path/type/identity queries and FILE_OPEN-only reads.

use super::{NativeReaderError as Error, SourceIo, component_ok, joined, relative_ok, root_ok, time_left};
use std::{
    cell::RefCell,
    fs::File,
    io::Read,
    mem::size_of,
    os::windows::{
        fs::MetadataExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
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
            CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
            FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_TRAVERSE, FILE_TYPE_DISK, FileIdInfo, GetFileInformationByHandleEx, GetFileType,
            GetVolumeInformationByHandleW, OPEN_EXISTING,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};
const SYNCHRONIZE: u32 = 0x0010_0000;
const GENERIC_READ: u32 = 0x8000_0000;

#[derive(Default)]
pub struct WindowsSourceIo {
    checkouts: RefCell<Vec<CheckoutLease>>,
}
struct CheckoutLease {
    root: String,
    checkout: String,
    chain: Vec<Held>,
}
impl SourceIo for WindowsSourceIo {
    fn identity_hash(&self) -> Result<String, Error> {
        let hostname = gethostname::gethostname();
        Ok(super::remote::identity_hash(hostname.to_str().ok_or(Error::Source)?))
    }
    fn system_directories(&self) -> Result<(String, String), Error> {
        use windows_sys::Win32::System::SystemInformation::{GetSystemDirectoryW, GetSystemWindowsDirectoryW};
        let mut root = [0u16; 32768];
        let mut system = [0u16; 32768];
        // SAFETY: both APIs receive fixed writable buffers with exact capacities.
        let (a, b) =
            unsafe { (GetSystemWindowsDirectoryW(root.as_mut_ptr(), root.len() as u32), GetSystemDirectoryW(system.as_mut_ptr(), system.len() as u32)) };
        if a == 0 || b == 0 || a as usize >= root.len() || b as usize >= system.len() {
            return Err(Error::Source);
        }
        Ok((String::from_utf16(&root[..a as usize]).map_err(|_| Error::Source)?, String::from_utf16(&system[..b as usize]).map_err(|_| Error::Source)?))
    }
    fn verify_checkout(&self, root: &str, checkout: &str, deadline: Instant) -> Result<(), Error> {
        let mut chain = open_checked(root, checkout, false, deadline)?;
        if !chain.last().ok_or(Error::Source)?.directory {
            return Err(Error::Source);
        }
        // .git can be a regular pointer file or directory. Open relative to the
        // retained checkout, never through the original absolute path.
        chain.push(open_child(&chain.last().ok_or(Error::Source)?.file, ".git", false, deadline)?);
        unchanged(&chain, deadline)?;
        self.checkouts.borrow_mut().push(CheckoutLease { root: root.to_owned(), checkout: checkout.to_owned(), chain });
        Ok(())
    }
    fn read_approved(&self, root: &str, relative: &str, maximum: usize, deadline: Instant) -> Result<Vec<u8>, Error> {
        if maximum > 16 * 1024 * 1024 {
            return Err(Error::Limit);
        }
        if !root_ok(root) || !relative_ok(relative, false) {
            return Err(Error::Policy);
        }
        let checkouts = self.checkouts.borrow();
        let (lease, tail) = checkouts
            .iter()
            .filter(|lease| lease.root == root)
            .filter_map(|lease| {
                if lease.checkout == "." {
                    Some((lease, relative))
                } else {
                    relative.strip_prefix(&lease.checkout).and_then(|tail| tail.strip_prefix('/')).map(|tail| (lease, tail))
                }
            })
            .max_by_key(|(lease, _)| lease.checkout.len())
            .ok_or(Error::Source)?;
        unchanged(&lease.chain, deadline)?;
        let checkout = lease.chain.get(lease.chain.len().checked_sub(2).ok_or(Error::Source)?).ok_or(Error::Source)?;
        let names = tail.split('/').collect::<Vec<_>>();
        let mut chain: Vec<Held> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let parent = chain.last().unwrap_or(checkout);
            if !parent.directory {
                return Err(Error::Source);
            }
            let child = open_child(&parent.file, name, index + 1 == names.len(), deadline)?;
            chain.push(child);
        }
        let file = &mut chain.last_mut().ok_or(Error::Source)?.file;
        let size = file.metadata().map_err(|_| Error::Source)?.len();
        if size > maximum as u64 {
            return Err(Error::Limit);
        }
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            time_left(deadline)?;
            let count = file.read(&mut chunk).map_err(|_| Error::Source)?;
            time_left(deadline)?;
            if count == 0 {
                break;
            }
            if bytes.len().checked_add(count).is_none_or(|n| n > maximum) {
                return Err(Error::Limit);
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        if bytes.len() as u64 != size || file.metadata().map_err(|_| Error::Source)?.len() != size {
            return Err(Error::Source);
        }
        unchanged(&chain, deadline)?;
        Ok(bytes)
    }
}

#[derive(Eq, PartialEq)]
struct Identity {
    volume: u64,
    file: [u8; 16],
}
fn identity(file: &File) -> Result<Identity, Error> {
    let mut id = FILE_ID_INFO::default();
    // SAFETY: held live handle and correctly sized writable output.
    if unsafe { GetFileInformationByHandleEx(file.as_raw_handle(), FileIdInfo, (&mut id as *mut FILE_ID_INFO).cast(), size_of::<FILE_ID_INFO>() as u32) } == 0 {
        return Err(Error::Source);
    }
    Ok(Identity { volume: id.VolumeSerialNumber, file: id.FileId.Identifier })
}
struct Held {
    file: File,
    id: Identity,
    directory: bool,
}
fn held(handle: HANDLE) -> Result<Held, Error> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(Error::Source);
    }
    // SAFETY: this successfully opened, non-inheritable handle is transferred once.
    let file = File::from(unsafe { OwnedHandle::from_raw_handle(handle) });
    let metadata = file.metadata().map_err(|_| Error::Source)?;
    // SAFETY: file owns the live handle throughout this readonly query.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (!metadata.is_dir() && !metadata.is_file())
    {
        return Err(Error::Source);
    }
    Ok(Held { id: identity(&file)?, directory: metadata.is_dir(), file })
}
fn unchanged(chain: &[Held], deadline: Instant) -> Result<(), Error> {
    for item in chain {
        time_left(deadline)?;
        let metadata = item.file.metadata().map_err(|_| Error::Source)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || metadata.is_dir() != item.directory || identity(&item.file)? != item.id {
            return Err(Error::Source);
        }
    }
    time_left(deadline)
}
fn open_child(parent: &File, name: &str, read: bool, deadline: Instant) -> Result<Held, Error> {
    if !component_ok(name) {
        return Err(Error::Policy);
    }
    time_left(deadline)?;
    let mut units = name.encode_utf16().collect::<Vec<_>>();
    if units.is_empty() || units.len() > 32760 || units.contains(&0) {
        return Err(Error::Policy);
    }
    let bytes = u16::try_from(units.len() * 2).map_err(|_| Error::Limit)?;
    let mut unicode = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: units.as_mut_ptr() };
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
    // SAFETY: one validated component, retained parent and live outputs.
    // FILE_OPEN cannot create/overwrite. No OBJ_INHERIT, write/delete access,
    // or write/delete sharing is requested. Absolute descendants are not reopened.
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            if read { GENERIC_READ | SYNCHRONIZE } else { FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE },
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
        return Err(Error::Source);
    }
    let file = held(handle)?;
    time_left(deadline)?;
    if read && file.directory {
        return Err(Error::Source);
    }
    Ok(file)
}

/// Anchor at a local drive root once, then open single components through held
/// parent handles. Local NTFS only. No enumeration or source hash reopen.
fn open_checked(root: &str, relative: &str, read_leaf: bool, deadline: Instant) -> Result<Vec<Held>, Error> {
    if !root_ok(root) || !relative_ok(relative, true) {
        return Err(Error::Policy);
    }
    let full = joined(root, relative);
    if full.len() > 4096 {
        return Err(Error::Limit);
    }
    time_left(deadline)?;
    let root_name = format!("{}:\\", &full[..1]).encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    // SAFETY: NUL-terminated local drive root; no inherited/security/template handle.
    let anchor = held(unsafe {
        CreateFileW(
            root_name.as_ptr(),
            FILE_READ_ATTRIBUTES | FILE_TRAVERSE,
            FILE_SHARE_READ,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL,
            null_mut(),
        )
    })?;
    if !anchor.directory {
        return Err(Error::Source);
    }
    let mut filesystem = [0u16; 32];
    // SAFETY: held directory handle, optional outputs null and bounded output.
    if unsafe {
        GetVolumeInformationByHandleW(
            anchor.file.as_raw_handle(),
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
        return Err(Error::Unsupported);
    }
    let mut chain = vec![anchor];
    let names = full[3..].split('/').filter(|name| !name.is_empty()).collect::<Vec<_>>();
    for (index, name) in names.iter().enumerate() {
        let leaf = index + 1 == names.len();
        let parent = chain.last().ok_or(Error::Source)?;
        if !parent.directory {
            return Err(Error::Source);
        }
        let child = open_child(&parent.file, name, leaf && read_leaf, deadline)?;
        if !leaf && !child.directory {
            return Err(Error::Source);
        }
        chain.push(child);
    }
    if read_leaf && chain.last().is_none_or(|leaf| leaf.directory) {
        return Err(Error::Source);
    }
    unchanged(&chain, deadline)?;
    Ok(chain)
}
