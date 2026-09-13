//! Shared Axvisor kernel support.

extern crate alloc;

/// Line-safe guest output, host-log backlog, and fixed console transport.
pub mod console_mux;

#[cfg(feature = "fs")]
mod shell_fs;

/// Shell helpers shared by the Axvisor binary.
#[doc(hidden)]
pub mod shell_support {
    use alloc::string::String;

    /// Formats a text fragment submitted by the Axvisor shell.
    pub fn format_fragment(args: core::fmt::Arguments<'_>) -> String {
        alloc::fmt::format(args)
    }

    /// Formats a complete text line submitted by the Axvisor shell.
    ///
    /// The shared host-output queue preserves raw bytes because it also carries
    /// guest output. Shell-owned lines must therefore provide their own CRLF.
    pub fn format_line(args: core::fmt::Arguments<'_>) -> String {
        let mut output = format_fragment(args);
        output.push_str("\r\n");
        output
    }

    #[cfg(feature = "fs")]
    pub use super::shell_fs::{
        CopyMode, RemoveOptions, collect_directory_entry_names, copy_after_rename_failure,
        copy_operands, copy_path, ensure_recursive_destination_outside_source, ignore_remove_error,
        metadata_for_remove, move_file_or_dir, path_basename, remove_path, touch_file,
        touch_file_at,
    };
}
