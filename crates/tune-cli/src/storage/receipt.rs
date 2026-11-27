//! Reserve a name before running the job, then publish the complete JSON without replacing a file.
use same_file::Handle;
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

fn save(file: &mut File, report: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *file, report)?;
    file.write_all(b"\n")?;
    file.sync_all()
}

fn require_unused(report: &Path) -> io::Result<()> {
    match fs::symlink_metadata(report) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(io::Error::new(io::ErrorKind::AlreadyExists, "report already exists")),
        Err(e) => Err(e),
    }
}

pub(super) struct ReportClaim {
    directory: PathBuf,
    directory_handle: Handle,
    marker: PathBuf,
    handle: Handle,
}

impl ReportClaim {
    pub(super) fn acquire(report: &Path, initial: &Value) -> io::Result<Self> {
        require_unused(report)?;
        let mut directory = report.as_os_str().to_owned();
        directory.push(".incomplete");
        let directory = PathBuf::from(directory);
        fs::create_dir(&directory)?;
        let reserve = || -> io::Result<Self> {
            // Another run may have finished between the first check and our exclusive mkdir.
            require_unused(report)?;
            let mut marker = tempfile::Builder::new().prefix("started-").suffix(".json").tempfile_in(&directory)?;
            save(marker.as_file_mut(), initial)?;
            let directory_handle = Handle::from_path(&directory)?;
            let handle = Handle::from_file(marker.as_file().try_clone()?)?;
            let (_, marker) = marker.keep()?;
            Ok(Self { directory: directory.clone(), directory_handle, marker, handle })
        };
        let result = reserve();
        if result.is_err() {
            let _ = fs::remove_dir(&directory);
        }
        result
    }

    fn check_marker(&self) -> io::Result<()> {
        if super::volume::link(&self.directory.symlink_metadata()?)
            || Handle::from_path(&self.directory)? != self.directory_handle
            || super::volume::link(&self.marker.symlink_metadata()?)
            || Handle::from_path(&self.marker)? != self.handle
        {
            return Err(io::Error::other("report reservation changed"));
        }
        Ok(())
    }

    pub(super) fn publish(self, destination: &Path, report: &Value) -> io::Result<()> {
        self.check_marker()?;
        let mut result = tempfile::Builder::new().prefix("result-").suffix(".json").tempfile_in(&self.directory)?;
        save(result.as_file_mut(), report)?;
        if let Err(error) = result.persist_noclobber(destination) {
            // The caller can recover the finished measurement alongside the initial receipt.
            error.file.keep()?;
            return Err(error.error);
        }
        self.check_marker()?;
        drop(self.handle);
        drop(self.directory_handle);
        fs::remove_file(self.marker)?;
        // Never recursively delete a claim. A foreign file or non-empty directory survives cleanup.
        fs::remove_dir(self.directory)
    }
}
