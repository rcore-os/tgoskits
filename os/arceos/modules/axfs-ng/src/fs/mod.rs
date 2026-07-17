use alloc::sync::Arc;

use axfs_ng_vfs::{Filesystem, VfsResult};

#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::FilesystemKind;
use crate::{BlockDevice, block::BlockRegion};

#[cfg(feature = "ext4")]
mod ext4;
#[cfg(feature = "fat")]
mod fat;

/// Create a filesystem instance from a block device.
#[cfg(any(feature = "ext4", feature = "fat"))]
pub fn new_default(dev: Arc<dyn BlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    new_ext4(dev, region)
}

/// Create a filesystem instance from a detected filesystem kind.
#[cfg(any(feature = "ext4", feature = "fat"))]
pub(crate) fn new_by_kind(
    dev: Arc<dyn BlockDevice>,
    region: BlockRegion,
    kind: FilesystemKind,
) -> VfsResult<Filesystem> {
    match kind {
        FilesystemKind::Ext4 => new_ext4(dev, region),
        FilesystemKind::Fat => new_fat(dev, region),
    }
}

/// Creates a filesystem instance from a synchronous block service.
#[cfg(any(feature = "ext4", feature = "fat"))]
pub fn new_from_device(dev: Arc<dyn BlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    new_default(dev, region)
}

#[cfg(any(feature = "ext4", feature = "fat"))]
pub(crate) fn new_from_device_with_kind(
    dev: Arc<dyn BlockDevice>,
    region: BlockRegion,
    kind: FilesystemKind,
) -> VfsResult<Filesystem> {
    new_by_kind(dev, region, kind)
}

#[cfg(not(any(feature = "ext4", feature = "fat")))]
pub fn new_from_device(_dev: Arc<dyn BlockDevice>, _region: BlockRegion) -> VfsResult<Filesystem> {
    Err(ax_errno::AxError::Unsupported)
}

#[cfg(not(any(feature = "ext4", feature = "fat")))]
pub(crate) fn new_from_device_with_kind(
    _dev: Arc<dyn BlockDevice>,
    _region: BlockRegion,
    _kind: crate::FilesystemKind,
) -> VfsResult<Filesystem> {
    Err(ax_errno::AxError::Unsupported)
}

#[cfg(feature = "ext4")]
fn new_ext4(dev: Arc<dyn BlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    ext4::Ext4Filesystem::new(dev, region)
}

#[cfg(all(any(feature = "ext4", feature = "fat"), not(feature = "ext4")))]
fn new_ext4(_dev: Arc<dyn BlockDevice>, _region: BlockRegion) -> VfsResult<Filesystem> {
    Err(ax_errno::AxError::Unsupported)
}

#[cfg(feature = "fat")]
fn new_fat(dev: Arc<dyn BlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    fat::FatFilesystem::new(dev, region)
}

#[cfg(all(any(feature = "ext4", feature = "fat"), not(feature = "fat")))]
fn new_fat(_dev: Arc<dyn BlockDevice>, _region: BlockRegion) -> VfsResult<Filesystem> {
    Err(ax_errno::AxError::Unsupported)
}
