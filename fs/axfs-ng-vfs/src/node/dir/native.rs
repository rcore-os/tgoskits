//! A closed continuation of one actual native unlink observation.

use alloc::boxed::Box;

use super::{cache::NameChange, *};
use crate::{
    Location, Metadata, MutationCredentials, NamespaceAdmission, PreparedUnlink, PreparedWrite,
};

/// Backend implementation boundary, never returned by a public raw factory.
/// A driver owns its parents, while its gate borrows the external FS receiver.
pub trait NamespaceDriver: LockedAttempt {
    /// End backend exclusion and transfer its actual typed completion state.
    fn finish(self: Box<Self>, outcome: NamespaceOutcome<NamespaceEffect>) -> NamespaceCompletion;
}

/// Start can require completion even before a target has been observed.
#[must_use = "resume or cancel observations, then complete outside every gate"]
pub enum NamespaceStart<'a> {
    Observed(Box<ObservedUnlink<'a>>),
    Complete(NamespaceCompletion),
}

impl NamespaceStart<'_> {
    /// Ordinary native operations immediately consume the same fixed core.
    pub fn resume(self) -> NamespaceCompletion {
        match self {
            Self::Observed(observed) => observed.resume(),
            Self::Complete(completion) => completion,
        }
    }
}

/// An unforgeable, owner-bound backend request. Only VFS admission constructs
/// it; supplying a different parent, observation or commit is not an option.
pub struct NamespaceDriverRequest<'a> {
    command: NamespaceMutation<'a>,
    admission: &'a NamespaceAdmission<'a>,
    retired: &'a MutationRetire,
}

impl<'a> NamespaceDriverRequest<'a> {
    pub fn command(&self) -> &NamespaceMutation<'a> {
        &self.command
    }

    pub fn admission(&self) -> &NamespaceAdmission<'a> {
        self.admission
    }

    pub fn retired(&self) -> &'a MutationRetire {
        self.retired
    }

    /// Completes a Staging or pre-observation failure with its real typed cause.
    pub fn complete(self, completion: NamespaceCompletion) -> NamespaceStart<'a> {
        NamespaceStart::Complete(
            completion
                .retain_parents(self.command.parents())
                .bind(self.admission.owner),
        )
    }

    pub fn fail(self, error: VfsError) -> NamespaceStart<'a> {
        self.complete(NamespaceCompletion::new(
            NamespaceOutcome::unchanged(Err(error)),
            None,
        ))
    }

    /// Backend-only handoff. The caller cannot obtain a request except through
    /// the bound FS receiver. No raw driver is returned to the operation caller.
    pub fn observe(self, mut driver: Box<dyn NamespaceDriver + 'a>) -> NamespaceStart<'a> {
        if !matches!(self.command, NamespaceMutation::Unlink(_)) {
            let outcome = self.command.execute(&mut *driver, self.retired);
            return self.complete(driver.finish(outcome));
        }
        let observed = match self.command.observe_locked(&mut *driver, self.retired) {
            Ok(observed) => observed,
            Err(error) => {
                return self.complete(driver.finish(NamespaceOutcome::unchanged(Err(error))));
            }
        };
        // FAT has sampled old holders and retained this round's candidate.
        // Establish the writer boundary immediately, before any publication.
        let change = self.command.begin_observed_change(self.retired);
        NamespaceStart::Observed(Box::new(ObservedUnlink {
            request: Some(self),
            driver: Some(driver),
            observed: Some(observed),
            change: Some(change),
        }))
    }
}

/// One non-cloneable native observation. Only snapshots and consuming actions
/// are exposed; the original driver and unconsumed input remain private.
///
/// ```compile_fail
/// fn twice(observed: Box<axfs_ng_vfs::ObservedUnlink<'_>>) {
///     let _first = observed.resume();
///     let _second = observed.resume();
/// }
/// ```
///
/// The real admission/receiver borrow cannot escape its owner:
///
/// ```compile_fail
/// fn escape<'a>(
///     root: &'a axfs_ng_vfs::Location,
///     write: &'a axfs_ng_vfs::PreparedWrite,
///     retired: &'a axfs_ng_vfs::MutationRetire,
/// ) -> axfs_ng_vfs::NamespaceStart<'a> {
///     let prepared = root.prepare_unlink("entry").unwrap();
///     let admissions = axfs_ng_vfs::NamespaceAdmissions::acquire(write).unwrap();
///     admissions.for_location(root).unwrap().unlink(
///         prepared, false, &axfs_ng_vfs::MutationCredentials::root(), write, retired,
///     )
/// }
/// ```
pub struct ObservedUnlink<'a> {
    // End publication before dropping the real backend driver on cancellation.
    change: Option<NameChange<'a>>,
    driver: Option<Box<dyn NamespaceDriver + 'a>>,
    observed: Option<(Metadata, MutationObservation)>,
    request: Option<NamespaceDriverRequest<'a>>,
}

impl ObservedUnlink<'_> {
    pub fn parent(&self) -> &Metadata {
        &self.observed.as_ref().expect("unconsumed observation").0
    }

    pub fn target(&self) -> &RenameNode {
        match &self.observed.as_ref().expect("unconsumed observation").1 {
            MutationObservation::Unlink(target) => target,
            _ => unreachable!("native unlink observation"),
        }
    }

    pub fn resume(mut self: Box<Self>) -> NamespaceCompletion {
        let request = self.request.take().expect("unconsumed request");
        let mut driver = self.driver.take().expect("unconsumed driver");
        let outcome = request.command.resume_locked(
            &mut *driver,
            request.retired,
            self.observed.take().expect("unconsumed observation"),
            self.change.take().expect("open publication"),
        );
        driver
            .finish(outcome)
            .retain_parents(request.command.parents())
            .bind(request.admission.owner)
    }

    pub fn cancel(mut self: Box<Self>) -> NamespaceCompletion {
        let request = self.request.take().expect("unconsumed request");
        self.retire_observation(request.retired);
        drop(self.change.take());
        self.driver
            .take()
            .expect("unconsumed driver")
            .finish(NamespaceOutcome::unchanged(Ok(())))
            .retain_parents(request.command.parents())
            .bind(request.admission.owner)
    }

    fn retire_observation(&mut self, retired: &MutationRetire) {
        if let Some((_, MutationObservation::Unlink(target))) = self.observed.take() {
            retired.retire(target);
        }
    }
}

impl Drop for ObservedUnlink<'_> {
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            self.retire_observation(request.retired);
        }
        // Drop only releases publication and gate rights. Explicit cancellation
        // returns typed completion; Drop never progresses a journal or reaps.
    }
}

impl NamespaceAdmission<'_> {
    /// Executes a data command with the existing same-FS admission. This is also
    /// the boundary for subsequent create/whiteout commands in one outer gate.
    pub fn start<'a>(
        &'a self,
        command: NamespaceMutation<'a>,
        retired: &'a MutationRetire,
    ) -> NamespaceStart<'a> {
        let (source, destination) = command.parents();
        // Validation failures also retire potentially cross-FS parent owners.
        retired.retire(source.clone());
        retired.retire(destination.clone());
        if let Err(error) = self
            .check_owner(source.filesystem())
            .and_then(|()| self.check_owner(destination.filesystem()))
        {
            return NamespaceStart::Complete(
                NamespaceCompletion::new(NamespaceOutcome::unchanged(Err(error)), None)
                    .retain_parents(command.parents())
                    .bind(self.owner),
            );
        }
        // Borrowed commands and drivers cannot be put in the Any retirement bag.
        // Retain only their owned parent nodes until all admissions have ended.
        self.owner.begin_namespace(NamespaceDriverRequest {
            command,
            admission: self,
            retired,
        })
    }

    /// Starts one prepared unlink without observing the target in advance.
    pub fn unlink<'a>(
        &'a self,
        prepared: PreparedUnlink,
        directory: bool,
        credentials: &MutationCredentials<'a>,
        write: &'a PreparedWrite,
        retired: &'a MutationRetire,
    ) -> NamespaceStart<'a> {
        let parent = prepared.parent_owner();
        let target = super::command::MutationTarget::new(&parent, prepared.name(), credentials);
        // Preparation owns another parent reference; keep that reference out
        // of all nested destructors even on failed input validation.
        retired.retire(parent);
        match target {
            Ok(target) => self.start(
                NamespaceMutation::Unlink(UnlinkRequest::new(target, directory, prepared, write)),
                retired,
            ),
            Err(error) => NamespaceStart::Complete(
                NamespaceCompletion::new(NamespaceOutcome::unchanged(Err(error)), None)
                    .bind(self.owner),
            ),
        }
    }

    /// Creates using the fixed existing authorizer and the same operation right.
    pub fn create<'a>(
        &'a self,
        parent: &Location,
        name: &str,
        attributes: CreateAttributes<'a>,
        credentials: &MutationCredentials<'a>,
        write: &'a PreparedWrite,
        retired: &'a MutationRetire,
    ) -> NamespaceStart<'a> {
        match super::command::MutationTarget::new(parent, name, credentials) {
            Ok(target) => self.start(
                NamespaceMutation::Create(CreateRequest::new(
                    target,
                    attributes,
                    None,
                    None,
                    CreationWrite::Ready(write),
                )),
                retired,
            ),
            Err(error) => NamespaceStart::Complete(
                NamespaceCompletion::new(NamespaceOutcome::unchanged(Err(error)), None)
                    .bind(self.owner),
            ),
        }
    }
}
