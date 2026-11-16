//! Command-local preparation survives attempts, never their authorization.

use alloc::{sync::Arc, vec::Vec};
use core::{
    any::Any,
    cell::{Cell, RefCell},
};

use crate::{
    FilesystemOps, MutationRetire, NamespaceOperationScope, NamespaceOutcome, NodeIdentity,
    PreparedCreation, PreparedWrite, VfsError, VfsResult, prepared_write::PreparedView,
};

/// A backend's owned plan for hidden work between fully completed attempts.
///
/// Neither method may retain an admission, driver, authorization, borrowed user
/// memory or an operation context. `prepare` never installs a visible name;
/// the next attempt must authorize installation afresh. `cleanup` explicitly
/// removes residual hidden work and reports failure. Drop performs no rollback.
pub trait NamespacePreparation: Any + Send + Sync {
    fn prepare(&self, write: &PreparedWrite) -> VfsResult<()>;
    fn cleanup(&self, write: &PreparedWrite) -> VfsResult<()>;
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}

struct Preparation {
    owner: usize,
    key: NodeIdentity,
    task: Arc<dyn NamespacePreparation>,
    ready: bool,
}

/// One outer command, borrowing write preparation throughout every attempt.
///
/// Use `run_write`/`run_creation` for complete execution. Manual protocol users
/// must call `finish` after all their attempt scopes end. Drop releases passive
/// owners only; it does not clean hidden files or finish a backend operation.
#[must_use = "complete all attempts, then explicitly finish command preparation"]
pub struct NamespaceCommandScope<'a> {
    pub(super) prepared: PreparedView<'a>,
    tasks: RefCell<Vec<Preparation>>,
    active: Cell<bool>,
}

impl<'a> NamespaceCommandScope<'a> {
    pub fn new(write: &'a PreparedWrite) -> Self {
        Self::from_prepared(PreparedView::write(write))
    }

    pub fn for_creation(creation: &'a PreparedCreation) -> Self {
        Self::from_prepared(PreparedView::creation(creation))
    }

    fn from_prepared(prepared: PreparedView<'a>) -> Self {
        Self {
            prepared,
            tasks: RefCell::new(Vec::new()),
            active: Cell::new(false),
        }
    }

    pub(super) fn check_idle(&self) -> VfsResult<()> {
        if self.active.get() {
            Err(VfsError::BadState)
        } else {
            Ok(())
        }
    }

    pub(super) fn start_attempt(&self) {
        self.active.set(true);
    }

    pub(super) fn end_attempt(&self) {
        self.active.set(false);
    }

    /// Enters a fresh attempt, reusing no driver or authorization from a prior one.
    pub fn attempt<T>(&self) -> VfsResult<NamespaceOperationScope<'_, T>> {
        NamespaceOperationScope::acquire(self)
    }

    pub fn run_write<T>(
        write: &'a PreparedWrite,
        operation: impl FnOnce(&Self) -> VfsResult<NamespaceOutcome<T>>,
    ) -> VfsResult<NamespaceOutcome<T>> {
        Self::run_prepared(PreparedView::write(write), operation)
    }

    pub fn run_creation<T>(
        creation: &'a PreparedCreation,
        operation: impl FnOnce(&Self) -> VfsResult<NamespaceOutcome<T>>,
    ) -> VfsResult<NamespaceOutcome<T>> {
        Self::run_prepared(PreparedView::creation(creation), operation)
    }

    pub(crate) fn run_prepared<T>(
        prepared: PreparedView<'a>,
        operation: impl FnOnce(&Self) -> VfsResult<NamespaceOutcome<T>>,
    ) -> VfsResult<NamespaceOutcome<T>> {
        let command = Self::from_prepared(prepared);
        let outcome = operation(&command);
        command.finish(outcome)
    }

    /// Executes pending plans only after the caller has ended every attempt.
    /// Plans never borrow the collection while calling a backend or lower layer.
    pub fn prepare(&self) -> VfsResult<()> {
        self.check_idle()?;
        let tasks = self.tasks.borrow();
        let mut pending = Vec::new();
        pending
            .try_reserve(tasks.len())
            .map_err(|_| VfsError::NoMemory)?;
        for (index, task) in tasks.iter().enumerate() {
            if !task.ready {
                pending.push((index, task.task.clone()));
            }
        }
        drop(tasks);
        if pending.is_empty() {
            return Ok(());
        }
        let write = self.prepared.require_write()?;
        for (index, task) in pending {
            task.prepare(write)?;
            self.tasks.borrow_mut()[index].ready = true;
        }
        Ok(())
    }

    /// Explicit cleanup preserves the original error and actual effect state.
    /// Every task runs even if a previous cleanup failed. Backend plans must
    /// diagnose residual objects when their cleanup fails.
    pub fn finish<T>(
        self,
        outcome: VfsResult<NamespaceOutcome<T>>,
    ) -> VfsResult<NamespaceOutcome<T>> {
        let mut completion = Ok(());
        for preparation in self.tasks.into_inner() {
            let result = self
                .prepared
                .require_write()
                .and_then(|write| preparation.task.cleanup(write));
            completion = completion.and(result);
        }
        outcome.map(|outcome| outcome.with_completion(completion))
    }
}

/// Only the privately bound request factory chooses the receiver namespace.
pub struct NamespacePreparationContext<'a> {
    command: &'a NamespaceCommandScope<'a>,
    owner: usize,
    retired: &'a MutationRetire,
}

pub enum PreparationState<T> {
    Pending(Arc<T>),
    Ready(Arc<T>),
}

impl<'a> NamespacePreparationContext<'a> {
    pub(super) fn new(
        command: &'a NamespaceCommandScope<'a>,
        owner: &dyn FilesystemOps,
        retired: &'a MutationRetire,
    ) -> Self {
        Self {
            command,
            owner: super::super::namespace_admission::owner_address(owner),
            retired,
        }
    }

    /// Reads only this receiver's existing plan, never another FS's local key.
    pub fn get<T: NamespacePreparation>(
        &self,
        key: &NodeIdentity,
    ) -> VfsResult<Option<PreparationState<T>>> {
        let tasks = self.command.tasks.borrow();
        let Some(found) = tasks
            .iter()
            .find(|task| task.owner == self.owner && task.key == *key)
        else {
            return Ok(None);
        };
        let ready = found.ready;
        let task = found.task.clone();
        drop(tasks);
        let task = task
            .into_any()
            .downcast::<T>()
            .map_err(|_| VfsError::BadState)?;
        Ok(Some(if ready {
            PreparationState::Ready(task)
        } else {
            PreparationState::Pending(task)
        }))
    }

    /// Register hidden preparation only from a freshly authorized commit which
    /// returns Retry without a visible effect. A rejected/losing candidate is
    /// retired with the complete outer attempt, never under its owner gate.
    pub fn insert<T: NamespacePreparation>(
        &self,
        key: NodeIdentity,
        task: Arc<T>,
    ) -> VfsResult<PreparationState<T>> {
        self.retired.retire(task.clone());
        if let Some(existing) = self.get(&key)? {
            return Ok(existing);
        }
        let mut tasks = self.command.tasks.borrow_mut();
        tasks.try_reserve(1).map_err(|_| VfsError::NoMemory)?;
        tasks.push(Preparation {
            owner: self.owner,
            key,
            task: task.clone(),
            ready: false,
        });
        Ok(PreparationState::Pending(task))
    }
}
