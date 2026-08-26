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

    /// Formats text submitted by the Axvisor shell.
    ///
    /// The shared host-output queue preserves raw bytes because it also carries
    /// guest output. Shell-owned lines must therefore provide their own CRLF.
    pub fn format_output(args: core::fmt::Arguments<'_>, newline: bool) -> String {
        let mut output = alloc::fmt::format(args);
        if newline {
            output.push_str("\r\n");
        }
        output
    }

    #[cfg(feature = "fs")]
    pub use super::shell_fs::{
        CopyMode, RemoveOptions, collect_directory_entry_names, copy_after_rename_failure,
        copy_operands, copy_path, ensure_recursive_destination_outside_source, ignore_remove_error,
        metadata_for_remove, move_file_or_dir, path_basename, remove_path, touch_file,
        touch_file_at,
    };

    #[cfg(test)]
    mod tests {
        use super::format_output;

        #[test]
        fn shell_newline_is_a_terminal_crlf_sequence() {
            assert_eq!(
                format_output(format_args!("shell line"), true),
                "shell line\r\n"
            );
        }

        #[test]
        fn shell_fragment_has_no_implicit_line_ending() {
            assert_eq!(format_output(format_args!("prompt: "), false), "prompt: ");
        }
    }
}
