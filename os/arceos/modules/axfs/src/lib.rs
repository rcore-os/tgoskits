//! ArceOS filesystem module.
//!
//! Provides high-level filesystem operations built on top of the VFS layer,
//! including file I/O with page caching, directory traversal, and
//! `std::fs`-like APIs.
//!
//! Public API tiers:
//!
//! - Primary filesystem API: [`File`], [`OpenOptions`], [`FsContext`], and
//!   [`FS_CONTEXT`] are the shared entry points used by ArceOS, StarryOS, and
//!   Axvisor-facing library code.
//! - Filesystem construction: [`new_filesystem_from_dyn`] and
//!   [`new_filesystem_from_dyn_by_kind`] create mountable filesystems from
//!   caller-provided block devices.
//! - Runtime filesystem initialization: [`init_filesystems`] scans block
//!   devices, selects a root filesystem, and initializes [`FS_CONTEXT`].

#![cfg_attr(all(not(test), not(doc)), no_std)]
#![allow(clippy::new_ret_no_self)]

extern crate alloc;

#[macro_use]
extern crate log;

use ax_fs_vfs::Filesystem;

mod block;
mod fs;
mod fs_policy;
mod highlevel;

#[cfg(feature = "devfs")]
pub use ax_fs_devfs as devfs;
#[cfg(feature = "ramfs")]
pub use ax_fs_ramfs as ramfs;
pub use block::{BlockRegion, FsBlockDevice, SharedBlockDevice, VolumeReader};
/// Create a filesystem from a dynamic (boxed) block device.
pub use fs::{
    FilesystemKind, new_from_dyn as new_filesystem_from_dyn,
    new_from_dyn_by_kind as new_filesystem_from_dyn_by_kind,
};
pub use fs_policy::{
    DiscoveredFilesystem, discovered_filesystems, init_filesystems, mount_discovered_filesystem,
};
pub use highlevel::*;

/// Initializes the global root filesystem context from an already constructed
/// filesystem.
pub fn init_root_filesystem(fs: Filesystem) {
    info!("Initialize filesystem subsystem...");
    info!("  filesystem type: {:?}", fs.name());
    let mp = ax_fs_vfs::Mountpoint::new_root(&fs);
    let root = mp.root_location();
    ROOT_FS_CONTEXT.call_once(|| FsContext::new(root));
}
