use alloc::{borrow::Cow, sync::Arc};
use core::task::Context;

use ax_errno::AxResult;
use ax_fs_ng::MountNamespace as FsMountNamespace;
use ax_kspin::SpinNoIrq;
use axnsproxy::{
    CgroupNamespace, IpcNamespace, MntNamespace as ProxyMntNamespace, NetNamespace, PidNamespace,
    UserNamespace, UtNamespace,
};
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::{
    CLONE_NEWCGROUP, CLONE_NEWIPC, CLONE_NEWNET, CLONE_NEWNS, CLONE_NEWPID, CLONE_NEWUSER,
    CLONE_NEWUTS,
};

use super::FileLike;

/// A file descriptor that references a specific kernel namespace.
///
/// Created by opening a file under `/proc/<pid>/ns/<type>`.  The fd is
/// passed to `setns(2)` to join the referenced namespace.
pub enum NsFd {
    Uts(Arc<SpinNoIrq<UtNamespace>>),
    Ipc(Arc<SpinNoIrq<IpcNamespace>>),
    Mnt {
        ns: Arc<SpinNoIrq<ProxyMntNamespace>>,
        fs_ns: Arc<FsMountNamespace>,
    },
    Pid(Arc<SpinNoIrq<PidNamespace>>),
    Net(Arc<SpinNoIrq<NetNamespace>>),
    User(Arc<SpinNoIrq<UserNamespace>>),
    Cgroup(Arc<SpinNoIrq<CgroupNamespace>>),
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
    fn path(&self) -> Cow<'_, str> {
        match self {
            NsFd::Uts(_) => "anon_inode:[uts_ns]".into(),
            NsFd::Ipc(_) => "anon_inode:[ipc_ns]".into(),
            NsFd::Mnt { .. } => "anon_inode:[mnt_ns]".into(),
            NsFd::Pid(_) => "anon_inode:[pid_ns]".into(),
            NsFd::Net(_) => "anon_inode:[net_ns]".into(),
            NsFd::User(_) => "anon_inode:[user_ns]".into(),
            NsFd::Cgroup(_) => "anon_inode:[cgroup_ns]".into(),
        }
    }

    fn stat(&self) -> AxResult<super::Kstat> {
        let ino = match self {
            NsFd::Uts(ns) => ns.lock().id,
            NsFd::Ipc(ns) => ns.lock().ns_id,
            NsFd::Mnt { ns, .. } => ns.lock().id(),
            NsFd::Pid(ns) => ns.lock().id,
            NsFd::Net(ns) => ns.lock().ns_id,
            NsFd::User(ns) => ns.lock().id,
            NsFd::Cgroup(ns) => ns.lock().id(),
        };
        Ok(super::Kstat {
            ino,
            mode: 0o100_444, // S_IFREG | 0444
            nlink: 1,
            blksize: 4096,
            ..super::Kstat::default()
        })
    }
}

impl Pollable for NsFd {
    fn poll(&self) -> IoEvents {
        IoEvents::empty()
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}
