//! Mount-owned mapping leases prevent reuse until a write receipt is accepted.

use alloc::{sync::Arc, vec::Vec};

use crate::{Ext4Error, Ext4Result, InodeNumber};

#[derive(Clone)]
pub(super) struct InodeWriteIdentity {
    pub(super) inode: InodeNumber,
    owner: Arc<()>,
}

#[derive(Default)]
pub(in crate::ext4::owned) struct InodeWriteOwners {
    writes: Vec<InodeWriteIdentity>,
}

impl InodeWriteOwners {
    pub(in crate::ext4::owned) fn ensure_inode_idle(&self, inode: InodeNumber) -> Ext4Result<()> {
        if self.writes.iter().any(|write| write.inode == inode) {
            Err(Ext4Error::busy().with_operation("inode:data_write_pending"))
        } else {
            Ok(())
        }
    }

    pub(in crate::ext4::owned) fn ensure_drained(&self) -> Ext4Result<()> {
        if self.writes.is_empty() {
            Ok(())
        } else {
            Err(Ext4Error::busy().with_operation("filesystem:data_write_pending"))
        }
    }

    pub(super) fn register(&mut self, inode: InodeNumber) -> Ext4Result<InodeWriteIdentity> {
        self.ensure_inode_idle(inode)?;
        self.writes
            .try_reserve(1)
            .map_err(|_| Ext4Error::no_memory())?;
        let identity = InodeWriteIdentity {
            inode,
            owner: Arc::new(()),
        };
        self.writes.push(identity.clone());
        Ok(identity)
    }

    pub(super) fn validate(&self, identity: &InodeWriteIdentity) -> Ext4Result<()> {
        if self.writes.iter().any(|write| {
            write.inode == identity.inode && Arc::ptr_eq(&write.owner, &identity.owner)
        }) {
            Ok(())
        } else {
            Err(Ext4Error::invalid_input().with_operation("inode:unexpected_write_receipt"))
        }
    }

    pub(super) fn release(&mut self, identity: &InodeWriteIdentity) {
        // Validation has already succeeded under the same exclusive mount
        // borrow. Retain unrelated writes even when they target another inode.
        self.writes
            .retain(|write| !Arc::ptr_eq(&write.owner, &identity.owner));
    }
}
