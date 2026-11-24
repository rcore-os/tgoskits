//! Mount access-time policy and the inode-owned automatic update capability.

use alloc::string::String;
use core::time::Duration;

use crate::{
    LinkCaller, LinkTarget, Location, Metadata, MetadataRequest, NamespaceCompletion, NodeType,
    VfsResult,
};

/// Access-time work required by a mount before following a symbolic link.
///
/// The mount supplies the policy; the producer must also account for its
/// native inode restrictions and timestamp precision before opting into a
/// cache-only read. This predicate does not authorize a metadata update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessTimePolicy {
    /// Mount or shared-filesystem state suppresses access-time work.
    NoAtime,
    /// Update after metadata changes or a full day of whole Unix seconds.
    Relative,
    /// Update whenever the current timestamp differs from the wall clock.
    Strict,
}

impl AccessTimePolicy {
    /// Tests current inode timestamps against an already sampled wall clock.
    /// Relative age uses whole Unix seconds, including the one-day boundary.
    pub fn needs_update(self, metadata: &Metadata, now: Duration) -> bool {
        if self == Self::NoAtime || metadata.atime == now {
            return false;
        }
        self == Self::Strict
            || metadata.mtime >= metadata.atime
            || metadata.ctime >= metadata.atime
            || now.as_secs().saturating_sub(metadata.atime.as_secs()) >= 24 * 60 * 60
    }
}

/// Optional capability for automatic access-time work on the actual inode.
///
/// The VFS discovers this capability before acquiring write admission. A
/// producer opts in only after auditing its inode flags, timestamp precision,
/// and commit path. User timestamp requests continue through `change_metadata`.
pub trait AccessTimeOps: Send + Sync {
    /// Executes the fixed automatic-access request under the inode's real lock.
    /// The current timestamps and clock must be sampled in that same attempt.
    fn touch(&self, request: &MetadataRequest<'_>) -> NamespaceCompletion<Metadata>;
}

impl Location {
    /// Reads link display text after best-effort automatic access-time work.
    /// Non-link errors and caller-dependent link authorization remain with the
    /// producer. A failed timestamp update does not replace its result.
    pub fn read_link(&self, caller: &dyn LinkCaller) -> VfsResult<String> {
        self.touch_link_access_time(caller);
        self.entry().read_link(caller)
    }

    /// Resolves a link after the walker has accepted its policy and budget.
    pub fn link_target(&self, caller: &dyn LinkCaller) -> VfsResult<LinkTarget> {
        self.touch_link_access_time(caller);
        self.entry().link_target(caller)
    }

    fn touch_link_access_time(&self, caller: &dyn LinkCaller) {
        if self.node_type() != NodeType::Symlink || self.entry().access_time_ops().is_none() {
            return;
        }
        let policy = self.mountpoint().link_time_policy();
        if policy != AccessTimePolicy::NoAtime {
            // Linux touch_atime is best effort. PreparedWrite still enforces
            // both mount-local and shared-filesystem read-only state; failure
            // leaves this metadata unchanged without failing link resolution.
            let _ = self.run_metadata(
                super::MetadataIntent::Accessed(policy),
                &caller.credentials(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceId, NodePermission};

    #[test]
    fn access_time_policy_uses_inode_order_and_whole_second_age() {
        let mut metadata = Metadata {
            device: 0,
            inode: 1,
            nlink: 1,
            mode: NodePermission::default(),
            node_type: crate::NodeType::Symlink,
            uid: 0,
            gid: 0,
            size: 0,
            block_size: 4096,
            blocks: 0,
            rdev: DeviceId(0),
            atime: Duration::new(86_401, 999_999_999),
            mtime: Duration::ZERO,
            ctime: Duration::ZERO,
        };
        let now = Duration::from_secs(172_801);
        assert!(!AccessTimePolicy::Relative.needs_update(&metadata, now - Duration::from_secs(1)));
        // Linux compares tv_sec: the subsecond part does not delay this edge.
        assert!(AccessTimePolicy::Relative.needs_update(&metadata, now));
        assert!(!AccessTimePolicy::NoAtime.needs_update(&metadata, now));

        metadata.atime = now + Duration::from_secs(1);
        assert!(!AccessTimePolicy::Relative.needs_update(&metadata, now));
        assert!(AccessTimePolicy::Strict.needs_update(&metadata, now));
        metadata.mtime = metadata.atime;
        assert!(AccessTimePolicy::Relative.needs_update(&metadata, now));
        metadata.mtime = Duration::ZERO;
        metadata.ctime = metadata.atime;
        assert!(AccessTimePolicy::Relative.needs_update(&metadata, now));

        metadata.atime = now;
        assert!(!AccessTimePolicy::Relative.needs_update(&metadata, now));
        assert!(!AccessTimePolicy::Strict.needs_update(&metadata, now));
    }
}
