//! Data carried by create, hard-link and removal attempts.

use super::*;
use crate::{DeviceId, Location, Metadata, MutationCredentials, PreparedWrite};

/// Initial contents are installed before the name becomes visible.
#[derive(Clone, Copy, Debug)]
pub enum CreateContent<'a> {
    Node(NodeType, DeviceId),
    Symlink(&'a str),
}

impl CreateContent<'_> {
    pub fn node_type(self) -> NodeType {
        match self {
            Self::Node(kind, _) => kind,
            Self::Symlink(_) => NodeType::Symlink,
        }
    }
}

/// Creation attributes are not credentials. The backend selects ownership
/// semantics from its locked observation, not from this caller-provided data.
#[derive(Clone, Copy, Debug)]
pub struct CreateAttributes<'a> {
    pub content: CreateContent<'a>,
    pub permission: NodePermission,
}

/// Ownership semantics selected by the backend in the current locked attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateOwnerPolicy {
    /// Use the caller's identity with the current parent's SGID inheritance.
    ParentSetgid,
    /// Use the caller's UID/GID and the kernfs directory creation mode mask.
    Caller,
}

impl CreateOwnerPolicy {
    fn initial_attributes(
        self,
        attributes: CreateAttributes<'_>,
        parent: &Metadata,
        credentials: &MutationCredentials<'_>,
    ) -> (NodePermission, (u32, u32)) {
        let mut permission = attributes.permission;
        if self == Self::Caller && attributes.content.node_type() == NodeType::Directory {
            permission = NodePermission::from_bits_truncate(permission.bits() & 0o1777);
        }
        let gid = if self == Self::ParentSetgid && parent.mode.contains(NodePermission::SET_GID) {
            if attributes.content.node_type() == NodeType::Directory {
                permission.insert(NodePermission::SET_GID);
            }
            parent.gid
        } else {
            credentials.fsgid
        };
        (permission, (credentials.fsuid, gid))
    }
}

/// Explicit authority for approved kernel archive initialization.
///
/// Ordinary caller credentials cannot be converted into this authority. Kernel
/// initialization code must opt in explicitly; syscall adapters must not use it.
pub struct TrustedInitialization {
    credentials: MutationCredentials<'static>,
}

impl TrustedInitialization {
    pub const fn kernel() -> Self {
        Self {
            credentials: MutationCredentials::root(),
        }
    }

    pub(crate) fn credentials(&self) -> &MutationCredentials<'static> {
        &self.credentials
    }
}

/// A failed preparation is deliberately retained until the current slot is
/// observed. It can never authorize a namespace modification.
#[derive(Clone, Copy)]
pub enum CreationWrite<'a> {
    Ready(&'a PreparedWrite),
    Denied(VfsError),
}

impl<'a> CreationWrite<'a> {
    pub(crate) fn require(self, parent: &Location) -> VfsResult<&'a PreparedWrite> {
        match self {
            Self::Ready(write) => {
                write.write_for(parent)?.check_location(parent)?;
                Ok(write)
            }
            Self::Denied(error) => Err(error),
        }
    }
}

/// The final-object access needed by an existing-object create/open request.
/// Symlinks are followed by the existing path session outside the owner gate.
#[derive(Clone, Copy, Debug)]
pub struct ExistingOpen {
    pub access: NodePermission,
}

#[derive(Clone)]
pub struct CreateRequest<'a> {
    pub(super) target: MutationTarget<'a>,
    attributes: CreateAttributes<'a>,
    existing: Option<ExistingOpen>,
    restore_owner: Option<(u32, u32)>,
    write: CreationWrite<'a>,
}

impl<'a> CreateRequest<'a> {
    pub(crate) fn new(
        target: MutationTarget<'a>,
        attributes: CreateAttributes<'a>,
        existing: Option<ExistingOpen>,
        restore_owner: Option<(u32, u32)>,
        write: CreationWrite<'a>,
    ) -> Self {
        Self {
            target,
            attributes,
            existing,
            restore_owner,
            write,
        }
    }

    pub fn parent(&self) -> &DirEntry {
        self.target.parent.entry()
    }
    pub fn name(&self) -> &str {
        &self.target.name
    }
    pub fn attributes(&self) -> CreateAttributes<'_> {
        self.attributes
    }
}

#[derive(Clone)]
pub struct LinkRequest<'a> {
    pub(super) target: MutationTarget<'a>,
    source: &'a Location,
    write: &'a PreparedWrite,
}

impl<'a> LinkRequest<'a> {
    pub(crate) fn new(
        target: MutationTarget<'a>,
        source: &'a Location,
        write: &'a PreparedWrite,
    ) -> Self {
        Self {
            target,
            source,
            write,
        }
    }
    pub fn parent(&self) -> &DirEntry {
        self.target.parent.entry()
    }
    pub fn name(&self) -> &str {
        &self.target.name
    }
    pub fn source(&self) -> &DirEntry {
        self.source.entry()
    }
}

pub struct UnlinkRequest<'a> {
    pub(super) target: MutationTarget<'a>,
    directory: bool,
    _prepared: crate::PreparedUnlink,
    write: &'a PreparedWrite,
}

impl<'a> UnlinkRequest<'a> {
    pub(crate) fn new(
        target: MutationTarget<'a>,
        directory: bool,
        prepared: crate::PreparedUnlink,
        write: &'a PreparedWrite,
    ) -> Self {
        Self {
            target,
            directory,
            _prepared: prepared,
            write,
        }
    }
    pub fn parent(&self) -> &DirEntry {
        self.target.parent.entry()
    }
    pub fn name(&self) -> &str {
        &self.target.name
    }
    pub fn directory(&self) -> bool {
        self.directory
    }
}

#[derive(Clone)]
pub(crate) struct MutationTarget<'a> {
    parent: Location,
    name: String,
    credentials: MutationCredentials<'a>,
    domain: usize,
}

impl<'a> MutationTarget<'a> {
    pub(crate) fn new(
        parent: &Location,
        name: &str,
        credentials: &MutationCredentials<'a>,
    ) -> VfsResult<Self> {
        verify_entry_name(name)?;
        let domain = parent.entry().as_dir()?.inner().namespace_domain();
        Ok(Self {
            parent: parent.clone(),
            name: name.into(),
            credentials: *credentials,
            domain,
        })
    }
}

/// Command-specific observations cannot be interchanged by the authorizer.
pub enum MutationObservation {
    Create {
        target: Option<RenameNode>,
        owner_policy: CreateOwnerPolicy,
    },
    Link {
        source: RenameNode,
        target: Option<RenameNode>,
    },
    Unlink(RenameNode),
    Rename(RenameObservation),
}

/// Only the fixed VFS authorizer constructs these commit capabilities.
pub enum AuthorizedMutation<'a> {
    Create(AuthorizedCreate<'a>),
    Link(AuthorizedLink<'a>),
    Unlink(AuthorizedUnlink<'a>),
    Rename(AuthorizedRename<'a>),
}

pub struct AuthorizedCreate<'a> {
    request: &'a CreateRequest<'a>,
    permission: NodePermission,
    owner: (u32, u32),
}

impl AuthorizedCreate<'_> {
    pub fn request(&self) -> &CreateRequest<'_> {
        self.request
    }
    pub fn permission(&self) -> NodePermission {
        self.permission
    }
    pub fn owner(&self) -> (u32, u32) {
        self.owner
    }
}

pub struct AuthorizedLink<'a> {
    request: &'a LinkRequest<'a>,
    source: &'a RenameNode,
}

impl AuthorizedLink<'_> {
    pub fn request(&self) -> &LinkRequest<'_> {
        self.request
    }
    pub fn source(&self) -> &RenameNode {
        self.source
    }
}

pub struct AuthorizedUnlink<'a> {
    request: &'a UnlinkRequest<'a>,
    target: &'a RenameNode,
}

impl AuthorizedUnlink<'_> {
    pub fn request(&self) -> &UnlinkRequest<'_> {
        self.request
    }
    pub fn target(&self) -> &RenameNode {
        self.target
    }
}

impl<'request> NamespaceMutation<'request> {
    /// Immutable parent references for backend owner validation before locking.
    pub fn parents(&self) -> (&DirEntry, &DirEntry) {
        match self {
            Self::Create(request) => (request.parent(), request.parent()),
            Self::Link(request) => (request.parent(), request.parent()),
            Self::Unlink(request) => (request.parent(), request.parent()),
            Self::Rename(request) => (request.source_parent(), request.destination_parent()),
        }
    }

    pub fn execute(
        &self,
        attempt: &mut dyn LockedAttempt,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        if let Self::Rename(request) = self {
            return request
                .execute(attempt, retired)
                .map(NamespaceEffect::Rename);
        }
        let observed = match self.observe_locked(attempt, retired) {
            Ok(observed) => observed,
            Err(error) => return NamespaceOutcome::unchanged(Err(error)),
        };
        let change = self.begin_observed_change(retired);
        self.resume_locked(attempt, retired, observed, change)
    }

    pub(super) fn target(&self) -> &MutationTarget<'request> {
        match self {
            Self::Create(request) => &request.target,
            Self::Link(request) => &request.target,
            Self::Unlink(request) => &request.target,
            Self::Rename(_) => unreachable!(),
        }
    }

    pub(super) fn observe_locked(
        &self,
        attempt: &mut dyn LockedAttempt,
        retired: &MutationRetire,
    ) -> VfsResult<(Metadata, MutationObservation)> {
        CommandGuard {
            attempt,
            command: self,
            target: self.target(),
            retired,
        }
        .observe()
    }

    pub(super) fn begin_observed_change<'a>(
        &self,
        retired: &'a MutationRetire,
    ) -> super::cache::NameChange<'a> {
        let target = self.target();
        target
            .parent
            .entry()
            .as_dir()
            .expect("validated parent")
            .begin_locked_name_change(target.parent.entry().clone(), target.domain, retired)
    }

    pub(super) fn resume_locked(
        &self,
        attempt: &mut dyn LockedAttempt,
        retired: &MutationRetire,
        observed: (Metadata, MutationObservation),
        mut change: super::cache::NameChange<'_>,
    ) -> NamespaceOutcome<NamespaceEffect> {
        let target = self.target();
        let directory = target.parent.entry().as_dir().expect("validated parent");
        let mut guard = CommandGuard {
            attempt,
            command: self,
            target,
            retired,
        };
        let mut outcome = match guard.slot(observed).and_then(CommandSlot::authorize) {
            Ok(authorization) => authorization.commit(),
            Err(error) => NamespaceOutcome::unchanged(Err(error)),
        };
        retired.retire(outcome.observation().cloned());
        if outcome.state() == NamespaceState::Applied {
            let result = outcome
                .observation()
                .ok_or(VfsError::BadState)
                .and_then(|effect| directory.publish_locked_command(effect, retired));
            if let Err(error) = result {
                directory.invalidate_locked_name(&target.name, retired);
                let effect = outcome.observation().cloned();
                let result = outcome.into_result().and(Err(error));
                outcome = NamespaceOutcome::uncertain(effect, result);
            }
        } else if outcome.state() == NamespaceState::Uncertain {
            directory.invalidate_locked_name(&target.name, retired);
        }
        change.finish();
        outcome
    }
}

struct CommandGuard<'a, 'request> {
    attempt: &'a mut dyn LockedAttempt,
    command: &'a NamespaceMutation<'request>,
    target: &'a MutationTarget<'request>,
    retired: &'a MutationRetire,
}

struct CommandSlot<'slot, 'attempt, 'request> {
    guard: &'slot mut CommandGuard<'attempt, 'request>,
    parent: Metadata,
    observed: MutationObservation,
}

enum CommandAuthorization<'slot, 'attempt, 'request> {
    Commit(
        CommandSlot<'slot, 'attempt, 'request>,
        Option<(NodePermission, (u32, u32))>,
    ),
    Existing(CommandSlot<'slot, 'attempt, 'request>),
}

impl<'attempt, 'request> CommandGuard<'attempt, 'request> {
    fn observe(&mut self) -> VfsResult<(Metadata, MutationObservation)> {
        let (parent, _) = self.attempt.parents()?;
        self.target
            .credentials
            .check_permission(&parent, NodePermission::OTHER_EXEC)?;
        let observed = self.attempt.observe(self.command, self.retired)?;
        Ok((parent, observed))
    }

    fn slot(
        &mut self,
        (parent, mut observed): (Metadata, MutationObservation),
    ) -> VfsResult<CommandSlot<'_, 'attempt, 'request>> {
        let candidate = match (&self.command, &mut observed) {
            (NamespaceMutation::Create(_), MutationObservation::Create { target, .. }) => {
                target.as_mut()
            }
            (NamespaceMutation::Link(_), MutationObservation::Link { source, target }) => {
                self.retired.retire(source.entry.clone());
                target.as_mut()
            }
            (NamespaceMutation::Unlink(_), MutationObservation::Unlink(target)) => Some(target),
            _ => return Err(VfsError::BadState),
        };
        let directory = self.target.parent.entry().as_dir()?;
        if let Some(candidate) = candidate {
            candidate.entry = directory.canonicalize_locked(
                &candidate.canonical_name,
                candidate.entry.clone(),
                candidate.cacheable,
                self.retired,
            );
            self.retired.retire(candidate.entry.clone());
        } else {
            directory.invalidate_locked_name(&self.target.name, self.retired);
        }
        #[cfg(feature = "host-test")]
        if matches!(self.command, NamespaceMutation::Unlink(_))
            && let Some(probe) = directory.unlink_probe()
        {
            // After the real candidate publication, before authorization.
            probe.pause_after_publication();
        }
        Ok(CommandSlot {
            guard: self,
            parent,
            observed,
        })
    }
}

impl<'slot, 'attempt, 'request> CommandSlot<'slot, 'attempt, 'request> {
    fn authorize(self) -> VfsResult<CommandAuthorization<'slot, 'attempt, 'request>> {
        let credentials = &self.guard.target.credentials;
        let parent = &self.guard.target.parent;
        match (self.guard.command, &self.observed) {
            (
                NamespaceMutation::Create(request),
                MutationObservation::Create {
                    target,
                    owner_policy,
                },
            ) => {
                if let Some(target) = target {
                    let existing = request.existing.ok_or(VfsError::AlreadyExists)?;
                    if target.entry.node_type() != NodeType::Symlink {
                        credentials.check_permission(&target.metadata, existing.access)?;
                    }
                    return Ok(CommandAuthorization::Existing(self));
                }
                request.write.require(parent)?;
                credentials.check_permission(
                    &self.parent,
                    NodePermission::OTHER_WRITE | NodePermission::OTHER_EXEC,
                )?;
                if let CreateContent::Node(kind, device) = request.attributes.content {
                    if matches!(kind, NodeType::CharacterDevice | NodeType::BlockDevice)
                        && !(kind == NodeType::CharacterDevice && device == DeviceId::default())
                        && !credentials.cap_mknod
                    {
                        return Err(VfsError::OperationNotPermitted);
                    }
                    if matches!(kind, NodeType::Unknown | NodeType::Symlink) {
                        return Err(VfsError::InvalidInput);
                    }
                }
                let (permission, owner) = if let Some(owner) = request.restore_owner {
                    (request.attributes.permission, owner)
                } else {
                    owner_policy.initial_attributes(request.attributes, &self.parent, credentials)
                };
                Ok(CommandAuthorization::Commit(
                    self,
                    Some((permission, owner)),
                ))
            }
            (NamespaceMutation::Link(request), MutationObservation::Link { source, target }) => {
                request.write.write_for(parent)?.check_location(parent)?;
                if target.is_some() {
                    return Err(VfsError::AlreadyExists);
                }
                credentials.check_permission(
                    &self.parent,
                    NodePermission::OTHER_WRITE | NodePermission::OTHER_EXEC,
                )?;
                if source.entry.is_dir() {
                    return Err(VfsError::OperationNotPermitted);
                }
                if !credentials.cap_fowner && credentials.fsuid != source.metadata.uid {
                    let safe = source.entry.node_type() == NodeType::RegularFile
                        && !source.metadata.mode.contains(NodePermission::SET_UID)
                        && !(source.metadata.mode.contains(NodePermission::SET_GID)
                            && source.metadata.mode.contains(NodePermission::GROUP_EXEC))
                        && credentials
                            .check_permission(
                                &source.metadata,
                                NodePermission::OTHER_READ | NodePermission::OTHER_WRITE,
                            )
                            .is_ok();
                    if !safe {
                        return Err(VfsError::OperationNotPermitted);
                    }
                }
                Ok(CommandAuthorization::Commit(self, None))
            }
            (NamespaceMutation::Unlink(request), MutationObservation::Unlink(target)) => {
                request.write.write_for(parent)?.check_location(parent)?;
                credentials.check_permission(
                    &self.parent,
                    NodePermission::OTHER_WRITE | NodePermission::OTHER_EXEC,
                )?;
                credentials.check_sticky(&self.parent, &target.metadata)?;
                match (target.entry.is_dir(), request.directory) {
                    (true, false) => return Err(VfsError::IsADirectory),
                    (false, true) => return Err(VfsError::NotADirectory),
                    _ => {}
                }
                if target.entry.as_dir().is_ok_and(|dir| dir.is_mountpoint()) {
                    return Err(VfsError::ResourceBusy);
                }
                if request.directory && !target.empty {
                    return Err(VfsError::DirectoryNotEmpty);
                }
                Ok(CommandAuthorization::Commit(self, None))
            }
            _ => Err(VfsError::BadState),
        }
    }
}

impl CommandAuthorization<'_, '_, '_> {
    fn commit(self) -> NamespaceOutcome<NamespaceEffect> {
        let (slot, attributes) = match self {
            Self::Existing(slot) => {
                let MutationObservation::Create {
                    target: Some(target),
                    ..
                } = &slot.observed
                else {
                    unreachable!()
                };
                return NamespaceOutcome::unchanged_observed(
                    NamespaceEffect::Create(CreateEffect {
                        selected: target.selected(),
                        created: false,
                    }),
                    Ok(()),
                );
            }
            Self::Commit(slot, attributes) => (slot, attributes),
        };
        let authorized = match (slot.guard.command, &slot.observed) {
            (
                NamespaceMutation::Create(request),
                MutationObservation::Create { target: None, .. },
            ) => {
                let (permission, owner) = attributes.expect("authorized creation attributes");
                AuthorizedMutation::Create(AuthorizedCreate {
                    request,
                    permission,
                    owner,
                })
            }
            (
                NamespaceMutation::Link(request),
                MutationObservation::Link {
                    source,
                    target: None,
                },
            ) => AuthorizedMutation::Link(AuthorizedLink { request, source }),
            (NamespaceMutation::Unlink(request), MutationObservation::Unlink(target)) => {
                AuthorizedMutation::Unlink(AuthorizedUnlink { request, target })
            }
            _ => unreachable!("command-specific authorization"),
        };
        let outcome = slot.guard.attempt.commit(&authorized, slot.guard.retired);
        if let (MutationObservation::Link { source, .. }, Some(NamespaceEffect::Link(alias))) =
            (&slot.observed, outcome.observation())
        {
            // Alias-local metadata must share the existing content-cache owner.
            // Retire any previous value rather than destroying it under a gate.
            let shared = source.entry.user_data().clone();
            let old = core::mem::replace(&mut *alias.entry.user_data(), shared);
            slot.guard.retired.retire(old);
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::*;

    #[test]
    fn backend_creation_policy_chooses_owner_and_directory_mode() {
        let parent = Metadata {
            device: 1,
            inode: 1,
            nlink: 2,
            mode: NodePermission::from_bits_truncate(0o2777),
            node_type: NodeType::Directory,
            uid: 0,
            gid: 23,
            size: 0,
            block_size: 4096,
            blocks: 0,
            rdev: DeviceId::default(),
            atime: Duration::ZERO,
            mtime: Duration::ZERO,
            ctime: Duration::ZERO,
        };
        let credentials = MutationCredentials {
            fsuid: 1000,
            fsgid: 1001,
            ..MutationCredentials::root()
        };
        let observations = [
            (CreateOwnerPolicy::ParentSetgid, 0o755),
            (CreateOwnerPolicy::Caller, 0o7755),
        ]
        .map(|(policy, mode)| {
            let (permission, owner) = policy.initial_attributes(
                CreateAttributes {
                    content: CreateContent::Node(NodeType::Directory, DeviceId::default()),
                    permission: NodePermission::from_bits_truncate(mode),
                },
                &parent,
                &credentials,
            );
            (permission.bits(), owner)
        });

        assert_eq!(observations, [(0o2755, (1000, 23)), (0o1755, (1000, 1001))]);
    }
}
