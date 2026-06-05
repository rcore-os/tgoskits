use alloc::boxed::Box;

use axfs_ng_vfs::{Filesystem, VfsResult};

use crate::block::{BlockRegion, FsBlockDevice};

cfg_if::cfg_if! {
    if #[cfg(feature = "ext4")] {
        mod ext4;
        type DefaultFilesystem = ext4::Ext4Filesystem;
    } else if #[cfg(feature = "fat")] {
        mod fat;
        type DefaultFilesystem = fat::FatFilesystem;
    } else {
        struct DefaultFilesystem;
        impl DefaultFilesystem {
            pub fn new(_dev: Box<dyn FsBlockDevice>, _region: BlockRegion) -> VfsResult<Filesystem> {
                panic!("No filesystem feature enabled");
            }
        }
    }
}

/// Create a filesystem instance from a block device.
pub fn new_default(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    DefaultFilesystem::new(dev, region)
}

/// Create a filesystem instance from a boxed block device.
///
/// Use this for loop devices and other block backends created outside the
/// platform probe path.
#[cfg(all(feature = "ext4", feature = "vfs"))]
pub fn new_from_dyn(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    ext4::Ext4Filesystem::new_from_boxed(dev, region)
}
