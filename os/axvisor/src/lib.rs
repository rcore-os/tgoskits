//! Shared Axvisor kernel support.

extern crate alloc;
#[cfg(axtest)]
extern crate self as axvisor;

/// Line-safe guest output, host-log backlog, and fixed console transport.
pub mod console_mux;

// The production guest-console mux belongs to the Axvisor binary, whose Cargo
// target deliberately has `test = false`.  Compile that exact implementation
// into the freestanding axtest image with inert process-boundary adapters so
// its internal state-machine and bounded-endpoint regressions really run.
#[cfg(axtest)]
mod host {
    pub(super) fn submit_host_bytes(_bytes: &[u8]) {}
}

#[cfg(axtest)]
mod manager {
    use alloc::vec::Vec;

    use anyhow::Result;
    use axvm::{AxVMRef, VMId};

    pub(super) struct AxvmManager;

    impl AxvmManager {
        pub(super) fn notify_vm(_vm_id: VMId) -> Result<()> {
            Ok(())
        }

        pub(super) fn vm_by_id(_vm_id: VMId) -> Option<AxVMRef> {
            None
        }

        pub(super) fn vm_list() -> Vec<AxVMRef> {
            Vec::new()
        }
    }
}

#[cfg(axtest)]
#[path = "guest_console/mux/mod.rs"]
mod guest_console_mux_axtest;

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
