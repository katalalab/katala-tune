#![cfg(windows)]
//! Actual NTFS lease tests. Successful opens are not execution/admission witnesses.
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::fs::{OpenOptionsExt, symlink_dir, symlink_file};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tune_core::collect::PinnedWindowsExecutable;
use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE};

// Ancestor leases can interfere with another test's fixture setup. This binary
// performs no process spawning and serializes every fixture and lease lifetime.
static SERIAL: Mutex<()> = Mutex::new(());
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(15)
}

fn hash_file(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let count = file.read(&mut bytes).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

struct Fixture {
    root: PathBuf,
    install: PathBuf,
    program: PathBuf,
    digest: String,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("katala-pin-{label}-{}-{stamp}-{sequence}", std::process::id()));
        // Exclusive creation: cleanup below owns only this newly created tree.
        fs::create_dir(&root).unwrap();
        let install = root.join("install");
        fs::create_dir(&install).unwrap();
        let source = std::env::current_exe().unwrap();
        // The expected bytes come from the running test artifact before copying,
        // independently of the candidate file that the pin implementation reads.
        let digest = hash_file(&source);
        let program = install.join("reader.exe");
        fs::copy(source, &program).unwrap();
        Self { root, install, program, digest }
    }

    fn pin(&self) -> PinnedWindowsExecutable {
        match PinnedWindowsExecutable::open(&self.program, &self.digest, deadline()) {
            Ok(pin) => pin,
            Err(error) => panic!("ordinary trusted-copy pin must succeed: kind={:?} os={:?}", error.kind(), error.raw_os_error()),
        }
    }

    fn baseline(&self) {
        drop(self.pin());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assert_rejected(path: &Path, digest: &str) {
    assert!(PinnedWindowsExecutable::open(path, digest, deadline()).is_err(), "an invalid or mutable image must not receive a lease");
}

#[test]
fn trusted_copy_and_unicode_name_keep_the_expected_hash_and_guid_path() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut fixture = Fixture::new("unicode");
    let unicode = fixture.install.join("試験😀.exe");
    fs::rename(&fixture.program, &unicode).unwrap();
    fixture.program = unicode;
    let pin = fixture.pin();
    assert_eq!(pin.sha256(), fixture.digest);
    assert!(pin.executable().as_os_str().to_string_lossy().starts_with(r"\\?\Volume{"));
    assert_eq!(pin.executable().file_name().unwrap(), "試験😀.exe");
    assert_eq!(hash_file(&fixture.program), fixture.digest);
    drop(pin);
}

#[test]
fn an_existing_writer_prevents_the_lease_until_its_handle_closes() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = Fixture::new("writer");
    fixture.baseline();
    let writer = OpenOptions::new().write(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).open(&fixture.program).unwrap();
    assert_rejected(&fixture.program, &fixture.digest);
    drop(writer);
    fixture.baseline();
}

struct WritableView(windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS);

impl WritableView {
    #[allow(unsafe_code)]
    fn new(path: &Path) -> Self {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use std::ptr::null;
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, PAGE_READWRITE};

        let file = OpenOptions::new().read(true).write(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).open(path).unwrap();
        // SAFETY: owned fixture file, unnamed mapping, no inherited handle, and
        // a read/write mapping of the existing file size. No bytes are changed.
        let raw = unsafe { CreateFileMappingW(file.as_raw_handle(), null(), PAGE_READWRITE, 0, 0, null()) };
        assert!(!raw.is_null(), "writable mapping setup must succeed: os={:?}", io::Error::last_os_error().raw_os_error());
        // SAFETY: a successful mapping call transfers one owned handle.
        let mapping = unsafe { OwnedHandle::from_raw_handle(raw) };
        // SAFETY: live mapping object and a bounded whole-file view; no pointer dereference.
        let view = unsafe { MapViewOfFile(mapping.as_raw_handle(), FILE_MAP_WRITE, 0, 0, 0) };
        assert!(!view.Value.is_null(), "writable view setup must succeed: os={:?}", io::Error::last_os_error().raw_os_error());
        // Only the writable view remains. Rejecting it must not depend on the
        // original writer file handle still being open.
        drop(file);
        drop(mapping);
        Self(view)
    }
}

impl Drop for WritableView {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: this object owns exactly this live view and never copies it.
        let _ = unsafe { windows_sys::Win32::System::Memory::UnmapViewOfFile(self.0) };
    }
}

#[test]
fn an_existing_writable_view_is_rejected_after_the_writer_handle_closed() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = Fixture::new("mapping");
    fixture.baseline();
    let mapping = WritableView::new(&fixture.program);
    assert_rejected(&fixture.program, &fixture.digest);
    drop(mapping);
    fixture.baseline();
}

#[test]
fn held_leaf_and_ancestors_reject_rename_and_hardlink_writes() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = Fixture::new("mutation");
    let alias = fixture.root.join("same-file.exe");
    fs::hard_link(&fixture.program, &alias).unwrap();
    let moved_leaf = fixture.install.join("moved.exe");
    let moved_install = fixture.root.join("moved-install");
    let pin = fixture.pin();
    assert!(fs::rename(&fixture.program, &moved_leaf).is_err(), "the leased leaf must not be renamed");
    assert!(fs::rename(&fixture.install, &moved_install).is_err(), "a leased ancestor must not be renamed");
    assert!(OpenOptions::new().write(true).open(&alias).is_err(), "a hardlink must not bypass deny-write sharing");
    assert_eq!(hash_file(&fixture.program), fixture.digest);
    drop(pin);
    // Verify that failures above were caused by the retained lease, rather than
    // permanent fixture permissions or unsupported operations.
    drop(OpenOptions::new().write(true).open(&alias).unwrap());
    fs::rename(&fixture.program, &moved_leaf).unwrap();
    fs::rename(&moved_leaf, &fixture.program).unwrap();
    fs::rename(&fixture.install, &moved_install).unwrap();
}

#[test]
fn a_leaf_reparse_point_is_rejected_even_with_the_target_hash() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = Fixture::new("leaf-link");
    fixture.baseline();
    let link = fixture.install.join("linked-reader.exe");
    symlink_file(&fixture.program, &link)
        .unwrap_or_else(|error| panic!("required file-symlink privilege/setup unavailable: kind={:?} os={:?}", error.kind(), error.raw_os_error()));
    assert_rejected(&link, &fixture.digest);
    fs::remove_file(link).unwrap();
}

#[test]
fn an_ancestor_reparse_point_is_rejected_even_with_the_target_hash() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = Fixture::new("ancestor-link");
    fixture.baseline();
    let link = fixture.root.join("linked-install");
    symlink_dir(&fixture.install, &link)
        .unwrap_or_else(|error| panic!("required directory-symlink privilege/setup unavailable: kind={:?} os={:?}", error.kind(), error.raw_os_error()));
    assert_rejected(&link.join("reader.exe"), &fixture.digest);
    fs::remove_dir(link).unwrap();
}

#[test]
fn wrong_hash_and_an_expired_absolute_deadline_never_return_a_pin() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fixture = Fixture::new("admission");
    fixture.baseline();
    let wrong = "0".repeat(64);
    assert_ne!(fixture.digest, wrong);
    assert_rejected(&fixture.program, &wrong);
    let expired = Instant::now();
    let error = PinnedWindowsExecutable::open(&fixture.root.join("missing.exe"), &fixture.digest, expired).err().expect("expired admission must fail");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "the deadline must be checked before looking up the missing image");
    fixture.baseline();
}
