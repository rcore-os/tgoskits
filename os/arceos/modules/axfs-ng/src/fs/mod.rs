#[cfg(feature = "ext4")]
use alloc::boxed::Box;

#[cfg(feature = "ext4")]
use ax_driver::prelude::BlockDriverOps;
use ax_driver::{AxBlockDevice, PartitionRegion};
use axfs_ng_vfs::{Filesystem, VfsResult};

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
            pub fn new(_dev: AxBlockDevice, _region: PartitionRegion) -> VfsResult<Filesystem> {
                panic!("No filesystem feature enabled");
            }
        }
    }
}

/// Create a filesystem instance from a block device.
pub fn new_default(dev: AxBlockDevice, region: PartitionRegion) -> VfsResult<Filesystem> {
    DefaultFilesystem::new(dev, region)
}

/// Create a filesystem instance from a dynamic (boxed) block device.
///
/// Use this for loop devices and other block backends that don't match
/// the compile-time `AxBlockDevice` type alias.
#[cfg(feature = "ext4")]
pub fn new_from_dyn(
    dev: Box<dyn BlockDriverOps>,
    region: PartitionRegion,
) -> VfsResult<Filesystem> {
    ext4::Ext4Filesystem::new_from_boxed(dev, region)
}
