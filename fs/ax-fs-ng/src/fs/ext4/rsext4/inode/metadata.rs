//! Inode attributes and persistence policy decoded from the canonical cache.

use alloc::sync::Arc;
use core::any::Any;

use axfs_ng_vfs::{
    DeviceId, FilesystemOps, Metadata, MetadataUpdate, NodeFlags, NodeOps, NodePermission,
    VfsResult, WritebackPolicy, XattrOps,
};
use rsext4::{DeviceNumber, Ext4Timestamp, FilePermissions, InodeFlags, InodeMetadataUpdate};

use super::{super::util::directory_entry_type_to_vfs, Inode, into_vfs_err};

impl NodeOps for Inode {
    fn inode(&self) -> u64 {
        self.number().as_u64()
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let inode = self.lifetime.metadata().map_err(into_vfs_err)?;
        let node_type = directory_entry_type_to_vfs(inode.file_type());
        let block_size = self.fs().block_size();
        Ok(Metadata {
            inode: self.number().as_u64(),
            device: 0,
            nlink: inode.links as _,
            mode: NodePermission::from_bits_truncate(inode.mode),
            node_type,
            uid: inode.uid,
            gid: inode.gid,
            size: inode.size,
            block_size,
            blocks: inode.blocks,
            rdev: inode
                .device_number
                .map(|device| DeviceId::new(device.major(), device.minor()))
                .unwrap_or_default(),
            atime: core::time::Duration::from_secs(u64::from(inode.atime)),
            mtime: core::time::Duration::from_secs(u64::from(inode.mtime)),
            ctime: core::time::Duration::from_secs(u64::from(inode.ctime)),
        })
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        let _inode = self.content_access().write()?;
        let metadata = InodeMetadataUpdate {
            permissions: update
                .mode
                .map(|mode| FilePermissions::new(mode.bits()))
                .transpose()
                .map_err(into_vfs_err)?,
            owner: update.owner,
            device_number: update
                .rdev
                .map(|device| DeviceNumber::new(device.major(), device.minor()))
                .transpose()
                .map_err(into_vfs_err)?,
            atime: update.atime.map(Self::timestamp).transpose()?,
            mtime: update.mtime.map(Self::timestamp).transpose()?,
            ..Default::default()
        };
        self.mutate(|ext4| ext4.update_inode_metadata(self.number(), metadata))
            .map_err(into_vfs_err)?;
        self.finish_metadata_change()
    }

    fn len(&self) -> VfsResult<u64> {
        self.lifetime
            .metadata()
            .map(|inode| inode.size)
            .map_err(into_vfs_err)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        &**self.fs()
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        self.fs().sync_to_disk()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::BLOCKING
    }

    fn writeback_policy(&self) -> VfsResult<WritebackPolicy> {
        let flags = self.lifetime.metadata().map_err(into_vfs_err)?.flags;
        let mut policy = WritebackPolicy::empty();
        policy.set(
            WritebackPolicy::SYNCHRONOUS,
            flags.contains(InodeFlags::SYNC),
        );
        policy.set(
            WritebackPolicy::DIRECTORY_SYNC,
            flags.contains(InodeFlags::DIRECTORY_SYNC),
        );
        Ok(policy)
    }

    fn xattr_ops(&self) -> Option<&dyn XattrOps> {
        Some(self)
    }
}

impl Inode {
    fn timestamp(value: core::time::Duration) -> VfsResult<Ext4Timestamp> {
        let seconds = i64::try_from(value.as_secs())
            .map_err(|_| into_vfs_err(rsext4::Ext4Error::overflow()))?;
        Ok(Ext4Timestamp::new(seconds, value.subsec_nanos()))
    }
}
