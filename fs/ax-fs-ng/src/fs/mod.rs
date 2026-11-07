#[cfg(any(feature = "ext4", feature = "fat"))]
use alloc::boxed::Box;
use alloc::sync::Arc;

pub mod memory;

use axfs_ng_vfs::{Filesystem, VfsResult};

use crate::DeviceRef;
#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::FilesystemKind;
#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::block::BlockRegion;
#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::block::FsBlockDevice;
#[cfg(any(feature = "ext4", feature = "fat"))]
use crate::block_error_to_vfs_error;

#[cfg(any(feature = "ext4", feature = "fat"))]
struct NativeFilesystem {
    device: alloc::sync::Weak<DeviceRef>,
    region: BlockRegion,
    filesystem: axfs_ng_vfs::WeakFilesystem,
}

// This weak lookup only deduplicates published filesystems. Physical leases
// arbitrate construction and maintenance. Never hold this lock across device I/O.
#[cfg(any(feature = "ext4", feature = "fat"))]
static NATIVE_FILESYSTEMS: crate::os::sync::Mutex<alloc::vec::Vec<NativeFilesystem>> =
    crate::os::sync::Mutex::new(alloc::vec::Vec::new());

#[cfg(feature = "ext4")]
mod ext4;
#[cfg(feature = "fat")]
mod fat;

/// Opens a physical ext4 volume described by a current layout observation.
///
/// The descriptor must belong to `dev`; stale layouts and conflicting access
/// owners are rejected before filesystem construction. Read-only publication
/// may first perform journal recovery under exclusive write access.
#[cfg(all(feature = "vfs", any(feature = "ext4", feature = "fat")))]
pub fn new_from_handle(
    dev: Arc<DeviceRef>,
    descriptor: &ax_block::VolumeDescriptor,
    readonly: bool,
) -> VfsResult<Filesystem> {
    new_from_handle_with_kind_readonly(dev, descriptor, FilesystemKind::Ext4, readonly)
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
    dev: Arc<DeviceRef>,
    descriptor: &ax_block::VolumeDescriptor,
    kind: FilesystemKind,
) -> VfsResult<Filesystem> {
    new_from_handle_with_kind_readonly(dev, descriptor, kind, false)
}

#[cfg(any(feature = "ext4", feature = "fat"))]
pub(crate) fn new_from_handle_with_kind_readonly(
    dev: Arc<DeviceRef>,
    descriptor: &ax_block::VolumeDescriptor,
    kind: FilesystemKind,
    readonly: bool,
) -> VfsResult<Filesystem> {
    let region = descriptor.region();
    dev.validate_volume_descriptor(descriptor)
        .map_err(block_error_to_vfs_error)?;
    let device = Arc::downgrade(&dev);
    let mounted = {
        let mut registry = NATIVE_FILESYSTEMS.lock();
        registry.retain(|entry| entry.filesystem.is_alive());
        registry
            .iter()
            .find(|entry| entry.region == region && entry.device.ptr_eq(&device))
            .map(|entry| entry.filesystem.clone())
    };
    // A failed upgrade can release a partially retained backend; its final
    // destructor must also run after releasing the registry lock.
    if let Some(filesystem) = mounted.and_then(|mounted| mounted.upgrade()) {
        // Remount updates this shared superblock state. A construction-time
        // snapshot would accept or reject later mounts using stale policy.
        if filesystem.is_readonly() != readonly {
            return Err(axfs_ng_vfs::VfsError::ResourceBusy);
        }
        return Ok(filesystem);
    }
    // Retain the writable device's original authority for recovery or remount.
    // A read-only publication revokes current writes without losing that grant.
    let preparation_access = if dev.device_info().read_only {
        ax_block::VolumeAccess::ReadOnly
    } else {
        ax_block::VolumeAccess::ReadWrite
    };
    let lease = dev
        .open_volume(descriptor, preparation_access)
        .map_err(block_error_to_vfs_error)?;
    let permissions = lease.permissions();
    let backend: Box<dyn FsBlockDevice> = Box::new(lease);
    let filesystem = match kind {
        FilesystemKind::Ext4 => {
            #[cfg(feature = "ext4")]
            {
                ext4::Ext4Filesystem::new_with_access(backend, region, readonly, Some(permissions))?
            }
            #[cfg(not(feature = "ext4"))]
            {
                return Err(axfs_ng_vfs::VfsError::OperationNotSupported);
            }
        }
        FilesystemKind::Fat => {
            if readonly {
                permissions
                    .restrict_read_only()
                    .map_err(block_error_to_vfs_error)?;
            }
            new_fat(backend, region, Some(permissions))?
        }
    };
    filesystem.set_readonly(readonly || filesystem.is_readonly());
    let mut registry = NATIVE_FILESYSTEMS.lock();
    registry.push(NativeFilesystem {
        device,
        region,
        filesystem: filesystem.downgrade(),
    });
    Ok(filesystem)
}

#[cfg(not(any(feature = "ext4", feature = "fat")))]
pub(crate) fn new_from_handle_with_kind(
    _dev: Arc<DeviceRef>,
    _descriptor: &ax_block::VolumeDescriptor,
    _kind: crate::FilesystemKind,
) -> VfsResult<Filesystem> {
    panic!("No filesystem feature enabled");
}

#[cfg(not(any(feature = "ext4", feature = "fat")))]
pub(crate) fn new_from_handle_with_kind_readonly(
    _dev: Arc<DeviceRef>,
    _descriptor: &ax_block::VolumeDescriptor,
    _kind: crate::FilesystemKind,
    _readonly: bool,
) -> VfsResult<Filesystem> {
    Err(axfs_ng_vfs::VfsError::OperationNotSupported)
}

#[cfg(all(feature = "ext4", feature = "vfs"))]
fn new_ext4(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    ext4::Ext4Filesystem::new(dev, region)
}

#[cfg(feature = "fat")]
fn new_fat(
    dev: Box<dyn FsBlockDevice>,
    region: BlockRegion,
    permissions: Option<ax_block::VolumePermissions>,
) -> VfsResult<Filesystem> {
    fat::FatFilesystem::new(dev, region, permissions)
}

#[cfg(all(any(feature = "ext4", feature = "fat"), not(feature = "fat")))]
fn new_fat(
    _dev: Box<dyn FsBlockDevice>,
    _region: BlockRegion,
    _permissions: Option<ax_block::VolumePermissions>,
) -> VfsResult<Filesystem> {
    Err(axfs_ng_vfs::VfsError::Unsupported)
}

/// Publishes an already prepared mount; disk recovery and GPT I/O finish before
/// entering the registry. Physical access admission owns geometry exclusion.
#[cfg(feature = "ext4")]
pub(crate) fn new_from_prepared_ext4(
    dev: Arc<DeviceRef>,
    region: BlockRegion,
    mounted: MountedExt4,
) -> VfsResult<Filesystem> {
    let filesystem = ext4::Ext4Filesystem::from_mounted(mounted)?;
    let mut registry = NATIVE_FILESYSTEMS.lock();
    registry.retain(|entry| entry.filesystem.is_alive());
    registry.push(NativeFilesystem {
        device: Arc::downgrade(&dev),
        region,
        filesystem: filesystem.downgrade(),
    });
    Ok(filesystem)
}

#[cfg(feature = "ext4")]
pub(crate) use ext4::{MountedExt4, ext4_error_to_vfs_error, prepare_root_growth};
