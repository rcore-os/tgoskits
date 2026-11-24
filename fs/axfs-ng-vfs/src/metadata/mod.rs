//! Current-inode metadata intentions and the fixed authorization protocol.

mod authorize;
mod forward;
mod restore;

use core::time::Duration;

use crate::{
    DeviceId, Location, Metadata, MetadataUpdate, MutationCredentials, MutationRetire,
    NamespaceAdmission, NamespaceCompletion, NamespaceOperationContext, NamespaceOutcome,
    NamespaceState, NodePermission, PreparedWrite, VfsResult,
};

/// Access-time work required by a mount before following a symbolic link.
///
/// The mount supplies the policy; the producer must also account for its
/// native inode restrictions and timestamp precision before opting into a
/// cache-only read. This predicate does not authorize a metadata update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessTimePolicy {
    /// Mount or shared-filesystem state suppresses access-time work.
    NoAtime,
    /// Update after metadata changes or a full day of whole Unix seconds.
    Relative,
    /// Update whenever the current timestamp differs from the wall clock.
    Strict,
}

impl AccessTimePolicy {
    /// Tests current inode timestamps against an already sampled wall clock.
    /// Relative age uses whole Unix seconds, including the one-day boundary.
    pub fn needs_update(self, metadata: &Metadata, now: Duration) -> bool {
        if self == Self::NoAtime || metadata.atime == now {
            return false;
        }
        self == Self::Strict
            || metadata.mtime >= metadata.atime
            || metadata.ctime >= metadata.atime
            || now.as_secs().saturating_sub(metadata.atime.as_secs()) >= 24 * 60 * 60
    }
}

/// An inode timestamp intention, resolved only inside the real owner lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeChange {
    Keep,
    Now,
    Set(Duration),
}

/// A user metadata intention, never a patch made from an earlier snapshot.
#[derive(Clone, Copy, Debug)]
pub enum MetadataChange {
    Chmod(NodePermission),
    Chown {
        uid: Option<u32>,
        gid: Option<u32>,
    },
    Times {
        atime: TimeChange,
        mtime: TimeChange,
    },
}

/// Internal origin is selected by a capability-bearing factory, never a flag.
#[derive(Clone)]
pub(crate) enum MetadataIntent {
    User(MetadataChange),
    Forwarded(forward::ForwardedChange),
    StagingRestoration(MetadataUpdate),
    Initialization(MetadataUpdate),
}

impl MetadataIntent {
    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Self::User(change) => change.is_empty(),
            Self::Forwarded(_) => false,
            Self::Initialization(update) | Self::StagingRestoration(update) => {
                update.mode.is_none()
                    && update.owner.is_none()
                    && update.rdev.is_none()
                    && update.atime.is_none()
                    && update.mtime.is_none()
            }
        }
    }
}

impl MetadataChange {
    /// Only two omitted timestamps are empty; chown(None, None) is not.
    pub fn is_empty(&self) -> bool {
        matches!(
            self,
            Self::Times {
                atime: TimeChange::Keep,
                mtime: TimeChange::Keep
            }
        )
    }
}

/// Backend-owned, stack-local access to one inode under its real exclusion.
/// Implementations must retain that exclusion through observe, time and commit.
/// No ordinary VFS operation or raw driver is exposed by this protocol.
pub trait LockedMetadataAttempt {
    fn observe(&mut self) -> VfsResult<Metadata>;
    fn wall_time(&mut self) -> Duration;
    fn commit(&mut self, change: AuthorizedMetadataChange<'_>) -> NamespaceOutcome<Metadata>;
}

/// A fixed-authorizer-only capability, consumed by the same locked attempt.
/// Its lifetime ends inside `MetadataRequest::execute`; it cannot be retained
/// for a retry or exported as a caller-created final patch.
pub struct AuthorizedMetadataChange<'a> {
    _request: &'a MetadataRequest<'a>,
    mode: Option<NodePermission>,
    uid: Option<u32>,
    gid: Option<u32>,
    atime: Option<Duration>,
    mtime: Option<Duration>,
    rdev: Option<DeviceId>,
    ctime: Option<Duration>,
    removal: forward::SetidRemoval,
}

impl AuthorizedMetadataChange<'_> {
    /// Returns the final permission update, preserving whether it participates.
    pub const fn mode(&self) -> Option<NodePermission> {
        self.mode
    }

    /// Returns the requested UID field, including an explicit same-value update.
    pub const fn uid(&self) -> Option<u32> {
        self.uid
    }

    /// Returns the requested GID field, without filling an omitted group.
    pub const fn gid(&self) -> Option<u32> {
        self.gid
    }

    /// Returns the resolved access time, or None when it must remain untouched.
    pub const fn atime(&self) -> Option<Duration> {
        self.atime
    }

    /// Returns the resolved modification time, preserving field participation.
    pub const fn mtime(&self) -> Option<Duration> {
        self.mtime
    }

    /// Returns the approved initialization of a special inode's device number.
    pub const fn rdev(&self) -> Option<DeviceId> {
        self.rdev
    }

    /// Returns the implicit user-command timestamp. Initialization leaves the
    /// field absent, preserving its existing native precision and value.
    pub const fn ctime(&self) -> Option<Duration> {
        self.ctime
    }

    /// Faithfully applies participating fields to the locked inode metadata.
    /// Backends must not add their own implicit chown/setid policy.
    pub fn apply(self, metadata: &mut Metadata) {
        if let Some(mode) = self.mode {
            metadata.mode = mode;
        }
        if let Some(uid) = self.uid {
            metadata.uid = uid;
        }
        if let Some(gid) = self.gid {
            metadata.gid = gid;
        }
        if let Some(atime) = self.atime {
            metadata.atime = atime;
        }
        if let Some(mtime) = self.mtime {
            metadata.mtime = mtime;
        }
        if let Some(rdev) = self.rdev {
            metadata.rdev = rdev;
        }
        if let Some(ctime) = self.ctime {
            metadata.ctime = ctime;
        }
    }
}

/// Opaque request owning the original target and borrowing outer rights.
/// Only the VFS can construct or dispatch it; backends execute it on the
/// receiver's real owner, not a substituted inode.
pub struct MetadataRequest<'a> {
    target: Location,
    intent: MetadataIntent,
    credentials: MutationCredentials<'a>,
    context: &'a NamespaceOperationContext<'a>,
}

impl<'a> MetadataRequest<'a> {
    /// Borrows the actual receiver alias retained by the original command.
    /// Naming relationships must still be observed afresh under its owner gate.
    pub fn target(&self) -> &Location {
        &self.target
    }

    /// Borrows direct backing reads bound to the original target's receiver,
    /// not to this request's address or to a caller-selected filesystem.
    pub fn backing_queries(&self) -> crate::BackingQueryContext<'a> {
        crate::BackingQueryContext::new(
            self.context,
            self.context
                .admission(&self.target)
                .expect("validated receiver"),
        )
    }

    /// Borrows only the target receiver's command-local preparation plans.
    pub fn preparations(&self) -> crate::NamespacePreparationContext<'a> {
        self.context.preparations_for(self.admission().owner)
    }

    /// Classifies the original intent for native namespace exclusion.
    /// Chown(None, None) can still clear setid bits; Times never changes owners.
    pub const fn changes_permissions_or_ownership(&self) -> bool {
        match &self.intent {
            MetadataIntent::User(change) => matches!(
                change,
                MetadataChange::Chmod(_) | MetadataChange::Chown { .. }
            ),
            MetadataIntent::Forwarded(change) => change.changes_permissions_or_ownership(),
            MetadataIntent::Initialization(update) | MetadataIntent::StagingRestoration(update) => {
                update.mode.is_some() || update.owner.is_some() || update.rdev.is_some()
            }
        }
    }

    /// One fresh observe/authorize/commit attempt. Retrying backends must end
    /// this borrow, release their locks, and call again with a fresh attempt.
    pub fn execute(&self, attempt: &mut dyn LockedMetadataAttempt) -> NamespaceOutcome<Metadata> {
        let observed = match attempt.observe() {
            Ok(observed) => observed,
            Err(error) => return NamespaceOutcome::unchanged(Err(error)),
        };
        match self.authorize(&observed, attempt) {
            Ok(authorized) => attempt.commit(authorized),
            Err(error) => NamespaceOutcome::unchanged_observed(observed, Err(error)),
        }
    }

    /// Borrows fixed backing commands, never the operation's completion owner.
    pub fn context(&self) -> &NamespaceOperationContext<'_> {
        self.context
    }

    /// Borrows the same operation admission, without entering it again.
    pub fn admission(&self) -> &NamespaceAdmission<'_> {
        self.context
            .admission(&self.target)
            .expect("validated receiver")
    }

    /// Borrows the outer retirement bag; inner owners cannot clear it.
    pub fn retired(&self) -> &MutationRetire {
        self.context.retired()
    }
}

pub(crate) fn dispatch(
    context: &NamespaceOperationContext<'_>,
    target: &Location,
    intent: MetadataIntent,
    credentials: &MutationCredentials<'_>,
) -> NamespaceCompletion<Metadata> {
    let request = MetadataRequest {
        target: target.clone(),
        intent,
        credentials: *credentials,
        context,
    };
    request.target.entry().change_metadata(&request)
}

impl Location {
    /// Changes metadata under the actual inode owner with explicit credentials.
    /// All-Keep times require neither write authority nor an inode observation.
    /// Other requests finish pressure, release all admissions, retire owners,
    /// then finish backend work before converting to an ordinary result.
    /// A completed Retry starts a fresh observation and authorization attempt.
    pub fn change_metadata(
        &self,
        change: MetadataChange,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        self.run_metadata(MetadataIntent::User(change), credentials)
    }

    fn run_metadata(
        &self,
        intent: MetadataIntent,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        if intent.is_empty() {
            return Ok(());
        }
        let write = PreparedWrite::acquire(self)?;
        self.run_metadata_prepared(intent, credentials, &write)
    }

    pub(crate) fn run_metadata_prepared(
        &self,
        intent: MetadataIntent,
        credentials: &MutationCredentials<'_>,
        write: &PreparedWrite,
    ) -> VfsResult<()> {
        if intent.is_empty() {
            return Ok(());
        }
        crate::NamespaceCommandScope::run_write(write, |command| {
            loop {
                let scope = command.attempt::<Metadata>()?;
                {
                    let context = scope.context();
                    scope.apply_metadata_intent(&context, self, intent.clone(), credentials)?;
                }
                let outcome = scope.finish().unwrap_or_else(|_| {
                    unreachable!("fixed metadata dispatch binds the admitted owner")
                });
                if outcome.state() != NamespaceState::Retry {
                    return Ok(outcome);
                }
                command.prepare()?;
            }
        })?
        .into_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_time_policy_uses_inode_order_and_whole_second_age() {
        let mut metadata = Metadata {
            device: 0,
            inode: 1,
            nlink: 1,
            mode: NodePermission::default(),
            node_type: crate::NodeType::Symlink,
            uid: 0,
            gid: 0,
            size: 0,
            block_size: 4096,
            blocks: 0,
            rdev: DeviceId(0),
            atime: Duration::new(86_401, 999_999_999),
            mtime: Duration::ZERO,
            ctime: Duration::ZERO,
        };
        let now = Duration::from_secs(172_801);
        assert!(!AccessTimePolicy::Relative.needs_update(&metadata, now - Duration::from_secs(1)));
        // Linux compares tv_sec: the subsecond part does not delay this edge.
        assert!(AccessTimePolicy::Relative.needs_update(&metadata, now));
        assert!(!AccessTimePolicy::NoAtime.needs_update(&metadata, now));

        metadata.atime = now + Duration::from_secs(1);
        assert!(!AccessTimePolicy::Relative.needs_update(&metadata, now));
        assert!(AccessTimePolicy::Strict.needs_update(&metadata, now));
        metadata.mtime = metadata.atime;
        assert!(AccessTimePolicy::Relative.needs_update(&metadata, now));
        metadata.mtime = Duration::ZERO;
        metadata.ctime = metadata.atime;
        assert!(AccessTimePolicy::Relative.needs_update(&metadata, now));

        metadata.atime = now;
        assert!(!AccessTimePolicy::Relative.needs_update(&metadata, now));
        assert!(!AccessTimePolicy::Strict.needs_update(&metadata, now));
    }
}
