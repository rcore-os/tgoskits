//! Native namespace references shared by ns descriptors and VFS object nodes.

use alloc::{
    collections::BTreeMap,
    format,
    string::String,
    sync::{Arc, Weak},
};

use ax_fs_ng::vfs::FS_CONTEXT;
use axfs_ng_vfs::Location;

use super::NsFd;
use crate::{
    StarryError, StarryResult,
    pseudofs::object::{ObjectInode, ObjectNode},
    sync::Mutex,
    task::UserTaskRef,
};

#[derive(Clone, Copy)]
pub(crate) enum NamespaceKind {
    Uts,
    Ipc,
    Mnt,
    Pid,
    Net,
    User,
    Cgroup,
}

impl NamespaceKind {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "uts" => Self::Uts,
            "ipc" => Self::Ipc,
            "mnt" => Self::Mnt,
            "pid" => Self::Pid,
            "net" => Self::Net,
            "user" => Self::User,
            "cgroup" => Self::Cgroup,
            _ => return None,
        })
    }

    pub(crate) fn key(self) -> u64 {
        self as u64
    }

    fn name(self) -> &'static str {
        match self {
            Self::Uts => "uts",
            Self::Ipc => "ipc",
            Self::Mnt => "mnt",
            Self::Pid => "pid",
            Self::Net => "net",
            Self::User => "user",
            Self::Cgroup => "cgroup",
        }
    }
}

// This owns namespace references only, not a file description or copied state.
#[derive(Clone)]
pub(crate) enum NamespaceObject {
    Uts(Arc<crate::sync::RawSpinLock<crate::namespace::UtNamespace>>),
    Ipc(Arc<crate::sync::RawSpinLock<crate::namespace::IpcNamespace>>),
    Mnt {
        ns: Arc<crate::sync::RawSpinLock<crate::namespace::MntNamespace>>,
        fs_ns: Arc<ax_fs_ng::MountNamespace>,
    },
    Pid(crate::task::PidNamespaceRef),
    Net(Arc<crate::sync::RawSpinLock<crate::namespace::NetNamespace>>),
    User(Arc<crate::sync::RawSpinLock<crate::namespace::UserNamespace>>),
    Cgroup(Arc<crate::sync::RawSpinLock<crate::namespace::CgroupNamespace>>),
}

// Only weak vnode identities are indexed; namespace state stays with its owner.
static INODES: Mutex<BTreeMap<(u64, u64), Weak<ObjectInode>>> = Mutex::new(BTreeMap::new());

impl NamespaceObject {
    pub(crate) fn for_task(task: &UserTaskRef, kind: NamespaceKind) -> StarryResult<Self> {
        let thread = task.as_thread();
        let update = thread.proc_data.namespace_update();
        let proxy = update.snapshot();
        Ok(match kind {
            NamespaceKind::Uts => Self::Uts(proxy.uts_ns.clone()),
            NamespaceKind::Ipc => Self::Ipc(proxy.ipc_ns.clone()),
            NamespaceKind::Mnt => {
                let context = thread
                    .clone_scope_item(&FS_CONTEXT)
                    .ok_or(StarryError::NotFound)?;
                let fs_ns = context.lock().mount_namespace().clone();
                Self::Mnt {
                    ns: proxy.mnt_ns.clone(),
                    fs_ns,
                }
            }
            NamespaceKind::Pid => Self::Pid(thread.proc_data.identity().active_namespace()),
            NamespaceKind::Net => Self::Net(proxy.net_ns.clone()),
            NamespaceKind::User => Self::User(proxy.user_ns.clone()),
            NamespaceKind::Cgroup => Self::Cgroup(proxy.cgroup_ns.clone()),
        })
    }

    pub(crate) fn from_fd(fd: &NsFd) -> Self {
        match fd {
            NsFd::Uts(ns) => Self::Uts(ns.clone()),
            NsFd::Ipc(ns) => Self::Ipc(ns.clone()),
            NsFd::Mnt { ns, fs_ns } => Self::Mnt {
                ns: ns.clone(),
                fs_ns: fs_ns.clone(),
            },
            NsFd::Pid(ns) => Self::Pid(ns.clone()),
            NsFd::Net(ns) => Self::Net(ns.clone()),
            NsFd::User(ns) => Self::User(ns.clone()),
            NsFd::Cgroup(ns) => Self::Cgroup(ns.clone()),
        }
    }

    pub(crate) fn into_fd(self) -> NsFd {
        match self {
            Self::Uts(ns) => NsFd::Uts(ns),
            Self::Ipc(ns) => NsFd::Ipc(ns),
            Self::Mnt { ns, fs_ns } => NsFd::Mnt { ns, fs_ns },
            Self::Pid(ns) => NsFd::Pid(ns),
            Self::Net(ns) => NsFd::Net(ns),
            Self::User(ns) => NsFd::User(ns),
            Self::Cgroup(ns) => NsFd::Cgroup(ns),
        }
    }

    fn kind(&self) -> NamespaceKind {
        match self {
            Self::Uts(_) => NamespaceKind::Uts,
            Self::Ipc(_) => NamespaceKind::Ipc,
            Self::Mnt { .. } => NamespaceKind::Mnt,
            Self::Pid(_) => NamespaceKind::Pid,
            Self::Net(_) => NamespaceKind::Net,
            Self::User(_) => NamespaceKind::User,
            Self::Cgroup(_) => NamespaceKind::Cgroup,
        }
    }

    pub(crate) fn inode(&self) -> u64 {
        match self {
            Self::Uts(ns) => ns.lock_irqsave().id,
            Self::Ipc(ns) => ns.lock_irqsave().ns_id,
            Self::Mnt { ns, .. } => ns.lock_irqsave().id(),
            Self::Pid(ns) => ns.id().get(),
            Self::Net(ns) => ns.lock_irqsave().ns_id,
            Self::User(ns) => ns.lock_irqsave().id,
            Self::Cgroup(ns) => ns.lock_irqsave().id(),
        }
    }

    pub(crate) fn display(&self) -> String {
        format!("{}:[{}]", self.kind().name(), self.inode())
    }

    pub(crate) fn location(self) -> Location {
        let number = self.inode();
        let key = (self.kind().key(), number);
        let candidate = ObjectInode::namespace(self.kind().key(), number);
        let inode = {
            let mut inodes = INODES.lock();
            inodes.retain(|_, inode| inode.strong_count() != 0);
            if let Some(inode) = inodes.get(&key).and_then(Weak::upgrade) {
                inode
            } else {
                inodes.insert(key, Arc::downgrade(&candidate));
                candidate.clone()
            }
        };
        inode.location(self)
    }

    pub(crate) fn at_location(location: &Location) -> Option<Self> {
        let node = location.entry().downcast::<ObjectNode<Self>>().ok()?;
        Some(node.object.clone())
    }
}
