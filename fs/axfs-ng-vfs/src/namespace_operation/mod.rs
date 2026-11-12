//! One stack-local owner for recursively returned, typed completion work.

mod backing_query;
pub use backing_query::{BackingQueryContext, BackingQueryRequest};
pub(crate) mod registration;
#[cfg(test)]
pub(crate) mod tests;

use alloc::vec::Vec;
use core::cell::RefCell;

use registration::{Registration, Reservation};

use crate::{
    Location, Metadata, MetadataChange, MountWriteGuard, MutationCredentials, MutationRetire,
    NamespaceAdmission, NamespaceAdmissions, NamespaceCompletion, NamespaceEffect,
    NamespaceOutcome, NamespaceQuery, NamespaceRead, NamespaceStart, NamespaceState,
    PreparedUnlink, PreparedWrite, RenameOptions, VfsError, VfsResult,
    prepared_write::PreparedView,
};

/// An owned snapshot, never a transferable completion or write capability.
#[derive(Debug, Clone)]
pub struct NamespaceObservation<T> {
    outcome: NamespaceOutcome<T>,
}

impl<T> NamespaceObservation<T> {
    pub fn state(&self) -> NamespaceState {
        self.outcome.state()
    }

    pub fn observation(&self) -> Option<&T> {
        self.outcome.observation()
    }

    pub fn result(&self) -> VfsResult<()> {
        self.outcome.result()
    }

    pub(crate) fn capture(outcome: &NamespaceOutcome<T>) -> Self
    where
        T: Clone,
    {
        Self {
            outcome: outcome.clone(),
        }
    }
}

/// Owns all four completion phases of one attempt, borrowing external preparation.
///
/// Drop releases admissions before any retired or pending owner, but does not
/// run pressure, finish, rollback or a new namespace commit. A failed owner
/// preflight returns this entire owner intact and permits borrowing it again.
pub struct NamespaceOperationScope<'a, T = NamespaceEffect> {
    // One fallibly allocated state keeps Err(Self) small without allocating on
    // an owner mismatch. This vector always contains exactly one operation,
    // not a bound on the recursively growing completion collection.
    state: Vec<OperationState<'a, T>>,
}

struct OperationState<'a, T> {
    admissions: Option<NamespaceAdmissions<'a>>,
    retired: Option<MutationRetire>,
    children: RefCell<Registration>,
    root: RefCell<Option<NamespaceCompletion<T>>>,
    prepared: PreparedView<'a>,
}

impl<'a, T> NamespaceOperationScope<'a, T> {
    pub fn acquire(write: &'a PreparedWrite) -> VfsResult<Self> {
        Self::acquire_prepared(PreparedView::write(write))
    }

    /// Borrows complete creation preparation, including a fully read-only graph
    /// when creation write preparation failed. No rights are acquired in gates.
    pub fn acquire_creation(creation: &'a crate::PreparedCreation) -> VfsResult<Self> {
        Self::acquire_prepared(PreparedView::creation(creation))
    }

    pub(crate) fn acquire_prepared(prepared: PreparedView<'a>) -> VfsResult<Self> {
        let mut state = Vec::new();
        state.try_reserve_exact(1).map_err(|_| VfsError::NoMemory)?;
        state.push(OperationState {
            admissions: Some(NamespaceAdmissions::acquire_prepared(prepared)?),
            retired: Some(MutationRetire::new()),
            children: RefCell::new(Registration::new()),
            root: RefCell::new(None),
            prepared,
        });
        Ok(Self { state })
    }

    fn state(&self) -> &OperationState<'a, T> {
        &self.state[0]
    }

    /// Borrows prepared rights without acquiring additional admissions.
    pub fn context(&self) -> NamespaceOperationContext<'_> {
        let state = self.state();
        NamespaceOperationContext {
            admissions: state.admissions.as_ref().expect("live scope"),
            retired: state.retired.as_ref().expect("live scope"),
            children: &state.children,
            prepared: state.prepared,
        }
    }

    fn check_context(&self, context: &NamespaceOperationContext<'_>) -> VfsResult<()> {
        let state = self.state();
        if core::ptr::eq(&state.children, context.children)
            && !state.children.borrow().root_claimed
            && state.root.borrow().is_none()
        {
            Ok(())
        } else {
            Err(VfsError::BadState)
        }
    }

    /// Completes all known responsibilities; a mismatch consumes no phase.
    ///
    /// Children run in actual return order, root last. Every pressure precedes
    /// every admission exit; retirement precedes every finish. Only new phase
    /// errors participate in phase ordering, never a child's original rejection.
    pub fn finish(mut self) -> Result<NamespaceOutcome<T>, Self> {
        let state = self.state();
        let admissions = state.admissions.as_ref().expect("live scope");
        if state
            .root
            .borrow()
            .as_ref()
            .is_some_and(|root| !root.admitted_by(admissions))
            || !state.children.borrow().admitted_by(admissions)
        {
            return Err(self);
        }
        let state = &mut self.state[0];
        let registration = state.children.get_mut();
        let incomplete = registration.incomplete || registration.reservations != 0;
        let mut phase = Ok(());
        for child in &mut registration.children {
            phase = phase.and(child.pressure());
        }
        // Keep the root's original result independent of all new phase errors.
        let root = state.root.get_mut().take().map(|root| {
            let (deferred, completion) = root.pressure_step();
            phase = phase.and(completion);
            deferred
        });
        drop(state.admissions.take());
        drop(state.retired.take());
        for child in &mut state.children.get_mut().children {
            phase = phase.and(child.finish());
        }
        let outcome = match root {
            Some(root) => {
                let (outcome, completion) = root.finish_step();
                phase = phase.and(completion);
                outcome
            }
            None => NamespaceOutcome::uncertain(None, Err(VfsError::BadState)),
        };
        if incomplete {
            phase = phase.and(Err(VfsError::BadState));
        }
        Ok(outcome.with_completion(phase))
    }
}

impl NamespaceOperationScope<'_, Metadata> {
    /// Starts exactly one root metadata attempt using this scope's borrowed view.
    /// Empty times produce neither a root nor an observation.
    pub fn change_metadata(
        &self,
        context: &NamespaceOperationContext<'_>,
        target: &Location,
        change: MetadataChange,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<Option<NamespaceObservation<Metadata>>> {
        if change.is_empty() {
            return Ok(None);
        }
        self.check_context(context)?;
        context.retired.retire(target.clone());
        context.dispatch_metadata(
            target,
            change,
            credentials,
            Reservation::root_metadata(context.children, &self.state().root),
        )
    }
}

impl NamespaceOperationScope<'_, NamespaceEffect> {
    /// Starts one root creation or existing-object selection. Even an unchanged
    /// selection transfers its typed completion before returning this snapshot.
    pub fn create<'a>(
        &'a self,
        context: &'a NamespaceOperationContext<'a>,
        parent: &'a Location,
        name: &str,
        attributes: crate::CreateAttributes<'a>,
        intent: crate::CreateIntent,
        credentials: &MutationCredentials<'a>,
    ) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
        self.check_context(context)?;
        context.retired.retire(parent.clone());
        crate::node::start_create(
            context,
            parent,
            name,
            attributes,
            crate::node::CreateSelection::Ordinary(intent),
            credentials,
            Reservation::root_namespace(context.children, &self.state().root),
        )
    }

    pub(crate) fn restore_create<'a>(
        &'a self,
        context: &'a NamespaceOperationContext<'a>,
        parent: &'a Location,
        name: &str,
        attributes: crate::CreateAttributes<'a>,
        owner: (u32, u32),
        initialization: &crate::TrustedInitialization,
    ) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
        self.check_context(context)?;
        context.retired.retire(parent.clone());
        crate::node::start_create(
            context,
            parent,
            name,
            attributes,
            crate::node::CreateSelection::Restore(owner),
            initialization.credentials(),
            Reservation::root_namespace(context.children, &self.state().root),
        )
    }

    /// Starts exactly one root native hard-link attempt. Completion remains in
    /// this scope even when the returned observation reports an error.
    pub fn link<'a>(
        &'a self,
        context: &'a NamespaceOperationContext<'a>,
        parent: &'a Location,
        name: &str,
        source: &'a Location,
        credentials: &MutationCredentials<'a>,
    ) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
        self.check_context(context)?;
        context.retired.retire(parent.clone());
        context.retired.retire(source.clone());
        crate::node::start_link(
            context,
            parent,
            name,
            source,
            credentials,
            Reservation::root_namespace(context.children, &self.state().root),
        )
    }

    /// Starts exactly one root native rename with both slots in one attempt.
    pub fn rename<'a>(
        &'a self,
        context: &'a NamespaceOperationContext<'a>,
        source: (&'a Location, &'a str),
        destination: (&'a Location, &'a str),
        options: RenameOptions,
        credentials: &'a MutationCredentials<'a>,
    ) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
        self.check_context(context)?;
        context.retired.retire(source.0.clone());
        context.retired.retire(destination.0.clone());
        crate::node::start_rename(
            context,
            source,
            destination,
            options,
            credentials,
            Reservation::root_namespace(context.children, &self.state().root),
        )
    }

    /// Starts one root native unlink; the caller retains the named context
    /// through consuming resume/cancel, and ends that borrow before finishing.
    pub fn unlink<'a>(
        &'a self,
        context: &'a NamespaceOperationContext<'a>,
        prepared: PreparedUnlink,
        directory: bool,
        credentials: &MutationCredentials<'a>,
    ) -> VfsResult<NamespaceStart<'a>> {
        self.check_context(context)?;
        context.retired.retire(prepared.parent_owner());
        context.dispatch_unlink(
            prepared,
            directory,
            credentials,
            Reservation::root_namespace(context.children, &self.state().root),
        )
    }
}

impl<T> Drop for NamespaceOperationScope<'_, T> {
    fn drop(&mut self) {
        let state = &mut self.state[0];
        drop(state.admissions.take());
        drop(state.retired.take());
        state.children.get_mut().children.clear();
        drop(state.root.get_mut().take());
    }
}

/// Borrow-only backend capabilities; no ownership of admission or completion.
///
/// Registry borrows never cover a backend invocation. This view is stack-local
/// and does not add a lock, Send/Sync requirement, fallback right or re-entry.
/// Command inputs retire before fallible reservation, including attempts that
/// never enter the backend. Private dispatch helpers rely on this retention.
pub struct NamespaceOperationContext<'a> {
    admissions: &'a NamespaceAdmissions<'a>,
    retired: &'a MutationRetire,
    children: &'a RefCell<Registration>,
    prepared: PreparedView<'a>,
}

impl<'a> NamespaceOperationContext<'a> {
    /// Starts a prepared child create/open without re-entering any admission.
    pub fn create(
        &'a self,
        parent: &'a Location,
        name: &str,
        attributes: crate::CreateAttributes<'a>,
        intent: crate::CreateIntent,
        credentials: &MutationCredentials<'a>,
    ) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
        self.retired.retire(parent.clone());
        let reservation = Reservation::child(self.children)?;
        crate::node::start_create(
            self,
            parent,
            name,
            attributes,
            crate::node::CreateSelection::Ordinary(intent),
            credentials,
            reservation,
        )
    }

    /// Reads a prepared receiver without entering another admission.
    /// As with `NamespaceAdmission::query`, no native driver or backend gate may
    /// be held; the snapshot does not authorize a later backing mutation.
    pub fn query(&self, query: NamespaceQuery<'_>) -> VfsResult<NamespaceRead> {
        self.retired.retire(query.entry().clone());
        let admission = self.admission_for_owner(query.entry().filesystem())?;
        BackingQueryRequest::new(self, admission, query).dispatch()
    }

    /// Looks up one exact-mount backing component, without following mounts or
    /// links. Call outside native drivers/backend gates and apply the caller's
    /// own search and backing-identity checks.
    pub fn lookup_backing(&self, parent: &Location, name: &str) -> VfsResult<Location> {
        self.retired.retire(parent.clone());
        backing_query::fixed_lookup(
            parent,
            self.query(NamespaceQuery::Lookup {
                parent: parent.entry(),
                name,
            })?,
        )
    }

    /// Returns only a real exact-mount write lease; read pins cannot satisfy it.
    pub fn write_for(&self, target: &Location) -> VfsResult<&MountWriteGuard> {
        self.prepared.require_write()?.write_for(target)
    }

    pub fn change_metadata(
        &self,
        target: &Location,
        change: MetadataChange,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<Option<NamespaceObservation<Metadata>>> {
        if change.is_empty() {
            return Ok(None);
        }
        self.retired.retire(target.clone());
        let reservation = Reservation::child(self.children)?;
        self.dispatch_metadata(target, change, credentials, reservation)
    }

    pub fn unlink(
        &'a self,
        prepared: PreparedUnlink,
        directory: bool,
        credentials: &MutationCredentials<'a>,
    ) -> VfsResult<NamespaceStart<'a>> {
        self.retired.retire(prepared.parent_owner());
        let reservation = Reservation::child(self.children)?;
        self.dispatch_unlink(prepared, directory, credentials, reservation)
    }

    /// Starts a child hard link using only prepared rights. Its typed completion
    /// is registered before this read-only observation is returned.
    pub fn link(
        &'a self,
        parent: &'a Location,
        name: &str,
        source: &'a Location,
        credentials: &MutationCredentials<'a>,
    ) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
        self.retired.retire(parent.clone());
        self.retired.retire(source.clone());
        let reservation = Reservation::child(self.children)?;
        crate::node::start_link(self, parent, name, source, credentials, reservation)
    }

    /// Starts a child rename; neither authorization nor completion escapes.
    pub fn rename(
        &'a self,
        source: (&'a Location, &'a str),
        destination: (&'a Location, &'a str),
        options: RenameOptions,
        credentials: &'a MutationCredentials<'a>,
    ) -> VfsResult<NamespaceObservation<NamespaceEffect>> {
        self.retired.retire(source.0.clone());
        self.retired.retire(destination.0.clone());
        let reservation = Reservation::child(self.children)?;
        crate::node::start_rename(self, source, destination, options, credentials, reservation)
    }

    pub(crate) fn admission(&self, target: &Location) -> VfsResult<&'a NamespaceAdmission<'a>> {
        self.admissions.for_location(target)
    }

    /// Borrows the common retirement bag; it cannot be cleared by a backend.
    pub fn retired(&self) -> &'a MutationRetire {
        self.retired
    }

    pub(crate) fn admission_for_owner(
        &self,
        owner: &dyn crate::FilesystemOps,
    ) -> VfsResult<&'a NamespaceAdmission<'a>> {
        self.admissions.for_owner(owner)
    }

    pub(crate) fn write(&self) -> VfsResult<&'a PreparedWrite> {
        self.prepared.require_write()
    }

    pub(crate) fn creation_write(&self, target: &Location) -> VfsResult<crate::CreationWrite<'a>> {
        self.admission(target)?.check_owner(target.filesystem())?;
        self.prepared.creation_write(target)
    }

    fn dispatch_metadata(
        &self,
        target: &Location,
        change: MetadataChange,
        credentials: &MutationCredentials<'_>,
        mut reservation: Reservation<'_>,
    ) -> VfsResult<Option<NamespaceObservation<Metadata>>> {
        self.write_for(target)?.check_location(target)?;
        let admission = self.admission(target)?;
        reservation.start();
        let completion =
            crate::metadata::dispatch(self, target, change, credentials).bind(admission.owner);
        Ok(Some(reservation.metadata(completion)))
    }

    fn dispatch_unlink(
        &'a self,
        prepared: PreparedUnlink,
        directory: bool,
        credentials: &MutationCredentials<'a>,
        reservation: Reservation<'a>,
    ) -> VfsResult<NamespaceStart<'a>> {
        crate::node::start_unlink(self, prepared, directory, credentials, reservation)
    }
}
