mod cgroup;
mod ipc;
mod mnt;
mod net;
mod user;
mod uts;

use alloc::sync::Arc;

pub use ax_cgroup::{CgroupNamespace, CgroupNode};
pub use cgroup::{ROOT_CGROUP_NS, new_cgroup_namespace};
pub use ipc::{IpcNamespace, ROOT_IPC_NS};
pub use mnt::{MntNamespace, ROOT_MNT_NS};
pub use net::{NetNamespace, ROOT_NET_NS};
pub use user::{ROOT_USER_NS, UserNamespace};
pub use uts::{ROOT_UTS_NS, UtNamespace, build_utsname};

use crate::sync::IrqMutex;
pub(crate) use crate::task::{PidNamespace, PidNamespaceRef, ROOT_PID_NS};

/// Aggregates all namespace types for a process.
///
/// `ProcessData` holds a single `IrqMutex<NsProxy>` field. Clone and unshare
/// operations work through `NsProxy` methods so that syscall handlers do not
/// manipulate namespace internals directly.
pub(crate) struct NsProxy {
    /// The UTS namespace (hostname, domainname).
    pub(crate) uts_ns: Arc<IrqMutex<UtNamespace>>,
    /// The IPC namespace (System V IPC objects).
    pub(crate) ipc_ns: Arc<IrqMutex<IpcNamespace>>,
    /// The mount namespace (filesystem mount points).
    pub(crate) mnt_ns: Arc<IrqMutex<MntNamespace>>,
    /// PID namespace used by future children.
    ///
    /// Linux keeps this choice until another `unshare(CLONE_NEWPID)` or
    /// `setns(CLONE_NEWPID)` replaces it. Forking a child never consumes it.
    pub(crate) pid_ns_for_children: PidNamespaceRef,
    /// The network namespace (interfaces, routing, sockets).
    pub(crate) net_ns: Arc<IrqMutex<NetNamespace>>,
    /// The user namespace (UID/GID mappings).
    pub(crate) user_ns: Arc<IrqMutex<UserNamespace>>,
    /// The cgroup namespace (cgroup hierarchy view).
    pub(crate) cgroup_ns: Arc<IrqMutex<CgroupNamespace>>,
}

impl NsProxy {
    /// Create a new [`NsProxy`] pointing to the root namespaces.
    pub fn new_root() -> Self {
        Self {
            uts_ns: ROOT_UTS_NS.clone(),
            ipc_ns: ROOT_IPC_NS.clone(),
            mnt_ns: ROOT_MNT_NS.clone(),
            pid_ns_for_children: ROOT_PID_NS.clone(),
            net_ns: ROOT_NET_NS.clone(),
            user_ns: ROOT_USER_NS.clone(),
            cgroup_ns: ROOT_CGROUP_NS.clone(),
        }
    }

    /// Clone all namespace references (shallow `Arc` clone).
    ///
    /// Used by `fork` / `clone` (without `CLONE_NEW*` flags) so the child
    /// shares the same namespaces as the parent. The future-child PID
    /// namespace is persistent and therefore inherited unchanged.
    pub fn clone_all(&self) -> Self {
        Self {
            uts_ns: self.uts_ns.clone(),
            ipc_ns: self.ipc_ns.clone(),
            mnt_ns: self.mnt_ns.clone(),
            pid_ns_for_children: self.pid_ns_for_children.clone(),
            net_ns: self.net_ns.clone(),
            user_ns: self.user_ns.clone(),
            cgroup_ns: self.cgroup_ns.clone(),
        }
    }

    /// Clone namespace references for a transactional `unshare(2)` update.
    ///
    /// Unlike [`Self::clone_all`], this preserves a PID namespace already
    /// staged for the next child. Preparing an unrelated unshare operation
    /// must not discard pending `unshare(CLONE_NEWPID)` or `setns` state.
    pub fn clone_for_unshare(&self) -> Self {
        Self {
            uts_ns: self.uts_ns.clone(),
            ipc_ns: self.ipc_ns.clone(),
            mnt_ns: self.mnt_ns.clone(),
            pid_ns_for_children: self.pid_ns_for_children.clone(),
            net_ns: self.net_ns.clone(),
            user_ns: self.user_ns.clone(),
            cgroup_ns: self.cgroup_ns.clone(),
        }
    }

    pub fn unshare_uts(&mut self) {
        let new_inner = self.uts_ns.lock().clone_ns();
        self.uts_ns = Arc::new(IrqMutex::new(new_inner));
    }

    pub fn unshare_ipc(&mut self) {
        let new_inner = self.ipc_ns.lock().clone_ns();
        self.ipc_ns = Arc::new(IrqMutex::new(new_inner));
    }

    pub fn unshare_mnt(&mut self) {
        let new_inner = self.mnt_ns.lock().clone_ns();
        self.mnt_ns = Arc::new(IrqMutex::new(new_inner));
    }

    /// Prepare a new PID namespace for the next child of this process.
    ///
    /// Called by `unshare(CLONE_NEWPID)`.  The calling process stays in
    /// its current PID namespace; the new namespace is consumed by the
    /// next `fork` / `clone` child, which becomes PID 1 in that namespace.
    pub fn prepare_pid_ns_for_children(&mut self, active: PidNamespaceRef) {
        self.pid_ns_for_children = PidNamespace::new_child(active);
    }

    pub fn unshare_net(&mut self) {
        let new_inner = self.net_ns.lock().clone_ns();
        self.net_ns = Arc::new(IrqMutex::new(new_inner));
    }

    pub fn unshare_user(&mut self) {
        let new_inner = self.user_ns.lock().clone_ns();
        self.user_ns = Arc::new(IrqMutex::new(new_inner));
    }

    pub fn unshare_cgroup(&mut self, root: Arc<CgroupNode>) {
        self.cgroup_ns = new_cgroup_namespace(root);
    }

    /// Replace the UTS namespace with an existing one (used by `setns(2)`).
    pub fn set_ns_uts(&mut self, ns: Arc<IrqMutex<UtNamespace>>) {
        self.uts_ns = ns;
    }

    /// Replace the IPC namespace with an existing one (used by `setns(2)`).
    pub fn set_ns_ipc(&mut self, ns: Arc<IrqMutex<IpcNamespace>>) {
        self.ipc_ns = ns;
    }

    /// Replace the mount namespace with an existing one (used by `setns(2)`).
    pub fn set_ns_mnt(&mut self, ns: Arc<IrqMutex<MntNamespace>>) {
        self.mnt_ns = ns;
    }

    /// Stage a PID namespace for the next child (used by `setns(2)`).
    ///
    /// Linux `setns(CLONE_NEWPID)` never moves the calling process into the
    /// target PID namespace.  Instead the next `fork` / `clone` (without
    /// `CLONE_NEWPID`) child enters it and becomes PID 1 there.  This mirrors
    /// `unshare(CLONE_NEWPID)` — both paths replace the persistent
    /// `pid_ns_for_children` selection. The caller must be single-threaded.
    pub fn set_ns_pid(&mut self, ns: PidNamespaceRef) {
        self.pid_ns_for_children = ns;
    }

    /// Replace the network namespace with an existing one (used by `setns(2)`).
    pub fn set_ns_net(&mut self, ns: Arc<IrqMutex<NetNamespace>>) {
        self.net_ns = ns;
    }

    /// Replace the user namespace with an existing one (used by `setns(2)`).
    pub fn set_ns_user(&mut self, ns: Arc<IrqMutex<UserNamespace>>) {
        self.user_ns = ns;
    }

    /// Replace the cgroup namespace with an existing one (used by `setns(2)`).
    pub fn set_ns_cgroup(&mut self, ns: Arc<IrqMutex<CgroupNamespace>>) {
        self.cgroup_ns = ns;
    }

    /// Release the process-owned cgroup namespace after its final thread exits.
    ///
    /// Exited scheduler tasks may retain `ProcessData` after userspace has
    /// reaped the process, so cgroup root ownership cannot rely on `NsProxy`
    /// destruction being synchronous with process exit.
    pub fn release_cgroup_namespace(&mut self) {
        self.cgroup_ns = ROOT_CGROUP_NS.clone();
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    extern crate std;

    use super::*;

    fn init_cgroup() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(ax_cgroup::init);
    }

    #[test]
    fn clone_for_unshare_preserves_cgroup_namespace() {
        init_cgroup();
        let nsproxy = NsProxy::new_root();
        let cloned = nsproxy.clone_for_unshare();

        assert!(Arc::ptr_eq(&nsproxy.cgroup_ns, &cloned.cgroup_ns));
    }

    #[test]
    fn final_process_exit_releases_cgroup_namespace_root() {
        init_cgroup();
        let mut nsproxy = NsProxy::new_root();
        nsproxy.unshare_cgroup(ax_cgroup::root());
        let exiting_namespace = nsproxy.cgroup_ns.clone();

        assert!(!Arc::ptr_eq(&exiting_namespace, &ROOT_CGROUP_NS));
        assert_eq!(Arc::strong_count(&exiting_namespace), 2);
        nsproxy.release_cgroup_namespace();

        assert!(Arc::ptr_eq(&nsproxy.cgroup_ns, &ROOT_CGROUP_NS));
        assert_eq!(Arc::strong_count(&exiting_namespace), 1);
    }
}
