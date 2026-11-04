use alloc::{borrow::Cow, string::String, sync::Arc};

use ax_fs_ng::{MountNamespace as FsMountNamespace, vfs::FS_CONTEXT};
use axfs_ng_vfs::Location;
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::{
    CLONE_NEWCGROUP, CLONE_NEWIPC, CLONE_NEWNET, CLONE_NEWNS, CLONE_NEWPID, CLONE_NEWUSER,
    CLONE_NEWUTS,
};

use super::FileLike;
use crate::{
    StarryError, StarryResult,
    namespace::{
        CgroupNamespace, IpcNamespace, MntNamespace as ProxyMntNamespace, NetNamespace,
        UserNamespace, UtNamespace,
    },
    pseudofs::{SimpleDir, proc::NsDir},
    sync::RawSpinLock,
    task::UserTaskRef,
};

/// Whether `name` is one of the `/proc/<pid>/ns/<type>` entry names.
pub(crate) fn is_ns_type(name: &str) -> bool {
    matches!(
        name,
        "uts" | "ipc" | "mnt" | "pid" | "net" | "user" | "cgroup"
    )
}

/// The owning task and namespace type of a reached `/proc/<pid>/ns/<type>`
/// magic link, if `loc` is one.
///
/// The owning task comes from the directory entry that was actually reached,
/// so a bind mount that redirects a `/proc/<pid>/ns` directory yields the
/// namespaces of the process that directory belongs to instead of re-parsing
/// the path text.
pub(crate) fn ns_target_from_link(loc: &Location) -> Option<(UserTaskRef, String)> {
    let parent = loc.parent()?;
    let dir = parent.entry().as_dir().ok()?;
    let ns_dir = dir.downcast::<SimpleDir<NsDir>>().ok()?;
    let task = ns_dir.ops().ns_task()?;
    let ns_type = loc.name().into_owned();
    is_ns_type(&ns_type).then_some((task, ns_type))
}

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

    /// Builds the namespace descriptor for one `/proc/<pid>/ns/<type>` entry
    /// of `task`, resolving the mount namespace from the target's own
    /// filesystem context.
    pub(crate) fn from_task(task: &UserTaskRef, ns_type: &str) -> StarryResult<Self> {
        let mnt_fs_ns = if ns_type == "mnt" {
            let Some(fs_context) = task.as_thread().clone_scope_item(&FS_CONTEXT) else {
                return Err(StarryError::NotFound);
            };
            Some(fs_context.lock().mount_namespace().clone())
        } else {
            None
        };
        Self::from_task_with_mount_ns(task, ns_type, mnt_fs_ns)
    }

    /// Builds the namespace descriptor with an already-resolved mount
    /// namespace.
    ///
    /// Path lookups that follow an ns magic link already hold the caller's
    /// filesystem context lock, so they must not re-enter
    /// [`FS_CONTEXT`] for the same process; the mount namespace they supply
    /// is only used by `setns` consumers, which never receive this
    /// stat-path object.
    pub(crate) fn from_task_with_mount_ns(
        task: &UserTaskRef,
        ns_type: &str,
        mnt_fs_ns: Option<Arc<FsMountNamespace>>,
    ) -> StarryResult<Self> {
        let proc_data = &task.as_thread().proc_data;

        let nsproxy = proc_data.namespace_snapshot();
        let nsfd = match ns_type {
            "uts" => NsFd::Uts(nsproxy.uts_ns.clone()),
            "ipc" => NsFd::Ipc(nsproxy.ipc_ns.clone()),
            "mnt" => NsFd::Mnt {
                ns: nsproxy.mnt_ns.clone(),
                fs_ns: mnt_fs_ns.unwrap(),
            },
            "pid" => NsFd::Pid(proc_data.identity().active_namespace()),
            "net" => NsFd::Net(nsproxy.net_ns.clone()),
            "user" => NsFd::User(nsproxy.user_ns.clone()),
            "cgroup" => NsFd::Cgroup(nsproxy.cgroup_ns.clone()),
            _ => return Err(StarryError::NotFound),
        };

        drop(nsproxy);
        Ok(nsfd)
    }
}

impl FileLike for NsFd {
    fn validate_write_access(&self) -> StarryResult {
        Err(crate::StarryError::InvalidInput)
    }

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

    fn stat(&self) -> StarryResult<super::Kstat> {
        let ino = match self {
            NsFd::Uts(ns) => ns.lock_irqsave().id,
            NsFd::Ipc(ns) => ns.lock_irqsave().ns_id,
            NsFd::Mnt { ns, .. } => ns.lock_irqsave().id(),
            NsFd::Pid(ns) => ns.id().get(),
            NsFd::Net(ns) => ns.lock_irqsave().ns_id,
            NsFd::User(ns) => ns.lock_irqsave().id,
            NsFd::Cgroup(ns) => ns.lock_irqsave().id(),
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

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}
