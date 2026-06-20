#[cfg(any(feature = "ext4", feature = "fat"))]
use alloc::boxed::Box;
use alloc::sync::Arc;

use axfs_ng_vfs::{Filesystem, VfsResult};

#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::FilesystemKind;
#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::block::FsBlockDevice;
use crate::{BlockDeviceHandle, block::BlockRegion};

#[cfg(feature = "ext4")]
mod ext4;
#[cfg(feature = "fat")]
mod fat;

/// Create a filesystem instance from a block device.
#[cfg(any(feature = "ext4", feature = "fat"))]
pub fn new_default(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    new_ext4(dev, region)
}

/// Create a filesystem instance from a detected filesystem kind.
#[cfg(any(feature = "ext4", feature = "fat"))]
pub(crate) fn new_by_kind(
    dev: Box<dyn FsBlockDevice>,
    region: BlockRegion,
    kind: FilesystemKind,
) -> VfsResult<Filesystem> {
    match kind {
        FilesystemKind::Ext4 => new_ext4(dev, region),
        FilesystemKind::Fat => new_fat(dev, region),
    }
}

/// Create a filesystem instance from a boxed block device.
///
/// Use this for loop devices and other block backends created outside the
/// platform probe path.
#[cfg(any(feature = "ext4", feature = "fat"))]
pub fn new_from_handle(dev: Arc<BlockDeviceHandle>, region: BlockRegion) -> VfsResult<Filesystem> {
    new_default(crate::block::boxed_native_handle_block_device(dev), region)
}

#[cfg(any(feature = "ext4", feature = "fat"))]
pub(crate) fn new_from_handle_with_kind(
    dev: Arc<BlockDeviceHandle>,
    region: BlockRegion,
    kind: FilesystemKind,
) -> VfsResult<Filesystem> {
    new_by_kind(
        crate::block::boxed_native_handle_block_device(dev),
        region,
        kind,
    )
}

#[cfg(not(any(feature = "ext4", feature = "fat")))]
pub fn new_from_handle(
    _dev: Arc<BlockDeviceHandle>,
    _region: BlockRegion,
) -> VfsResult<Filesystem> {
    panic!("No filesystem feature enabled");
}

#[cfg(not(any(feature = "ext4", feature = "fat")))]
pub(crate) fn new_from_handle_with_kind(
    _dev: Arc<BlockDeviceHandle>,
    _region: BlockRegion,
    _kind: crate::FilesystemKind,
) -> VfsResult<Filesystem> {
    panic!("No filesystem feature enabled");
}

#[cfg(feature = "ext4")]
fn new_ext4(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    ext4::Ext4Filesystem::new(dev, region)
}

#[cfg(all(any(feature = "ext4", feature = "fat"), not(feature = "ext4")))]
fn new_ext4(_dev: Box<dyn FsBlockDevice>, _region: BlockRegion) -> VfsResult<Filesystem> {
    Err(ax_errno::AxError::Unsupported)
}

#[cfg(feature = "fat")]
fn new_fat(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    fat::FatFilesystem::new(dev, region)
}

#[cfg(all(any(feature = "ext4", feature = "fat"), not(feature = "fat")))]
fn new_fat(_dev: Box<dyn FsBlockDevice>, _region: BlockRegion) -> VfsResult<Filesystem> {
    Err(ax_errno::AxError::Unsupported)
}
