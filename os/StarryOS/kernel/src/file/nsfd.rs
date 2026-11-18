use alloc::{borrow::Cow, format, sync::Arc};

use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::{
    CLONE_NEWCGROUP, CLONE_NEWIPC, CLONE_NEWNET, CLONE_NEWNS, CLONE_NEWPID, CLONE_NEWUSER,
    CLONE_NEWUTS,
};

use super::FileLike;
use crate::StarryResult;

mod object;
pub(crate) use object::{NamespaceKind, NamespaceObject};

/// A file descriptor that references a specific kernel namespace.
///
/// Created by opening a file under `/proc/<pid>/ns/<type>`.  The fd is
/// passed to `setns(2)` to join the referenced namespace.
pub struct NsFd {
    object: NamespaceObject,
    inode: Arc<crate::pseudofs::object::ObjectInode>,
}

impl NsFd {
    pub(crate) fn object(&self) -> &NamespaceObject {
        &self.object
    }

    pub(crate) fn location(&self) -> axfs_ng_vfs::Location {
        self.inode.location(self.object.clone())
    }

    /// Return the `CLONE_NEW*` constant for this namespace.
    pub fn ns_type(&self) -> u32 {
        match &self.object {
            NamespaceObject::Uts(_) => CLONE_NEWUTS,
            NamespaceObject::Ipc(_) => CLONE_NEWIPC,
            NamespaceObject::Mnt(_) => CLONE_NEWNS,
            NamespaceObject::Pid(_) => CLONE_NEWPID,
            NamespaceObject::Net(_) => CLONE_NEWNET,
            NamespaceObject::User(_) => CLONE_NEWUSER,
            NamespaceObject::Cgroup(_) => CLONE_NEWCGROUP,
        }
    }
}

impl FileLike for NsFd {
    fn validate_write_access(&self) -> StarryResult {
        Err(crate::StarryError::InvalidInput)
    }

    fn path(&self) -> Cow<'_, str> {
        format!("{}:[{}]", self.object.kind().name(), self.inode.inode()).into()
    }

    fn stat(&self) -> StarryResult<super::Kstat> {
        let location = self.location();
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
