//! Shared Axvisor kernel support.

extern crate alloc;

pub mod builtin;

/// Line-safe guest output, host-log backlog, and fixed console transport.
pub mod console_mux;

/// Filesystem operations backing the Axvisor shell commands.
pub mod shell_fs;
