//! Native NTFS operations relative to retained no-delete-sharing parent handles.
#![allow(unsafe_code)]
use super::filesystem::{Kind, RenameFailure, State};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        fs::MetadataExt,
        io::{AsRawHandle, FromRawHandle, IntoRawHandle},
    },
    path::{Component, Path, Prefix},
    ptr::{null, null_mut},
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_NO_RECALL, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
            NtCreateFile,
        },
    },
    Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, RtlNtStatusToDosError, UNICODE_STRING},
        Storage::FileSystem::*,
        System::IO::IO_STATUS_BLOCK,
    },
};
const SYNCHRONIZE: u32 = 0x0010_0000;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const DELETE_ACCESS: u32 = 0x0001_0000;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    volume: u64,
    file: [u8; 16],
}
fn file(handle: HANDLE) -> io::Result<File> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE { Err(io::Error::last_os_error()) }
    // SAFETY: fresh non-inheritable handle is transferred once.
    else { Ok(unsafe { File::from_raw_handle(handle) }) }
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn component(name: &OsStr) -> io::Result<Vec<u16>> {
    let units: Vec<_> = name.encode_wide().collect();
    if units.is_empty()
        || units.len() > 255
        || units.iter().any(|c| matches!(*c, 0..=31 | 47 | 58 | 92))
        || units.last().is_some_and(|c| matches!(*c, 32 | 46))
        || name == "."
        || name == ".."
    {
        return Err(invalid());
    }
    Ok(units)
}
pub(super) fn root(path: &Path) -> io::Result<(File, Vec<OsString>)> {
    let mut parts = path.components();
    let drive = match parts.next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d,
            _ => return Err(invalid()),
        },
        _ => return Err(invalid()),
    };
    if !matches!(parts.next(), Some(Component::RootDir)) {
        return Err(invalid());
    }
    let mut names = Vec::new();
    for part in parts {
        match part {
            Component::Normal(n) => {
                component(n)?;
                names.push(n.to_owned());
            }
            _ => return Err(invalid()),
        }
    }
    let root = [drive as u16, b':' as u16, b'\\' as u16, 0];
    // SAFETY: bounded drive root, no inheritance; parent handles deny deletion,
    // so the directory chain cannot be renamed while the transaction retains it.
    let file = file(unsafe {
        CreateFileW(
            root.as_ptr(),
            FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL,
            null_mut(),
        )
    })?;
    let mut filesystem = [0u16; 32];
    // SAFETY: live directory handle and bounded writable filesystem-name buffer.
    if unsafe {
        GetVolumeInformationByHandleW(file.as_raw_handle(), null_mut(), 0, null_mut(), null_mut(), null_mut(), filesystem.as_mut_ptr(), filesystem.len() as u32)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if filesystem[..5] != [78, 84, 70, 83, 0] || !state(&file)?.directory {
        return Err(invalid());
    }
    Ok((file, names))
}
fn child(parent: &File, name: &OsStr, access: u32, share: u32, disposition: u32, directory: bool) -> io::Result<File> {
    let mut units = component(name)?;
    let bytes = u16::try_from(units.len() * 2).map_err(|_| invalid())?;
    let mut unicode = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: units.as_mut_ptr() };
    let attrs = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &mut unicode,
        Attributes: OBJ_DONT_REPARSE | OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: null_mut(),
        SecurityQualityOfService: null_mut(),
    };
    let mut status = IO_STATUS_BLOCK::default();
    let mut handle = null_mut();
    // FILE_DIRECTORY_FILE + FILE_OPEN_NO_RECALL is rejected by NtCreateFile.
    // Existing directories use a type-neutral no-reparse/no-recall open and an
    // immediate held-handle directory check below. Exclusive new directories
    // require FILE_DIRECTORY_FILE; FILE_CREATE and OBJ_DONT_REPARSE refuse an
    // occupied/reparse leaf instead of opening it, so no existing file is recalled.
    let options = if directory && disposition == FILE_CREATE {
        FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT
    } else {
        FILE_OPEN_REPARSE_POINT | FILE_OPEN_NO_RECALL | FILE_SYNCHRONOUS_IO_NONALERT | if directory { 0 } else { FILE_NON_DIRECTORY_FILE }
    };
    // SAFETY: one checked component, retained parent, correctly sized NT output.
    // FILE_CREATE is exclusive; no truncate/overwrite disposition is available.
    let result =
        unsafe { NtCreateFile(&mut handle, access | SYNCHRONIZE, &attrs, &mut status, null(), FILE_ATTRIBUTE_NORMAL, share, disposition, options, null(), 0) };
    if result < 0 {
        // SAFETY: pure conversion of the returned NTSTATUS to a Win32 error.
        return Err(io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(result) } as i32));
    }
    let file = file(handle)?;
    let observed = state(&file)?;
    if directory && !observed.directory {
        return Err(invalid());
    }
    Ok(file)
}
pub(super) fn open(parent: &File, name: &OsStr, kind: Kind) -> io::Result<File> {
    match kind {
        Kind::Directory => child(parent, name, FILE_READ_ATTRIBUTES | FILE_TRAVERSE, FILE_SHARE_READ | FILE_SHARE_WRITE, FILE_OPEN, true),
        Kind::Read => child(parent, name, GENERIC_READ, FILE_SHARE_READ, FILE_OPEN, false),
        Kind::Create => child(parent, name, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ, FILE_CREATE, false),
    }
}
pub(super) fn state(file: &File) -> io::Result<State> {
    let metadata = file.metadata()?;
    // SAFETY: handle remains owned for all fixed-size read-only metadata queries.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(invalid());
    }
    let mut id = FILE_ID_INFO::default();
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let mut basic = FILE_BASIC_INFO::default();
    // SAFETY: all outputs have the binding-defined layout and exact capacity.
    if unsafe { GetFileInformationByHandleEx(file.as_raw_handle(), FileIdInfo, (&mut id as *mut FILE_ID_INFO).cast(), size_of::<FILE_ID_INFO>() as u32) } == 0
        || unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0
        || unsafe {
            GetFileInformationByHandleEx(file.as_raw_handle(), FileBasicInfo, (&mut basic as *mut FILE_BASIC_INFO).cast(), size_of::<FILE_BASIC_INFO>() as u32)
        } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(State {
        identity: Identity { volume: id.VolumeSerialNumber, file: id.FileId.Identifier },
        size: metadata.len(),
        links: info.nNumberOfLinks as u64,
        regular: metadata.is_file(),
        directory: metadata.is_dir(),
        mode: metadata.file_attributes(),
        times: [basic.LastWriteTime as i128, basic.ChangeTime as i128],
    })
}
pub(super) fn named(parent: &File, name: &OsStr) -> io::Result<State> {
    // An attribute-only open permits either file or directory, still refuses every
    // reparse point. It shares the caller's access but grants no write authority.
    let mut units = component(name)?;
    let bytes = (units.len() * 2) as u16;
    let mut unicode = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: units.as_mut_ptr() };
    let attrs = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &mut unicode,
        Attributes: OBJ_DONT_REPARSE | OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: null_mut(),
        SecurityQualityOfService: null_mut(),
    };
    let mut status = IO_STATUS_BLOCK::default();
    let mut handle = null_mut();
    // SAFETY: same bounded parent-relative FILE_OPEN contract as child().
    let result = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            &attrs,
            &mut status,
            null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_OPEN_NO_RECALL | FILE_SYNCHRONOUS_IO_NONALERT,
            null(),
            0,
        )
    };
    if result < 0 {
        // SAFETY: conversion of an NTSTATUS returned by this call.
        return Err(io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(result) } as i32));
    }
    let file = file(handle)?;
    let result = state(&file);
    let closed = close(file);
    match (result, closed) {
        (_, Err(e)) | (Err(e), _) => Err(e),
        (Ok(v), Ok(())) => Ok(v),
    }
}
pub(super) fn close(file: File) -> io::Result<()> {
    let handle = file.into_raw_handle();
    // SAFETY: transferred ownership, one close only and no later File drop.
    if unsafe { CloseHandle(handle) } != 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}
pub(super) fn make_executable(_file: &File) -> io::Result<()> {
    Ok(())
}
pub(super) fn executable(s: &State) -> bool {
    s.regular && s.mode & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_READONLY) == 0
}
pub(super) fn mkdir(parent: &File, name: &OsStr) -> io::Result<()> {
    close(child(parent, name, FILE_READ_ATTRIBUTES | FILE_TRAVERSE, FILE_SHARE_READ | FILE_SHARE_WRITE, FILE_CREATE, true)?)
}
pub(super) fn rename(parent: &File, from: &OsStr, to: &OsStr, id: &Identity) -> Result<(), RenameFailure> {
    let file = child(parent, from, FILE_READ_ATTRIBUTES | DELETE_ACCESS, FILE_SHARE_READ | FILE_SHARE_DELETE, FILE_OPEN, false)?;
    if state(&file)?.identity != *id {
        return Err(invalid().into());
    }
    let units = component(to)?;
    let bytes = offset_of!(FILE_RENAME_INFO, FileName) + units.len() * 2;
    // usize backing gives the native struct pointer alignment, unlike Vec<u8>.
    let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: aligned storage is large enough for all fixed fields and UTF-16 tail;
    // RootDirectory is retained, ReplaceIfExists=false enforces no-replace rename.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = parent.as_raw_handle();
        (*info).FileNameLength = (units.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(units.as_ptr(), std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(), units.len());
    }
    // SAFETY: buffer stays live and immutable during the synchronous native call.
    let ok = unsafe { SetFileInformationByHandle(file.as_raw_handle(), FileRenameInfo, info.cast(), bytes as u32) };
    let result = if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) };
    let closed = super::filesystem::checked_close(file).map_err(|_| io::Error::other("checked rename close failed"));
    match (result, closed) {
        (Err(error), _) => Err(error.into()),
        (Ok(()), Err(error)) => Err(RenameFailure { error, moved: true }),
        (Ok(()), Ok(())) => Ok(()),
    }
}
pub(super) fn remove(parent: &File, name: &OsStr, id: &Identity) -> io::Result<()> {
    let file = child(parent, name, FILE_READ_ATTRIBUTES | DELETE_ACCESS, FILE_SHARE_READ | FILE_SHARE_DELETE, FILE_OPEN, false)?;
    let s = state(&file)?;
    if s.identity != *id || !s.regular || s.links != 1 {
        return Err(invalid());
    }
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: delete only this identity-checked opened file, never a pathname peer.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    let result = if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) };
    let closed = close(file);
    result.and(closed)
}
