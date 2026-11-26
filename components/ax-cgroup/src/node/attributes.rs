//! Persistent inode attributes and first-time controller interface publication.

use core::{sync::atomic::Ordering, time::Duration};

use super::{CgroupMutationGuard, CgroupNode};
use crate::{CgroupError, CgroupFsAttributes, CgroupFsTarget, CgroupFsUpdate, CgroupResult};

impl CgroupNode {
    /// Return the stable inode number for an entity. Does not check visibility.
    pub const fn fs_inode(&self, target: CgroupFsTarget) -> u64 {
        target.inode(self.id)
    }

    /// Read the attributes of an installed inode, including a retained dead one.
    pub fn fs_attributes(&self, target: CgroupFsTarget) -> CgroupResult<CgroupFsAttributes> {
        self.domain.lock().fs_attributes(self, target)
    }

    /// Update installed inode attributes after the caller's authorization.
    pub fn update_fs_attributes(
        &self,
        target: CgroupFsTarget,
        update: CgroupFsUpdate,
    ) -> CgroupResult<()> {
        self.domain
            .lock()
            .update_fs_attributes(self, target, update)
    }

    /// Enable pids for direct children.
    ///
    /// Only `+pids` is supported. The write actor supplies candidate owners,
    /// resolved against each child; interface kinds fix their modes.
    pub fn write_subtree_control(
        &self,
        data: &str,
        creator: (u32, u32),
        now: Duration,
    ) -> CgroupResult<()> {
        self.domain
            .lock()
            .write_subtree_control(self, data, creator, now)
    }
}

impl CgroupMutationGuard<'_> {
    /// Read the directory's logical attribute source without testing liveness.
    /// Only this hierarchy's guard can observe it for a creation decision.
    pub fn directory_has_explicit_attrs(&self, node: &CgroupNode) -> CgroupResult<bool> {
        self.check_domain(node)?;
        Ok(node.fs.lock_irqsave().directory_has_explicit_attrs())
    }

    /// Snapshot an installed inode under the hierarchy gate without testing liveness.
    pub fn fs_attributes(
        &self,
        node: &CgroupNode,
        target: CgroupFsTarget,
    ) -> CgroupResult<CgroupFsAttributes> {
        self.check_domain(node)?;
        node.fs.lock_irqsave().attributes(target)
    }

    /// Apply a caller-authorized patch to one installed inode, including a retained dead one.
    pub fn update_fs_attributes(
        &self,
        node: &CgroupNode,
        target: CgroupFsTarget,
        update: CgroupFsUpdate,
    ) -> CgroupResult<()> {
        self.check_domain(node)?;
        node.fs.lock_irqsave().update(target, update)
    }

    /// Enable the existing pids controller with explicitly initialized interfaces.
    pub fn write_subtree_control(
        &self,
        node: &CgroupNode,
        data: &str,
        creator: (u32, u32),
        now: Duration,
    ) -> CgroupResult<()> {
        self.check_live(node)?;
        if data.trim() != "+pids" || !node.available_controllers().contains(&"pids") {
            return Err(CgroupError::InvalidInput);
        }
        if node.pids_enabled_for_children.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Every slot is already reserved in its node. Installation cannot
        // allocate or fail after it starts, and gate readers cannot see a prefix.
        for child in node.children.lock_irqsave().values() {
            child.fs.lock_irqsave().install_pids(child.id, creator, now);
        }
        // Existing controller readers acquire this same flag, including through
        // the children's shared parent reference.
        node.pids_enabled_for_children
            .store(true, Ordering::Release);
        Ok(())
    }
}
