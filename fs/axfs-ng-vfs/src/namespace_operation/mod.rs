//! One stack-local owner for recursively returned, typed completion work.

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
    PreparedUnlink, PreparedWrite, VfsError, VfsResult,
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
    write: &'a PreparedWrite,
}

impl<'a, T> NamespaceOperationScope<'a, T> {
    pub fn acquire(write: &'a PreparedWrite) -> VfsResult<Self> {
        let mut state = Vec::new();
        state.try_reserve_exact(1).map_err(|_| VfsError::NoMemory)?;
        state.push(OperationState {
            admissions: Some(NamespaceAdmissions::acquire(write)?),
            retired: Some(MutationRetire::new()),
            children: RefCell::new(Registration::new()),
            root: RefCell::new(None),
            write,
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
            write: state.write,
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
        context.dispatch_metadata(
            target,
            change,
            credentials,
            Reservation::root_metadata(context.children, &self.state().root),
        )
    }
}

impl NamespaceOperationScope<'_, NamespaceEffect> {
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
pub struct NamespaceOperationContext<'a> {
    admissions: &'a NamespaceAdmissions<'a>,
    retired: &'a MutationRetire,
    children: &'a RefCell<Registration>,
    write: &'a PreparedWrite,
}

impl<'a> NamespaceOperationContext<'a> {
    pub fn query(&self, query: NamespaceQuery<'_>) -> VfsResult<NamespaceRead> {
        let admission = match self.admissions.for_owner(query.entry().filesystem()) {
            Ok(admission) => admission,
            Err(error) => {
                self.retired.retire(query.entry().clone());
                return Err(error);
            }
        };
        admission.query(query, self.retired)
    }

    pub fn lookup_backing(&self, parent: &Location, name: &str) -> VfsResult<Location> {
        let admission = match self.admission(parent) {
            Ok(admission) => admission,
            Err(error) => {
                self.retired.retire(parent.clone());
                return Err(error);
            }
        };
        admission.lookup_backing(parent, name, self.retired)
    }

    /// Returns only a real exact-mount write lease; read pins cannot satisfy it.
    pub fn write_for(&self, target: &Location) -> VfsResult<&MountWriteGuard> {
        self.write.write_for(target)
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
        let reservation = Reservation::child(self.children)?;
        self.dispatch_metadata(target, change, credentials, reservation)
    }

    pub fn unlink(
        &'a self,
        prepared: PreparedUnlink,
        directory: bool,
        credentials: &MutationCredentials<'a>,
    ) -> VfsResult<NamespaceStart<'a>> {
        let reservation = Reservation::child(self.children)?;
        self.dispatch_unlink(prepared, directory, credentials, reservation)
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

    pub(crate) fn write(&self) -> &'a PreparedWrite {
        self.write
    }

    fn dispatch_metadata(
        &self,
        target: &Location,
        change: MetadataChange,
        credentials: &MutationCredentials<'_>,
        mut reservation: Reservation<'_>,
    ) -> VfsResult<Option<NamespaceObservation<Metadata>>> {
        self.retired.retire(target.clone());
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
