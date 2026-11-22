//! No filesystem fallback on platforms outside the two native build tuples.
use super::filesystem::{Kind, RenameFailure, State};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::Path,
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Identity;
fn unsupported<T>() -> io::Result<T> {
    Err(io::ErrorKind::Unsupported.into())
}
pub(super) fn root(_: &Path) -> io::Result<(File, Vec<OsString>)> {
    unsupported()
}
pub(super) fn open(_: &File, _: &OsStr, _: Kind) -> io::Result<File> {
    unsupported()
}
pub(super) fn state(_: &File) -> io::Result<State> {
    unsupported()
}
pub(super) fn named(_: &File, _: &OsStr) -> io::Result<State> {
    unsupported()
}
pub(super) fn close(_: File) -> io::Result<()> {
    unsupported()
}
pub(super) fn make_executable(_: &File) -> io::Result<()> {
    unsupported()
}
pub(super) fn executable(_: &State) -> bool {
    false
}
pub(super) fn mkdir(_: &File, _: &OsStr) -> io::Result<()> {
    unsupported()
}
pub(super) fn rename(_: &File, _: &OsStr, _: &OsStr, _: &Identity) -> Result<(), RenameFailure> {
    Err(io::Error::from(io::ErrorKind::Unsupported).into())
}
pub(super) fn remove(_: &File, _: &OsStr, _: &Identity) -> io::Result<()> {
    unsupported()
}
#[cfg(test)]
mod tests {
    #[test]
    fn unsupported_platform_never_opens_source_or_falls_back_to_home() {
        use super::super::{Cause, inspect_bound_target, prepare_native_sampler};
        assert_eq!(prepare_native_sampler(std::path::Path::new("/unused-native-source")).unwrap_err().cause(), Cause::UnsupportedPlatform);
        assert_eq!(inspect_bound_target().unwrap_err().cause(), Cause::UnsupportedPlatform);
    }
}
