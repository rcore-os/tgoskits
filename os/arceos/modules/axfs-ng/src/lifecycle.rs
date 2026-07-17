//! Generation-based filesystem freeze, detach, and remount lifecycle.

use alloc::sync::Arc;
use core::{cell::Cell, fmt, marker::PhantomData};

use ax_errno::AxError;
use ax_kspin::SpinNoPreempt;

/// Identifies one published filesystem generation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FsGeneration(u64);

impl FsGeneration {
    const INITIAL: Self = Self(1);

    /// Returns the integer generation for diagnostics and persisted recipes.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Observable filesystem lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsRuntimeState {
    Mounted,
    Freezing,
    Detached,
    Remounting,
    Failed,
}

/// Non-blocking progress of one filesystem freeze transaction.
///
/// Callers may reschedule and query the same [`FsFreezePermit`] again after
/// the reported generation leases are released. Querying progress never
/// waits, invokes callbacks, or performs filesystem I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsFreezeProgress {
    /// Existing users of the frozen generation still need to drain.
    Pending {
        /// Filesystem operations that started before the freeze boundary.
        active_operations: usize,
        /// Externally visible handles retaining the frozen generation.
        open_handles: usize,
    },
    /// No operation or open-handle lease retains the frozen generation.
    Drained,
}

/// Failure returned by a filesystem lifecycle transition or lease request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FsRuntimeError {
    #[error("filesystem handle belongs to a stale mount generation")]
    StaleGeneration,
    #[error("filesystem lifecycle transition is invalid from the current state")]
    InvalidTransition,
    #[error("filesystem lifecycle transaction permit is no longer current")]
    InvalidPermit,
    #[error("filesystem still has active operations or open handles")]
    Busy,
    #[error("filesystem generation or transaction counter is exhausted")]
    GenerationExhausted,
}

impl FsRuntimeError {
    pub(crate) const fn into_ax_error(self) -> AxError {
        match self {
            Self::Busy => AxError::ResourceBusy,
            Self::GenerationExhausted => AxError::OutOfRange,
            Self::StaleGeneration | Self::InvalidTransition | Self::InvalidPermit => {
                AxError::BadState
            }
        }
    }
}

/// Snapshot used by handoff orchestration and diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FsRuntimeSnapshot {
    /// Current lifecycle state.
    pub state: FsRuntimeState,
    /// Last successfully published generation.
    pub generation: FsGeneration,
    /// Operations that began before the current freeze boundary.
    pub active_operations: usize,
    /// Externally visible handles retaining the published generation.
    pub open_handles: usize,
}

/// Unique authorization to finish or cancel one freeze transaction.
#[derive(Debug)]
pub struct FsFreezePermit {
    generation: FsGeneration,
    transaction: u64,
    not_sync: PhantomData<Cell<()>>,
}

/// Unique authorization to publish or fail one remount transaction.
#[derive(Debug)]
pub struct FsRemountPermit {
    previous_generation: FsGeneration,
    next_generation: FsGeneration,
    transaction: u64,
    not_sync: PhantomData<Cell<()>>,
}

/// Coordinates freeze, detach, and remount without owning a block runtime.
#[derive(Clone)]
pub struct FsRuntime {
    inner: Arc<FsRuntimeInner>,
}

struct FsRuntimeInner {
    state: SpinNoPreempt<FsRuntimeData>,
}

struct FsRuntimeData {
    state: FsRuntimeState,
    generation: FsGeneration,
    generation_cursor: u64,
    transaction: u64,
    active_operations: usize,
    open_handles: usize,
}

/// Lease held while one filesystem operation is in flight.
pub struct FsOperationLease {
    inner: Arc<FsRuntimeInner>,
    generation: FsGeneration,
}

/// Cloneable generation identity used by internal backends and mappings.
///
/// Unlike [`FsOpenHandleLease`], this token is not an externally visible open
/// handle and does not delay a freeze. Every operation started through it is
/// still rejected once freezing begins and is counted until completion.
#[derive(Clone)]
pub(crate) struct FsGenerationAccess {
    inner: Arc<FsRuntimeInner>,
    generation: FsGeneration,
}

/// Cloneable lease held by one externally visible file or directory handle.
///
/// Clones share one counted lease. The runtime therefore observes the lease as
/// live until the final clone is dropped, while cloning an already-open handle
/// during `Freezing` cannot create an untracked reference.
#[derive(Clone)]
pub struct FsOpenHandleLease {
    inner: Arc<FsOpenHandleLeaseInner>,
}

struct FsOpenHandleLeaseInner {
    runtime: Arc<FsRuntimeInner>,
    generation: FsGeneration,
}

impl FsRuntime {
    /// Creates a mounted runtime at generation one.
    pub fn new_mounted() -> Self {
        Self {
            inner: Arc::new(FsRuntimeInner {
                state: SpinNoPreempt::new(FsRuntimeData {
                    state: FsRuntimeState::Mounted,
                    generation: FsGeneration::INITIAL,
                    generation_cursor: FsGeneration::INITIAL.0,
                    transaction: 0,
                    active_operations: 0,
                    open_handles: 0,
                }),
            }),
        }
    }

    /// Returns a consistent lifecycle and lease-count snapshot.
    pub fn snapshot(&self) -> FsRuntimeSnapshot {
        let state = self.inner.state.lock();
        FsRuntimeSnapshot {
            state: state.state,
            generation: state.generation,
            active_operations: state.active_operations,
            open_handles: state.open_handles,
        }
    }

    /// Starts one operation in `expected_generation`.
    ///
    /// New operations are rejected as soon as freezing begins.
    pub fn begin_operation(
        &self,
        expected_generation: FsGeneration,
    ) -> Result<FsOperationLease, FsRuntimeError> {
        let mut state = self.inner.state.lock();
        validate_mounted_generation(&state, expected_generation)?;
        state.active_operations = state
            .active_operations
            .checked_add(1)
            .ok_or(FsRuntimeError::Busy)?;
        Ok(FsOperationLease {
            inner: self.inner.clone(),
            generation: expected_generation,
        })
    }

    /// Opens a generation-bound file or directory handle.
    pub fn open_handle(
        &self,
        expected_generation: FsGeneration,
    ) -> Result<FsOpenHandleLease, FsRuntimeError> {
        let mut state = self.inner.state.lock();
        validate_mounted_generation(&state, expected_generation)?;
        state.open_handles = state
            .open_handles
            .checked_add(1)
            .ok_or(FsRuntimeError::Busy)?;
        Ok(FsOpenHandleLease {
            inner: Arc::new(FsOpenHandleLeaseInner {
                runtime: self.inner.clone(),
                generation: expected_generation,
            }),
        })
    }

    /// Opens a handle as part of an operation that was admitted before freeze.
    ///
    /// Freeze rejects fresh operations and handles, but it must allow an
    /// already-counted operation to finish atomically. A successful open keeps
    /// freeze blocked through the returned handle after the operation itself
    /// completes.
    pub(crate) fn open_handle_during(
        &self,
        expected_generation: FsGeneration,
        operation: &FsOperationLease,
    ) -> Result<FsOpenHandleLease, FsRuntimeError> {
        self.validate_operation(expected_generation, operation)?;

        let mut state = self.inner.state.lock();
        if state.generation != expected_generation {
            return Err(FsRuntimeError::StaleGeneration);
        }
        if !matches!(
            state.state,
            FsRuntimeState::Mounted | FsRuntimeState::Freezing
        ) {
            return Err(FsRuntimeError::InvalidTransition);
        }
        state.open_handles = state
            .open_handles
            .checked_add(1)
            .ok_or(FsRuntimeError::Busy)?;
        Ok(FsOpenHandleLease {
            inner: Arc::new(FsOpenHandleLeaseInner {
                runtime: self.inner.clone(),
                generation: expected_generation,
            }),
        })
    }

    pub(crate) fn continue_generation_access(
        &self,
        expected_generation: FsGeneration,
        operation: &FsOperationLease,
    ) -> Result<FsGenerationAccess, FsRuntimeError> {
        let access = FsGenerationAccess {
            inner: self.inner.clone(),
            generation: expected_generation,
        };
        access.validate_operation(operation)?;
        Ok(access)
    }

    pub(crate) fn validate_operation(
        &self,
        expected_generation: FsGeneration,
        operation: &FsOperationLease,
    ) -> Result<(), FsRuntimeError> {
        FsGenerationAccess {
            inner: self.inner.clone(),
            generation: expected_generation,
        }
        .validate_operation(operation)
    }

    #[cfg(feature = "vfs")]
    pub(crate) fn bind_generation_access(&self, generation: FsGeneration) -> FsGenerationAccess {
        let state = self.inner.state.lock();
        debug_assert!(
            (state.state == FsRuntimeState::Mounted && state.generation == generation)
                || (state.state == FsRuntimeState::Remounting
                    && state.generation_cursor == generation.0),
            "filesystem contexts may bind only the mounted or pending remount generation"
        );
        drop(state);
        FsGenerationAccess {
            inner: self.inner.clone(),
            generation,
        }
    }

    /// Prevents new operations and handles for the mounted generation.
    pub fn begin_freeze(
        &self,
        expected_generation: FsGeneration,
    ) -> Result<FsFreezePermit, FsRuntimeError> {
        let mut state = self.inner.state.lock();
        validate_mounted_generation(&state, expected_generation)?;
        state.transaction = next_counter(state.transaction)?;
        state.state = FsRuntimeState::Freezing;
        Ok(FsFreezePermit {
            generation: expected_generation,
            transaction: state.transaction,
            not_sync: PhantomData,
        })
    }

    /// Verifies that all operations and externally visible handles drained.
    pub fn ensure_freeze_drained(&self, permit: &FsFreezePermit) -> Result<(), FsRuntimeError> {
        match self.freeze_progress(permit)? {
            FsFreezeProgress::Drained => Ok(()),
            FsFreezeProgress::Pending { .. } => Err(FsRuntimeError::Busy),
        }
    }

    /// Returns the current, non-blocking drain progress for `permit`.
    ///
    /// # Errors
    ///
    /// Returns [`FsRuntimeError`] if `permit` no longer identifies the active
    /// freeze transaction.
    pub fn freeze_progress(
        &self,
        permit: &FsFreezePermit,
    ) -> Result<FsFreezeProgress, FsRuntimeError> {
        let state = self.inner.state.lock();
        validate_freeze_permit(&state, permit)?;
        if state.active_operations == 0 && state.open_handles == 0 {
            return Ok(FsFreezeProgress::Drained);
        }
        Ok(FsFreezeProgress::Pending {
            active_operations: state.active_operations,
            open_handles: state.open_handles,
        })
    }

    /// Returns a failed handoff to the mounted state without changing generation.
    pub fn cancel_freeze(&self, permit: &FsFreezePermit) -> Result<(), FsRuntimeError> {
        let mut state = self.inner.state.lock();
        validate_freeze_permit(&state, permit)?;
        state.state = FsRuntimeState::Mounted;
        Ok(())
    }

    /// Publishes that the old filesystem has been synchronized and detached.
    pub fn finish_detach(&self, permit: &FsFreezePermit) -> Result<(), FsRuntimeError> {
        let mut state = self.inner.state.lock();
        validate_freeze_permit(&state, permit)?;
        validate_no_generation_leases(&state)?;
        state.state = FsRuntimeState::Detached;
        Ok(())
    }

    /// Marks a partially completed detach as failed and fail-closed.
    pub fn fail_detach(&self, permit: &FsFreezePermit) -> Result<(), FsRuntimeError> {
        let mut state = self.inner.state.lock();
        validate_freeze_permit(&state, permit)?;
        validate_no_generation_leases(&state)?;
        state.state = FsRuntimeState::Failed;
        Ok(())
    }

    /// Begins rebuilding a detached or previously failed filesystem.
    pub fn begin_remount(&self) -> Result<FsRemountPermit, FsRuntimeError> {
        let mut state = self.inner.state.lock();
        if !matches!(
            state.state,
            FsRuntimeState::Detached | FsRuntimeState::Failed
        ) {
            return Err(FsRuntimeError::InvalidTransition);
        }
        validate_no_generation_leases(&state)?;
        let next_generation = FsGeneration(next_counter(state.generation_cursor)?);
        let next_transaction = next_counter(state.transaction)?;
        state.generation_cursor = next_generation.0;
        state.transaction = next_transaction;
        state.state = FsRuntimeState::Remounting;
        Ok(FsRemountPermit {
            previous_generation: state.generation,
            next_generation,
            transaction: state.transaction,
            not_sync: PhantomData,
        })
    }

    /// Validates a remount permit before exposing its root context.
    ///
    /// The permit is not `Sync` and remains exclusively owned by the caller.
    /// Once this succeeds, no lifecycle transition can invalidate the permit
    /// before [`Self::finish_remount`] consumes it.
    pub fn validate_remount_publication(
        &self,
        permit: &FsRemountPermit,
    ) -> Result<(), FsRuntimeError> {
        let state = self.inner.state.lock();
        validate_remount_permit(&state, permit)?;
        validate_no_generation_leases(&state)
    }

    /// Publishes a successfully reconstructed filesystem as a new generation.
    pub fn finish_remount(&self, permit: FsRemountPermit) -> Result<FsGeneration, FsRuntimeError> {
        let mut state = self.inner.state.lock();
        validate_remount_permit(&state, &permit)?;
        validate_no_generation_leases(&state)?;
        state.generation = permit.next_generation;
        state.state = FsRuntimeState::Mounted;
        Ok(state.generation)
    }

    /// Records an unrecoverable remount failure without publishing a root.
    pub fn fail_remount(&self, permit: FsRemountPermit) -> Result<(), FsRuntimeError> {
        let mut state = self.inner.state.lock();
        validate_remount_permit(&state, &permit)?;
        validate_no_generation_leases(&state)?;
        state.state = FsRuntimeState::Failed;
        Ok(())
    }
}

impl Default for FsRuntime {
    fn default() -> Self {
        Self::new_mounted()
    }
}

impl fmt::Debug for FsRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.snapshot().fmt(formatter)
    }
}

impl FsRemountPermit {
    /// Returns the generation that a successful remount must publish.
    pub const fn next_generation(&self) -> FsGeneration {
        self.next_generation
    }
}

impl FsOperationLease {
    /// Returns the generation in which this operation began.
    pub const fn generation(&self) -> FsGeneration {
        self.generation
    }

    pub(crate) fn generation_access(&self) -> FsGenerationAccess {
        FsGenerationAccess {
            inner: self.inner.clone(),
            generation: self.generation,
        }
    }

    pub(crate) fn authorizes_same_generation(&self, other: &Self) -> bool {
        self.generation == other.generation && Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl FsGenerationAccess {
    pub(crate) fn begin_operation(&self) -> Result<FsOperationLease, FsRuntimeError> {
        let runtime = FsRuntime {
            inner: self.inner.clone(),
        };
        runtime.begin_operation(self.generation)
    }

    pub(crate) fn validate(&self) -> Result<(), FsRuntimeError> {
        let state = self.inner.state.lock();
        validate_mounted_generation(&state, self.generation)
    }

    pub(crate) fn validate_operation(
        &self,
        operation: &FsOperationLease,
    ) -> Result<(), FsRuntimeError> {
        if !Arc::ptr_eq(&self.inner, &operation.inner) {
            return Err(FsRuntimeError::InvalidPermit);
        }
        if self.generation != operation.generation {
            return Err(FsRuntimeError::StaleGeneration);
        }
        Ok(())
    }
}

impl fmt::Debug for FsGenerationAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FsGenerationAccess")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for FsOperationLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FsOperationLease")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Drop for FsOperationLease {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock();
        debug_assert_eq!(state.generation, self.generation);
        state.active_operations = state
            .active_operations
            .checked_sub(1)
            .expect("filesystem operation lease count must not underflow");
    }
}

impl FsOpenHandleLease {
    /// Returns the generation in which this handle was opened.
    pub fn generation(&self) -> FsGeneration {
        self.inner.generation
    }

    /// Starts an operation owned by this open handle.
    pub fn begin_operation(&self) -> Result<FsOperationLease, FsRuntimeError> {
        let runtime = FsRuntime {
            inner: self.inner.runtime.clone(),
        };
        runtime.begin_operation(self.inner.generation)
    }

    /// Verifies that this handle still belongs to the mounted generation.
    pub fn validate(&self) -> Result<(), FsRuntimeError> {
        let state = self.inner.runtime.state.lock();
        validate_mounted_generation(&state, self.inner.generation)
    }

    pub(crate) fn generation_access(&self) -> FsGenerationAccess {
        FsGenerationAccess {
            inner: self.inner.runtime.clone(),
            generation: self.inner.generation,
        }
    }
}

impl fmt::Debug for FsOpenHandleLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FsOpenHandleLease")
            .field("generation", &self.inner.generation)
            .finish_non_exhaustive()
    }
}

impl Drop for FsOpenHandleLeaseInner {
    fn drop(&mut self) {
        let mut state = self.runtime.state.lock();
        debug_assert_eq!(state.generation, self.generation);
        state.open_handles = state
            .open_handles
            .checked_sub(1)
            .expect("filesystem open-handle lease count must not underflow");
    }
}

fn validate_mounted_generation(
    state: &FsRuntimeData,
    expected: FsGeneration,
) -> Result<(), FsRuntimeError> {
    if state.generation != expected {
        return Err(FsRuntimeError::StaleGeneration);
    }
    if state.state != FsRuntimeState::Mounted {
        return Err(FsRuntimeError::InvalidTransition);
    }
    Ok(())
}

fn validate_freeze_permit(
    state: &FsRuntimeData,
    permit: &FsFreezePermit,
) -> Result<(), FsRuntimeError> {
    if state.generation != permit.generation {
        return Err(FsRuntimeError::StaleGeneration);
    }
    if state.transaction != permit.transaction {
        return Err(FsRuntimeError::InvalidPermit);
    }
    if state.state != FsRuntimeState::Freezing {
        return Err(FsRuntimeError::InvalidTransition);
    }
    Ok(())
}

fn validate_no_generation_leases(state: &FsRuntimeData) -> Result<(), FsRuntimeError> {
    if state.active_operations != 0 || state.open_handles != 0 {
        return Err(FsRuntimeError::Busy);
    }
    Ok(())
}

fn validate_remount_permit(
    state: &FsRuntimeData,
    permit: &FsRemountPermit,
) -> Result<(), FsRuntimeError> {
    if state.generation != permit.previous_generation {
        return Err(FsRuntimeError::StaleGeneration);
    }
    if state.transaction != permit.transaction {
        return Err(FsRuntimeError::InvalidPermit);
    }
    if state.generation_cursor != permit.next_generation.0 {
        return Err(FsRuntimeError::InvalidPermit);
    }
    if state.state != FsRuntimeState::Remounting {
        return Err(FsRuntimeError::InvalidTransition);
    }
    Ok(())
}

fn next_counter(value: u64) -> Result<u64, FsRuntimeError> {
    value
        .checked_add(1)
        .ok_or(FsRuntimeError::GenerationExhausted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freeze_waits_for_operation_and_shared_open_handle_lease() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let operation = runtime.begin_operation(generation).unwrap();
        let handle = runtime.open_handle(generation).unwrap();
        let cloned_handle = handle.clone();

        let freeze = runtime.begin_freeze(generation).unwrap();
        assert_eq!(
            runtime.ensure_freeze_drained(&freeze),
            Err(FsRuntimeError::Busy)
        );
        assert_eq!(
            cloned_handle.begin_operation().unwrap_err(),
            FsRuntimeError::InvalidTransition
        );

        drop(operation);
        drop(handle);
        assert_eq!(
            runtime.ensure_freeze_drained(&freeze),
            Err(FsRuntimeError::Busy)
        );
        drop(cloned_handle);
        runtime.ensure_freeze_drained(&freeze).unwrap();
        runtime.finish_detach(&freeze).unwrap();
        assert_eq!(runtime.snapshot().state, FsRuntimeState::Detached);
    }

    #[test]
    fn generation_access_becomes_stale_without_counting_as_an_open_handle() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let handle = runtime.open_handle(generation).unwrap();
        let access = handle.generation_access();
        drop(handle);

        let freeze = runtime.begin_freeze(generation).unwrap();
        assert_eq!(
            access.begin_operation().unwrap_err(),
            FsRuntimeError::InvalidTransition
        );
        assert_eq!(runtime.snapshot().open_handles, 0);
        runtime.ensure_freeze_drained(&freeze).unwrap();
    }

    #[test]
    fn an_existing_operation_authorizes_only_its_own_generation_after_freeze() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let handle = runtime.open_handle(generation).unwrap();
        let access = handle.generation_access();
        drop(handle);
        let operation = runtime.begin_operation(generation).unwrap();
        let foreign_runtime = FsRuntime::new_mounted();
        let foreign_generation = foreign_runtime.snapshot().generation;
        let foreign_operation = foreign_runtime.begin_operation(foreign_generation).unwrap();

        let freeze = runtime.begin_freeze(generation).unwrap();

        assert_eq!(
            access.begin_operation().unwrap_err(),
            FsRuntimeError::InvalidTransition
        );
        assert_eq!(access.validate_operation(&operation), Ok(()));
        assert_eq!(
            access.validate_operation(&foreign_operation),
            Err(FsRuntimeError::InvalidPermit)
        );
        drop(operation);
        runtime.ensure_freeze_drained(&freeze).unwrap();
    }

    #[test]
    fn admitted_operation_can_publish_a_counted_handle_after_freeze_begins() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let operation = runtime.begin_operation(generation).unwrap();
        let foreign_runtime = FsRuntime::new_mounted();
        let foreign_generation = foreign_runtime.snapshot().generation;
        let foreign_operation = foreign_runtime.begin_operation(foreign_generation).unwrap();
        let freeze = runtime.begin_freeze(generation).unwrap();

        assert_eq!(
            runtime.open_handle(generation).unwrap_err(),
            FsRuntimeError::InvalidTransition
        );
        assert_eq!(
            runtime
                .open_handle_during(generation, &foreign_operation)
                .unwrap_err(),
            FsRuntimeError::InvalidPermit
        );
        assert_eq!(
            runtime
                .open_handle_during(FsGeneration(generation.get() + 1), &operation)
                .unwrap_err(),
            FsRuntimeError::StaleGeneration
        );
        assert_eq!(runtime.snapshot().open_handles, 0);

        let handle = runtime.open_handle_during(generation, &operation).unwrap();
        assert_eq!(runtime.snapshot().open_handles, 1);

        drop(operation);
        assert_eq!(
            runtime.ensure_freeze_drained(&freeze),
            Err(FsRuntimeError::Busy)
        );
        drop(handle);
        runtime.ensure_freeze_drained(&freeze).unwrap();
    }

    #[test]
    fn handles_from_an_old_generation_are_permanently_stale() {
        let runtime = FsRuntime::new_mounted();
        let old_generation = runtime.snapshot().generation;
        let handle = runtime.open_handle(old_generation).unwrap();

        let freeze = runtime.begin_freeze(old_generation).unwrap();
        drop(handle);
        runtime.finish_detach(&freeze).unwrap();
        let remount = runtime.begin_remount().unwrap();
        let new_generation = runtime.finish_remount(remount).unwrap();

        assert_ne!(old_generation, new_generation);
        assert_eq!(
            runtime.begin_operation(old_generation).unwrap_err(),
            FsRuntimeError::StaleGeneration
        );
        assert!(runtime.begin_operation(new_generation).is_ok());
    }

    #[test]
    fn cancelled_freeze_preserves_generation_and_allows_new_operations() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let freeze = runtime.begin_freeze(generation).unwrap();

        runtime.cancel_freeze(&freeze).unwrap();

        assert_eq!(runtime.snapshot().generation, generation);
        assert!(runtime.begin_operation(generation).is_ok());
    }

    #[test]
    fn failed_remount_can_retry_without_publishing_failed_root() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let freeze = runtime.begin_freeze(generation).unwrap();
        runtime.finish_detach(&freeze).unwrap();
        let failed = runtime.begin_remount().unwrap();
        let unpublished_generation = failed.next_generation();
        runtime.fail_remount(failed).unwrap();

        assert_eq!(runtime.snapshot().state, FsRuntimeState::Failed);
        assert_eq!(runtime.snapshot().generation, generation);

        let retry = runtime.begin_remount().unwrap();
        assert_ne!(retry.next_generation(), unpublished_generation);
        assert_eq!(
            retry.next_generation().get(),
            unpublished_generation.get() + 1
        );
        assert_eq!(runtime.finish_remount(retry).unwrap().get(), 3);
    }

    #[test]
    fn partial_detach_failure_cannot_be_cancelled_back_to_mounted() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let freeze = runtime.begin_freeze(generation).unwrap();

        runtime.fail_detach(&freeze).unwrap();

        assert_eq!(runtime.snapshot().state, FsRuntimeState::Failed);
        assert_eq!(
            runtime.cancel_freeze(&freeze),
            Err(FsRuntimeError::InvalidTransition)
        );
        assert!(runtime.begin_remount().is_ok());
    }

    #[test]
    fn detach_failure_cannot_bypass_live_generation_leases() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let operation = runtime.begin_operation(generation).unwrap();
        let freeze = runtime.begin_freeze(generation).unwrap();

        assert_eq!(runtime.fail_detach(&freeze), Err(FsRuntimeError::Busy));
        assert_eq!(runtime.snapshot().state, FsRuntimeState::Freezing);
        assert!(matches!(
            runtime.begin_remount(),
            Err(FsRuntimeError::InvalidTransition)
        ));

        drop(operation);
        runtime.fail_detach(&freeze).unwrap();
        assert_eq!(runtime.snapshot().state, FsRuntimeState::Failed);
    }

    #[test]
    fn freeze_progress_reports_each_pending_lease_class_until_drained() {
        let runtime = FsRuntime::new_mounted();
        let generation = runtime.snapshot().generation;
        let operation = runtime.begin_operation(generation).unwrap();
        let handle = runtime.open_handle(generation).unwrap();
        let cloned_handle = handle.clone();
        let freeze = runtime.begin_freeze(generation).unwrap();

        assert_eq!(
            runtime.freeze_progress(&freeze).unwrap(),
            FsFreezeProgress::Pending {
                active_operations: 1,
                open_handles: 1,
            }
        );
        drop(operation);
        assert_eq!(
            runtime.freeze_progress(&freeze).unwrap(),
            FsFreezeProgress::Pending {
                active_operations: 0,
                open_handles: 1,
            }
        );
        drop(handle);
        assert!(matches!(
            runtime.freeze_progress(&freeze).unwrap(),
            FsFreezeProgress::Pending { .. }
        ));
        drop(cloned_handle);
        assert_eq!(
            runtime.freeze_progress(&freeze).unwrap(),
            FsFreezeProgress::Drained
        );
    }
}
