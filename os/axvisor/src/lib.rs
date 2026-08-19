//! Shared Axvisor kernel support.

#[cfg(feature = "fs")]
mod shell_fs;

/// Shell filesystem operations shared by the Axvisor binary.
#[cfg(feature = "fs")]
#[doc(hidden)]
pub mod shell_support {
    pub use super::shell_fs::{
        CopyMode, RemoveOptions, collect_directory_entry_names, copy_after_rename_failure,
        copy_operands, copy_path, ensure_recursive_destination_outside_source, ignore_remove_error,
        metadata_for_remove, move_file_or_dir, path_basename, remove_path, touch_file,
        touch_file_at,
    };
}
