//! Inspection and manipulation of the process’s environment.

#[cfg(feature = "fs")]
extern crate alloc;

#[cfg(feature = "fs")]
use {crate::io, alloc_crate::string::String};

/// Returns the current working directory as a [`String`].
#[cfg(feature = "fs")]
pub fn current_dir() -> io::Result<String> {
    crate::os::arceos::fs::ax_current_dir()
}

/// Changes the current working directory to the specified path.
#[cfg(feature = "fs")]
pub fn set_current_dir(path: &str) -> io::Result<()> {
    crate::os::arceos::fs::ax_set_current_dir(path)
}
