use alloc::{boxed::Box, sync::Arc};

use axfs_ng_vfs::{Filesystem, VfsResult};

use crate::{
    BlockDeviceHandle,
    block::{BlockRegion, FsBlockDevice},
};

cfg_if::cfg_if! {
    if #[cfg(feature = "ext4")] {
        mod ext4;
        type DefaultFilesystem = ext4::Ext4Filesystem;
    } else if #[cfg(feature = "fat")] {
        mod fat;
        type DefaultFilesystem = fat::FatFilesystem;
    } else {
        #[allow(dead_code)]
        struct DefaultFilesystem;
        #[allow(dead_code)]
        impl DefaultFilesystem {
            pub fn new(_dev: Box<dyn FsBlockDevice>, _region: BlockRegion) -> VfsResult<Filesystem> {
                panic!("No filesystem feature enabled");
            }
        }
    }
}

/// Create a filesystem instance from a block device.
#[cfg(any(feature = "ext4", feature = "fat"))]
pub fn new_default(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    DefaultFilesystem::new(dev, region)
}

/// Create a filesystem instance from a boxed block device.
///
/// Use this for loop devices and other block backends created outside the
/// platform probe path.
#[cfg(any(feature = "ext4", feature = "fat"))]
pub fn new_from_handle(dev: Arc<BlockDeviceHandle>, region: BlockRegion) -> VfsResult<Filesystem> {
    new_default(crate::block::boxed_native_handle_block_device(dev), region)
}

#[cfg(not(any(feature = "ext4", feature = "fat")))]
pub fn new_from_handle(
    _dev: Arc<BlockDeviceHandle>,
    _region: BlockRegion,
) -> VfsResult<Filesystem> {
    panic!("No filesystem feature enabled");
}
