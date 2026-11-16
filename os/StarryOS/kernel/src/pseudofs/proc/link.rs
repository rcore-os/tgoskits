//! Dynamic proc links retain entry identity, never a target or an authorization.

use alloc::{string::String, sync::Arc, vec};

use axfs_ng_vfs::{LinkOps, LinkTarget, Location, NodeIdentity, VfsError, VfsResult};

use super::{SimpleFile, SimpleFs, require_proc_task};
use crate::{
    file::{
        Directory, FD_TABLE, File, FileLike, MountTableFile, NamespaceKind, NamespaceObject,
        descriptor_path, display_location,
    },
    task::{UserTaskRef, WeakUserTaskRef, authorize_proc_read, current_user_task},
};

#[derive(Clone, Copy)]
pub(super) enum ProcLinkKind {
    Fd(u32),
    Exe,
    Cwd,
    Root,
    Namespace(NamespaceKind),
}

/// A SimpleFs instance owns one fixed proc PID view; retaining it prevents
/// instance-address reuse while any corresponding semantic identity is live.
pub(super) fn identity(
    fs: &Arc<SimpleFs>,
    task: &UserTaskRef,
    kind: u64,
    slot: u64,
) -> NodeIdentity {
    NodeIdentity::new(vec![
        Arc::as_ptr(fs) as u64,
        task.as_thread().pid_identity().id().get(),
        task.id().as_u64(),
        kind,
        slot,
    ])
}

pub(super) fn directory_identity(
    fs: &Arc<SimpleFs>,
    task: WeakUserTaskRef,
    kind: u64,
) -> Option<NodeIdentity> {
    let task = task.upgrade().ok().flatten()?;
    Some(identity(fs, &task, kind, 0))
}

pub(super) fn node(fs: Arc<SimpleFs>, task: &UserTaskRef, kind: ProcLinkKind) -> Arc<SimpleFile> {
    let (entry_kind, slot) = match kind {
        ProcLinkKind::Fd(fd) => (10, fd as u64),
        ProcLinkKind::Exe => (11, 0),
        ProcLinkKind::Cwd => (12, 0),
        ProcLinkKind::Root => (13, 0),
        ProcLinkKind::Namespace(kind) => (14, kind.key()),
    };
    let identity = identity(&fs, task, entry_kind, slot);
    SimpleFile::new_link(
        fs,
        identity,
        ProcLink {
            task: task.downgrade(),
            kind,
        },
    )
}

struct ProcLink {
    task: WeakUserTaskRef,
    kind: ProcLinkKind,
}

// A single callback snapshot keeps the referenced object alive after the task,
// fd-table, MM and filesystem-context locks have all been released.
enum TargetSnapshot {
    File(Arc<dyn FileLike>),
    Location(Location),
    Namespace(NamespaceObject),
}

impl ProcLink {
    fn snapshot(&self) -> VfsResult<TargetSnapshot> {
        let target = require_proc_task(&self.task)?;
        authorize_proc_read(&current_user_task(), &target)?;
        if target.state() == ax_std::os::arceos::task::thread::ThreadState::Exited {
            return Err(VfsError::NotFound);
        }
        let thread = target.as_thread();
        Ok(match self.kind {
            ProcLinkKind::Fd(fd) => {
                let table = thread.clone_scope_item(&FD_TABLE);
                let file = table
                    .read()
                    .get(fd as usize)
                    .ok_or(VfsError::NotFound)?
                    .inner
                    .clone();
                TargetSnapshot::File(file)
            }
            ProcLinkKind::Exe => {
                let executable = {
                    let mm = thread
                        .proc_data
                        .pin_aspace()
                        .map_err(|_| VfsError::NotFound)?;
                    let executable = mm.lock().executable_file();
                    executable.ok_or(VfsError::NotFound)?
                };
                TargetSnapshot::Location(executable.location().clone())
            }
            ProcLinkKind::Cwd | ProcLinkKind::Root => {
                let context = thread.snapshot_fs_binding()?.ok_or(VfsError::NotFound)?;
                let location = match self.kind {
                    ProcLinkKind::Cwd => context.fs.current_dir().clone(),
                    _ => context.fs.root_dir().clone(),
                };
                TargetSnapshot::Location(location)
            }
            ProcLinkKind::Namespace(kind) => {
                TargetSnapshot::Namespace(NamespaceObject::for_task(&target, kind)?)
            }
        })
    }
}

impl LinkOps for ProcLink {
    fn target(&self) -> VfsResult<LinkTarget> {
        let location = match self.snapshot()? {
            TargetSnapshot::File(file) => descriptor_path(&file)?.location().clone(),
            TargetSnapshot::Location(location) => location,
            TargetSnapshot::Namespace(namespace) => namespace.location(),
        };
        Ok(LinkTarget::Object(location))
    }

    fn read_link(&self) -> VfsResult<String> {
        match self.snapshot()? {
            TargetSnapshot::File(file) => {
                if let Some(file) = file.downcast_ref::<File>() {
                    display_location(file.inner().location())
                } else if let Some(directory) = file.downcast_ref::<Directory>() {
                    display_location(directory.inner())
                } else if let Some(table) = file.downcast_ref::<MountTableFile>() {
                    display_location(table.inner().inner().location())
                } else {
                    Ok(file.path().into_owned())
                }
            }
            TargetSnapshot::Namespace(namespace) => Ok(namespace.display()),
            TargetSnapshot::Location(location) => display_location(&location),
        }
    }
}
