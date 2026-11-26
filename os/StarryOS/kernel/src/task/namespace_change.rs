//! Prepared task bindings publish fixed references and retire outside writers.

use alloc::sync::Arc;

use ax_fs_ng::{
    FS_CONTEXT, FsContextOwner, FsContextSnapshot, FsPivotRetirement, FsSharingRequirement,
    FsUpdateRetirement, PreparedFsUpdate,
};
use axfs_ng_vfs::{MountCloneRetirement, MountRootRetirement, NamespaceClone, PathHandle};
use linux_raw_sys::general::{CAP_SYS_ADMIN, CAP_SYS_CHROOT};
use scope_local::Scope;

use super::{
    Cred, FsBindingSnapshot, PreparedNamespacePublication, ProcessNamespaceUpdate, Thread,
};
use crate::{
    StarryError, StarryResult,
    file::{FD_TABLE, FileTableScope},
    namespace::{MNT_NAMESPACE, MntNamespace, NsProxy, UserNamespace, has_namespace_capability},
    sync::RawSpinLock,
};

enum FsBindingChange {
    Independent {
        owner: FsContextOwner,
        mnt: Arc<MntNamespace>,
    },
    Update {
        command: Option<PreparedFsUpdate>,
        mnt: Option<Arc<MntNamespace>>,
    },
}

struct PreparedFsBinding {
    source: FsBindingSnapshot,
    sharing: FsSharingRequirement,
    change: Option<FsBindingChange>,
}

/// Field order releases the sleepable writer before any possibly final owner,
/// including preparation errors and a caller abandoning a prepared change.
pub(crate) struct NamespaceChange<'task> {
    writer: Option<ProcessNamespaceUpdate<'task>>,
    thread: &'task Thread,
    proxy: Arc<NsProxy>,
    caller: Arc<Cred>,
    binding: Option<PreparedFsBinding>,
    replacement_proxy: Option<NsProxy>,
    publication: Option<PreparedNamespacePublication>,
    retired_proxy: Option<Arc<NsProxy>>,
    files: Option<FileTableScope>,
    retired_files: Option<FileTableScope>,
    cloned: Option<NamespaceClone>,
    mapped: Option<FsContextSnapshot>,
    clone_retirement: Option<MountCloneRetirement>,
    target_mnt: Option<Arc<MntNamespace>>,
    root_retirement: Option<MountRootRetirement>,
    path_retirement: FsUpdateRetirement,
    update_retirement: FsUpdateRetirement,
    pivot_paths: Option<(PathHandle, PathHandle)>,
    pivot_source: Option<FsBindingSnapshot>,
    pivot_retirement: FsPivotRetirement,
}

/// Already counted child ownership. Installation happens before child startup.
pub(crate) struct ChildFsBinding {
    owner: FsContextOwner,
    mnt: Arc<MntNamespace>,
}

impl ChildFsBinding {
    pub(crate) fn install(self, scope: &mut Scope) {
        let old_owner = FS_CONTEXT.scope_mut(scope).replace(self.owner);
        let old_mnt = MNT_NAMESPACE.scope_mut(scope).replace(self.mnt);
        // An unpublished Scope is ordinary task context with no outer writer.
        drop((old_owner, old_mnt));
    }
}

impl<'task> NamespaceChange<'task> {
    pub(crate) fn new(thread: &'task Thread) -> StarryResult<Self> {
        let writer = thread.proc_data.namespace_update();
        let proxy = writer.snapshot();
        let caller = thread.cred();
        let mut change = Self {
            writer: Some(writer),
            thread,
            proxy,
            caller,
            binding: None,
            replacement_proxy: None,
            publication: None,
            retired_proxy: None,
            files: None,
            retired_files: None,
            cloned: None,
            mapped: None,
            clone_retirement: None,
            target_mnt: None,
            root_retirement: None,
            path_retirement: Default::default(),
            update_retirement: Default::default(),
            pivot_paths: None,
            pivot_source: None,
            pivot_retirement: Default::default(),
        };
        let source = thread
            .snapshot_fs_binding_under(change.writer.as_ref().expect("writer"))?
            .ok_or(StarryError::BadState)?;
        change.binding = Some(PreparedFsBinding {
            source,
            sharing: FsSharingRequirement::AnyOwner,
            change: None,
        });
        Ok(change)
    }

    pub(crate) fn source(&self) -> &FsBindingSnapshot {
        &self.binding.as_ref().expect("selected binding").source
    }

    pub(crate) fn original_proxy(&self) -> &NsProxy {
        &self.proxy
    }

    pub(crate) fn caller(&self) -> &Cred {
        &self.caller
    }

    pub(crate) fn caller_credentials(&self) -> &Arc<Cred> {
        &self.caller
    }

    pub(crate) fn selected_user_namespace(&self) -> &Arc<RawSpinLock<UserNamespace>> {
        &self
            .replacement_proxy
            .as_ref()
            .unwrap_or(&self.proxy)
            .user_ns
    }

    pub(crate) fn proxy_mut(&mut self) -> &mut NsProxy {
        if self.replacement_proxy.is_none() {
            self.replacement_proxy = Some(self.proxy.clone_for_unshare());
        }
        self.replacement_proxy.as_mut().expect("prepared proxy")
    }

    pub(crate) fn has_capability(
        &self,
        target: &Arc<RawSpinLock<UserNamespace>>,
        capability: u32,
    ) -> bool {
        has_namespace_capability(&self.caller, &self.proxy.user_ns, target, capability)
    }

    pub(crate) fn replace_files(&mut self, files: FileTableScope) {
        assert!(self.files.is_none());
        self.files = Some(files);
    }

    pub(crate) fn prepare_fork(&mut self) {
        let owner = self.source().fs.fork_owner();
        let mnt = self.source().mnt.clone();
        self.set_change(FsBindingChange::Independent { owner, mnt });
    }

    pub(crate) fn prepare_namespace_clone(&mut self) -> StarryResult<()> {
        self.cloned = Some(self.source().fs.namespace().clone_namespace());
        let binding = self.binding.as_ref().expect("selected binding");
        self.mapped = Some(
            binding
                .source
                .fs
                .remap(self.cloned.as_mut().expect("prepared clone"))?,
        );
        let owner = self.mapped.as_ref().expect("mapped paths").fork_owner();
        let (created, retirement) = self.cloned.take().expect("prepared clone").into_parts();
        self.clone_retirement = Some(retirement);
        let creator = self
            .replacement_proxy
            .as_ref()
            .unwrap_or(&self.proxy)
            .user_ns
            .clone();
        let mnt = MntNamespace::from_created(created, creator);
        self.set_change(FsBindingChange::Independent { owner, mnt });
        Ok(())
    }

    pub(crate) fn prepare_shared_child(&mut self) -> StarryResult<()> {
        let source = self.binding.as_ref().expect("selected binding");
        let owner = self.thread.with_current_scope_mut(|scope| {
            check_source(scope, source)?;
            FS_CONTEXT
                .scope_mut(scope)
                .as_mut()
                .expect("validated owner")
                .share_for_child()
                .map_err(StarryError::from)
        })?;
        let mnt = self.source().mnt.clone();
        self.set_change(FsBindingChange::Independent { owner, mnt });
        Ok(())
    }

    pub(crate) fn into_child(mut self) -> StarryResult<(ChildFsBinding, Option<NsProxy>)> {
        // No child preparation value may be finalized under the parent writer.
        drop(self.writer.take());
        match self
            .binding
            .as_mut()
            .expect("selected binding")
            .change
            .take()
        {
            Some(FsBindingChange::Independent { owner, mnt }) => {
                Ok((ChildFsBinding { owner, mnt }, self.replacement_proxy.take()))
            }
            _ => Err(StarryError::BadState),
        }
    }

    pub(crate) fn require_sole_owner(&mut self) -> StarryResult<()> {
        let binding = self.binding.as_mut().expect("selected binding");
        self.thread.with_current_scope_mut(|scope| {
            check_binding(scope, &binding.source, FsSharingRequirement::SoleOwner)
        })?;
        binding.sharing = FsSharingRequirement::SoleOwner;
        Ok(())
    }

    pub(crate) fn prepare_cwd(&mut self, path: PathHandle) -> StarryResult<()> {
        let (command, retirement) = self.source().fs.prepare_cwd(path);
        self.path_retirement = retirement;
        self.set_change(FsBindingChange::Update {
            command: Some(command?),
            mnt: None,
        });
        Ok(())
    }

    pub(crate) fn prepare_root(&mut self, path: PathHandle) -> StarryResult<()> {
        let (command, retirement) = self.source().fs.prepare_root(path);
        self.path_retirement = retirement;
        self.set_change(FsBindingChange::Update {
            command: Some(command?),
            mnt: None,
        });
        Ok(())
    }

    pub(crate) fn prepare_mount_join(
        &mut self,
        target: Arc<MntNamespace>,
        sharing: FsSharingRequirement,
    ) -> StarryResult<()> {
        self.target_mnt = Some(target);
        let target = self.target_mnt.as_ref().expect("retained target");
        let user_ns = &self
            .replacement_proxy
            .as_ref()
            .unwrap_or(&self.proxy)
            .user_ns;
        if !self.has_capability(target.creator(), CAP_SYS_ADMIN)
            || !self.has_capability(user_ns, CAP_SYS_CHROOT)
            || !self.has_capability(user_ns, CAP_SYS_ADMIN)
        {
            return Err(StarryError::OperationNotPermitted);
        }
        if sharing == FsSharingRequirement::SoleOwner {
            self.require_sole_owner()?;
        }
        let target = self.target_mnt.as_ref().expect("retained target");
        let (root, retirement) = target.core().snapshot_root();
        self.root_retirement = Some(retirement);
        let command = self.source().fs.prepare_join(
            root?,
            self.binding.as_ref().expect("selected binding").sharing,
        );
        self.set_change(FsBindingChange::Update {
            command: Some(command),
            mnt: Some(self.target_mnt.as_ref().expect("retained target").clone()),
        });
        Ok(())
    }

    fn set_change(&mut self, change: FsBindingChange) {
        let binding = self.binding.as_mut().expect("selected binding");
        assert!(
            binding.change.is_none(),
            "one filesystem change per publication"
        );
        binding.change = Some(change);
    }

    pub(crate) fn pivot_root(
        mut self,
        new_root: PathHandle,
        put_old: PathHandle,
    ) -> StarryResult<()> {
        self.pivot_paths = Some((new_root, put_old));
        if !self.has_capability(self.source().mnt.creator(), CAP_SYS_ADMIN) {
            return Err(StarryError::OperationNotPermitted);
        }
        // Linux freezes fs root after permission checks. Shared paths can still
        // change afterwards; the topology operation uses this exact selection.
        self.pivot_source = self
            .thread
            .snapshot_fs_binding_under(self.writer.as_ref().expect("writer"))?;
        let source = self.pivot_source.as_ref().ok_or(StarryError::BadState)?;
        let (new_root, put_old) = self.pivot_paths.as_ref().expect("retained paths");
        let (result, retirement) = source.fs.pivot_root(new_root, put_old);
        self.pivot_retirement = retirement;
        result?;
        crate::file::notify_mount_namespace_changed(source.mnt.core());
        drop(self.writer.take());
        Ok(())
    }

    pub(crate) fn commit(mut self) -> StarryResult<()> {
        if let Some(proxy) = self.replacement_proxy.take() {
            self.publication = Some(
                self.writer
                    .as_ref()
                    .expect("writer")
                    .prepare_publication(proxy),
            );
        }
        let thread = self.thread;
        thread.with_current_scope_mut(|scope| self.apply(scope))?;
        if let Some(publication) = &mut self.publication {
            self.retired_proxy = Some(
                self.writer
                    .as_mut()
                    .expect("writer")
                    .publish_prepared(publication),
            );
        }
        drop(self.writer.take());
        Ok(())
    }

    fn apply(&mut self, scope: &mut Scope) -> StarryResult<()> {
        let binding = self.binding.as_mut().expect("selected binding");
        check_source(scope, binding)?;
        match &mut binding.change {
            Some(FsBindingChange::Independent { owner, mnt }) => {
                core::mem::swap(
                    FS_CONTEXT
                        .scope_mut(scope)
                        .as_mut()
                        .expect("validated owner"),
                    owner,
                );
                core::mem::swap(
                    MNT_NAMESPACE
                        .scope_mut(scope)
                        .as_mut()
                        .expect("validated adapter"),
                    mnt,
                );
            }
            Some(FsBindingChange::Update { command, mnt }) => {
                let (result, retirement) = FS_CONTEXT
                    .scope_mut(scope)
                    .as_mut()
                    .expect("validated owner")
                    .apply_update(command.take().expect("prepared command"));
                self.update_retirement = retirement;
                result?;
                if let Some(mnt) = mnt {
                    core::mem::swap(
                        MNT_NAMESPACE
                            .scope_mut(scope)
                            .as_mut()
                            .expect("validated adapter"),
                        mnt,
                    );
                }
            }
            None => {}
        }
        if let Some(files) = self.files.take() {
            self.retired_files = Some(core::mem::replace(&mut *FD_TABLE.scope_mut(scope), files));
        }
        Ok(())
    }
}

fn check_source(scope: &Scope, binding: &PreparedFsBinding) -> StarryResult<()> {
    check_binding(scope, &binding.source, binding.sharing)
}

fn check_binding(
    scope: &Scope,
    source: &FsBindingSnapshot,
    sharing: FsSharingRequirement,
) -> StarryResult<()> {
    let owner = FS_CONTEXT.scope(scope);
    let mnt = MNT_NAMESPACE.scope(scope);
    let (Some(owner), Some(mnt)) = (owner.as_ref(), mnt.as_ref()) else {
        return Err(StarryError::BadState);
    };
    if !Arc::ptr_eq(owner.namespace(), mnt.core()) || !Arc::ptr_eq(mnt, &source.mnt) {
        return Err(StarryError::BadState);
    }
    owner.check_source(&source.fs, sharing)?;
    Ok(())
}
