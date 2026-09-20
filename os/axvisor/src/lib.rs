//! Shared Axvisor kernel support.

extern crate alloc;

/// Line-safe guest output, host-log backlog, and fixed console transport.
pub mod console_mux;

/// Filesystem operations backing the Axvisor shell commands.
#[cfg(feature = "fs")]
pub mod shell_fs;
