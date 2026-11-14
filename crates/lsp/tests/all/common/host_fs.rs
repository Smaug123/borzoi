//! The test harness's file-system reads, which set up and inspect fixtures
//! rather than observe a client's workspace. The crate's `clippy.toml` bans
//! the `std` calls (the library observes the file system only through
//! `borzoi::observed`), and allowing that lint wholesale here would also allow
//! the child-process spawns it bans, so tests reach the `std` calls through
//! this module instead. `borzoi::observed::host_fs` is the inline test
//! modules' twin.

#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};

pub fn read(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    std::fs::read(path)
}

pub fn read_to_string(path: impl AsRef<Path>) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

pub fn read_dir(path: impl AsRef<Path>) -> std::io::Result<std::fs::ReadDir> {
    std::fs::read_dir(path)
}

pub fn metadata(path: impl AsRef<Path>) -> std::io::Result<std::fs::Metadata> {
    std::fs::metadata(path)
}

pub fn canonicalize(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

/// Set the file's modification time, as an extraction that keeps an
/// archive's times does.
pub fn set_modified(path: impl AsRef<Path>, time: std::time::SystemTime) -> std::io::Result<()> {
    std::fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(time)
}

/// The `Path` probes, under names that do not collide with the inherent
/// methods (which would win method resolution).
pub trait HostPath {
    fn host_exists(&self) -> bool;
    fn host_is_file(&self) -> bool;
    fn host_is_dir(&self) -> bool;
    fn host_is_symlink(&self) -> bool;
    fn host_canonicalize(&self) -> std::io::Result<PathBuf>;
}

impl HostPath for Path {
    fn host_exists(&self) -> bool {
        self.exists()
    }
    fn host_is_file(&self) -> bool {
        self.is_file()
    }
    fn host_is_dir(&self) -> bool {
        self.is_dir()
    }
    fn host_is_symlink(&self) -> bool {
        self.is_symlink()
    }
    fn host_canonicalize(&self) -> std::io::Result<PathBuf> {
        self.canonicalize()
    }
}
