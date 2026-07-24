//! Process credentials and Linux capability state.
//!
//! This module keeps the per-thread credential snapshot used by StarryOS
//! permission checks.  It tracks UID/GID families, supplementary groups, and
//! the five Linux capability sets exposed through `capget(2)`, `capset(2)`,
//! `/proc/<pid>/status`, and selected `prctl(2)` operations.

use alloc::sync::Arc;

use linux_raw_sys::general::{
    CAP_CHOWN, CAP_DAC_OVERRIDE, CAP_FOWNER, CAP_LAST_CAP, CAP_NET_ADMIN, CAP_NET_RAW, CAP_SETGID,
    CAP_SETPCAP, CAP_SETUID, CAP_SYS_ADMIN, CAP_SYS_BOOT, CAP_SYS_MODULE, CAP_SYS_NICE,
    CAP_SYS_RAWIO, CAP_SYS_RESOURCE,
};

const CAP_MASK: u64 = (1u64 << (CAP_LAST_CAP + 1)) - 1;

/// Return the bit mask for a single Linux capability number.
fn cap_bit(cap: u32) -> u64 {
    if cap <= CAP_LAST_CAP { 1u64 << cap } else { 0 }
}

/// Process credentials used for identity and permission checks.
///
/// The capability fields mirror Linux's inheritable, permitted, effective,
/// bounding, and ambient sets.  StarryOS stores the currently known capability
/// range in a `u64`, which is sufficient for `CAP_LAST_CAP`.
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
    /// Inheritable Linux capabilities.
    pub cap_inheritable: u64,
    /// Permitted Linux capabilities.
    pub cap_permitted: u64,
    /// Effective Linux capabilities.
    pub cap_effective: u64,
    /// Capability bounding set.
    pub cap_bounding: u64,
    /// Ambient Linux capabilities.
    pub cap_ambient: u64,
}

impl Cred {
    /// Return the mask of all Linux capabilities known to this kernel.
    pub const fn cap_mask() -> u64 {
        CAP_MASK
    }

    /// Create root credentials with all permitted/effective capabilities.
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
            cap_inheritable: 0,
            cap_permitted: CAP_MASK,
            cap_effective: CAP_MASK,
            cap_bounding: CAP_MASK,
            cap_ambient: 0,
        }
    }

    /// Create credentials for an unprivileged identity.
    ///
    /// The bounding set remains full so future privileged transitions can
    /// still be represented, but the effective/permitted/ambient sets start
    /// empty.
    pub fn unprivileged(uid: u32, gid: u32) -> Self {
        Self {
            uid,
            gid,
            euid: uid,
            egid: gid,
            suid: uid,
            sgid: gid,
            fsuid: uid,
            fsgid: gid,
            groups: Arc::from([].as_slice()),
            cap_inheritable: 0,
            cap_permitted: 0,
            cap_effective: 0,
            cap_bounding: CAP_MASK,
            cap_ambient: 0,
        }
    }

    /// Check whether a capability is present in the effective set.
    pub fn has_cap(&self, cap: u32) -> bool {
        self.cap_effective & cap_bit(cap) != 0
    }

    /// Recompute capability state after UID/GID credential changes.
    ///
    /// This models the usual Linux setxid transitions: leaving all-root UID
    /// state drops permitted/effective/ambient caps, losing euid 0 clears the
    /// effective set, and regaining euid 0 restores effective from permitted.
    pub fn apply_id_change_capability_rules(&mut self, old: &Self) {
        let old_all_root = old.uid == 0 && old.euid == 0 && old.suid == 0;
        let new_all_nonroot = self.uid != 0 && self.euid != 0 && self.suid != 0;

        if old_all_root && new_all_nonroot {
            self.cap_permitted = 0;
            self.cap_effective = 0;
            self.cap_ambient = 0;
        } else if old.euid == 0 && self.euid != 0 {
            self.cap_effective = 0;
            self.cap_ambient = 0;
        } else if old.euid != 0 && self.euid == 0 {
            self.cap_effective = self.cap_permitted;
        }

        self.cap_permitted &= CAP_MASK;
        self.cap_effective &= self.cap_permitted;
        self.cap_inheritable &= CAP_MASK;
        self.cap_bounding &= CAP_MASK;
        self.cap_ambient &= self.cap_permitted & self.cap_inheritable;
    }

    /// Limit all capability sets to the kernel-known range and internal
    /// invariants.
    pub fn sanitize_capabilities(&mut self) {
        self.cap_inheritable &= CAP_MASK;
        self.cap_permitted &= CAP_MASK;
        self.cap_effective &= self.cap_permitted;
        self.cap_bounding &= CAP_MASK;
        self.cap_ambient &= self.cap_permitted & self.cap_inheritable;
    }

    /// Check whether this credential has the privilege to change user IDs
    /// (equivalent to `CAP_SETUID`).
    pub fn has_cap_setuid(&self) -> bool {
        self.has_cap(CAP_SETUID)
    }

    /// Check whether this credential has the privilege to change group IDs
    /// (equivalent to `CAP_SETGID`).
    pub fn has_cap_setgid(&self) -> bool {
        self.has_cap(CAP_SETGID)
    }

    /// Check whether this credential may create raw network sockets
    /// (equivalent to `CAP_NET_RAW`).
    pub fn has_cap_net_raw(&self) -> bool {
        self.has_cap(CAP_NET_RAW)
    }

    /// Check whether this credential may perform network administration -
    /// configuring interfaces, routes and addresses, and creating or attaching
    /// TUN/TAP devices (equivalent to `CAP_NET_ADMIN`).
    pub fn has_cap_net_admin(&self) -> bool {
        self.has_cap(CAP_NET_ADMIN)
    }

    /// Check whether this credential may raise scheduling priority
    /// (equivalent to `CAP_SYS_NICE`).
    pub fn has_cap_sys_nice(&self) -> bool {
        self.has_cap(CAP_SYS_NICE)
    }

    /// Check whether this credential may change process resource limits
    /// (equivalent to `CAP_SYS_RESOURCE`).
    pub fn has_cap_sys_resource(&self) -> bool {
        self.has_cap(CAP_SYS_RESOURCE)
    }

    /// Check whether this credential may bypass file read/write/execute
    /// permission checks (equivalent to `CAP_DAC_OVERRIDE`).
    pub fn has_cap_dac_override(&self) -> bool {
        self.has_cap(CAP_DAC_OVERRIDE)
    }

    /// Check whether this credential may perform broad system administration
    /// operations (equivalent to `CAP_SYS_ADMIN`).
    pub fn has_cap_sys_admin(&self) -> bool {
        self.has_cap(CAP_SYS_ADMIN)
    }

    /// Check whether this credential may reboot the system
    /// (equivalent to `CAP_SYS_BOOT`).
    pub fn has_cap_sys_boot(&self) -> bool {
        self.has_cap(CAP_SYS_BOOT)
    }

    /// Check whether this credential may perform raw I/O — direct access to
    /// physical memory / device addresses (equivalent to `CAP_SYS_RAWIO`, the
    /// capability Linux requires for `/dev/mem`-class access). Gates handing a
    /// raw physical address to a DMA engine, which can otherwise reach arbitrary
    /// system memory.
    pub fn has_cap_sys_rawio(&self) -> bool {
        self.has_cap(CAP_SYS_RAWIO)
    }

    /// Check whether this credential may load or unload kernel modules
    /// (equivalent to `CAP_SYS_MODULE`).
    pub fn has_cap_sys_module(&self) -> bool {
        self.has_cap(CAP_SYS_MODULE)
    }

    /// Check whether this credential may inspect another process
    /// (equivalent to `CAP_SYS_PTRACE` — approximated as euid == 0).
    pub fn has_cap_sys_ptrace(&self) -> bool {
        self.euid == 0
    }

    /// Check whether this credential has the privilege to change file
    /// ownership (equivalent to `CAP_CHOWN`).
    pub fn has_cap_chown(&self) -> bool {
        self.has_cap(CAP_CHOWN)
    }

    /// Check whether this credential has the privilege to bypass file
    /// ownership checks (equivalent to `CAP_FOWNER`).
    pub fn has_cap_fowner(&self) -> bool {
        self.has_cap(CAP_FOWNER)
    }

    /// Check whether the caller may adjust capability sets.
    pub fn has_cap_setpcap(&self) -> bool {
        self.has_cap(CAP_SETPCAP)
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
