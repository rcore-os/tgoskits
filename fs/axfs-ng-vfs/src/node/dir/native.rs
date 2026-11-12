//! Controlled native commands and the closed continuation of an unlink observation.
//!
//! Scope/context entries retain complete inputs before reservation or validation;
//! these private startup helpers must not perform duplicate input retirement.

use alloc::boxed::Box;

use super::{cache::NameChange, *};
use crate::{
    Metadata, MutationCredentials, NamespaceAdmission, NamespaceObservation,
    NamespaceOperationContext, PreparedUnlink, namespace_operation::registration::Reservation,
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
    Complete(NamespaceObservation<NamespaceEffect>),
}

impl NamespaceStart<'_> {
    /// Ordinary native operations immediately consume the same fixed core.
    pub fn resume(self) -> NamespaceObservation<NamespaceEffect> {
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
    context: &'a NamespaceOperationContext<'a>,
    reservation: Reservation<'a>,
}

impl<'a> NamespaceDriverRequest<'a> {
    pub fn command(&self) -> &NamespaceMutation<'a> {
        &self.command
    }

    pub fn admission(&self) -> &NamespaceAdmission<'a> {
        self.context
            .admission_for_owner(self.command.parents().0.filesystem())
            .expect("validated receiver")
    }

    pub fn retired(&self) -> &'a MutationRetire {
        self.context.retired()
    }

    pub fn context(&self) -> &NamespaceOperationContext<'a> {
        self.context
    }

    /// Borrows direct backing reads bound to this request's actual receiver.
    /// The view borrows the operation, so the request can still be consumed by
    /// `observe`/`complete`; it is not a driver or authorization capability.
    pub fn backing_queries(&self) -> crate::BackingQueryContext<'a> {
        crate::BackingQueryContext::new(
            self.context,
            self.context
                .admission_for_owner(self.command.parents().0.filesystem())
                .expect("validated receiver"),
        )
    }

    /// Completes a Staging or pre-observation failure with its real typed cause.
    pub fn complete(self, completion: NamespaceCompletion) -> NamespaceStart<'a> {
        let completion = completion
            .retain_parents(self.command.parents())
            .bind(self.admission().owner);
        NamespaceStart::Complete(self.reservation.namespace(completion))
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
            let outcome = self.command.execute(&mut *driver, self.context.retired());
            return self.complete(driver.finish(outcome));
        }
        let observed = match self
            .command
            .observe_locked(&mut *driver, self.context.retired())
        {
            Ok(observed) => observed,
            Err(error) => {
                return self.complete(driver.finish(NamespaceOutcome::unchanged(Err(error))));
            }
        };
        // FAT has sampled old holders and retained this round's candidate.
        // Establish the writer boundary immediately, before any publication.
        let change = self.command.begin_observed_change(self.context.retired());
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
///     credentials: &'a axfs_ng_vfs::MutationCredentials<'a>,
/// ) -> axfs_ng_vfs::NamespaceStart<'a> {
///     let prepared = root.prepare_unlink("entry").unwrap();
///     let scope = axfs_ng_vfs::NamespaceOperationScope::acquire(write).unwrap();
///     let context = scope.context();
///     scope.unlink(&context, prepared, false, credentials).unwrap()
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

    pub fn resume(mut self: Box<Self>) -> NamespaceObservation<NamespaceEffect> {
        let request = self.request.take().expect("unconsumed request");
        let mut driver = self.driver.take().expect("unconsumed driver");
        let outcome = request.command.resume_locked(
            &mut *driver,
            request.context.retired(),
            self.observed.take().expect("unconsumed observation"),
            self.change.take().expect("open publication"),
        );
        let completion = driver
            .finish(outcome)
            .retain_parents(request.command.parents())
            .bind(request.admission().owner);
        request.reservation.namespace(completion)
    }

    pub fn cancel(mut self: Box<Self>) -> NamespaceObservation<NamespaceEffect> {
        let request = self.request.take().expect("unconsumed request");
        self.retire_observation(request.context.retired());
        drop(self.change.take());
        let completion = self
            .driver
            .take()
            .expect("unconsumed driver")
            .finish(NamespaceOutcome::unchanged(Ok(())))
            .retain_parents(request.command.parents())
            .bind(request.admission().owner);
        request.reservation.namespace(completion)
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
            self.retire_observation(request.context.retired());
        }
        // Drop only releases publication and gate rights. Explicit cancellation
        // returns typed completion; Drop never progresses a journal or reaps.
    }
}

pub(crate) fn start_unlink<'a>(
    context: &'a NamespaceOperationContext<'a>,
    prepared: PreparedUnlink,
    directory: bool,
    credentials: &MutationCredentials<'a>,
    reservation: Reservation<'a>,
) -> VfsResult<NamespaceStart<'a>> {
    let parent = prepared.parent_owner();
    let target = super::command::MutationTarget::new(&parent, prepared.name(), credentials)?;
    context.write_for(&parent)?.check_location(&parent)?;
    let command = NamespaceMutation::Unlink(UnlinkRequest::new(
        target,
        directory,
        prepared,
        context.write()?,
    ));
    start_command(context, &parent, command, reservation)
}

pub(crate) fn start_link<'a>(
    context: &'a NamespaceOperationContext<'a>,
    parent: &'a crate::Location,
    name: &str,
    source: &'a crate::Location,
    credentials: &MutationCredentials<'a>,
    reservation: Reservation<'a>,
) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
    if !Arc::ptr_eq(parent.mountpoint(), source.mountpoint()) {
        return Err(VfsError::CrossesDevices);
    }
    let target = super::command::MutationTarget::new(parent, name, credentials)?;
    let write = context.write_for(parent)?;
    write.check_location(parent)?;
    let owner = write.filesystem() as *const dyn crate::FilesystemOps as *const ();
    for input in [parent, source] {
        if input.filesystem() as *const dyn crate::FilesystemOps as *const () != owner {
            return Err(VfsError::CrossesDevices);
        }
    }
    let command = NamespaceMutation::Link(LinkRequest::new(target, source, context.write()?));
    Ok(start_command(context, parent, command, reservation)?.resume())
}

pub(crate) fn start_create<'a>(
    context: &'a NamespaceOperationContext<'a>,
    parent: &'a crate::Location,
    name: &str,
    attributes: CreateAttributes<'a>,
    selection: super::command::CreateSelection,
    credentials: &MutationCredentials<'a>,
    reservation: Reservation<'a>,
) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
    let target = super::command::MutationTarget::new(parent, name, credentials)?;
    parent.entry().as_dir()?.ensure_linked()?;
    let write = context.creation_write(parent)?;
    let command = NamespaceMutation::Create(selection.request(target, attributes, write));
    Ok(start_command(context, parent, command, reservation)?.resume())
}

pub(crate) fn start_rename<'a>(
    context: &'a NamespaceOperationContext<'a>,
    source: (&'a crate::Location, &'a str),
    destination: (&'a crate::Location, &'a str),
    options: RenameOptions,
    credentials: &'a MutationCredentials<'a>,
    reservation: Reservation<'a>,
) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
    if !Arc::ptr_eq(source.0.mountpoint(), destination.0.mountpoint()) {
        return Err(VfsError::CrossesDevices);
    }
    let command = NamespaceMutation::Rename(RenameRequest::new(
        source,
        destination,
        options,
        credentials,
        context.write()?,
    )?);
    Ok(start_command(context, source.0, command, reservation)?.resume())
}

/// The only native startup path: no registry borrow spans a backend callback.
fn start_command<'a>(
    context: &'a NamespaceOperationContext<'a>,
    receiver: &crate::Location,
    command: NamespaceMutation<'a>,
    mut reservation: Reservation<'a>,
) -> VfsResult<NamespaceStart<'a>> {
    let admission = context.admission(receiver)?;
    reservation.start();
    Ok(admission.owner.begin_namespace(NamespaceDriverRequest {
        command,
        context,
        reservation,
    }))
}
