//! Native namespace references shared by ns descriptors and VFS object nodes.

use alloc::{
    collections::BTreeMap,
    format,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
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

/// Weak references to the actual namespace allocations, never temporary wrappers.
enum NamespaceOwner {
    Uts(Weak<crate::sync::RawSpinLock<crate::namespace::UtNamespace>>),
    Ipc(Weak<crate::sync::RawSpinLock<crate::namespace::IpcNamespace>>),
    Mnt(Weak<ax_fs_ng::MountNamespace>),
    Pid(Weak<crate::task::PidNamespace>),
    Net(Weak<crate::sync::RawSpinLock<crate::namespace::NetNamespace>>),
    User(Weak<crate::sync::RawSpinLock<crate::namespace::UserNamespace>>),
    Cgroup(Weak<crate::sync::RawSpinLock<crate::namespace::CgroupNamespace>>),
}

impl NamespaceOwner {
    fn key(&self) -> (u64, usize) {
        match self {
            Self::Uts(owner) => (NamespaceKind::Uts.key(), owner.as_ptr() as usize),
            Self::Ipc(owner) => (NamespaceKind::Ipc.key(), owner.as_ptr() as usize),
            Self::Mnt(owner) => (NamespaceKind::Mnt.key(), owner.as_ptr() as usize),
            Self::Pid(owner) => (NamespaceKind::Pid.key(), owner.as_ptr() as usize),
            Self::Net(owner) => (NamespaceKind::Net.key(), owner.as_ptr() as usize),
            Self::User(owner) => (NamespaceKind::User.key(), owner.as_ptr() as usize),
            Self::Cgroup(owner) => (NamespaceKind::Cgroup.key(), owner.as_ptr() as usize),
        }
    }

    fn is_alive(&self) -> bool {
        // Do not temporarily upgrade: releasing the last namespace reference
        // under the index mutex could run its destructor inside that lock.
        match self {
            Self::Uts(owner) => owner.strong_count() != 0,
            Self::Ipc(owner) => owner.strong_count() != 0,
            Self::Mnt(owner) => owner.strong_count() != 0,
            Self::Pid(owner) => owner.strong_count() != 0,
            Self::Net(owner) => owner.strong_count() != 0,
            Self::User(owner) => owner.strong_count() != 0,
            Self::Cgroup(owner) => owner.strong_count() != 0,
        }
    }
}

struct NamespaceInode {
    // A weak allocation reference also prevents address reuse while indexed.
    owner: NamespaceOwner,
    inode: Arc<ObjectInode>,
}

type InodeRecords = BTreeMap<(u64, usize), NamespaceInode>;

/// The sole namespace inode index. Inodes do not retain namespace owners.
struct NamespaceInodes(Mutex<InodeRecords>);

static INODES: NamespaceInodes = NamespaceInodes(Mutex::new(BTreeMap::new()));

impl NamespaceInodes {
    fn get(&self, namespace: &NamespaceObject) -> Arc<ObjectInode> {
        let owner = namespace.downgrade();
        if let Some(inode) = self.find(&owner) {
            return inode;
        }
        // Allocation, including the nsfs slab lock, is outside the index lock.
        self.publish(owner, ObjectInode::namespace())
    }

    fn find(&self, owner: &NamespaceOwner) -> Option<Arc<ObjectInode>> {
        let mut retired = Vec::new();
        let inode = {
            let mut records = self.0.lock();
            Self::retire_dead(&mut records, &mut retired);
            records.get(&owner.key()).map(|record| record.inode.clone())
        };
        drop(retired);
        inode
    }

    fn publish(&self, owner: NamespaceOwner, candidate: Arc<ObjectInode>) -> Arc<ObjectInode> {
        let mut retired = Vec::new();
        let inode = {
            let mut records = self.0.lock();
            Self::retire_dead(&mut records, &mut retired);
            records
                .entry(owner.key())
                .or_insert_with(|| NamespaceInode {
                    owner,
                    inode: candidate.clone(),
                })
                .inode
                .clone()
        };
        drop(retired);
        // A losing candidate releases its slab slot after the mutex guard.
        drop(candidate);
        inode
    }

    fn retire_dead(records: &mut InodeRecords, retired: &mut Vec<NamespaceInode>) {
        let dead: Vec<_> = records
            .iter()
            .filter_map(|(key, record)| (!record.owner.is_alive()).then_some(*key))
            .collect();
        // Reserve before moving any owning record. The caller's retired vector
        // outlives its mutex guard, including during unwinding.
        retired.reserve(dead.len());
        for key in dead {
            if let Some(record) = records.remove(&key) {
                retired.push(record);
            }
        }
    }
}

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

    fn downgrade(&self) -> NamespaceOwner {
        match self {
            Self::Uts(ns) => NamespaceOwner::Uts(Arc::downgrade(ns)),
            Self::Ipc(ns) => NamespaceOwner::Ipc(Arc::downgrade(ns)),
            // The filesystem namespace owns the actual mount tree. The proxy
            // namespace reference is retained separately for setns compatibility.
            Self::Mnt { fs_ns, .. } => NamespaceOwner::Mnt(Arc::downgrade(fs_ns)),
            Self::Pid(ns) => NamespaceOwner::Pid(Arc::downgrade(ns)),
            Self::Net(ns) => NamespaceOwner::Net(Arc::downgrade(ns)),
            Self::User(ns) => NamespaceOwner::User(Arc::downgrade(ns)),
            Self::Cgroup(ns) => NamespaceOwner::Cgroup(Arc::downgrade(ns)),
        }
    }

    pub(crate) fn inode(&self) -> u64 {
        INODES.get(self).inode()
    }

    pub(crate) fn display(&self) -> String {
        format!("{}:[{}]", self.kind().name(), self.inode())
    }

    pub(crate) fn location(self) -> Location {
        let inode = INODES.get(&self);
        inode.location(self)
    }

    pub(crate) fn at_location(location: &Location) -> Option<Self> {
        let node = location.entry().downcast::<ObjectNode<Self>>().ok()?;
        Some(node.object.clone())
    }
}

#[cfg(all(test, axtest))]
mod tests {
    use alloc::{format, sync::Arc};

    use super::{INODES, NamespaceObject, ObjectInode, ObjectNode};
    use crate::{
        file::FileLike,
        namespace::{IpcNamespace, NetNamespace},
        sync::RawSpinLock,
    };

    fn observe_namespace(namespace: &NamespaceObject) -> (u64, u64) {
        let fd = namespace.clone().into_fd();
        let fd_stat = fd.stat().unwrap();
        let location = NamespaceObject::from_fd(&fd).location();
        let vnode_stat = location.metadata().unwrap();
        let identity = (vnode_stat.device, vnode_stat.inode);
        assert_eq!((fd_stat.dev, fd_stat.ino), identity);
        assert_eq!(namespace.inode(), vnode_stat.inode);
        assert_eq!(
            namespace.display(),
            format!("{}:[{}]", namespace.kind().name(), vnode_stat.inode)
        );
        let root = location.mountpoint().root_location().metadata().unwrap();
        assert_ne!(identity, (root.device, root.inode));
        identity
    }

    #[test]
    fn namespace_views_share_unique_allocated_inodes() {
        // Independent native ID domains may legally contain the same number.
        // Using those IDs as nsfs inode numbers makes this real IPC/Net pair,
        // and the nsfs root inode, collide in the old implementation.
        let ipc = NamespaceObject::Ipc(Arc::new(RawSpinLock::new(IpcNamespace { ns_id: 1 })));
        let net = NamespaceObject::Net(Arc::new(RawSpinLock::new(NetNamespace { ns_id: 1 })));
        let other_ipc = NamespaceObject::Ipc(Arc::new(RawSpinLock::new(IpcNamespace { ns_id: 2 })));
        let ipc_identity = observe_namespace(&ipc);
        let net_identity = observe_namespace(&net);
        let other_identity = observe_namespace(&other_ipc);
        assert_ne!(ipc_identity, net_identity);
        assert_ne!(ipc_identity, other_identity);
        assert_ne!(net_identity, other_identity);
        assert_eq!(observe_namespace(&ipc), ipc_identity);
    }

    #[test]
    fn namespace_index_canonicalizes_races_and_retires_dead_owners() {
        let native = Arc::new(RawSpinLock::new(IpcNamespace { ns_id: 1 }));
        let weak_native = Arc::downgrade(&native);
        let namespace = NamespaceObject::Ipc(native.clone());
        let owner = namespace.downgrade();

        // Pause one observer at the real allocation/publication boundary,
        // then let another observer publish before the first one resumes.
        assert!(INODES.find(&owner).is_none());
        let candidate = ObjectInode::namespace();
        let weak_candidate = Arc::downgrade(&candidate);
        let vnode = namespace.clone().location();
        let winner = INODES.publish(owner, candidate);
        assert!(
            Arc::ptr_eq(
                &winner,
                &vnode
                    .entry()
                    .downcast::<ObjectNode<NamespaceObject>>()
                    .unwrap()
                    .inode
            ),
            "a late candidate must reuse the already published inode"
        );
        assert!(weak_candidate.upgrade().is_none());
        let number = winner.inode();
        let weak_inode = Arc::downgrade(&winner);
        drop(winner);
        let fd = namespace.clone().into_fd();
        assert_eq!(fd.stat().unwrap().ino, number);
        drop(fd);
        drop(vnode);
        drop(namespace);

        // No temporary NamespaceObject, fd or vnode remains. The actual
        // namespace allocation alone must keep the indexed inode stable.
        assert!(weak_inode.upgrade().is_some());
        assert_eq!(NamespaceObject::Ipc(native.clone()).inode(), number);
        let vnode = NamespaceObject::Ipc(native.clone()).location();
        let fd = NamespaceObject::Ipc(native.clone()).into_fd();
        drop(native);

        let trigger = NamespaceObject::Net(Arc::new(RawSpinLock::new(NetNamespace { ns_id: 1 })));
        trigger.inode();
        assert!(weak_native.upgrade().is_some());
        let peer = NamespaceObject::Ipc(Arc::new(RawSpinLock::new(IpcNamespace { ns_id: 2 })));
        let peer_inode = INODES.get(&peer);
        let weak_peer_inode = Arc::downgrade(&peer_inode);
        drop(peer_inode);
        drop(vnode);
        assert_eq!(fd.stat().unwrap().ino, number);
        drop(fd);
        drop(peer);
        assert!(
            weak_native.upgrade().is_none(),
            "the index must not own the namespace"
        );
        trigger.inode();
        assert!(
            weak_inode.upgrade().is_none(),
            "dead records and weak entry caches must not leak inodes"
        );
        assert!(
            weak_peer_inode.upgrade().is_none(),
            "one access must retire all dead owners"
        );
    }
}
