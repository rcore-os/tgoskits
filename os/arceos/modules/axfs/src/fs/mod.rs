use alloc::boxed::Box;

use ax_fs_vfs::{Filesystem, VfsResult};

use crate::block::{BlockRegion, FsBlockDevice};

#[cfg(feature = "ext4")]
mod ext4;
#[cfg(feature = "fat")]
mod fat;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemKind {
    #[cfg(feature = "ext4")]
    Ext4,
    #[cfg(feature = "fat")]
    Fat,
}

impl FilesystemKind {
    pub const fn name(self) -> &'static str {
        filesystem_name(self)
    }
}

pub(crate) fn new_default_from_dyn(
    dev: Box<dyn FsBlockDevice>,
    region: BlockRegion,
) -> VfsResult<Filesystem> {
    cfg_if::cfg_if! {
        if #[cfg(feature = "ext4")] {
            ext4::Ext4Filesystem::new(dev, region)
        } else if #[cfg(feature = "fat")] {
            fat::FatFilesystem::new(dev, region)
        } else {
            let _ = (dev, region);
            Err(ax_fs_vfs::VfsError::Unsupported)
        }
    }
}

pub(crate) fn new_by_kind_from_dyn(
    dev: Box<dyn FsBlockDevice>,
    region: BlockRegion,
    kind: FilesystemKind,
) -> VfsResult<Filesystem> {
    cfg_if::cfg_if! {
        if #[cfg(any(feature = "ext4", feature = "fat"))] {
            match kind {
                #[cfg(feature = "ext4")]
                FilesystemKind::Ext4 => ext4::Ext4Filesystem::new(dev, region),
                #[cfg(feature = "fat")]
                FilesystemKind::Fat => fat::FatFilesystem::new(dev, region),
            }
        } else {
            let _ = (dev, region, kind);
            Err(ax_fs_vfs::VfsError::Unsupported)
        }
    }
}

pub(crate) const fn filesystem_name(kind: FilesystemKind) -> &'static str {
    match kind {
        #[cfg(feature = "ext4")]
        FilesystemKind::Ext4 => "ext4",
        #[cfg(feature = "fat")]
        FilesystemKind::Fat => "fat",
    }
}

/// Create a filesystem instance from a dynamic (boxed) block device.
///
/// Use this for loop devices and other block backends that don't match
/// the compile-time `AxBlockDevice` type alias.
pub fn new_from_dyn(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
    new_default_from_dyn(dev, region)
}

/// Create a filesystem instance of a known kind from a dynamic block device.
pub fn new_from_dyn_by_kind(
    dev: Box<dyn FsBlockDevice>,
    region: BlockRegion,
    kind: FilesystemKind,
) -> VfsResult<Filesystem> {
    new_by_kind_from_dyn(dev, region, kind)
}
