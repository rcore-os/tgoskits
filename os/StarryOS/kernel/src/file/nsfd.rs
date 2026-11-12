use alloc::{borrow::Cow, sync::Arc};

use ax_fs_ng::MountNamespace as FsMountNamespace;
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::{
    CLONE_NEWCGROUP, CLONE_NEWIPC, CLONE_NEWNET, CLONE_NEWNS, CLONE_NEWPID, CLONE_NEWUSER,
    CLONE_NEWUTS,
};

use super::FileLike;
use crate::{
    StarryResult,
    namespace::{
        CgroupNamespace, IpcNamespace, MntNamespace as ProxyMntNamespace, NetNamespace,
        UserNamespace, UtNamespace,
    },
    sync::RawSpinLock,
};

mod object;
pub(crate) use object::{NamespaceKind, NamespaceObject};

/// A file descriptor that references a specific kernel namespace.
///
/// Created by opening a file under `/proc/<pid>/ns/<type>`.  The fd is
/// passed to `setns(2)` to join the referenced namespace.
pub enum NsFd {
    Uts(Arc<RawSpinLock<UtNamespace>>),
    Ipc(Arc<RawSpinLock<IpcNamespace>>),
    Mnt {
        ns: Arc<RawSpinLock<ProxyMntNamespace>>,
        fs_ns: Arc<FsMountNamespace>,
    },
    Pid(crate::namespace::PidNamespaceRef),
    Net(Arc<RawSpinLock<NetNamespace>>),
    User(Arc<RawSpinLock<UserNamespace>>),
    Cgroup(Arc<RawSpinLock<CgroupNamespace>>),
}

impl NsFd {
    /// Return the `CLONE_NEW*` constant for this namespace.
    pub fn ns_type(&self) -> u32 {
        match self {
            NsFd::Uts(_) => CLONE_NEWUTS,
            NsFd::Ipc(_) => CLONE_NEWIPC,
            NsFd::Mnt { .. } => CLONE_NEWNS,
            NsFd::Pid(_) => CLONE_NEWPID,
            NsFd::Net(_) => CLONE_NEWNET,
            NsFd::User(_) => CLONE_NEWUSER,
            NsFd::Cgroup(_) => CLONE_NEWCGROUP,
        }
    }
}

impl FileLike for NsFd {
    fn validate_write_access(&self) -> StarryResult {
        Err(crate::StarryError::InvalidInput)
    }

    fn path(&self) -> Cow<'_, str> {
        NamespaceObject::from_fd(self).display().into()
    }

    fn stat(&self) -> StarryResult<super::Kstat> {
        let location = NamespaceObject::from_fd(self).location();
        Ok(super::metadata_to_kstat(&location.metadata()?))
    }
}

impl Pollable for NsFd {
    fn poll(&self) -> IoEvents {
        IoEvents::empty()
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}
