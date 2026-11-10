//! Fixed rename authorization and publication inside a backend-owned attempt.

use alloc::{boxed::Box, string::String, vec::Vec};
use core::{any::Any, cell::RefCell};

use super::{NamespaceNode, NamespaceOutcome, NamespaceState, RenameAction, RenameEffect};
use crate::{
    DirEntry, Location, Metadata, MutationCredentials, NodePermission, NodeType, PreparedWrite,
    RenameOptions, VfsError, VfsResult,
};

/// Lock-external destruction owned by the outermost namespace operation.
///
/// Create before acquiring backend gates. Inner operations borrow this bag;
/// they cannot clear it. No value is destroyed until its outer owner drops.
#[derive(Default)]
pub struct MutationRetire {
    owners: RefCell<Vec<Box<dyn Any>>>,
}

impl MutationRetire {
    pub fn new() -> Self {
        Self::default()
    }

    /// Transfers an owned resource, including candidates and global evictions.
    pub fn retire<T: Any>(&self, owner: T) {
        self.owners.borrow_mut().push(Box::new(owner));
    }
}

/// A concrete namespace command, not an arbitrary backend callback.
pub enum NamespaceMutation<'a> {
    Rename(RenameRequest<'a>),
}

/// Unforgeable request borrowing the actual mount's write authority.
pub struct RenameRequest<'a> {
    source: (&'a Location, &'a str),
    destination: (&'a Location, &'a str),
    options: RenameOptions,
    credentials: &'a MutationCredentials<'a>,
    write: &'a PreparedWrite,
    domain: usize,
}

impl<'a> RenameRequest<'a> {
    pub(crate) fn new(
        source: (&'a Location, &'a str),
        destination: (&'a Location, &'a str),
        options: RenameOptions,
        credentials: &'a MutationCredentials<'a>,
        write: &'a PreparedWrite,
    ) -> VfsResult<Self> {
        write.write_for(source.0)?.check_location(source.0)?;
        write
            .write_for(destination.0)?
            .check_location(destination.0)?;
        let root = source.0.mountpoint().root_location();
        let owner = root.filesystem() as *const dyn crate::FilesystemOps as *const ();
        for parent in [source.0, destination.0] {
            if parent.filesystem() as *const dyn crate::FilesystemOps as *const () != owner {
                return Err(VfsError::CrossesDevices);
            }
        }
        crate::path::verify_entry_name(source.1)?;
        crate::path::verify_entry_name(destination.1)?;
        let domain = source.0.entry().as_dir()?.inner().namespace_domain();
        Ok(Self {
            source,
            destination,
            options,
            credentials,
            write,
            domain,
        })
    }

    /// Immutable parent identity; never call ordinary metadata/lookup under a gate.
    pub fn source_parent(&self) -> &DirEntry {
        self.source.0.entry()
    }
    pub fn destination_parent(&self) -> &DirEntry {
        self.destination.0.entry()
    }
    pub fn source_name(&self) -> &str {
        self.source.1
    }
    pub fn destination_name(&self) -> &str {
        self.destination.1
    }
    pub fn options(&self) -> RenameOptions {
        self.options
    }

    /// Borrows the outer operation's already-prepared backing write leases.
    pub fn write_authority(&self) -> &PreparedWrite {
        self.write
    }

    /// Executes exactly one attempt while the backend retains real exclusion.
    /// Every retry calls this again with a newly acquired backend attempt.
    pub fn execute(
        &self,
        attempt: &mut dyn LockedAttempt,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<RenameEffect> {
        let source = self
            .source_parent()
            .as_dir()
            .expect("validated rename parent");
        let destination = self
            .destination_parent()
            .as_dir()
            .expect("validated rename parent");
        let mut source_change = source.begin_locked_name_change(self.domain, retired);
        let mut destination_change =
            destination.begin_related_locked_name_change(&source_change, retired);
        let mut guard = MutationGuard {
            attempt,
            request: self,
            retired,
        };
        let mut outcome = match guard.slots().and_then(ParentSlot::authorize) {
            Ok(authorized) => authorized.commit(),
            Err(error) => NamespaceOutcome::unchanged(Err(error)),
        };
        if matches!(
            outcome.state(),
            NamespaceState::Applied | NamespaceState::Uncertain
        ) {
            let published = if outcome.state() == NamespaceState::Applied {
                outcome
                    .observation()
                    .ok_or(VfsError::BadState)
                    .and_then(|effect| source.publish_locked_rename(destination, effect, retired))
            } else {
                Err(VfsError::BadState)
            };
            if let Err(error) = published {
                source.invalidate_locked_name(self.source_name(), retired);
                destination.invalidate_locked_name(self.destination_name(), retired);
                if let Some(effect) = outcome.observation() {
                    source.invalidate_locked_name(&effect.source_name, retired);
                    destination.invalidate_locked_name(&effect.destination_name, retired);
                }
                if outcome.state() == NamespaceState::Applied {
                    let effect = outcome.observation().cloned();
                    let result = outcome.into_result().and(Err(error));
                    outcome = NamespaceOutcome::uncertain(effect, result);
                }
            }
        }
        source_change.finish_pair(&mut destination_change);
        outcome
    }
}

/// A backend-selected entity plus its current, locked authorization metadata.
pub struct RenameNode {
    pub entry: DirEntry,
    pub metadata: Metadata,
    pub canonical_name: String,
    pub cacheable: bool,
    pub empty: bool,
}

impl RenameNode {
    pub fn object(&self) -> NamespaceNode {
        NamespaceNode {
            inode: self.entry.inode(),
            node_type: self.entry.node_type(),
            identity: self.entry.identity().cloned(),
        }
    }
}

pub struct RenameObservation {
    pub source: RenameNode,
    pub target: Option<RenameNode>,
}

/// Backend-private implementations borrow the actual held owner guard.
///
/// No ordinary lookup, admission, or metadata callback is available here.
/// The backend must retain allocations selected during observation and return
/// only explicitly restartable, unapplied operations as `Retry`.
pub trait LockedAttempt {
    fn parents(&mut self) -> VfsResult<(Metadata, Metadata)>;
    fn observe(
        &mut self,
        request: &RenameRequest<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<RenameObservation>;
    fn is_ancestor(&mut self, ancestor: &NamespaceNode, directory: u64) -> VfsResult<bool>;
    fn commit(
        &mut self,
        authorized: &AuthorizedRename<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<RenameEffect>;
}

/// Only the fixed VFS authorizer can construct this backend commit capability.
pub struct AuthorizedRename<'a> {
    request: &'a RenameRequest<'a>,
    observed: &'a RenameObservation,
}

impl AuthorizedRename<'_> {
    pub fn request(&self) -> &RenameRequest<'_> {
        self.request
    }
    pub fn observation(&self) -> &RenameObservation {
        self.observed
    }
    pub fn effect(&self) -> RenameEffect {
        RenameEffect {
            source: self.observed.source.object(),
            target: self.observed.target.as_ref().map(RenameNode::object),
            action: if self.request.options.exchange() {
                RenameAction::Exchange
            } else {
                RenameAction::Move
            },
            source_name: self.observed.source.canonical_name.clone(),
            destination_name: self.observed.target.as_ref().map_or_else(
                || self.request.destination_name().into(),
                |target| target.canonical_name.clone(),
            ),
            source_cacheable: self.observed.source.cacheable,
            destination_cacheable: self
                .observed
                .target
                .as_ref()
                .map_or(self.observed.source.cacheable, |target| target.cacheable),
        }
    }
}

struct MutationGuard<'a, 'request> {
    attempt: &'a mut dyn LockedAttempt,
    request: &'a RenameRequest<'request>,
    retired: &'a MutationRetire,
}

struct ParentSlot<'slot, 'attempt, 'request> {
    guard: &'slot mut MutationGuard<'attempt, 'request>,
    parents: (Metadata, Metadata),
    observed: RenameObservation,
}

struct AuthorizedSlot<'slot, 'attempt, 'request> {
    slot: ParentSlot<'slot, 'attempt, 'request>,
    noop: bool,
}

impl<'attempt, 'request> MutationGuard<'attempt, 'request> {
    fn slots(&mut self) -> VfsResult<ParentSlot<'_, 'attempt, 'request>> {
        let parents = self.attempt.parents()?;
        self.request
            .credentials
            .check_permission(&parents.0, NodePermission::OTHER_EXEC)?;
        self.request
            .credentials
            .check_permission(&parents.1, NodePermission::OTHER_EXEC)?;
        let mut observed = self.attempt.observe(self.request, self.retired)?;
        observed.source.entry = self.request.source_parent().as_dir()?.canonicalize_locked(
            &observed.source.canonical_name,
            observed.source.entry,
            observed.source.cacheable,
            self.retired,
        );
        if let Some(target) = observed.target.as_mut() {
            target.entry = self
                .request
                .destination_parent()
                .as_dir()?
                .canonicalize_locked(
                    &target.canonical_name,
                    target.entry.clone(),
                    target.cacheable,
                    self.retired,
                );
        } else {
            self.request
                .destination_parent()
                .as_dir()?
                .invalidate_locked_name(self.request.destination_name(), self.retired);
        }
        self.retired.retire(observed.source.entry.clone());
        self.retired
            .retire(observed.target.as_ref().map(|target| target.entry.clone()));
        Ok(ParentSlot {
            guard: self,
            parents,
            observed,
        })
    }
}

impl<'slot, 'attempt, 'request> ParentSlot<'slot, 'attempt, 'request> {
    fn authorize(self) -> VfsResult<AuthorizedSlot<'slot, 'attempt, 'request>> {
        let request = self.guard.request;
        request
            .write
            .write_for(request.source.0)?
            .check_location(request.source.0)?;
        request
            .write
            .write_for(request.destination.0)?
            .check_location(request.destination.0)?;
        if request.options.no_replace() && self.observed.target.is_some() {
            return Err(VfsError::AlreadyExists);
        }
        if request.options.exchange() && self.observed.target.is_none() {
            return Err(VfsError::NotFound);
        }
        let noop = self.observed.target.as_ref().is_some_and(|target| {
            target
                .entry
                .same_backend_object(&self.observed.source.entry)
        });
        // Same-object renames do not modify either name, even with WHITEOUT.
        // Only the locked observation may bypass mutation-specific permissions.
        if noop {
            return Ok(AuthorizedSlot {
                slot: self,
                noop: true,
            });
        }
        for parent in [&self.parents.0, &self.parents.1] {
            request.credentials.check_permission(
                parent,
                NodePermission::OTHER_WRITE | NodePermission::OTHER_EXEC,
            )?;
        }
        request
            .credentials
            .check_sticky(&self.parents.0, &self.observed.source.metadata)?;
        if let Some(target) = &self.observed.target {
            request
                .credentials
                .check_sticky(&self.parents.1, &target.metadata)?;
            if !request.options.exchange() {
                match (
                    self.observed.source.entry.node_type(),
                    target.entry.node_type(),
                ) {
                    (NodeType::Directory, NodeType::Directory) if !target.empty => {
                        return Err(VfsError::DirectoryNotEmpty);
                    }
                    (NodeType::Directory, NodeType::Directory) => {}
                    (NodeType::Directory, _) => return Err(VfsError::NotADirectory),
                    (_, NodeType::Directory) => return Err(VfsError::IsADirectory),
                    _ => {}
                }
            }
        }
        // Changing a directory's parent also writes its native ".." entry.
        // Compare backend entities, not aliases, and use this attempt's metadata.
        if !request
            .source_parent()
            .same_backend_object(request.destination_parent())
        {
            if self.observed.source.entry.node_type() == NodeType::Directory {
                request.credentials.check_permission(
                    &self.observed.source.metadata,
                    NodePermission::OTHER_WRITE,
                )?;
            }
            if request.options.exchange()
                && let Some(target) = &self.observed.target
                && target.entry.node_type() == NodeType::Directory
            {
                request
                    .credentials
                    .check_permission(&target.metadata, NodePermission::OTHER_WRITE)?;
            }
        }
        if self.observed.source.entry.node_type() == NodeType::Directory
            && self.guard.attempt.is_ancestor(
                &self.observed.source.object(),
                request.destination_parent().inode(),
            )?
        {
            return Err(VfsError::InvalidInput);
        }
        if request.options.exchange()
            && let Some(target) = &self.observed.target
            && target.entry.node_type() == NodeType::Directory
            && self
                .guard
                .attempt
                .is_ancestor(&target.object(), request.source_parent().inode())?
        {
            return Err(VfsError::InvalidInput);
        }
        Ok(AuthorizedSlot {
            slot: self,
            noop: false,
        })
    }
}

impl AuthorizedSlot<'_, '_, '_> {
    fn commit(self) -> NamespaceOutcome<RenameEffect> {
        let authorized = AuthorizedRename {
            request: self.slot.guard.request,
            observed: &self.slot.observed,
        };
        if self.noop {
            let mut effect = authorized.effect();
            effect.action = RenameAction::Noop;
            return NamespaceOutcome::unchanged_observed(effect, Ok(()));
        }
        self.slot
            .guard
            .attempt
            .commit(&authorized, self.slot.guard.retired)
    }
}
