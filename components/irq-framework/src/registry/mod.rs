//! Dynamic IRQ registration, dispatch, and line-state coordination.

mod line;

use alloc::{boxed::Box, vec::Vec};
use core::{
    cell::UnsafeCell,
    pin::Pin,
    ptr,
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
};

use crate::{
    AutoEnable, CpuId, DetachedIrqAction, IrqAffinity, IrqContext, IrqDrainToken, IrqDrainWake,
    IrqError, IrqExecution, IrqHandle, IrqId, IrqLineControl, IrqOps, IrqOutcome, IrqRequest,
    IrqReturn, IrqScope, IrqStatus, ReattachIrqActionError, ReleasedIrqLineProof,
    action::Action,
    descriptor::{Descriptor, action_matches_cpu, recompute_scope_line_desired},
    detached::DetachedActionConfig,
    lock::MetadataLock,
};

const DESCRIPTOR_CATALOG_CAPACITY: usize = 4096;

/// Dynamic IRQ registry.
pub struct Registry<O: IrqOps> {
    ops: O,
    /// Protects only descriptor lookup and insertion. Mutable line state is
    /// protected by the stable descriptor's own lock.
    catalog_lock: MetadataLock,
    descriptor_catalog: [AtomicPtr<Descriptor>; DESCRIPTOR_CATALOG_CAPACITY],
    next_id: AtomicU64,
    state: UnsafeCell<RegistryState>,
}

unsafe impl<O: IrqOps + Send> Send for Registry<O> {}
unsafe impl<O: IrqOps + Sync> Sync for Registry<O> {}

struct RegistryState {
    /// Boxed descriptors retain a stable address for their per-line irqchip
    /// transition lock. Empty descriptors deliberately remain until registry
    /// teardown. Once a descriptor owns a prepared line binding, its scope and
    /// route remain canonical even while the action list is empty.
    retained: Vec<Pin<Box<Descriptor>>>,
}

impl RegistryState {
    fn new() -> Self {
        Self {
            retained: Vec::with_capacity(DESCRIPTOR_CATALOG_CAPACITY),
        }
    }
}

impl<O: IrqOps> Registry<O> {
    /// Creates an empty registry.
    pub fn new(ops: O) -> Self {
        Self {
            ops,
            catalog_lock: MetadataLock::new(),
            descriptor_catalog: [const { AtomicPtr::new(ptr::null_mut()) };
                DESCRIPTOR_CATALOG_CAPACITY],
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

        // Complete every allocation before the first fallible irqchip
        // ownership transition. Once `prepare_registration_line` succeeds,
        // publication contains only metadata writes and an infallible live
        // mask/unmask operation on the prepared endpoint.
        let id = self.allocate_action_id()?;
        let action = Box::new(Action::new(id, &mut request));
        let needs_prepare = self.begin_line_registration(irq, &request)?;
        if let Err(error) = self.prepare_registration_line(irq, &request, needs_prepare) {
            self.finish_line_registration(irq)
                .expect("failed IRQ preparation lost its registration reservation");
            return Err(error);
        }
        let action = Box::into_raw(action);
        let handle = IrqHandle { irq, id };
        let enabled = request.auto_enable == AutoEnable::Yes;
        self.commit_new_action_registration(irq, &request, action, enabled);
        self.apply_enabled(handle, request.scope, enabled)
            .expect("published IRQ action lost its prepared line binding");
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

        let result = self.with_descriptor(handle.irq, |descriptor| {
            Self::detach_action_locked(descriptor, handle)
        });

        let (config, action) = result?;
        let action = unsafe {
            // SAFETY: `detach_action_locked` unlinked this pointer only after
            // all descriptor dispatch readers drained, transferring unique
            // ownership from the registry to this call.
            Box::from_raw(action)
        };
        Ok(DetachedIrqAction::new(config, action))
    }

    /// Detaches the sole action and releases its prepared controller line.
    ///
    /// The selected action may use shared registration policy, but it must be
    /// the descriptor's only action. It must already be disabled and drained,
    /// and its global maskable line must have no desired/applied enable state or
    /// active controller claim. The descriptor reserves the line before calling
    /// the platform without framework locks. A failed platform release rolls
    /// the reservation back and leaves `handle`, its action, and the old binding
    /// usable.
    ///
    /// # Errors
    ///
    /// Returns [`IrqError::Busy`] if the action, a peer, registration, dispatch,
    /// controller claim, or line state still owns the descriptor. Platform
    /// release failures are returned after transactional rollback. Hard-IRQ
    /// callers receive [`IrqError::InIrqContext`].
    pub fn detach_action_and_release_line(
        &self,
        handle: IrqHandle,
    ) -> Result<(DetachedIrqAction, ReleasedIrqLineProof), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }

        let (prepared, config, action) = self.with_descriptor(handle.irq, |descriptor| {
            descriptor.begin_line_release(handle.id)
        })?;
        if let Err(error) = self.ops.release_line(prepared.binding()) {
            self.with_descriptor(handle.irq, |descriptor| {
                descriptor.rollback_line_release(prepared);
                Ok(())
            })
            .expect("failed IRQ line release lost its descriptor reservation");
            return Err(error);
        }

        self.with_descriptor(handle.irq, |descriptor| {
            assert!(
                unlink_action(descriptor, action),
                "released IRQ line lost its sole action before metadata commit"
            );
            unsafe {
                // SAFETY: the release reservation excludes registration and
                // dispatch, and successful platform release makes this the sole
                // remaining owner of the unlinked action allocation.
                (*action).prepare_for_detached_storage();
            }
            descriptor.finish_line_release(prepared);
            Ok(())
        })
        .expect("released IRQ line descriptor disappeared before metadata commit");

        let action = unsafe {
            // SAFETY: the infallible metadata commit above unlinked the sole
            // action and transferred unique allocation ownership to this call.
            Box::from_raw(action)
        };
        Ok((
            DetachedIrqAction::new(config, action),
            ReleasedIrqLineProof::new(handle.irq, prepared.binding()),
        ))
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

        let needs_prepare = match self.begin_detached_line_registration(config) {
            Ok(needs_prepare) => needs_prepare,
            Err(reason) => return Err(ReattachIrqActionError::new(reason, action)),
        };
        if let Err(reason) = self.prepare_detached_registration_line(config, needs_prepare) {
            self.finish_line_registration(config.irq)
                .expect("failed detached IRQ preparation lost its registration reservation");
            return Err(ReattachIrqActionError::new(reason, action));
        }

        let action = action.into_registered_raw(id);
        self.commit_reattached_action_registration(config, action);

        Ok(IrqHandle {
            irq: config.irq,
            id,
        })
    }

    /// Enables an IRQ action and its backing line.
    pub fn enable(&self, handle: IrqHandle) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        let scope = self.set_action_enabled(handle, true)?;
        self.apply_enabled(handle, scope, true)
    }

    /// Disables an IRQ action and its backing line.
    pub fn disable(&self, handle: IrqHandle) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        let scope = self.set_action_enabled(handle, false)?;
        self.apply_enabled(handle, scope, false)
    }

    /// Acquires fail-closed backing-line containment for an action from task
    /// context.
    ///
    /// Device activation uses this when it cannot prove that its exact source
    /// was masked before enabling the action. The action may still be
    /// disabled; its quench ownership nevertheless keeps a shared backing line
    /// masked until recovery establishes device-side containment and calls
    /// [`Self::release_quench`].
    ///
    /// # Errors
    ///
    /// Returns [`IrqError::InIrqContext`] from hard IRQ context, a scope or
    /// stale-handle error from the registry, or a controller error while
    /// applying the fail-closed line state.
    pub fn quench(&self, handle: IrqHandle) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        self.record_action_quench(handle, None)?;
        self.apply_line_state(handle.irq, None)
    }

    /// Acquires fail-closed containment for one instance of a per-CPU action.
    ///
    /// The caller supplies the exact CPU identity; this operation never uses a
    /// migratable current-CPU snapshot to select controller ownership.
    pub fn quench_per_cpu(&self, handle: IrqHandle, cpu: CpuId) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        self.record_action_quench(handle, Some(cpu))?;
        self.apply_line_state(handle.irq, Some(cpu))
    }

    /// Releases a fail-closed global-line quench owned by this action.
    ///
    /// A global handler returning [`IrqReturn::MaskLineAndWake`] masks the
    /// complete backing line without disabling the action. Recovery must first
    /// mask or reset the device's own interrupt source, then call this method
    /// so the action and unrelated peers sharing the line can run again.
    /// Per-CPU actions must use
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
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        let (scope, epoch, action) = self.begin_action_drain(handle, wake)?;
        self.apply_enabled(handle, scope, false)
            .expect("draining IRQ action lost its prepared line binding");
        unsafe {
            // SAFETY: `begin_action_drain` pins the descriptor until the
            // matching `end_dispatch` below, so the action cannot be unlinked
            // while an immediate notification reads it.
            (*action).signal_drain_if_ready();
        }
        self.end_reader_pin(handle.irq);
        Ok(IrqDrainToken { handle, epoch })
    }

    /// Returns whether the selected action and drain generation are complete.
    pub fn action_drain_complete(&self, token: IrqDrainToken) -> Result<bool, IrqError> {
        self.with_action(token.handle, |_, action| action.drain_complete(token.epoch))
    }

    /// Waits until no handler is in flight for this IRQ descriptor.
    pub fn synchronize(&self, handle: IrqHandle) -> Result<(), IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        loop {
            let in_flight = self.with_action(handle, |descriptor, _| {
                descriptor.in_flight.load(Ordering::Acquire)
            })?;
            if in_flight == 0 {
                return Ok(());
            }
            self.ops.relax();
        }
    }

    fn set_action_enabled(&self, handle: IrqHandle, enabled: bool) -> Result<IrqScope, IrqError> {
        self.with_descriptor(handle.irq, |descriptor| {
            if !descriptor.line_accepts_action_transition() {
                return Err(IrqError::Busy);
            }
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                (*action).set_enabled(enabled)?;
                let scope = (*action).scope;
                recompute_scope_line_desired(descriptor, scope);
                Ok(scope)
            }
        })
    }

    fn begin_action_drain(
        &self,
        handle: IrqHandle,
        wake: &'static IrqDrainWake,
    ) -> Result<(IrqScope, u64, *mut Action), IrqError> {
        self.with_descriptor(handle.irq, |descriptor| {
            if !descriptor.line_accepts_action_transition() {
                return Err(IrqError::Busy);
            }
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
                let drain_epoch = match (*action).begin_drain(wake) {
                    Ok(epoch) => epoch,
                    Err(error) => {
                        descriptor.in_flight.fetch_sub(1, Ordering::AcqRel);
                        return Err(error);
                    }
                };
                let scope = (*action).scope;
                recompute_scope_line_desired(descriptor, scope);
                Ok((scope, drain_epoch, action))
            }
        })
    }

    fn disable_action_from_irq(&self, handle: IrqHandle, cpu: CpuId) -> Result<(), IrqError> {
        let result = self.with_descriptor(handle.irq, |descriptor| {
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                let scope = (*action).scope;
                match scope {
                    IrqScope::Global => {
                        (*action).set_enabled(false)?;
                        recompute_scope_line_desired(descriptor, scope);
                    }
                    IrqScope::PerCpu { .. } => {
                        (*action).disable_on_cpu(cpu)?;
                        descriptor.recompute_line_desired(Some(cpu));
                    }
                }
                Ok(scope)
            }
        });
        match result? {
            IrqScope::Global => self.apply_line_state(handle.irq, None),
            IrqScope::PerCpu { cpus } if cpus.contains(cpu) => {
                // A hard handler must never synchronously rendezvous with a
                // remote CPU. Local suppression prevents another callback on
                // the observing CPU while healthy CPU instances remain live.
                self.apply_line_state(handle.irq, Some(cpu))
            }
            IrqScope::PerCpu { .. } => Err(IrqError::InvalidCpu),
        }
    }

    fn mask_line_from_irq(&self, handle: IrqHandle, cpu: CpuId) -> Result<(), IrqError> {
        let scope = self.record_action_quench(handle, Some(cpu))?;
        match scope {
            IrqScope::Global => self.apply_line_state(handle.irq, None),
            IrqScope::PerCpu { .. } => self.apply_line_state(handle.irq, Some(cpu)),
        }
    }

    fn record_action_quench(
        &self,
        handle: IrqHandle,
        cpu: Option<CpuId>,
    ) -> Result<IrqScope, IrqError> {
        self.with_descriptor(handle.irq, |descriptor| {
            if !descriptor.line_accepts_action_transition() {
                return Err(IrqError::Busy);
            }
            let action = descriptor
                .actions()
                .find(|action| unsafe { (**action).id == handle.id })
                .ok_or(IrqError::NotFound)?;
            unsafe {
                if (*action).detached.load(Ordering::Acquire) {
                    return Err(IrqError::NotFound);
                }
                if descriptor.line_control() != Some(IrqLineControl::Maskable) {
                    return Err(IrqError::Unsupported);
                }
                (*action).record_quench(cpu)?;
                let scope = (*action).scope;
                recompute_scope_line_desired(descriptor, scope);
                Ok(scope)
            }
        })
    }

    fn clear_action_quench(&self, handle: IrqHandle, cpu: Option<CpuId>) -> Result<(), IrqError> {
        self.with_descriptor(handle.irq, |descriptor| {
            if !descriptor.line_accepts_action_transition() {
                return Err(IrqError::Busy);
            }
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
        })
    }

    /// Returns a status snapshot for an IRQ action.
    pub fn status(&self, handle: IrqHandle) -> Result<IrqStatus, IrqError> {
        if self.ops.in_irq_context() {
            return Err(IrqError::InIrqContext);
        }
        let current_cpu = self.ops.current_cpu();
        let (action_enabled, quench_owned, line_enabled, in_flight, action_running) = self
            .with_action(handle, |descriptor, action| {
                let cpu = status_cpu(action.scope, current_cpu);
                (
                    action.enabled_on(cpu),
                    action.has_quench(),
                    descriptor.line_applied(cpu),
                    descriptor.in_flight.load(Ordering::Acquire),
                    action.running.load(Ordering::Acquire),
                )
            })?;
        Ok(IrqStatus {
            action_enabled,
            quench_owned,
            line_enabled,
            in_flight,
            action_running,
        })
    }

    /// Dispatches one claimed IRQ and completes its controller claim.
    ///
    /// This path performs no allocation or reclamation. It invokes only the
    /// endpoints already owned by enabled actions; endpoint providers must
    /// uphold the bounded hard-IRQ callback contract documented by
    /// [`crate::BoxedIrqHandler`]. A task-side quench release racing this
    /// dispatch cannot reopen a global line until every shared action has
    /// returned. `complete_claim` performs the controller EOI before the
    /// descriptor tail may reopen the line. The closure also runs
    /// when no action is registered, because a claimed controller interrupt
    /// must always be completed.
    ///
    /// `complete_claim` executes in hard-IRQ context and must not allocate,
    /// block, invoke arbitrary callbacks, or panic.
    pub fn dispatch(&self, irq: IrqId, cpu: CpuId, complete_claim: impl FnOnce()) -> IrqOutcome {
        let head = self.begin_dispatch(irq, cpu);
        let _guard = DispatchGuard {
            registry: self,
            irq,
            cpu,
            complete_claim: Some(complete_claim),
            descriptor_pinned: head.is_some(),
        };
        let Some(head) = head else {
            return IrqOutcome::default();
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
            if action.quench_applies(Some(cpu)) {
                continue;
            }

            let Some(_guard) = ActionRunGuard::enter(action, cpu) else {
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
                IrqReturn::DisableActionAndWake => {
                    outcome.handled = true;
                    outcome.wake = true;
                    if let Err(error) =
                        self.disable_action_from_irq(IrqHandle { irq, id: action.id }, cpu)
                    {
                        panic!(
                            "IRQ framework failed to disable an isolated action for {irq:?} on \
                             CPU {}: {error:?}",
                            cpu.0,
                        );
                    }
                }
                IrqReturn::MaskLineAndWake => {
                    outcome.handled = true;
                    outcome.wake = true;
                    if let Err(error) =
                        self.mask_line_from_irq(IrqHandle { irq, id: action.id }, cpu)
                    {
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
        let pending = self.percpu_lines_for_cpu_online(cpu);
        for (irq, binding, needs_initialization) in pending {
            if needs_initialization
                && let Err(error) = self.mask_prepared_percpu_line(irq, binding, cpu)
            {
                panic!(
                    "prepared per-CPU IRQ line {irq:?} could not initialize on CPU {}: {error:?}",
                    cpu.0
                );
            }
            self.apply_line_state(irq, Some(cpu))?;
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
        if matches!(request.scope, IrqScope::PerCpu { .. }) && request.affinity != IrqAffinity::Any
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

    fn commit_new_action_registration(
        &self,
        irq: IrqId,
        request: &IrqRequest,
        action: *mut Action,
        enabled: bool,
    ) {
        self.with_descriptor(irq, |descriptor| {
            assert!(
                descriptor.registration_held() && descriptor.line_binding().is_some(),
                "IRQ action publication requires an owned prepared line binding"
            );
            unsafe {
                // SAFETY: `action` is the unique allocation prepared by request,
                // and the registration reservation excludes another list commit.
                (*action).next = descriptor.head;
                (*action)
                    .set_enabled(enabled)
                    .expect("a newly constructed IRQ action cannot be busy");
            }
            descriptor.head = action;
            recompute_scope_line_desired(descriptor, request.scope);
            descriptor.finish_registration();
            Ok(())
        })
        .expect("prepared IRQ descriptor disappeared before publication");
    }

    fn commit_reattached_action_registration(
        &self,
        config: DetachedActionConfig,
        action: *mut Action,
    ) {
        self.with_descriptor(config.irq, |descriptor| {
            assert!(
                descriptor.registration_held() && descriptor.line_binding().is_some(),
                "reattached IRQ action publication requires a prepared line binding"
            );
            debug_assert_eq!(descriptor.irq, config.irq);
            unsafe {
                // SAFETY: `action` is uniquely owned by the consumed detached
                // token, and the registration reservation excludes list peers.
                (*action).next = descriptor.head;
            }
            descriptor.head = action;
            recompute_scope_line_desired(descriptor, config.scope);
            descriptor.finish_registration();
            Ok(())
        })
        .expect("prepared IRQ descriptor disappeared before reattach publication");
    }

    fn detach_action_locked(
        descriptor: &mut Descriptor,
        handle: IrqHandle,
    ) -> Result<(DetachedActionConfig, *mut Action), IrqError> {
        debug_assert_eq!(descriptor.irq, handle.irq);
        if descriptor.line_release_reserved() {
            return Err(IrqError::Busy);
        }
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

    fn with_action<T>(
        &self,
        handle: IrqHandle,
        f: impl FnOnce(&Descriptor, &Action) -> T,
    ) -> Result<T, IrqError> {
        self.with_descriptor(handle.irq, |descriptor| {
            let action = descriptor
                .actions()
                .map(|action| unsafe { &*action })
                .find(|action| action.id == handle.id && !action.detached.load(Ordering::Acquire))
                .ok_or(IrqError::NotFound)?;
            Ok(f(descriptor, action))
        })
    }

    fn begin_dispatch(&self, irq: IrqId, cpu: CpuId) -> Option<*mut Action> {
        self.with_descriptor(irq, |descriptor| {
            if descriptor.head.is_null() || !descriptor.dispatchable() {
                return Ok(None);
            }
            descriptor.begin_irq_claim(cpu);
            assert!(
                descriptor
                    .in_flight
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| count
                        .checked_add(1))
                    .is_ok(),
                "IRQ descriptor in-flight count overflowed"
            );
            Ok(Some(descriptor.head))
        })
        .ok()
        .flatten()
    }

    fn end_dispatch(&self, irq: IrqId, cpu: CpuId) {
        let apply_target = self
            .with_descriptor(irq, |descriptor| {
                let target = descriptor.end_irq_claim(cpu);
                let previous = descriptor
                    .in_flight
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                        count.checked_sub(1)
                    })
                    .expect("IRQ descriptor in-flight count underflowed");
                debug_assert_ne!(previous, 0);
                if descriptor.line_claims(target) == 0
                    && descriptor.line_desired(target)
                    && !descriptor.line_applied(target)
                {
                    return Ok(Some(target));
                }
                Ok(None)
            })
            .expect("claimed IRQ descriptor disappeared before dispatch tail");
        if let Some(target) = apply_target
            && let Err(error) = self.apply_line_state(irq, target)
        {
            panic!("IRQ controller failed to reopen {irq:?} at dispatch tail: {error:?}");
        }
    }

    fn end_reader_pin(&self, irq: IrqId) {
        self.with_descriptor(irq, |descriptor| {
            descriptor
                .in_flight
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    count.checked_sub(1)
                })
                .expect("IRQ descriptor in-flight count underflowed");
            Ok(())
        })
        .expect("pinned IRQ descriptor disappeared");
    }

    /// Returns the shutdown-stable descriptor address after a short catalog lookup.
    fn descriptor_ptr(&self, irq: IrqId) -> Option<*mut Descriptor> {
        let start = descriptor_hash(irq) % DESCRIPTOR_CATALOG_CAPACITY;
        for distance in 0..DESCRIPTOR_CATALOG_CAPACITY {
            let slot = (start + distance) % DESCRIPTOR_CATALOG_CAPACITY;
            let descriptor = self.descriptor_catalog[slot].load(Ordering::Acquire);
            if descriptor.is_null() {
                return None;
            }
            let matches = unsafe {
                // SAFETY: catalog entries are published only after their
                // pinned allocation enters the shutdown-lifetime owner arena.
                // Entries are never cleared, replaced, or reclaimed.
                (*descriptor).irq == irq
            };
            if matches {
                return Some(descriptor);
            }
        }
        None
    }

    fn vacant_descriptor_slot(&self, irq: IrqId) -> Option<usize> {
        let start = descriptor_hash(irq) % DESCRIPTOR_CATALOG_CAPACITY;
        (0..DESCRIPTOR_CATALOG_CAPACITY)
            .map(|distance| (start + distance) % DESCRIPTOR_CATALOG_CAPACITY)
            .find(|slot| {
                self.descriptor_catalog[*slot]
                    .load(Ordering::Acquire)
                    .is_null()
            })
    }

    /// Executes one descriptor-local transaction without retaining the catalog lock.
    fn with_descriptor<R>(
        &self,
        irq: IrqId,
        operation: impl FnOnce(&mut Descriptor) -> Result<R, IrqError>,
    ) -> Result<R, IrqError> {
        let descriptor = self.descriptor_ptr(irq).ok_or(IrqError::NotFound)?;
        let line_lock = unsafe {
            // SAFETY: descriptors are individually pinned and never removed;
            // the registry borrow outlives this complete local transaction.
            (&*descriptor).controller_lock()
        };
        let _line_guard = line_lock.guard(&self.ops);
        let descriptor = unsafe {
            // SAFETY: the descriptor-local lock uniquely owns all mutable
            // fields. The stable allocation cannot be removed or relocated.
            &mut *descriptor
        };
        operation(descriptor)
    }

    #[cfg(test)]
    fn descriptor(&self, irq: IrqId) -> Option<&Descriptor> {
        let descriptor = self.descriptor_ptr(irq)?;
        Some(unsafe {
            // SAFETY: unit tests use this escape hatch only for atomic
            // invariant injection. Descriptors remain pinned until registry
            // teardown and the returned borrow cannot outlive `self`.
            &*descriptor
        })
    }
}

fn descriptor_hash(irq: IrqId) -> usize {
    let key = (u64::from(irq.domain.0) << 32) | u64::from(irq.hwirq.0);
    let mixed = key.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    (mixed ^ (mixed >> 32)) as usize
}

struct DispatchGuard<'a, O: IrqOps, C: FnOnce()> {
    registry: &'a Registry<O>,
    irq: IrqId,
    cpu: CpuId,
    complete_claim: Option<C>,
    descriptor_pinned: bool,
}

struct ActionRunGuard<'a> {
    action: &'a Action,
}

impl<'a> ActionRunGuard<'a> {
    fn enter(action: &'a Action, cpu: CpuId) -> Option<Self> {
        if !action.try_enter(cpu) {
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

impl<O: IrqOps, C: FnOnce()> Drop for DispatchGuard<'_, O, C> {
    fn drop(&mut self) {
        self.complete_claim
            .take()
            .expect("IRQ claim completion closure was consumed twice")();
        if self.descriptor_pinned {
            self.registry.end_dispatch(self.irq, self.cpu);
        }
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
    use crate::{AutoEnable, HwIrq, IrqDomainId, IrqLineBinding, IrqLineControl, PreparedIrqLine};

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

        fn prepare_line(
            &self,
            irq: IrqId,
            _scope: IrqScope,
            _affinity: IrqAffinity,
        ) -> Result<PreparedIrqLine, IrqError> {
            Ok(PreparedIrqLine::new(
                IrqLineBinding::new(irq.hwirq.0, 1).unwrap(),
                IrqLineControl::Maskable,
            ))
        }

        fn set_line_enabled(&self, _binding: IrqLineBinding, _cpu: Option<CpuId>, _enabled: bool) {}

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

        let _ = registry.begin_dispatch(irq, CpuId(0));
    }

    #[test]
    #[should_panic(expected = "IRQ descriptor in-flight count underflowed")]
    fn descriptor_reader_count_never_underflows_after_dispatch() {
        let registry = Registry::new(TestOps);
        let irq = IrqId::new(IrqDomainId(1), HwIrq(3));
        registry
            .request(irq, IrqRequest::new(|_| IrqReturn::Handled))
            .unwrap();

        registry.end_reader_pin(irq);
    }
}
