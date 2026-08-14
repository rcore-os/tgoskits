//! Inspection and manipulation of the process’s environment.

#[cfg(feature = "fs")]
extern crate alloc;

#[cfg(feature = "fs")]
use {crate::StdResult, alloc::string::String};

/// Returns the current working directory as a [`String`].
#[cfg(feature = "fs")]
pub fn current_dir() -> StdResult<String> {
    Ok(ax_api::fs::ax_current_dir()?)
}

/// Changes the current working directory to the specified path.
#[cfg(feature = "fs")]
pub fn set_current_dir(path: &str) -> StdResult {
    ax_api::fs::ax_set_current_dir(path)?;
    Ok(())
}
