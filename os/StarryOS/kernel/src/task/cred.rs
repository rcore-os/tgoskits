//! Process credentials (uid, gid, supplementary groups).

use alloc::sync::Arc;

/// Process credentials tracking real, effective, saved, and filesystem
/// user/group IDs, plus supplementary groups.
#[derive(Clone, Debug)]
pub struct Cred {
    /// Real user ID.
    pub uid: u32,
    /// Real group ID.
    pub gid: u32,
    /// Effective user ID.
    pub euid: u32,
    /// Effective group ID.
    pub egid: u32,
    /// Saved set-user-ID.
    pub suid: u32,
    /// Saved set-group-ID.
    pub sgid: u32,
    /// Filesystem user ID.
    pub fsuid: u32,
    /// Filesystem group ID.
    pub fsgid: u32,
    /// Supplementary group list.
    pub groups: Arc<[u32]>,
}

impl Cred {
    /// Create root credentials (all IDs zero, no supplementary groups).
    pub fn root() -> Self {
        Self {
            uid: 0,
            gid: 0,
            euid: 0,
            egid: 0,
            suid: 0,
            sgid: 0,
            fsuid: 0,
            fsgid: 0,
            groups: Arc::from([].as_slice()),
        }
    }

    /// Check whether this credential has the privilege to change user IDs
    /// (equivalent to `CAP_SETUID` — approximated as euid == 0).
    pub fn has_cap_setuid(&self) -> bool {
        self.euid == 0
    }

    /// Check whether this credential has the privilege to change group IDs
    /// (equivalent to `CAP_SETGID` — approximated as euid == 0).
    pub fn has_cap_setgid(&self) -> bool {
        self.euid == 0
    }

    /// Check whether this credential has the privilege to change file
    /// ownership (equivalent to `CAP_CHOWN` — approximated as fsuid == 0,
    /// since this is a filesystem capability).
    pub fn has_cap_chown(&self) -> bool {
        self.fsuid == 0
    }

    /// Check whether this credential has the privilege to bypass file
    /// ownership checks (equivalent to `CAP_FOWNER` — approximated as
    /// fsuid == 0, since this is a filesystem capability).
    pub fn has_cap_fowner(&self) -> bool {
        self.fsuid == 0
    }

    /// Return true if `gid` is the process's fsgid or is in its
    /// supplementary group list. Uses fsgid (not egid) because this
    /// method is used for filesystem permission checks.
    pub fn in_group(&self, gid: u32) -> bool {
        self.fsgid == gid || self.groups.contains(&gid)
    }
}

impl Default for Cred {
    fn default() -> Self {
        Self::root()
    }
}
