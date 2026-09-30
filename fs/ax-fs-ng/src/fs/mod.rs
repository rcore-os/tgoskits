#[cfg(any(feature = "ext4", feature = "fat"))]
use alloc::boxed::Box;
use alloc::sync::Arc;

pub mod memory;

use axfs_ng_vfs::{Filesystem, VfsResult};

#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::FilesystemKind;
#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::block::FsBlockDevice;
#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::block_error_to_vfs_error;
use crate::{BlockDeviceHandle, block::BlockRegion};

#[cfg(any(feature = "ext4", feature = "fat"))]
struct NativeFilesystem {
    device: alloc::sync::Weak<BlockDeviceHandle>,
    region: BlockRegion,
    filesystem: axfs_ng_vfs::WeakFilesystem,
}

// Serialize superblock construction, including sleeping disk I/O. No block-cache
// or topology lock is held when entering this registry; its entries own neither
// devices nor filesystems, so detached mounts retain their normal lifetimes.
#[cfg(any(feature = "ext4", feature = "fat"))]
static NATIVE_FILESYSTEMS: crate::os::sync::SleepMutex<alloc::vec::Vec<NativeFilesystem>> =
    crate::os::sync::SleepMutex::new(alloc::vec::Vec::new());

#[cfg(feature = "ext4")]
mod ext4;
#[cfg(feature = "fat")]
mod fat;

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
    new_from_handle_with_kind(dev, region, FilesystemKind::Ext4)
}

/// Creates an ext4 filesystem using on-demand file I/O.
///
/// The filesystem owns `lease` until its final references are released. Device
/// callers must provide an open lease that keeps the source binding stable;
/// detaching a mount must not release that lease while files remain open.
/// I/O and flush errors propagate through the filesystem's block-I/O boundary.
#[cfg(all(feature = "ext4", feature = "vfs"))]
pub fn new_from_file<L: Send + 'static>(
    backend: crate::file::FileBackend,
    read_only: bool,
    lease: L,
) -> VfsResult<Filesystem> {
    let device = crate::block::file_image::FileImageDevice::new(backend, read_only, lease)?;
    let region = BlockRegion::from_num_blocks(device.num_blocks());
    new_ext4(Box::new(device), region)
}

#[cfg(any(feature = "ext4", feature = "fat"))]
pub(crate) fn new_from_handle_with_kind(
    dev: Arc<BlockDeviceHandle>,
    region: BlockRegion,
    kind: FilesystemKind,
) -> VfsResult<Filesystem> {
    let mut registry = NATIVE_FILESYSTEMS.lock();
    registry.retain(|entry| entry.filesystem.is_alive());
    for entry in &*registry {
        if entry.region == region
            && entry.device.ptr_eq(&Arc::downgrade(&dev))
            && let Some(fs) = entry.filesystem.upgrade()
        {
            return Ok(fs);
        }
    }
    let device = Arc::downgrade(&dev);
    let backend =
        crate::block::boxed_native_handle_block_device(dev).map_err(block_error_to_vfs_error)?;
    let filesystem = new_by_kind(backend, region, kind)?;
    registry.push(NativeFilesystem {
        device,
        region,
        filesystem: filesystem.downgrade(),
    });
    Ok(filesystem)
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
    Err(axfs_ng_vfs::VfsError::Unsupported)
}

#[cfg(feature = "fat")]
fn new_fat(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    fat::FatFilesystem::new(dev, region)
}

#[cfg(all(any(feature = "ext4", feature = "fat"), not(feature = "fat")))]
fn new_fat(_dev: Box<dyn FsBlockDevice>, _region: BlockRegion) -> VfsResult<Filesystem> {
    Err(axfs_ng_vfs::VfsError::Unsupported)
}
