//! Inspection and manipulation of the process’s environment.

#[cfg(any(feature = "fs", feature = "fs-api"))]
extern crate alloc;

#[cfg(any(feature = "fs", feature = "fs-api"))]
use {crate::io, alloc::string::String};

/// Returns the current working directory as a [`String`].
#[cfg(any(feature = "fs", feature = "fs-api"))]
pub fn current_dir() -> io::Result<String> {
    ax_api::fs::ax_current_dir()
}

/// Changes the current working directory to the specified path.
#[cfg(any(feature = "fs", feature = "fs-api"))]
pub fn set_current_dir(path: &str) -> io::Result<()> {
    ax_api::fs::ax_set_current_dir(path)
}
