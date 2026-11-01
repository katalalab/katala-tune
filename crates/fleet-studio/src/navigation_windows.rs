//! Windows-only OS handoff. Call on a dedicated thread, never the GPUI thread.
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf, Prefix};

use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_REPARSE_POINT, GetDriveTypeW};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize};
use windows::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    ILCreateFromPathW, ILFree, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, SHOpenFolderAndSelectItems, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

struct ComApartment;

impl ComApartment {
    fn new() -> Result<Self, ()> {
        // SAFETY: This dedicated worker has no existing COM apartment. A successful
        // initialization, including S_FALSE, is balanced by Drop on this thread.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.ok().map_err(|_| ())?;
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: `new` succeeded on this same thread and all COM objects using
        // this apartment have been dropped before the guard.
        unsafe { CoUninitialize() };
    }
}

pub(super) fn github(value: &str) -> Result<(), ()> {
    // Defense in depth: the public constructor is the only way to create a target.
    let url = url::Url::parse(value).map_err(|_| ())?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || value.chars().any(char::is_control)
    {
        return Err(());
    }
    let _com = ComApartment::new()?;
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpVerb: w!("open"),
        lpFile: PCWSTR(wide.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` and its NUL-terminated URL remain alive for the call.
    // No shell command, parameters, handles, or caller-controlled verb are used.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|_| ())
}

pub(super) fn directory(path: &Path) -> Result<(), ()> {
    // Check the drive before touching its contents, then reject all reparse
    // points from the root down rather than following junctions or symlinks.
    let mut components = path.components();
    let drive = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) => drive,
            _ => return Err(()),
        },
        _ => return Err(()),
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(());
    }
    let root_string = format!("{}:\\", char::from(drive));
    let root: Vec<u16> = root_string.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `root` is a valid, NUL-terminated drive root for this call.
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
    if drive_type != DRIVE_FIXED && drive_type != DRIVE_REMOVABLE {
        return Err(());
    }
    let mut current = PathBuf::from(root_string);
    let root_metadata = std::fs::symlink_metadata(&current).map_err(|_| ())?;
    if root_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 || !root_metadata.is_dir() {
        return Err(());
    }
    for component in components {
        match component {
            Component::Normal(name) => current.push(name),
            _ => return Err(()),
        }
        let metadata = std::fs::symlink_metadata(&current).map_err(|_| ())?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 || !metadata.is_dir() {
            return Err(());
        }
    }
    let path = current.to_str().ok_or(())?;
    let _com = ComApartment::new()?;
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated path that outlives this call.
    let item = unsafe { ILCreateFromPathW(PCWSTR(wide.as_ptr())) };
    if item.is_null() {
        return Err(());
    }
    let item = ItemId(item);
    // SAFETY: `item` is a valid absolute PIDL until the end of this call. With
    // no child array, the API opens the parent and selects this item; it never
    // invokes the item's default action.
    unsafe { SHOpenFolderAndSelectItems(item.0, None, 0) }.map_err(|_| ())
}

struct ItemId(*mut ITEMIDLIST);

impl Drop for ItemId {
    fn drop(&mut self) {
        // SAFETY: This PIDL was returned by ILCreateFromPathW and is freed once.
        unsafe { ILFree(Some(self.0)) };
    }
}
