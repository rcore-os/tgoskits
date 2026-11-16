//! Identity-bound ownership of newly created, unpublished backing nodes.

use alloc::{boxed::Box, format, string::String};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{
    CreateAttributes, CreateContent, CreateIntent, Location, Metadata, MetadataUpdate,
    MutationCredentials, NamespaceCommandScope, NamespaceEffect, NamespaceNode,
    NamespaceObservation, NamespaceOperationContext, NamespaceOutcome, NamespaceStart,
    NamespaceState, NodePermission, NodeType, PreparedWrite, VfsError, VfsResult,
    metadata::MetadataIntent,
};

mod cleanup;

static NEXT_STAGE: AtomicU64 = AtomicU64::new(1);

struct Binding {
    write: PreparedWrite,
    work: Location,
    name: String,
}

/// A factory-owned copy target. No constructor accepts an existing target or
/// caller-provided creation effect. Call every method outside admissions/gates.
/// Explicitly clean every failed or abandoned stage; Drop performs no I/O.
#[must_use = "restore and seal the new target, or explicitly clean it"]
pub struct StagingNode {
    binding: Binding,
    source: Location,
    metadata: Option<Metadata>,
    target: Option<Location>,
    restored: bool,
    uncertain: bool,
}

/// A complete new node whose restoration authority has been consumed.
/// Its installation must still be authorized in a fresh logical attempt.
#[must_use = "install or explicitly clean the sealed stage"]
pub struct SealedStaging {
    binding: Binding,
    target: Location,
    unlink: crate::PreparedUnlink,
    state: StageState,
}

/// One lock-external data copy, borrowing the original graph and new target.
/// Native cache adapters must finish and close their endpoints before restore.
pub struct StagingIo<'a> {
    source: crate::FileIoTarget,
    target: crate::FileIoTarget,
    len: u64,
    _write: &'a PreparedWrite,
}

impl StagingIo<'_> {
    pub fn source(&self) -> &Location {
        self.source.location()
    }
    pub fn target(&self) -> &Location {
        self.target.location()
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

enum StageState {
    Hidden,
    Installed,
    Detached,
    Uncertain,
    Cleaned,
}

impl StagingNode {
    /// Reserves a unique work name without creating a file or entering a gate.
    /// The caller selects its bound source/work roots; the factory checks the
    /// complete prepared graph and the backend's unlink lifetime capability.
    pub fn reserve(write: &PreparedWrite, work: &Location, source: &Location) -> VfsResult<Self> {
        write.write_for(work)?.check_location(work)?;
        if !work.filesystem().supports_staging() || !work.filesystem().has_native_namespace() {
            return Err(VfsError::OperationNotSupported);
        }
        if !crate::prepared_write::PreparedView::write(write)
            .filesystems()
            .any(|owner| core::ptr::addr_eq(owner, source.filesystem()))
        {
            return Err(VfsError::CrossesDevices);
        }
        let number = NEXT_STAGE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| VfsError::NoMemory)?;
        Ok(Self {
            binding: Binding {
                write: write.retain_staging(),
                work: work.clone(),
                name: format!(".vfs-stage-{number:016x}"),
            },
            source: source.clone(),
            metadata: None,
            target: None,
            restored: false,
            uncertain: false,
        })
    }

    /// Creates exactly one new native inode. A completion error still retains
    /// its actual created target for cleanup. Existing names never mint proof.
    pub fn create(&mut self, mounter: &MutationCredentials<'_>) -> VfsResult<()> {
        if self.target.is_some() || self.uncertain {
            return Err(VfsError::BadState);
        }
        self.binding.work.check_is_dir()?;
        let metadata = self.source.metadata()?;
        if metadata.node_type == NodeType::RegularFile {
            mounter.check_permission(&metadata, NodePermission::OTHER_READ)?;
        }
        let link = (metadata.node_type == NodeType::Symlink)
            .then(|| self.source.read_link())
            .transpose()?;
        let content = match &link {
            Some(link) => CreateContent::Symlink(link),
            None => CreateContent::Node(metadata.node_type, metadata.rdev),
        };
        let permission =
            NodePermission::from_bits_truncate(if metadata.node_type == NodeType::Directory {
                0o700
            } else {
                0o600
            });
        let outcome = NamespaceCommandScope::run_write(&self.binding.write, |command| {
            loop {
                let scope = command.attempt()?;
                {
                    let context = scope.context();
                    scope.create(
                        &context,
                        &self.binding.work,
                        &self.binding.name,
                        CreateAttributes {
                            content,
                            permission,
                        },
                        CreateIntent::Exclusive,
                        mounter,
                    )?;
                }
                let outcome = scope
                    .finish()
                    .unwrap_or_else(|_| unreachable!("bound stage creation"));
                if outcome.state() != NamespaceState::Retry {
                    return Ok(outcome);
                }
                command.prepare()?;
            }
        })?;
        if let Some(NamespaceEffect::Create(effect)) = outcome.observation()
            && effect.created
        {
            self.target = Some(Location::new(
                self.binding.work.mountpoint().clone(),
                effect.selected.entry.clone(),
            ));
            self.metadata = Some(metadata);
        }
        self.uncertain = outcome.state() == NamespaceState::Uncertain;
        outcome.into_result()?;
        if self.target.is_none() || self.uncertain {
            return Err(VfsError::BadState);
        }
        Ok(())
    }

    /// The selected source, retained independently of its current name.
    pub fn source(&self) -> &Location {
        &self.source
    }

    /// The newly created target for prepared cache/data I/O, before sealing.
    /// All endpoints must close before seal or cleanup.
    pub fn target(&self) -> VfsResult<&Location> {
        self.target
            .as_ref()
            .filter(|_| !self.uncertain)
            .ok_or(VfsError::BadState)
    }

    /// Attributes sampled by the factory before creating this target.
    pub fn source_metadata(&self) -> VfsResult<&Metadata> {
        self.metadata.as_ref().ok_or(VfsError::BadState)
    }

    /// Selects fixed native endpoints for one prepared data copy, preserving
    /// every intermediate path and saved-mounter check. Non-regular nodes have
    /// no byte-copy phase.
    pub fn native_data(&self) -> VfsResult<Option<StagingIo<'_>>> {
        let metadata = self.source_metadata()?;
        if metadata.node_type != NodeType::RegularFile {
            return Ok(None);
        }
        let target = self.target()?;
        self.binding
            .write
            .write_for(target)?
            .check_location(target)?;
        Ok(Some(StagingIo {
            source: crate::FileIoRequest::read(crate::PathHandle::acquire(self.source.clone())?)?
                .resolve()?,
            target: crate::FileIoRequest::write(
                crate::PathHandle::acquire(target.clone())?,
                &self.binding.write,
            )?
            .resolve()?,
            len: metadata.size,
            _write: &self.binding.write,
        }))
    }

    /// Copies the attributes exposed by the source's native capability before
    /// restoring ownership. Missing list support means no source attributes;
    /// real list/read/write errors retain this stage for explicit cleanup.
    pub fn copy_xattrs(&self, mounter: &MutationCredentials<'_>) -> VfsResult<()> {
        if self.restored {
            return Err(VfsError::BadState);
        }
        let target = self.target()?;
        mounter.check_permission(&target.metadata()?, NodePermission::OTHER_WRITE)?;
        let names = match self.source.entry().list_xattrs() {
            Ok(names) => names,
            Err(VfsError::OperationNotSupported) => return Ok(()),
            Err(error) => return Err(error),
        };
        for name in names {
            let value = self.source.entry().get_xattr(&name)?;
            target
                .entry()
                .set_xattr(&name, &value, crate::XattrSetMode::Upsert)?;
        }
        Ok(())
    }

    /// Restores only this factory's captured source fields with saved-mounter
    /// authorization, never boot/root fallback or ordinary chown kill policy.
    pub fn restore(&mut self, mounter: &MutationCredentials<'_>) -> VfsResult<()> {
        let target = self.target()?;
        let source = self.source_metadata()?;
        let fields = MetadataUpdate {
            mode: (source.node_type != NodeType::Symlink).then_some(source.mode),
            owner: Some((source.uid, source.gid)),
            rdev: None,
            atime: Some(source.atime),
            mtime: Some(source.mtime),
        };
        target.run_metadata_prepared(
            MetadataIntent::StagingRestoration(fields),
            mounter,
            &self.binding.write,
        )?;
        self.restored = true;
        Ok(())
    }

    /// Consumes restoration authority. A premature seal returns the complete
    /// owner so the caller can still finish or clean the real target.
    pub fn seal(self) -> Result<SealedStaging, (VfsError, Box<Self>)> {
        if !self.restored || self.uncertain || self.target.is_none() {
            return Err((VfsError::BadState, Box::new(self)));
        }
        if self
            .metadata
            .as_ref()
            .is_some_and(|metadata| metadata.node_type == NodeType::RegularFile)
        {
            match self.target.as_ref().expect("created target").metadata() {
                Ok(metadata)
                    if metadata.size == self.metadata.as_ref().expect("captured source").size => {}
                Ok(_) => return Err((VfsError::InvalidData, Box::new(self))),
                Err(error) => return Err((error, Box::new(self))),
            }
        }
        let unlink = match self.binding.work.prepare_unlink(&self.binding.name) {
            Ok(unlink) => unlink,
            Err(error) => return Err((error, Box::new(self))),
        };
        Ok(SealedStaging {
            binding: self.binding,
            target: self.target.expect("restored target"),
            unlink,
            state: StageState::Hidden,
        })
    }

    pub fn cleanup(&mut self, mounter: &MutationCredentials<'_>) -> VfsResult<()> {
        match &self.target {
            Some(target) => {
                let outcome = self.binding.cleanup(target, mounter)?;
                if outcome.state() == NamespaceState::Applied || outcome.result().is_ok() {
                    self.target = None;
                }
                outcome.into_result()
            }
            None if self.uncertain => Err(VfsError::BadState),
            None => Ok(()),
        }
    }
}

impl SealedStaging {
    /// The actual object to bind to the logical inode after installation.
    pub fn location(&self) -> &Location {
        &self.target
    }

    /// Installs under a fresh outer authorization, retaining the complete
    /// child's typed result. Both native slots are checked under the native
    /// owner lock before rename; a replacement cannot be mistaken for this
    /// stage or for the destination observation selected by the composite.
    pub fn install<'a>(
        &'a mut self,
        context: &'a NamespaceOperationContext<'a>,
        destination: (&'a Location, &'a str),
        expected_target: Option<NamespaceNode>,
        mounter: &'a MutationCredentials<'a>,
    ) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
        if !matches!(self.state, StageState::Hidden) {
            return Err(VfsError::BadState);
        }
        let expected = identity(&self.target);
        let result = context.install_staging(
            (&self.binding.work, &self.binding.name),
            destination,
            (expected, expected_target),
            mounter,
        )?;
        match result.state() {
            NamespaceState::Applied => self.state = StageState::Installed,
            NamespaceState::Uncertain => self.state = StageState::Uncertain,
            NamespaceState::Unchanged | NamespaceState::Retry => {}
        }
        Ok(result)
    }

    /// Unlinks this hidden stage while retaining its actual orphan inode.
    ///
    /// Bind the resulting location only after a fresh logical authorization.
    /// Seal prepared this private work parent outside every gate. Only this
    /// factory-owned, active-unlink-capable target may reuse that preparation;
    /// ordinary unlink preparation remains non-Clone. Retry observes and checks
    /// the native target again, without reacquiring the prepared write graph.
    pub fn detach<'a>(
        &'a mut self,
        context: &'a NamespaceOperationContext<'a>,
        mounter: &MutationCredentials<'a>,
    ) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
        if !matches!(self.state, StageState::Hidden) {
            return Err(VfsError::BadState);
        }
        let expected = identity(&self.target);
        let prepared =
            crate::PreparedUnlink::new(self.unlink.parent_owner(), self.unlink.name().into());
        let result = match context.unlink(prepared, self.target.is_dir(), mounter)? {
            NamespaceStart::Observed(observed) if expected.matches(&observed.target().entry) => {
                observed.resume()
            }
            NamespaceStart::Observed(observed) => {
                let cancelled = observed.cancel();
                NamespaceObservation::capture(&NamespaceOutcome::unchanged(
                    cancelled.result().and(Err(VfsError::BadState)),
                ))
            }
            NamespaceStart::Complete(result) => result,
        };
        match result.state() {
            NamespaceState::Applied => self.state = StageState::Detached,
            NamespaceState::Uncertain => self.state = StageState::Uncertain,
            NamespaceState::Unchanged | NamespaceState::Retry => {}
        }
        Ok(result)
    }

    pub fn cleanup(&mut self, mounter: &MutationCredentials<'_>) -> VfsResult<()> {
        if matches!(
            self.state,
            StageState::Installed | StageState::Detached | StageState::Cleaned
        ) {
            return Ok(());
        }
        let outcome = self.binding.cleanup(&self.target, mounter)?;
        if outcome.state() == NamespaceState::Applied || outcome.result().is_ok() {
            self.state = StageState::Cleaned;
        }
        outcome.into_result()
    }
}

fn identity(target: &Location) -> NamespaceNode {
    NamespaceNode {
        inode: target.inode(),
        node_type: target.node_type(),
        identity: target.entry().identity().cloned(),
    }
}
