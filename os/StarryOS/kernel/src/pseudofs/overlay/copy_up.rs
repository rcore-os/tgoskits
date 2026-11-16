//! Command-owned hidden copies; every I/O and cleanup runs outside owner gates.

use alloc::{boxed::Box, format, string::String, sync::Arc};
use core::{
    any::Any,
    sync::atomic::{AtomicU64, Ordering},
};

use axfs_ng_vfs::{
    CreateAttributes, CreateContent, CreateIntent, Location, Metadata, MetadataChange,
    NamespaceCommandScope, NamespaceEffect, NamespaceNode, NamespaceObservation,
    NamespaceOperationContext, NamespaceOutcome, NamespacePreparation, NamespaceStart,
    NamespaceState, NodePermission, NodeType, PreparedWrite, SealedStaging, StagingNode, VfsError,
    VfsResult,
};

use super::{OverlayFs, OverlayInode};
use crate::sync::Mutex;

static NEXT_WORK: AtomicU64 = AtomicU64::new(1);

pub(super) struct CopyPreparation {
    // Retaining the logical inode also prevents reuse of the preparation key.
    _inode: Arc<OverlayInode>,
    source: Location,
    work_root: Location,
    work_name: String,
    owner: Arc<OverlayFs>,
    state: Mutex<CopyState>,
}

enum WorkDirectory {
    Reserved,
    Created(Location),
    Uncertain,
    Removed,
}

enum CopyStage {
    Empty,
    Building(Box<StagingNode>),
    Ready(SealedStaging),
}

struct CopyState {
    work: WorkDirectory,
    stage: CopyStage,
}

impl CopyPreparation {
    pub(super) fn new(fs: &OverlayFs, inode: Arc<OverlayInode>) -> VfsResult<Self> {
        let work_root = fs.work_dir.as_ref().ok_or(VfsError::ReadOnlyFilesystem)?;
        let source = inode.lower.first().ok_or(VfsError::BadState)?;
        if !work_root.filesystem().supports_staging()
            || !work_root.filesystem().has_native_namespace()
            // Nested source attributes need their own typed list operation;
            // never silently discard attributes through a missing capability.
            || source.filesystem().stack_depth() != 0
        {
            return Err(VfsError::OperationNotSupported);
        }
        let number = NEXT_WORK
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| VfsError::NoMemory)?;
        Ok(Self {
            source: source.clone(),
            _inode: inode,
            work_root: work_root.clone(),
            work_name: format!(".ovl-work-{number:016x}"),
            owner: fs.this.upgrade().expect("live overlay receiver"),
            state: Mutex::new(CopyState {
                work: WorkDirectory::Reserved,
                stage: CopyStage::Empty,
            }),
        })
    }

    pub(super) fn target(&self) -> VfsResult<Location> {
        match &self.state.lock().stage {
            CopyStage::Ready(stage) => Ok(stage.location().clone()),
            _ => Err(VfsError::BadState),
        }
    }

    pub(super) fn install<'a>(
        &self,
        context: &'a NamespaceOperationContext<'a>,
        destination: (&'a Location, &'a str),
    ) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
        let credentials = self.owner.credentials();
        let mut state = self.state.lock();
        let CopyStage::Ready(stage) = &mut state.stage else {
            return Err(VfsError::BadState);
        };
        stage.install(context, destination, None, &credentials)
    }

    fn create_work(&self, write: &PreparedWrite, state: &mut CopyState) -> VfsResult<()> {
        if matches!(state.work, WorkDirectory::Reserved) {
            let credentials = self.owner.credentials();
            let outcome = NamespaceCommandScope::run_write(write, |command| {
                loop {
                    let scope = command.attempt()?;
                    {
                        let context = scope.context();
                        scope.create(
                            &context,
                            &self.work_root,
                            &self.work_name,
                            CreateAttributes {
                                content: CreateContent::Node(
                                    NodeType::Directory,
                                    Default::default(),
                                ),
                                permission: NodePermission::from_bits_truncate(0o700),
                            },
                            CreateIntent::Exclusive,
                            &credentials,
                        )?;
                    }
                    let outcome = scope
                        .finish()
                        .unwrap_or_else(|_| unreachable!("bound work create"));
                    if outcome.state() != NamespaceState::Retry {
                        return Ok(outcome);
                    }
                    command.prepare()?;
                }
            })?;
            if let Some(NamespaceEffect::Create(effect)) = outcome.observation()
                && effect.created
            {
                state.work = WorkDirectory::Created(Location::new(
                    self.work_root.mountpoint().clone(),
                    effect.selected.entry.clone(),
                ));
            } else if outcome.state() == NamespaceState::Uncertain {
                state.work = WorkDirectory::Uncertain;
            }
            let uncertain = outcome.state() == NamespaceState::Uncertain;
            outcome.into_result()?;
            if uncertain {
                return Err(VfsError::BadState);
            }
        }
        let WorkDirectory::Created(work) = &state.work else {
            return Err(VfsError::BadState);
        };
        // Creation under a SGID work root may inherit SGID. Clear it before
        // making any target, retaining the original complete write graph.
        let credentials = self.owner.credentials();
        NamespaceCommandScope::run_write(write, |command| {
            loop {
                let scope = command.attempt::<Metadata>()?;
                {
                    let context = scope.context();
                    scope.change_metadata(
                        &context,
                        work,
                        MetadataChange::Chmod(NodePermission::from_bits_truncate(0o700)),
                        &credentials,
                    )?;
                }
                let outcome = scope
                    .finish()
                    .unwrap_or_else(|_| unreachable!("bound work mode"));
                if outcome.state() != NamespaceState::Retry {
                    return Ok(outcome);
                }
                command.prepare()?;
            }
        })?
        .into_result()
    }

    fn clean_work(&self, write: &PreparedWrite, state: &mut CopyState) -> VfsResult<()> {
        let work = match &state.work {
            WorkDirectory::Reserved | WorkDirectory::Removed => return Ok(()),
            WorkDirectory::Uncertain => return Err(VfsError::BadState),
            WorkDirectory::Created(work) => work,
        };
        let expected = NamespaceNode {
            inode: work.inode(),
            node_type: work.node_type(),
            identity: work.entry().identity().cloned(),
        };
        let credentials = self.owner.credentials();
        let outcome = NamespaceCommandScope::run_write(write, |command| {
            loop {
                let prepared = self.work_root.prepare_unlink(&self.work_name)?;
                let scope = command.attempt()?;
                let mut mismatch = false;
                {
                    let context = scope.context();
                    match scope.unlink(&context, prepared, true, &credentials)? {
                        NamespaceStart::Observed(observed) => {
                            if expected.matches(&observed.target().entry) {
                                observed.resume();
                            } else {
                                observed.cancel();
                                mismatch = true;
                            }
                        }
                        NamespaceStart::Complete(_) => {}
                    }
                }
                let outcome = scope
                    .finish()
                    .unwrap_or_else(|_| unreachable!("bound work cleanup"));
                if mismatch {
                    return Ok(NamespaceOutcome::unchanged(
                        outcome.result().and(Err(VfsError::BadState)),
                    ));
                }
                if outcome.state() != NamespaceState::Retry {
                    return Ok(outcome);
                }
                command.prepare()?;
            }
        })?;
        if outcome.state() == NamespaceState::Applied {
            state.work = WorkDirectory::Removed;
        }
        outcome.into_result()
    }
}

impl NamespacePreparation for CopyPreparation {
    fn prepare(&self, write: &PreparedWrite) -> VfsResult<()> {
        // The command is idle here: no ancestor gate or admission is retained.
        // This private state lock can only descend to strictly lower owners.
        let mut state = self.state.lock();
        self.create_work(write, &mut state)?;
        if matches!(state.stage, CopyStage::Empty) {
            let WorkDirectory::Created(work) = &state.work else {
                return Err(VfsError::BadState);
            };
            state.stage =
                CopyStage::Building(Box::new(StagingNode::reserve(write, work, &self.source)?));
        }
        let credentials = self.owner.credentials();
        if let CopyStage::Building(stage) = &mut state.stage {
            stage.create(&credentials)?;
            ax_fs_ng::file::copy_staging_data(stage)?;
            stage.copy_xattrs(&credentials)?;
            stage.restore(&credentials)?;
        }
        let previous = core::mem::replace(&mut state.stage, CopyStage::Empty);
        match previous {
            CopyStage::Building(stage) => match stage.seal() {
                Ok(stage) => state.stage = CopyStage::Ready(stage),
                Err((error, stage)) => {
                    state.stage = CopyStage::Building(stage);
                    return Err(error);
                }
            },
            stage => state.stage = stage,
        }
        Ok(())
    }

    fn cleanup(&self, write: &PreparedWrite) -> VfsResult<()> {
        let mut state = self.state.lock();
        let credentials = self.owner.credentials();
        let result = match &mut state.stage {
            CopyStage::Empty => Ok(()),
            CopyStage::Building(stage) => stage.cleanup(&credentials),
            CopyStage::Ready(stage) => stage.cleanup(&credentials),
        };
        // Try both cleanups, preserving the first error and diagnosing residue.
        let result = result.and(self.clean_work(write, &mut state));
        if let Err(error) = result {
            warn!(
                "overlay hidden work {} cleanup failed: {:?}",
                self.work_name, error
            );
        }
        result
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}
