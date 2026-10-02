//! Persistent user xattrs and ext4-to-VFS error translation.

use alloc::vec::Vec;

use axfs_ng_vfs::{VfsError, VfsResult, XattrOps, XattrSetMode as VfsXattrSetMode};
use rsext4::{XattrNamespace, XattrSetMode};

use super::{Inode, into_vfs_err};

impl XattrOps for Inode {
    fn get_xattr(&self, name: &[u8]) -> VfsResult<Vec<u8>> {
        let name = Self::user_xattr_name(name)?;
        let mut state = self.fs().lock();
        state
            .ext4
            .get_xattr(self.number(), XattrNamespace::User, name)
            .map_err(Self::xattr_error)
    }

    fn list_xattrs(&self) -> VfsResult<Vec<Vec<u8>>> {
        let mut state = self.fs().lock();
        let names = state
            .ext4
            .list_xattrs(self.number())
            .map_err(Self::xattr_error)?;
        Ok(names
            .into_iter()
            .filter(|name| name.namespace == XattrNamespace::User)
            .map(|name| {
                let mut full_name = b"user.".to_vec();
                full_name.extend_from_slice(&name.name);
                full_name
            })
            .collect())
    }

    fn set_xattr(&self, name: &[u8], value: &[u8], mode: VfsXattrSetMode) -> VfsResult<()> {
        let name = Self::user_xattr_name(name)?;
        let mode = match mode {
            VfsXattrSetMode::Upsert => XattrSetMode::Upsert,
            VfsXattrSetMode::Create => XattrSetMode::Create,
            VfsXattrSetMode::Replace => XattrSetMode::Replace,
        };
        let _inode = self.content_access().write()?;
        self.mutate(|ext4| ext4.set_xattr(self.number(), XattrNamespace::User, name, value, mode))
            .map_err(Self::xattr_error)?;
        self.finish_metadata_change()
    }

    fn remove_xattr(&self, name: &[u8]) -> VfsResult<()> {
        let name = Self::user_xattr_name(name)?;
        let _inode = self.content_access().write()?;
        self.mutate(|ext4| ext4.remove_xattr(self.number(), XattrNamespace::User, name))
            .map_err(Self::xattr_error)?;
        self.finish_metadata_change()
    }
}

impl Inode {
    fn user_xattr_name(name: &[u8]) -> VfsResult<&[u8]> {
        const PREFIX: &[u8] = b"user.";
        let component = name
            .strip_prefix(PREFIX)
            .ok_or(VfsError::OperationNotSupported)?;
        if component.is_empty() {
            return Err(VfsError::InvalidInput);
        }
        Ok(component)
    }

    fn xattr_error(error: rsext4::Ext4Error) -> VfsError {
        if error.kind() == rsext4::Ext4ErrorKind::NotFound {
            VfsError::DataMissing
        } else {
            into_vfs_err(error)
        }
    }
}
