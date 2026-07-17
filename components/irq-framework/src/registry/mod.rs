//! Dynamic IRQ registration, dispatch, and line-state coordination.

mod line;

use alloc::{boxed::Box, vec::Vec};
use core::{
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    AutoEnable, CpuId, DetachedIrqAction, IrqAffinity, IrqContext, IrqContinuationToken,
    IrqContinuationWake, IrqDrainToken, IrqDrainWake, IrqError, IrqExecution, IrqHandle, IrqId,
    IrqOps, IrqOutcome, IrqRequest, IrqReturn, IrqScope, IrqStatus, ReattachIrqActionError,
    action::Action,
    descriptor::{Descriptor, action_matches_cpu, recompute_scope_line_desired},
    detached::DetachedActionConfig,
    lock::MetadataLock,
};

/// Dynamic IRQ registry.
pub struct Registry<O: IrqOps> {
    ops: O,
    lock: MetadataLock,
    next_id: AtomicU64,
    state: UnsafeCell<RegistryState>,
}

unsafe impl<O: IrqOps + Send> Send for Registry<O> {}
unsafe impl<O: IrqOps + Sync> Sync for Registry<O> {}

struct RegistryState {
    descriptors: Vec<Descriptor>,
}

impl RegistryState {
    fn new() -> Self {
        Self {
            descriptors: Vec::new(),
        }
    }
}

impl<O: IrqOps> Registry<O> {
    /// Creates an empty registry.
    pub fn new(ops: O) -> Self {
        Self {
            ops,
            lock: MetadataLock::new(),
            next_id: AtomicU64::new(1),
            state: UnsafeCell::new(RegistryState::new()),
        }
    }

    /// Registers an IRQ action.
    ///
    /// This allocation- and controller-facing transaction is task-context
    /// only. The device-side source must remain masked until this method
    /// succeeds. The action gate stays closed until affinity and line-state
    /// restoration commit, so a late IRQ cannot call an unpublished handler.
    pub fn request(&self, irq: IrqId, mut request: IrqRequest) -> Result<IrqHandle, IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        self.validate_request(&request)?;

        let id = self.allocate_action_id()?;
        let snapshot = self.snapshot_and_disable_scope_line(irq, request.scope)?;
        let action = Box::new(Action::new(id, &mut request));
        let action = Box::into_raw(action);
        let irq_state = self.lock.lock(&self.ops);
        let result = self.insert_action_locked(irq, &request, action);
        self.lock.unlock(&self.ops, irq_state);

        if let Err(err) = result {
            unsafe {
                drop(Box::from_raw(action));
            }
            let _ = self.restore_scope_line_snapshot(irq, request.scope, &snapshot);
            return Err(err);
        }

        let handle = IrqHandle { irq, id };
        if let Err(err) = self.apply_affinity(irq, request.affinity) {
            self.rollback_new_action(handle);
            let _ = self.restore_scope_line_snapshot(irq, request.scope, &snapshot);
            return Err(err);
        }
        let enabled = request.auto_enable == AutoEnable::Yes;
        if let Err(error) = self.publish_new_action(handle, enabled) {
            self.rollback_new_action(handle);
            let _ = self.restore_scope_line_snapshot(irq, request.scope, &snapshot);
            return Err(error);
        }
        if let Err(error) = self.apply_enabled(handle, request.scope, enabled) {
            self.rollback_new_action(handle);
            let _ = self.restore_scope_line_snapshot(irq, request.scope, &snapshot);
            return Err(error);
        }
        Ok(handle)
    }

    /// Frees an IRQ action.
    pub fn free(&self, handle: IrqHandle) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        self.disable(handle)?;
        self.synchronize(handle)?;
        drop(self.detach_action(handle)?);
        Ok(())
    }

    /// Removes a disabled, drained action while retaining its handler.
    ///
    /// The action must already be disabled and have no invocation or drain
    /// notification in flight. Success invalidates `handle` and removes the
    /// action from descriptor sharing and affinity decisions. Failure leaves
    /// the action registered and `handle` valid.
    ///
    /// # Errors
    ///
    /// Returns [`IrqError::Busy`] while the action or descriptor is in flight,
    /// [`IrqError::NotFound`] for a stale handle, and
    /// [`IrqError::InIrqContext`] from hard-IRQ context.
    pub fn detach_action(&self, handle: IrqHandle) -> Result<DetachedIrqAction, IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }

        let irq_state = self.lock.lock(&self.ops);
        let result = self.detach_action_locked(handle);
        self.lock.unlock(&self.ops, irq_state);

        let (config, action) = result?;
        let action = unsafe {
            // SAFETY: `detach_action_locked` unlinked this pointer only after
            // all descriptor dispatch readers drained, transferring unique
            // ownership from the registry to this call.
            Box::from_raw(action)
        };
        Ok(DetachedIrqAction::new(config, action))
    }

    /// Registers a detached action under a fresh handle while keeping it disabled.
    ///
    /// The original scope, sharing, affinity, and execution policy are
    /// preserved. A failed registration returns unique ownership inside
    /// [`ReattachIrqActionError`] so the caller can retry or explicitly drop
    /// the handler. This operation is task-context only. The defensive
    /// [`IrqError::InIrqContext`] result still owns the detached action and
    /// must not be dropped on a hard-IRQ stack because its handler may release
    /// heap-backed state.
    ///
    /// # Errors
    ///
    /// Returns an error containing the original action when descriptor policy,
    /// CPU availability, execution context, or controller affinity prevents
    /// registration.
    pub fn reattach_action(
        &self,
        action: DetachedIrqAction,
    ) -> Result<IrqHandle, ReattachIrqActionError> {
        if self.ops.in_irq_context() {
            return Err(ReattachIrqActionError::new(IrqError::InIrqContext, action));
        }

        let config = action.config();
        if let IrqAffinity::Fixed(cpu) = config.affinity
            && !self.ops.cpu_online(cpu)
        {
            return Err(ReattachIrqActionError::new(IrqError::CpuOffline, action));
        }

        let id = match self.allocate_action_id() {
            Ok(id) => id,
            Err(reason) => return Err(ReattachIrqActionError::new(reason, action)),
        };

        let irq_state = self.lock.lock(&self.ops);
        let descriptor_index = match self.prepare_reattach_descriptor_locked(config) {
            Ok(index) => index,
            Err(reason) => {
                self.lock.unlock(&self.ops, irq_state);
                return Err(ReattachIrqActionError::new(reason, action));
            }
        };

        let action = action.into_registered_raw(id);
        self.insert_reattached_action_locked(descriptor_index, config, action);
        self.lock.unlock(&self.ops, irq_state);

        if let Err(reason) = self.apply_affinity(config.irq, config.affinity) {
            let action = self.recover_failed_reattach(config, action);
            return Err(ReattachIrqActionError::new(reason, action));
        }

        Ok(IrqHandle {
            irq: config.irq,
            id,
        })
    }

    /// Enables an IRQ action and its backing line.
    pub fn enable(&self, handle: IrqHandle) -> Result<(), IrqError> {
        let scope = self.set_action_enabled(handle, true)?;

        if let Err(err) = self.apply_enabled(handle, scope, true) {
            let _ = self.disable(handle);
            return Err(err);
        }
        Ok(())
    }

    /// Disables an IRQ action and its backing line.
    pub fn disable(&self, handle: IrqHandle) -> Result<(), IrqError> {
        let scope = self.set_action_enabled(handle, false)?;
        self.apply_enabled(handle, scope, false)
    }

    /// Releases a fail-closed global-line quench owned by this action.
    ///
    /// A global handler returning [`IrqReturn::QuenchAndWake`] disables its
    /// action and masks the complete backing line. Recovery must first mask the
    /// device's own interrupt source, then call this method so unrelated
    /// actions sharing the line can run again. The global action itself
    /// remains disabled. Per-CPU actions must use
    /// [`Registry::release_per_cpu_quench`] instead.
    ///
    /// Calling this method again after a controller update failed is safe: the
    /// framework reapplies the descriptor's current desired line state.
    ///
    /// # Errors
    ///
    /// Returns [`IrqError::InIrqContext`] from hard-IRQ context,
    /// [`IrqError::InvalidCpu`] for a per-CPU action,
    /// [`IrqError::NotFound`] for a stale handle, or a controller error when
    /// the backing line cannot be updated.
    pub fn release_quench(&self, handle: IrqHandle) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }

        self.clear_action_quench(handle, None)?;
        self.apply_line_state(handle.irq, None)
    }

    /// Completes one ordinary deferred IRQ acknowledgement generation.
    ///
    /// The caller must have acknowledged the device-side source represented
    /// by `token`, or have masked that source as part of controller teardown.
    /// A stale or already-consumed token returns [`IrqError::NotFound`] and can
    /// never clear a newer continuation generation.
    pub fn finish_continuation(
        &self,
        token: IrqContinuationToken,
    ) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        let handle = token.handle;
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == handle.irq)
                .ok_or(IrqError::NotFound)?;
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                (*action).finish_continuation(token.epoch)?;
                recompute_scope_line_desired(descriptor, (*action).scope);
            }
            Ok(())
        })();
        self.lock.unlock(&self.ops, irq_state);
        result?;
        self.apply_line_state(handle.irq, None)
    }

    /// Releases one CPU's fail-closed quench ownership for a per-CPU action.
    ///
    /// Recovery must mask that CPU's device-side source before calling this
    /// method. Other CPUs retain independent quench ownership and remain
    /// masked until explicitly released.
    ///
    /// Calling this method again after a controller update failed is safe: the
    /// framework reapplies the selected CPU's current desired line state.
    ///
    /// # Errors
    ///
    /// Returns [`IrqError::InIrqContext`] from hard-IRQ context,
    /// [`IrqError::InvalidCpu`] for a global action or a CPU outside the action
    /// scope, [`IrqError::NotFound`] for a stale handle, or a controller error.
    pub fn release_per_cpu_quench(&self, handle: IrqHandle, cpu: CpuId) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }

        self.clear_action_quench(handle, Some(cpu))?;
        self.apply_line_state(handle.irq, Some(cpu))
    }

    /// Disables one action and notifies a fixed target after that action drains.
    ///
    /// Other actions sharing the descriptor are not part of the returned
    /// token and remain dispatchable. The notification may run in hard-IRQ
    /// context when the selected action's final invocation returns.
    pub fn disable_async(
        &self,
        handle: IrqHandle,
        wake: &'static IrqDrainWake,
    ) -> Result<IrqDrainToken, IrqError> {
        let (scope, epoch, action) = self.begin_action_drain(handle, wake)?;
        let apply_result = self.apply_enabled(handle, scope, false);
        unsafe {
            // SAFETY: `begin_action_drain` pins the descriptor until the
            // matching `end_dispatch` below, so the action cannot be unlinked
            // while an immediate notification reads it.
            (*action).signal_drain_if_ready();
        }
        self.end_dispatch(handle.irq);
        apply_result?;
        Ok(IrqDrainToken { handle, epoch })
    }

    /// Returns whether the selected action and drain generation are complete.
    pub fn action_drain_complete(&self, token: IrqDrainToken) -> Result<bool, IrqError> {
        self.with_action(token.handle, |action| action.drain_complete(token.epoch))
    }

    /// Waits until no handler is in flight for this IRQ descriptor.
    pub fn synchronize(&self, handle: IrqHandle) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        loop {
            let in_flight = self.with_action(handle, |_| {
                self.descriptor(handle.irq)
                    .map(|desc| desc.in_flight.load(Ordering::Acquire))
                    .unwrap_or(0)
            })?;
            if in_flight == 0 {
                return Ok(());
            }
            self.ops.relax();
        }
    }

    fn set_action_enabled(&self, handle: IrqHandle, enabled: bool) -> Result<IrqScope, IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == handle.irq)
                .ok_or(IrqError::NotFound)?;
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                (*action).set_enabled(enabled)?;
                (*action).clear_pending_enable_all();
                let scope = (*action).scope;
                recompute_scope_line_desired(descriptor, scope);
                Ok(scope)
            }
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn begin_action_drain(
        &self,
        handle: IrqHandle,
        wake: &'static IrqDrainWake,
    ) -> Result<(IrqScope, u64, *mut Action), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == handle.irq)
                .ok_or(IrqError::NotFound)?;
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                descriptor
                    .in_flight
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                        count.checked_add(1)
                    })
                    .map_err(|_| IrqError::Busy)?;
                let epoch = match (*action).begin_drain(wake) {
                    Ok(epoch) => epoch,
                    Err(error) => {
                        descriptor.in_flight.fetch_sub(1, Ordering::AcqRel);
                        return Err(error);
                    }
                };
                (*action).clear_pending_enable_all();
                let scope = (*action).scope;
                recompute_scope_line_desired(descriptor, scope);
                Ok((scope, epoch, action))
            }
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn quench_action(&self, handle: IrqHandle, cpu: CpuId) -> Result<(), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == handle.irq)
                .ok_or(IrqError::NotFound)?;
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                (*action).record_quench(cpu)?;
                let scope = (*action).scope;
                if scope == IrqScope::Global {
                    (*action).set_enabled(false)?;
                    (*action).clear_pending_enable_all();
                }
                recompute_scope_line_desired(descriptor, scope);
                Ok(scope)
            }
        })();
        self.lock.unlock(&self.ops, irq_state);

        match result? {
            IrqScope::Global => self.apply_line_state(handle.irq, None),
            IrqScope::PerCpu { .. } => self.apply_line_state(handle.irq, Some(cpu)),
        }
    }

    fn defer_action(
        &self,
        handle: IrqHandle,
        wake: &'static IrqContinuationWake,
    ) -> Result<(), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == handle.irq)
                .ok_or(IrqError::NotFound)?;
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                let epoch = (*action).begin_continuation()?;
                recompute_scope_line_desired(descriptor, (*action).scope);
                Ok(IrqContinuationToken { handle, epoch })
            }
        })();
        self.lock.unlock(&self.ops, irq_state);

        let token = result?;
        // The line must be masked before task-side work can observe the token.
        // A controller failure here is fatal to IRQ return, just like the
        // emergency quench path: publishing a token first could otherwise let
        // task context reopen a level source that was never excluded.
        self.apply_line_state(handle.irq, None)?;
        wake.notify(token);
        Ok(())
    }

    fn clear_action_quench(&self, handle: IrqHandle, cpu: Option<CpuId>) -> Result<(), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == handle.irq)
                .ok_or(IrqError::NotFound)?;
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                match ((*action).scope, cpu) {
                    (IrqScope::Global, None) => (*action).release_global_quench(),
                    (IrqScope::PerCpu { cpus }, Some(cpu)) if cpus.contains(cpu) => {
                        (*action).release_cpu_quench(cpu)?;
                    }
                    _ => return Err(IrqError::InvalidCpu),
                }
                descriptor.recompute_line_desired(cpu);
                Ok(())
            }
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn clear_action_quench_all(&self, handle: IrqHandle) -> Result<(), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == handle.irq)
                .ok_or(IrqError::NotFound)?;
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                (*action).release_quench_all();
                recompute_scope_line_desired(descriptor, (*action).scope);
            }
            Ok(())
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    /// Returns a status snapshot for an IRQ action.
    pub fn status(&self, handle: IrqHandle) -> Result<IrqStatus, IrqError> {
        let (scope, action_enabled, quench_owned, continuation_pending, in_flight) =
            self.with_action(handle, |action| {
                let in_flight = self
                    .descriptor(handle.irq)
                    .map(|desc| desc.in_flight.load(Ordering::Acquire))
                    .unwrap_or(0);
                (
                    action.scope,
                    action.enabled(),
                    action.has_quench(),
                    action.has_continuation(),
                    in_flight,
                )
            })?;
        let action_running =
            self.with_action(handle, |action| action.running.load(Ordering::Acquire))?;
        let cpu = status_cpu(scope, self.ops.current_cpu());
        let line_enabled = match self.ops.is_enabled(handle.irq, cpu) {
            Ok(enabled) => enabled,
            Err(IrqError::Unsupported) => self.framework_line_enabled(handle.irq, cpu)?,
            Err(err) => return Err(err),
        };
        let pending = match self.ops.is_pending(handle.irq, cpu) {
            Ok(pending) => pending,
            Err(IrqError::Unsupported) => false,
            Err(err) => return Err(err),
        };
        let in_service = match self.ops.is_in_service(handle.irq, cpu) {
            Ok(in_service) => in_service,
            Err(IrqError::Unsupported) => false,
            Err(err) => return Err(err),
        };
        Ok(IrqStatus {
            action_enabled,
            quench_owned,
            continuation_pending,
            line_enabled,
            pending,
            in_service,
            in_flight,
            action_running,
        })
    }

    /// Dispatches an IRQ on the given CPU from hard-IRQ context.
    ///
    /// This path performs no allocation or reclamation. It invokes only the
    /// endpoints already owned by enabled actions; endpoint providers must
    /// uphold the bounded hard-IRQ callback contract documented by
    /// [`crate::BoxedIrqHandler`].
    pub fn dispatch(&self, irq: IrqId, cpu: CpuId) -> IrqOutcome {
        let Some(head) = self.begin_dispatch(irq) else {
            return IrqOutcome::default();
        };
        let _guard = DispatchGuard {
            registry: self,
            irq,
        };

        let mut outcome = IrqOutcome::default();
        let ctx = IrqContext { irq, cpu };
        let mut next = head;
        while !next.is_null() {
            let action = unsafe { &*next };
            next = action.next;
            if action.detached.load(Ordering::Acquire) || !action_matches_cpu(action.scope, cpu) {
                continue;
            }
            if action.quench_applies(Some(cpu)) || action.has_continuation() {
                continue;
            }

            let Some(_guard) = ActionRunGuard::enter(action) else {
                continue;
            };

            outcome.called += 1;
            match action.call(ctx) {
                IrqReturn::Unhandled => {}
                IrqReturn::Handled => outcome.handled = true,
                IrqReturn::Wake => {
                    outcome.handled = true;
                    outcome.wake = true;
                }
                IrqReturn::Defer(wake) => {
                    outcome.handled = true;
                    outcome.wake = true;
                    if let Err(error) =
                        self.defer_action(IrqHandle { irq, id: action.id }, wake)
                    {
                        panic!(
                            "IRQ controller failed the deferred continuation invariant for \
                             {irq:?} on CPU {}: {error:?}",
                            cpu.0,
                        );
                    }
                }
                IrqReturn::QuenchAndWake => {
                    outcome.handled = true;
                    outcome.wake = true;
                    // The callback still owns its active gate reference. The
                    // metadata transition may clear the enabled bit while that
                    // reference is live, and line masking completes before the
                    // dispatch returns to the platform EOI path.
                    if let Err(error) = self.quench_action(IrqHandle { irq, id: action.id }, cpu) {
                        panic!(
                            "IRQ controller failed the emergency line quench invariant for \
                             {irq:?} on CPU {}: {error:?}",
                            cpu.0,
                        );
                    }
                }
            }
        }

        outcome
    }

    /// Marks a CPU online and applies pending per-CPU enables for that CPU.
    ///
    /// This startup operation allocates temporary bookkeeping and is not
    /// available from hard-IRQ context.
    pub fn cpu_online(&self, cpu: CpuId) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        if !self.ops.cpu_online(cpu) {
            return Err(IrqError::CpuOffline);
        }
        let pending = self.pending_enables_for_cpu(cpu);
        for irq in pending {
            self.apply_line_state(irq, Some(cpu))?;
            self.clear_pending_enable_for_cpu(irq, cpu);
        }
        Ok(())
    }

    /// Marks a CPU offline from the framework's perspective.
    pub fn cpu_offline(&self, cpu: CpuId) -> Result<(), IrqError> {
        if self.ops.cpu_online(cpu) {
            return Err(IrqError::Unsupported);
        }
        Ok(())
    }

    fn validate_request(&self, request: &IrqRequest) -> Result<(), IrqError> {
        if request.execution == IrqExecution::Concurrent && !request.supports_concurrent() {
            return Err(IrqError::Busy);
        }
        if let IrqScope::PerCpu { cpus } = request.scope
            && cpus.is_empty()
        {
            return Err(IrqError::InvalidCpu);
        }
        if let IrqAffinity::Fixed(cpu) = request.affinity
            && !self.ops.cpu_online(cpu)
        {
            return Err(IrqError::CpuOffline);
        }
        Ok(())
    }

    fn allocate_action_id(&self) -> Result<u64, IrqError> {
        self.next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| IrqError::Busy)
    }

    fn insert_action_locked(
        &self,
        irq: IrqId,
        request: &IrqRequest,
        action: *mut Action,
    ) -> Result<(), IrqError> {
        let state = unsafe { &mut *self.state.get() };
        let descriptor = match state
            .descriptors
            .iter_mut()
            .find(|descriptor| descriptor.irq == irq)
        {
            Some(descriptor) => descriptor,
            None => {
                state.descriptors.push(Descriptor::new(irq, request));
                state.descriptors.last_mut().ok_or(IrqError::NoMemory)?
            }
        };
        descriptor.compatible_with(request)?;
        unsafe {
            (*action).next = descriptor.head;
        }
        descriptor.head = action;
        recompute_scope_line_desired(descriptor, request.scope);
        Ok(())
    }

    fn detach_action_locked(
        &self,
        handle: IrqHandle,
    ) -> Result<(DetachedActionConfig, *mut Action), IrqError> {
        let state = unsafe { &mut *self.state.get() };
        let descriptor = state
            .descriptors
            .iter_mut()
            .find(|descriptor| descriptor.irq == handle.irq)
            .ok_or(IrqError::NotFound)?;
        let action = descriptor
            .actions()
            .find(|action| unsafe { (**action).id == handle.id })
            .ok_or(IrqError::NotFound)?;

        unsafe {
            // SAFETY: the metadata lock excludes list mutation, and the
            // descriptor-wide in-flight count proves no dispatch reader holds
            // this action or its next pointer.
            if (*action).detached.load(Ordering::Acquire) {
                return Err(IrqError::NotFound);
            }
            if !(*action).is_detachable() || descriptor.in_flight.load(Ordering::Acquire) != 0 {
                return Err(IrqError::Busy);
            }

            let config = descriptor.detached_config((*action).scope, (*action).execution);
            if !unlink_action(descriptor, action) {
                return Err(IrqError::NotFound);
            }
            (*action).prepare_for_detached_storage();
            Ok((config, action))
        }
    }

    fn prepare_reattach_descriptor_locked(
        &self,
        config: DetachedActionConfig,
    ) -> Result<usize, IrqError> {
        let state = unsafe { &mut *self.state.get() };
        let index = match state
            .descriptors
            .iter()
            .position(|descriptor| descriptor.irq == config.irq)
        {
            Some(index) => index,
            None => {
                state.descriptors.push(Descriptor::new_with_config(
                    config.irq,
                    config.share_mode,
                    config.affinity,
                ));
                state.descriptors.len() - 1
            }
        };
        state.descriptors[index].compatible_with_detached(config)?;
        Ok(index)
    }

    fn insert_reattached_action_locked(
        &self,
        descriptor_index: usize,
        config: DetachedActionConfig,
        action: *mut Action,
    ) {
        let state = unsafe { &mut *self.state.get() };
        let descriptor = &mut state.descriptors[descriptor_index];
        debug_assert_eq!(descriptor.irq, config.irq);
        unsafe {
            // SAFETY: `action` is uniquely owned by the consumed detached
            // token, and the metadata lock exclusively owns this list update.
            (*action).next = descriptor.head;
        }
        descriptor.head = action;
        recompute_scope_line_desired(descriptor, config.scope);
    }

    fn recover_failed_reattach(
        &self,
        config: DetachedActionConfig,
        action: *mut Action,
    ) -> DetachedIrqAction {
        unsafe {
            // SAFETY: the failed reattach has not returned its new handle, so
            // no caller can concurrently mutate this disabled action.
            (*action).detached.store(true, Ordering::Release);
        }
        loop {
            match self.try_remove_action(config.irq, action) {
                Ok(()) | Err(IrqError::NotFound) => break,
                Err(IrqError::Busy) => self.ops.relax(),
                Err(_) => unreachable!("IRQ action removal returned an undocumented error"),
            }
        }
        unsafe {
            // SAFETY: the action was never exposed through a returned handle,
            // has been removed from the descriptor, and is disabled/drained.
            DetachedIrqAction::from_registered_raw(config, action)
        }
    }

    fn try_remove_action(&self, irq: IrqId, action: *mut Action) -> Result<(), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == irq)
                .ok_or(IrqError::NotFound)?;
            if descriptor.in_flight.load(Ordering::Acquire) != 0 {
                return Err(IrqError::Busy);
            }
            unlink_action(descriptor, action)
                .then_some(())
                .ok_or(IrqError::NotFound)
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn rollback_new_action(&self, handle: IrqHandle) {
        let result: Result<(), IrqError> = (|| {
            self.set_action_enabled(handle, false)?;
            self.synchronize(handle)?;
            self.clear_action_quench_all(handle)?;
            drop(self.detach_action(handle)?);
            Ok(())
        })();
        if let Err(error) = result {
            panic!("failed to roll back unpublished IRQ action {handle:?}: {error:?}");
        }
    }

    fn publish_new_action(&self, handle: IrqHandle, enabled: bool) -> Result<(), IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let state = unsafe { &mut *self.state.get() };
            let descriptor = state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == handle.irq)
                .ok_or(IrqError::NotFound)?;
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                (*action).set_enabled(enabled)?;
                recompute_scope_line_desired(descriptor, (*action).scope);
            }
            Ok(())
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn with_action<T>(
        &self,
        handle: IrqHandle,
        f: impl FnOnce(&Action) -> T,
    ) -> Result<T, IrqError> {
        let irq_state = self.lock.lock(&self.ops);
        let result = (|| {
            let action = self.find_action(handle).ok_or(IrqError::NotFound)?;
            Ok(f(action))
        })();
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn begin_dispatch(&self, irq: IrqId) -> Option<*mut Action> {
        let irq_state = self.lock.lock(&self.ops);
        let result = {
            let state = unsafe { &mut *self.state.get() };
            state
                .descriptors
                .iter_mut()
                .find(|descriptor| descriptor.irq == irq)
                .and_then(|descriptor| {
                    if descriptor.head.is_null() {
                        None
                    } else {
                        assert!(
                            descriptor
                                .in_flight
                                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| count
                                    .checked_add(1),)
                                .is_ok(),
                            "IRQ descriptor in-flight count overflowed"
                        );
                        Some(descriptor.head)
                    }
                })
        };
        self.lock.unlock(&self.ops, irq_state);
        result
    }

    fn end_dispatch(&self, irq: IrqId) {
        let irq_state = self.lock.lock(&self.ops);
        let state = unsafe { &mut *self.state.get() };
        if let Some(descriptor) = state
            .descriptors
            .iter_mut()
            .find(|descriptor| descriptor.irq == irq)
        {
            assert!(
                descriptor
                    .in_flight
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                        count.checked_sub(1)
                    })
                    .is_ok(),
                "IRQ descriptor in-flight count underflowed"
            );
        }
        self.lock.unlock(&self.ops, irq_state);
    }

    fn find_action(&self, handle: IrqHandle) -> Option<&Action> {
        self.descriptor(handle.irq)?
            .actions()
            .map(|action| unsafe { &*action })
            .find(|action| action.id == handle.id && !action.detached.load(Ordering::Acquire))
    }

    fn descriptor(&self, irq: IrqId) -> Option<&Descriptor> {
        self.state_ref()
            .descriptors
            .iter()
            .find(|descriptor| descriptor.irq == irq)
    }

    fn state_ref(&self) -> &RegistryState {
        unsafe { &*self.state.get() }
    }
}

struct DispatchGuard<'a, O: IrqOps> {
    registry: &'a Registry<O>,
    irq: IrqId,
}

struct ActionRunGuard<'a> {
    action: &'a Action,
}

impl<'a> ActionRunGuard<'a> {
    fn enter(action: &'a Action) -> Option<Self> {
        if !action.try_enter() {
            return None;
        }
        match action.execution {
            IrqExecution::Concurrent => Some(Self { action }),
            IrqExecution::NonReentrant => {
                if action
                    .running
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    Some(Self { action })
                } else {
                    action.leave();
                    None
                }
            }
        }
    }
}

impl Drop for ActionRunGuard<'_> {
    fn drop(&mut self) {
        if self.action.execution == IrqExecution::NonReentrant {
            self.action.running.store(false, Ordering::Release);
        }
        self.action.leave();
    }
}

impl<O: IrqOps> Drop for DispatchGuard<'_, O> {
    fn drop(&mut self) {
        self.registry.end_dispatch(self.irq);
    }
}

fn status_cpu(scope: IrqScope, current: CpuId) -> Option<CpuId> {
    match scope {
        IrqScope::Global => None,
        IrqScope::PerCpu { .. } => Some(current),
    }
}

fn unlink_action(descriptor: &mut Descriptor, action: *mut Action) -> bool {
    let mut link = &mut descriptor.head as *mut *mut Action;
    // SAFETY: callers hold the registry metadata lock, which exclusively owns
    // every intrusive `next` link. They additionally prove that no dispatch
    // reader remains before reclaiming the unlinked allocation.
    while unsafe { !(*link).is_null() } {
        let current = unsafe { *link };
        if current == action {
            unsafe {
                *link = (*current).next;
                (*current).next = ptr::null_mut();
            }
            return true;
        }
        link = unsafe { &mut (*current).next as *mut *mut Action };
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AutoEnable, HwIrq, IrqDomainId};

    struct TestOps;

    // SAFETY: this single-threaded adapter invokes every CPU thunk inline and
    // never retains its raw argument.
    unsafe impl IrqOps for TestOps {
        type LocalIrqState = ();

        fn current_cpu(&self) -> CpuId {
            CpuId(0)
        }

        fn cpu_online(&self, cpu: CpuId) -> bool {
            cpu == CpuId(0)
        }

        fn in_irq_context(&self) -> bool {
            false
        }

        fn local_irq_save(&self) -> Self::LocalIrqState {}

        fn local_irq_restore(&self, _state: Self::LocalIrqState) {}

        fn run_on_cpu_sync(
            &self,
            _cpu: CpuId,
            f: unsafe fn(*mut ()),
            arg: *mut (),
        ) -> Result<(), IrqError> {
            unsafe { f(arg) };
            Ok(())
        }

        fn set_enabled(
            &self,
            _irq: IrqId,
            _cpu: Option<CpuId>,
            _enabled: bool,
        ) -> Result<(), IrqError> {
            Ok(())
        }

        fn is_enabled(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
            Ok(false)
        }

        fn is_pending(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
            Ok(false)
        }

        fn is_in_service(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
            Ok(false)
        }

        fn relax(&self) {
            core::hint::spin_loop();
        }
    }

    #[test]
    fn exhausted_action_id_never_wraps_into_a_stale_handle_generation() {
        let registry = Registry::new(TestOps);
        registry.next_id.store(u64::MAX, Ordering::Relaxed);
        let irq = IrqId::new(IrqDomainId(1), HwIrq(1));

        assert_eq!(
            registry.request(
                irq,
                IrqRequest::new(|_| IrqReturn::Handled).auto_enable(AutoEnable::No),
            ),
            Err(IrqError::Busy)
        );
        assert_eq!(registry.next_id.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    #[should_panic(expected = "IRQ descriptor in-flight count overflowed")]
    fn descriptor_reader_count_never_wraps_before_reclamation() {
        let registry = Registry::new(TestOps);
        let irq = IrqId::new(IrqDomainId(1), HwIrq(2));
        registry
            .request(irq, IrqRequest::new(|_| IrqReturn::Handled))
            .unwrap();
        registry
            .descriptor(irq)
            .unwrap()
            .in_flight
            .store(usize::MAX, Ordering::Release);

        let _ = registry.begin_dispatch(irq);
    }

    #[test]
    #[should_panic(expected = "IRQ descriptor in-flight count underflowed")]
    fn descriptor_reader_count_never_underflows_after_dispatch() {
        let registry = Registry::new(TestOps);
        let irq = IrqId::new(IrqDomainId(1), HwIrq(3));
        registry
            .request(irq, IrqRequest::new(|_| IrqReturn::Handled))
            .unwrap();

        registry.end_dispatch(irq);
    }
}
