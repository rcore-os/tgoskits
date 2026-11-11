//! Current-inode metadata intentions and the fixed authorization protocol.

mod authorize;

use core::time::Duration;

use crate::{
    Location, Metadata, MutationCredentials, MutationRetire, NamespaceAdmission,
    NamespaceAdmissions, NamespaceCompletion, NamespaceOutcome, NodePermission, PreparedWrite,
    VfsResult,
};

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
    ctime: Duration,
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

    /// Returns the real clock value selected for this locked attempt.
    pub const fn ctime(&self) -> Duration {
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
        metadata.ctime = self.ctime;
    }
}

/// Opaque request owning the original target and borrowing outer rights.
/// Only the VFS can construct or dispatch it; backends execute it on the
/// receiver's real owner, not a substituted inode.
pub struct MetadataRequest<'a> {
    target: Location,
    change: MetadataChange,
    credentials: MutationCredentials<'a>,
    write: &'a PreparedWrite,
    admission: &'a NamespaceAdmission<'a>,
    retired: &'a MutationRetire,
}

impl MetadataRequest<'_> {
    /// Classifies the original intent for native namespace exclusion.
    /// Chown(None, None) can still clear setid bits; Times never changes owners.
    pub const fn changes_permissions_or_ownership(&self) -> bool {
        matches!(
            self.change,
            MetadataChange::Chmod(_) | MetadataChange::Chown { .. }
        )
    }

    /// One fresh observe/authorize/commit attempt. Retrying backends must end
    /// this borrow, release their locks, and call again with a fresh attempt.
    pub fn execute(&self, attempt: &mut dyn LockedMetadataAttempt) -> NamespaceOutcome<Metadata> {
        let observed = match attempt.observe() {
            Ok(observed) => observed,
            Err(error) => return NamespaceOutcome::unchanged(Err(error)),
        };
        let now = attempt.wall_time();
        match self.authorize(&observed, now) {
            Ok(authorized) => attempt.commit(authorized),
            Err(error) => NamespaceOutcome::unchanged_observed(observed, Err(error)),
        }
    }

    /// Borrows the already-prepared backing write rights for layered owners.
    pub fn write_authority(&self) -> &PreparedWrite {
        self.write
    }

    /// Borrows the same operation admission, without entering it again.
    pub fn admission(&self) -> &NamespaceAdmission<'_> {
        self.admission
    }

    /// Borrows the outer retirement bag; inner owners cannot clear it.
    pub fn retired(&self) -> &MutationRetire {
        self.retired
    }
}

impl NamespaceAdmission<'_> {
    /// Executes using existing rights and returns typed completion responsibility.
    /// Empty times return None before even validating or retaining the target.
    /// All nonempty targets survive validation failures and backend failures
    /// until the outer retirement bag is dropped after every admission.
    pub fn change_metadata(
        &self,
        target: &Location,
        change: MetadataChange,
        credentials: &MutationCredentials<'_>,
        write: &PreparedWrite,
        retired: &MutationRetire,
    ) -> VfsResult<Option<NamespaceCompletion<Metadata>>> {
        if change.is_empty() {
            return Ok(None);
        }
        retired.retire(target.clone());
        write.write_for(target)?.check_location(target)?;
        self.check_owner(target.filesystem())?;
        let request = MetadataRequest {
            target: target.clone(),
            change,
            credentials: *credentials,
            write,
            admission: self,
            retired,
        };
        let completion = request.target.entry().change_metadata(&request);
        Ok(Some(completion.bind(self.owner)))
    }
}

impl Location {
    /// Changes metadata under the actual inode owner with explicit credentials.
    /// All-Keep times require neither write authority nor an inode observation.
    /// Other requests finish pressure, release all admissions, retire owners,
    /// then finish backend work before converting to an ordinary result.
    pub fn change_metadata(
        &self,
        change: MetadataChange,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        if change.is_empty() {
            return Ok(());
        }
        let write = PreparedWrite::acquire(self)?;
        let retired = MutationRetire::new();
        let admissions = NamespaceAdmissions::acquire(&write)?;
        let completion = admissions
            .for_location(self)?
            .change_metadata(self, change, credentials, &write, &retired)?
            .expect("nonempty metadata command");
        let mut deferred = admissions
            .pressure(alloc::vec![completion])
            .unwrap_or_else(|_| unreachable!("fixed metadata dispatch binds the admitted owner"));
        drop(admissions);
        drop(retired);
        deferred
            .pop()
            .expect("one metadata completion")
            .finish()
            .into_result()
    }
}
