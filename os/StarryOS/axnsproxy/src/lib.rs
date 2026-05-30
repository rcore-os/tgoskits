#![no_std]

extern crate alloc;

mod ipc;
mod mnt;
mod net;
mod pid;
mod user;
mod uts;

use alloc::sync::Arc;

use ax_kspin::SpinNoIrq;
pub use ipc::{IpcNamespace, ROOT_IPC_NS};
pub use mnt::{MntNamespace, ROOT_MNT_NS};
pub use net::{NetNamespace, ROOT_NET_NS};
pub use pid::{PidNamespace, ROOT_PID_NS};
pub use user::{ROOT_USER_NS, UserNamespace};
pub use uts::{ROOT_UTS_NS, UtNamespace, build_utsname};

/// Aggregates all namespace types for a process.
///
/// `ProcessData` holds a single `SpinNoIrq<NsProxy>` field.  Clone and unshare
/// operations work through `NsProxy` methods so that syscall handlers do not
/// manipulate namespace internals directly.
pub struct NsProxy {
    /// The UTS namespace (hostname, domainname).
    pub uts_ns: Arc<SpinNoIrq<UtNamespace>>,
    /// The IPC namespace (System V IPC objects).
    pub ipc_ns: Arc<SpinNoIrq<IpcNamespace>>,
    /// The mount namespace (filesystem mount points).
    pub mnt_ns: Arc<SpinNoIrq<MntNamespace>>,
    /// The PID namespace (process ID numbering).
    pub pid_ns: Arc<SpinNoIrq<PidNamespace>>,
    /// Pending PID namespace for the next child created via
    /// `unshare(CLONE_NEWPID)`.  Linux does not move the calling
    /// process into a new PID namespace; instead the next fork/clone
    /// child becomes the first process (PID 1) in the new namespace.
    pub child_pid_ns: Option<Arc<SpinNoIrq<PidNamespace>>>,
    /// The network namespace (interfaces, routing, sockets).
    pub net_ns: Arc<SpinNoIrq<NetNamespace>>,
    /// The user namespace (UID/GID mappings).
    pub user_ns: Arc<SpinNoIrq<UserNamespace>>,
}

impl NsProxy {
    /// Create a new [`NsProxy`] pointing to the root namespaces.
    pub fn new_root() -> Self {
        Self {
            uts_ns: ROOT_UTS_NS.clone(),
            ipc_ns: ROOT_IPC_NS.clone(),
            mnt_ns: ROOT_MNT_NS.clone(),
            pid_ns: ROOT_PID_NS.clone(),
            child_pid_ns: None,
            net_ns: ROOT_NET_NS.clone(),
            user_ns: ROOT_USER_NS.clone(),
        }
    }

    /// Clone all namespace references (shallow `Arc` clone).
    ///
    /// Used by `fork` / `clone` (without `CLONE_NEW*` flags) so the child
    /// shares the same namespaces as the parent.  `child_pid_ns` is never
    /// inherited — it is consumed by the first child that uses it.
    pub fn clone_all(&self) -> Self {
        Self {
            uts_ns: self.uts_ns.clone(),
            ipc_ns: self.ipc_ns.clone(),
            mnt_ns: self.mnt_ns.clone(),
            pid_ns: self.pid_ns.clone(),
            child_pid_ns: None,
            net_ns: self.net_ns.clone(),
            user_ns: self.user_ns.clone(),
        }
    }

    pub fn unshare_uts(&mut self) {
        let new_inner = self.uts_ns.lock().clone_ns();
        self.uts_ns = Arc::new(SpinNoIrq::new(new_inner));
    }

    pub fn unshare_ipc(&mut self) {
        let new_inner = self.ipc_ns.lock().clone_ns();
        self.ipc_ns = Arc::new(SpinNoIrq::new(new_inner));
    }

    pub fn unshare_mnt(&mut self) {
        let new_inner = self.mnt_ns.lock().clone_ns();
        self.mnt_ns = Arc::new(SpinNoIrq::new(new_inner));
    }

    /// Directly replace the PID namespace — used in `clone(CLONE_NEWPID)`.
    pub fn unshare_pid(&mut self) {
        let new_inner = self.pid_ns.lock().clone_ns();
        self.pid_ns = Arc::new(SpinNoIrq::new(new_inner));
    }

    /// Prepare a new PID namespace for the next child of this process.
    ///
    /// Called by `unshare(CLONE_NEWPID)`.  The calling process stays in
    /// its current PID namespace; the new namespace is consumed by the
    /// next `fork` / `clone` child, which becomes PID 1 in that namespace.
    pub fn prepare_child_pid_ns(&mut self) {
        let new_inner = self.pid_ns.lock().clone_ns();
        self.child_pid_ns = Some(Arc::new(SpinNoIrq::new(new_inner)));
    }

    pub fn unshare_net(&mut self) {
        let new_inner = self.net_ns.lock().clone_ns();
        self.net_ns = Arc::new(SpinNoIrq::new(new_inner));
    }

    pub fn unshare_user(&mut self) {
        let new_inner = self.user_ns.lock().clone_ns();
        self.user_ns = Arc::new(SpinNoIrq::new(new_inner));
    }

    /// Replace the UTS namespace with an existing one (used by `setns(2)`).
    pub fn set_ns_uts(&mut self, ns: Arc<SpinNoIrq<UtNamespace>>) {
        self.uts_ns = ns;
    }

    /// Replace the IPC namespace with an existing one (used by `setns(2)`).
    pub fn set_ns_ipc(&mut self, ns: Arc<SpinNoIrq<IpcNamespace>>) {
        self.ipc_ns = ns;
    }

    /// Replace the mount namespace with an existing one (used by `setns(2)`).
    pub fn set_ns_mnt(&mut self, ns: Arc<SpinNoIrq<MntNamespace>>) {
        self.mnt_ns = ns;
    }

    /// Replace the PID namespace with an existing one (used by `setns(2)`).
    ///
    /// Note: `setns(CLONE_NEWPID)` does not change the calling process's
    /// PID; it only affects the PID namespace for child processes created
    /// afterwards.  The caller must be single-threaded.
    pub fn set_ns_pid(&mut self, ns: Arc<SpinNoIrq<PidNamespace>>) {
        self.pid_ns = ns;
    }

    /// Replace the network namespace with an existing one (used by `setns(2)`).
    pub fn set_ns_net(&mut self, ns: Arc<SpinNoIrq<NetNamespace>>) {
        self.net_ns = ns;
    }

    /// Replace the user namespace with an existing one (used by `setns(2)`).
    pub fn set_ns_user(&mut self, ns: Arc<SpinNoIrq<UserNamespace>>) {
        self.user_ns = ns;
    }
}
