use alloc::{sync::Arc, vec::Vec};
use core::any::Any;

use ax_cgroup::{CgroupFileKind, CgroupFsTarget, CgroupNode};
use axfs_ng_vfs::{
    FileNodeOps, FilesystemOps, Metadata, MetadataRequest, MetadataUpdate, NamespaceCompletion,
    NodeFlags, NodeIdentity, NodeOps, VfsError, VfsResult,
};
use axpoll::{IoEvents, Pollable, SharedRegistrationSink};

use super::{SimpleFs, inode_identity, inode_metadata, update_inode_metadata};
use crate::task::{FsCredentialsSnapshot, current_user_task};

pub(super) struct CgroupFile {
    pub(super) fs: Arc<SimpleFs>,
    pub(super) cgroup: Arc<CgroupNode>,
    pub(super) kind: CgroupFileKind,
}

impl CgroupFile {
    fn read_content(&self) -> VfsResult<Vec<u8>> {
        Ok(match self.kind {
            CgroupFileKind::Controllers => crate::cgroup::controllers_text(&self.cgroup)
                .as_bytes()
                .to_vec(),
            CgroupFileKind::Procs => {
                let current = current_user_task();
                crate::cgroup::procs_text(&current, &self.cgroup).into_bytes()
            }
            CgroupFileKind::SubtreeControl => crate::cgroup::subtree_control_text(&self.cgroup)
                .as_bytes()
                .to_vec(),
            CgroupFileKind::PidsMax => crate::cgroup::pids_max_text(&self.cgroup)?.into_bytes(),
            CgroupFileKind::PidsCurrent => {
                crate::cgroup::pids_current_text(&self.cgroup)?.into_bytes()
            }
            CgroupFileKind::PidsPeak => crate::cgroup::pids_peak_text(&self.cgroup)?.into_bytes(),
            CgroupFileKind::PidsEvents => {
                crate::cgroup::pids_events_text(&self.cgroup)?.into_bytes()
            }
        })
    }
}

impl NodeOps for CgroupFile {
    fn inode(&self) -> u64 {
        self.cgroup.fs_inode(CgroupFsTarget::File(self.kind))
    }

    fn identity(&self) -> Option<NodeIdentity> {
        Some(inode_identity(&self.cgroup, self.inode()))
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        inode_metadata(&self.cgroup, CgroupFsTarget::File(self.kind))
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        update_inode_metadata(&self.cgroup, CgroupFsTarget::File(self.kind), update)
    }

    fn change_metadata(&self, request: &MetadataRequest<'_>) -> NamespaceCompletion<Metadata> {
        super::metadata::change_inode_metadata(
            &self.fs,
            &self.cgroup,
            CgroupFsTarget::File(self.kind),
            request,
        )
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.fs.as_ref()
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn len(&self) -> VfsResult<u64> {
        Ok(0)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

impl FileNodeOps for CgroupFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let content = self.read_content()?;
        let offset = offset as usize;
        if offset >= content.len() {
            return Ok(0);
        }

        let content = &content[offset..];
        let read = content.len().min(buf.len());
        buf[..read].copy_from_slice(&content[..read]);
        Ok(read)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        match self.kind {
            CgroupFileKind::Controllers => {
                return Err(VfsError::PermissionDenied);
            }
            CgroupFileKind::Procs => {
                let current = current_user_task();
                self.cgroup
                    .mutation_domain()
                    .lock()
                    .check_live(&self.cgroup)
                    .map_err(super::cgroup_error_to_vfs_error)?;
                crate::cgroup::write_procs(&current, self.cgroup.clone(), buf)?
            }
            CgroupFileKind::SubtreeControl => {
                let current = current_user_task();
                let caller = FsCredentialsSnapshot::capture(current.as_thread());
                crate::cgroup::write_subtree_control(&self.cgroup, buf, &caller)?
            }
            CgroupFileKind::PidsMax => crate::cgroup::write_pids_max(&self.cgroup, buf)?,
            CgroupFileKind::PidsCurrent | CgroupFileKind::PidsPeak | CgroupFileKind::PidsEvents => {
                return Err(VfsError::OperationNotPermitted);
            }
        }
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        self.write_at(buf, 0).map(|written| (written, 0))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        if len == 0 {
            // Opening a control file with O_TRUNC does not change its contents.
            Ok(())
        } else {
            Err(VfsError::InvalidInput)
        }
    }
}

impl Pollable for CgroupFile {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    /// # Safety
    /// Registration follows `Pollable`'s task/deferred-context contract. These
    /// always-ready files never invoke or retain the supplied sink.
    unsafe fn register_shared(&self, _sink: &mut dyn SharedRegistrationSink, _events: IoEvents) {}
}
