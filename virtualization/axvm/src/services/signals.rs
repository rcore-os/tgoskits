// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Run-bound vCPU signals, fixed interrupt queues, and task waits.
//!
//! [`RunSignals`] is the lower object shared with hard IRQ handlers. It owns
//! only atomics, short raw-lock queues, the IRQ notification cell, and targets
//! pre-bound at registration. The sleeping worker handle lives exclusively in
//! [`RunSignalWorker`](crate::irq::deferred::RunSignalWorker), which is owned by
//! the run control task rather than by this object.

use std::{
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    vec::Vec,
};

use ax_std::os::arceos::sync::RawSpinLock;

use crate::{
    AxVmResult, HostWaitQueueHandle, ax_err,
    host::task::{IrqNotification, ThreadWakeHandle},
    identity::{RunId, VcpuInstance},
    irq::model::{PendingVcpuInterrupt, RunEpoch, SourceEvent},
    manager::ControlShared,
    runtime::{
        kick::kick_target,
        queue::{INTERRUPT_SOURCE_CAPACITY, QueuedVcpuInterrupt, SlotUpdate, VcpuSignalSlot},
    },
    vcpu::VcpuSignals,
};
pub(crate) use crate::{irq::deferred::RunSignalWorker, runtime::queue::SignalError};

const IRQ_CLOSED_BIT: usize = 1usize << (usize::BITS - 1);
const IRQ_INFLIGHT_MASK: usize = !IRQ_CLOSED_BIT;
const POLL_OWNER_NONE: usize = usize::MAX;
const CONTROLLER_EVENT_CAPACITY: usize = 64;
// EOI completion is a separate fixed ingress.  A run can retain one
// acknowledged controller source for every source identity accepted by the
// architecture-specific vCPU queue, while unrelated device pulses are being
// drained.  Keeping this bound separate means a burst of ordinary events can
// never consume the slots needed to retire an already delivered interrupt.
const CONTROLLER_EOI_CAPACITY: usize = INTERRUPT_SOURCE_CAPACITY;

struct ControllerEventQueue<const CAPACITY: usize> {
    entries: [Option<SourceEvent>; CAPACITY],
    head: usize,
    len: usize,
}

impl<const CAPACITY: usize> ControllerEventQueue<CAPACITY> {
    const fn new() -> Self {
        Self {
            entries: [None; CAPACITY],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, event: SourceEvent) -> Result<(), SignalError> {
        if self.len == self.entries.len() {
            return Err(SignalError::Capacity);
        }
        let index = (self.head + self.len) % self.entries.len();
        self.entries[index] = Some(event);
        self.len += 1;
        Ok(())
    }

    fn drain(&mut self, output: &mut Vec<SourceEvent>) {
        while self.len != 0 {
            let event = self.entries[self.head]
                .take()
                .expect("controller event queue slot must be occupied");
            self.head = (self.head + 1) % self.entries.len();
            self.len -= 1;
            output.push(event);
        }
    }
}

/// Per-run, per-vCPU interrupt and work signal state.
///
/// Queues belong to the run and the vCPU identity, not to one activation. The
/// run owner retires the whole object after [`Self::close_interrupts`] and
/// [`Self::interrupts_quiet`], and that retirement is the only thing that drops
/// sources still pending for an inactive vCPU. Because a closed object rejects
/// every new publication, a stale run can never inject into its successor.
pub(crate) struct RunSignals {
    epoch: RunEpoch,
    vcpu_count: usize,
    slots: Box<[VcpuSignalSlot]>,
    registration_lock: RawSpinLock<()>,
    active_mask: AtomicUsize,
    work_without_owner: AtomicBool,
    poll_owner: AtomicUsize,
    irq_state: AtomicUsize,
    irq_pending: AtomicUsize,
    irq_notify: IrqNotification,
    controller_events: RawSpinLock<ControllerEventQueue<CONTROLLER_EVENT_CAPACITY>>,
    controller_eoi_events: RawSpinLock<ControllerEventQueue<CONTROLLER_EOI_CAPACITY>>,
    control: OnceLock<Weak<ControlShared>>,
}

/// Guards one hard-IRQ publisher against interrupt quiescence.
struct IrqProducerGuard<'run> {
    run: &'run RunSignals,
}

impl RunSignals {
    /// Creates the fixed lower signal state for one run.
    ///
    /// `vcpu_count` must fit this run's `usize` active-mask bitmap. It is not
    /// bounded by the host CPU count: several vCPUs of one run may be scheduled
    /// onto the same host CPU, so only the bitmap width limits the count. All
    /// source rings are allocated here and never grow.
    pub(crate) fn new(run: RunId, vcpu_count: usize) -> AxVmResult<Arc<Self>> {
        if vcpu_count == 0 || vcpu_count > usize::BITS as usize {
            return ax_err!(
                InvalidInput,
                format!(
                    "run {run:?} has {vcpu_count} vCPUs, but the signal bitmap supports 1..={}",
                    usize::BITS
                )
            );
        }

        let mut slots = Vec::with_capacity(vcpu_count);
        for _ in 0..vcpu_count {
            slots.push(VcpuSignalSlot::new());
        }

        Ok(Arc::new(Self {
            epoch: RunEpoch::new(run),
            vcpu_count,
            slots: slots.into_boxed_slice(),
            registration_lock: RawSpinLock::new(()),
            active_mask: AtomicUsize::new(0),
            work_without_owner: AtomicBool::new(false),
            poll_owner: AtomicUsize::new(POLL_OWNER_NONE),
            irq_state: AtomicUsize::new(0),
            irq_pending: AtomicUsize::new(0),
            irq_notify: IrqNotification::new(),
            controller_events: RawSpinLock::new(ControllerEventQueue::new()),
            controller_eoi_events: RawSpinLock::new(ControllerEventQueue::new()),
            control: OnceLock::new(),
        }))
    }

    /// Binds this run to its lifecycle owner before guest entry is admitted.
    pub(crate) fn bind_control(&self, control: Weak<ControlShared>) {
        let _ = self.control.set(control);
    }

    fn control(&self) -> Option<Arc<ControlShared>> {
        self.control.get().and_then(Weak::upgrade)
    }

    /// Publishes a shared-controller event from a device or hard-IRQ context.
    ///
    /// The fixed slot is the only state touched by the producer. The signal
    /// worker drains it in task context and posts to the lifecycle owner.
    pub(crate) fn publish_controller_event(&self, event: SourceEvent) -> Result<(), SignalError> {
        let producer = IrqProducerGuard::new(self)?;
        let result = match event {
            SourceEvent::Eoi { .. } => self.controller_eoi_events.lock_irqsave().push(event),
            SourceEvent::Pulse { .. } | SourceEvent::Level { .. } => {
                self.controller_events.lock_irqsave().push(event)
            }
        };
        drop(producer);
        if result.is_ok() {
            self.irq_notify.notify();
        }
        result
    }

    pub(crate) fn drain_controller_events(&self) -> Vec<SourceEvent> {
        let mut output = Vec::with_capacity(CONTROLLER_EVENT_CAPACITY + CONTROLLER_EOI_CAPACITY);
        self.controller_events.lock_irqsave().drain(&mut output);
        self.controller_eoi_events.lock_irqsave().drain(&mut output);
        output
    }

    pub(crate) fn post_controller_events(&self) {
        let Some(control) = self.control() else {
            return;
        };
        for event in self.drain_controller_events() {
            control.post_interrupt(event);
        }
    }

    pub(crate) const fn run_id(&self) -> RunId {
        self.epoch.run()
    }

    pub(crate) const fn epoch(&self) -> RunEpoch {
        self.epoch
    }

    #[cfg(target_arch = "x86_64")]
    pub(crate) const fn vcpu_count(&self) -> usize {
        self.vcpu_count
    }

    pub(crate) fn active_mask(&self) -> usize {
        self.active_mask.load(Ordering::Acquire)
    }

    fn vcpu_bit(&self, vcpu_id: usize) -> Option<usize> {
        (vcpu_id < self.vcpu_count)
            .then(|| 1usize.checked_shl(vcpu_id as u32))
            .flatten()
    }

    fn slot(&self, vcpu_id: usize) -> Option<&VcpuSignalSlot> {
        self.slots.get(vcpu_id)
    }

    /// Validates that `instance` is this run's own, in-range activation.
    ///
    /// Kept separate from the raw-lock registration so the identity rules can
    /// be exercised without a live host wake handle. A foreign run is an
    /// [`SignalError::InvalidSource`]; an id outside this run's bitmap is an
    /// [`SignalError::InvalidTarget`].
    fn validate_instance(&self, instance: VcpuInstance) -> Result<usize, SignalError> {
        if instance.run != self.epoch.run() {
            return Err(SignalError::InvalidSource);
        }
        self.vcpu_bit(instance.vcpu_id)
            .ok_or(SignalError::InvalidTarget)
    }

    pub(crate) fn contains_vcpu(&self, vcpu_id: usize) -> bool {
        self.vcpu_bit(vcpu_id).is_some()
    }

    /// Returns whether an exact vCPU activation is still registered.
    ///
    /// Controller owners use this check when completing a delivery token. The
    /// slot lock is a leaf synchronization boundary; no VM or device service is
    /// reached while it is held.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn is_current_instance(&self, instance: VcpuInstance) -> bool {
        self.slot(instance.vcpu_id)
            .is_some_and(|slot| slot.is_registered(instance))
    }

    /// Returns the activation currently registered for one vCPU.
    ///
    /// This is a task-side snapshot used to stamp a controller completion with
    /// the exact target activation. It never exposes the wake target or any
    /// upper-layer service.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn current_instance(&self, vcpu_id: usize) -> Option<VcpuInstance> {
        self.slot(vcpu_id)
            .and_then(crate::runtime::queue::VcpuSignalSlot::current_instance)
    }

    /// Binds one vCPU activation to its fixed wake and entry target.
    ///
    /// Task-context only. A closed run rejects new registrations, and a slot
    /// that still holds a different, not-yet-retired activation is not
    /// overwritten ([`SignalError::StaleInstance`]): the owner retires it with
    /// [`Self::unregister`] first. Re-registering the same activation is
    /// idempotent and retains its queued source identities; the retired target is
    /// dropped after both raw guards are released.
    pub(crate) fn register(
        &self,
        instance: VcpuInstance,
        signals: Arc<VcpuSignals>,
        wake: ThreadWakeHandle,
    ) -> Result<(), SignalError> {
        let bit = self.validate_instance(instance)?;
        let Some(slot) = self.slot(instance.vcpu_id) else {
            return Err(SignalError::InvalidTarget);
        };
        // Registration shares the closed/in-flight boundary: a run that closed
        // its interrupt admission cannot gain new execution targets.
        let producer = IrqProducerGuard::new(self)?;
        let registration = self.registration_lock.lock_irqsave();
        let retired = slot.register(instance, signals, wake)?;
        self.active_mask.fetch_or(bit, Ordering::Release);
        drop(registration);
        drop(producer);
        retired.retire();
        Ok(())
    }

    /// Removes exactly the named activation and leaves other targets active.
    ///
    /// A foreign or out-of-range instance removes nothing.
    pub(crate) fn unregister(&self, instance: VcpuInstance) {
        let Ok(bit) = self.validate_instance(instance) else {
            return;
        };
        let Some(slot) = self.slot(instance.vcpu_id) else {
            return;
        };

        let registration = self.registration_lock.lock_irqsave();
        let retired = slot.unregister(instance);
        if !matches!(retired, SlotUpdate::AlreadyUnregistered) {
            self.active_mask.fetch_and(!bit, Ordering::Release);
        }
        drop(registration);
        retired.retire();
    }

    /// Publishes an architecture-independent interrupt source.
    ///
    /// This method only publishes queue state. The caller must call
    /// [`Self::kick`] after this return if it owns the notification step.
    pub(crate) fn publish(
        &self,
        vcpu_id: usize,
        interrupt: PendingVcpuInterrupt,
    ) -> Result<(), SignalError> {
        self.publish_queued(vcpu_id, interrupt.into())
    }

    /// Publishes one source and reports whether it created a new queue entry.
    ///
    /// Controller owners use this result to retain one EOI source identity per
    /// actual delivery. Coalesced duplicate sources must not create a second
    /// completion token.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn publish_with_status(
        &self,
        vcpu_id: usize,
        interrupt: PendingVcpuInterrupt,
    ) -> Result<bool, SignalError> {
        self.publish_queued_with_status(vcpu_id, interrupt.into())
    }

    /// Publishes one concrete architecture interrupt source into the run queue.
    ///
    /// The queue belongs to the run and the vCPU identity, not to one activation,
    /// so an accepted publication is retained even when no execution target is
    /// registered: an acknowledged controller source (LoongArch physical IRQ or
    /// emulated EIOINTC vector) survives a `CPU_OFF` until a later activation
    /// drains it. Only an identity outside the architecture's accepted
    /// namespaces is rejected with [`SignalError::InvalidSource`], and nothing is
    /// recorded then.
    ///
    /// Like [`Self::kick_from_irq`] this is safe from hard IRQ: it takes only the
    /// short raw queue guard and the non-blocking producer counter, and it
    /// allocates nothing. Taking the producer counter here means
    /// [`Self::interrupts_quiet`] observes *every* in-flight publication, not
    /// only the deferred kick publications.
    pub(crate) fn publish_queued(
        &self,
        vcpu_id: usize,
        interrupt: QueuedVcpuInterrupt,
    ) -> Result<(), SignalError> {
        self.publish_queued_with_status(vcpu_id, interrupt)
            .map(|_| ())
    }

    #[cfg(target_arch = "x86_64")]
    fn publish_queued_with_status(
        &self,
        vcpu_id: usize,
        interrupt: QueuedVcpuInterrupt,
    ) -> Result<bool, SignalError> {
        let Some(slot) = self.slot(vcpu_id) else {
            return Err(SignalError::InvalidTarget);
        };
        let producer = IrqProducerGuard::new(self)?;
        let result = slot.publish(interrupt);
        drop(producer);
        result
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn publish_queued_with_status(
        &self,
        vcpu_id: usize,
        interrupt: QueuedVcpuInterrupt,
    ) -> Result<bool, SignalError> {
        let Some(slot) = self.slot(vcpu_id) else {
            return Err(SignalError::InvalidTarget);
        };
        let producer = IrqProducerGuard::new(self)?;
        let result = slot.publish(interrupt);
        drop(producer);
        result
    }

    /// Wakes one registered vCPU from ordinary task context.
    pub(crate) fn kick(&self, vcpu_id: usize) -> Result<(), SignalError> {
        let Some(slot) = self.slot(vcpu_id) else {
            return Err(SignalError::InvalidTarget);
        };
        let Some(target) = slot.target() else {
            return Err(SignalError::InactiveTarget);
        };
        kick_target(&target);
        Ok(())
    }

    /// A device wake shares the run's publication admission and retirement.
    pub(crate) fn notify_vcpu(&self, vcpu_id: usize) -> Result<(), SignalError> {
        let producer = IrqProducerGuard::new(self)?;
        let result = self.kick(vcpu_id);
        drop(producer);
        result
    }

    /// Publishes an IRQ kick to the pre-bound deferred worker.
    ///
    /// This path performs no VM/runtime lookup, allocation or sleeping lock.
    /// Canonical IRQ state precedes the sticky execution request and its paired
    /// entry barrier. A remote guest receives the host's non-blocking IPI
    /// notification immediately; the worker performs the task wake separately.
    pub(crate) fn kick_from_irq(&self, vcpu_id: usize) -> Result<(), SignalError> {
        let Some(bit) = self.vcpu_bit(vcpu_id) else {
            return Err(SignalError::InvalidTarget);
        };
        let Some(slot) = self.slot(vcpu_id) else {
            return Err(SignalError::InvalidTarget);
        };

        let producer = IrqProducerGuard::new(self)?;
        let Some(target) = slot.target() else {
            drop(producer);
            return Err(SignalError::InactiveTarget);
        };
        let signals = target.signals();
        signals.request_unblock();
        signals.publish_exit_request();
        if let Some(cpu_id) = signals.request_exit(crate::host::task::current_cpu_id()) {
            // ax-ipi::notify_cpu is non-blocking even if an IRQ interrupted
            // another sender. No raw state guard reaches this doorbell.
            crate::host::task::send_ipi(cpu_id);
        }
        self.irq_pending.fetch_or(bit, Ordering::Release);
        // The producer guard is deliberately held across the notification: the
        // wake is issued outside every raw guard, but interrupt quiescence must
        // not become observable until this notification has been delivered.
        self.irq_notify.notify();
        drop(producer);
        Ok(())
    }

    /// Returns whether this vCPU's run-owned queue has any pending source.
    ///
    /// The flag also covers sources published while the vCPU was inactive, so a
    /// retained controller source stays visible to the activation that later
    /// drains it.
    pub(crate) fn has_pending(&self, vcpu_id: usize) -> bool {
        self.slot(vcpu_id).is_some_and(VcpuSignalSlot::has_pending)
    }

    /// Drains exactly `activation`'s fixed queue.
    ///
    /// The run's pending flag is consulted first, so the common empty case does
    /// not reserve the complete ring. When a source is queued the output vector
    /// is allocated with the complete fixed capacity *before* the raw queue guard
    /// is taken; no allocation can occur inside that guard. A stale `activation`
    /// drains nothing and leaves the source pending for the current activation.
    pub(crate) fn drain(&self, vcpu_id: usize, activation: u64) -> Vec<QueuedVcpuInterrupt> {
        let Some(slot) = self.slot(vcpu_id) else {
            return Vec::new();
        };
        if !slot.has_pending() {
            return Vec::new();
        }
        let mut output = Vec::with_capacity(INTERRUPT_SOURCE_CAPACITY);
        slot.drain_into(activation, &mut output);
        output
    }

    /// Prevents new hard-IRQ publishers from entering this run.
    pub(crate) fn close_interrupts(&self) {
        self.irq_state.fetch_or(IRQ_CLOSED_BIT, Ordering::Release);
    }

    /// Returns true only after closing is visible and all IRQ publishers left.
    pub(crate) fn interrupts_quiet(&self) -> bool {
        let state = self.irq_state.load(Ordering::Acquire);
        state & IRQ_CLOSED_BIT != 0 && state & IRQ_INFLIGHT_MASK == 0
    }

    /// Publishes device work before waking the designated polling vCPU.
    ///
    /// The canonical, owner-independent work flag is published before any wake,
    /// and this notification shares [`Self::close_interrupts`]'s admission and
    /// in-flight boundary: a closed run rejects the old
    /// [`DeviceWorkPort`](crate::services::DeviceWorkPort) with
    /// [`SignalError::Closed`], and [`Self::interrupts_quiet`] cannot become true
    /// until this wake has been issued. A missing poll owner is still successful
    /// and sticky, and a designated-but-not-yet-registered poller is also
    /// successful because the canonical flag survives until that vCPU's next
    /// [`Self::take_work`].
    pub(crate) fn notify_work(&self) -> Result<(), SignalError> {
        let producer = IrqProducerGuard::new(self)?;
        // Canonical work is owner independent; publish it before the wake.
        self.work_without_owner.store(true, Ordering::Release);
        let result = match self.poll_owner() {
            Some(vcpu_id) => match self.kick(vcpu_id) {
                // The poller has no live target yet; the sticky flag waits for
                // its next entry instead of failing the device notification.
                Err(SignalError::InactiveTarget) => Ok(()),
                other => other,
            },
            None => Ok(()),
        };
        drop(producer);
        result
    }

    /// Clears canonical device work only for the current polling vCPU.
    ///
    /// Every notification publishes the same owner-independent flag, so changing
    /// the poller can never lose work that was concurrently published to the
    /// previous one.
    pub(crate) fn take_work(&self, vcpu_id: usize) -> bool {
        if self.vcpu_bit(vcpu_id).is_none() || self.poll_owner() != Some(vcpu_id) {
            return false;
        }
        self.work_without_owner.swap(false, Ordering::AcqRel)
    }

    /// Returns whether the designated polling vCPU still has device work.
    ///
    /// Work is canonical and owner independent. It is pending only for the vCPU
    /// the run owner has designated, so an undesignated vCPU neither wakes for it
    /// nor clears it.
    pub(crate) fn work_pending(&self, vcpu_id: usize) -> bool {
        self.vcpu_bit(vcpu_id).is_some()
            && self.poll_owner() == Some(vcpu_id)
            && self.work_without_owner.load(Ordering::Acquire)
    }

    /// Designates (or clears) the single polling vCPU. Task-context only.
    ///
    /// The owner is committed before any wake, so a concurrent
    /// [`Self::notify_work`] cannot hand work to a poller that is being replaced,
    /// and canonical work is never cleared here. When work is already pending the
    /// newly designated poller is woken outside every raw guard so it cannot stay
    /// parked; the sticky flag stays set for its [`Self::take_work`]. Designating
    /// an out-of-range vCPU is the only failure and it changes nothing.
    pub(crate) fn set_poll_owner(&self, owner: Option<usize>) -> Result<(), SignalError> {
        if let Some(vcpu_id) = owner
            && self.vcpu_bit(vcpu_id).is_none()
        {
            return Err(SignalError::InvalidTarget);
        }
        self.poll_owner
            .store(owner.unwrap_or(POLL_OWNER_NONE), Ordering::Release);
        let Some(vcpu_id) = owner else {
            return Ok(());
        };
        if !self.work_pending(vcpu_id) {
            return Ok(());
        }
        if let Some(target) = self.slot(vcpu_id).and_then(VcpuSignalSlot::target) {
            kick_target(&target);
        }
        Ok(())
    }

    pub(crate) fn poll_owner(&self) -> Option<usize> {
        match self.poll_owner.load(Ordering::Acquire) {
            POLL_OWNER_NONE => None,
            owner => Some(owner),
        }
    }

    pub(crate) fn notify_irq_worker(&self) {
        self.irq_notify.notify();
    }

    /// Waits on the lower IRQ notification cell.
    ///
    /// Only the fixed [`RunSignalWorker`] may call this task-context method.
    pub(crate) fn wait_for_irq_notification(&self) {
        self.irq_notify.wait();
    }

    fn take_irq_pending(&self) -> usize {
        self.irq_pending.swap(0, Ordering::AcqRel)
    }

    fn clear_irq_pending(&self) {
        self.irq_pending.store(0, Ordering::Release);
    }
}

impl<'run> IrqProducerGuard<'run> {
    fn new(run: &'run RunSignals) -> Result<Self, SignalError> {
        loop {
            let observed = run.irq_state.load(Ordering::Acquire);
            if observed & IRQ_CLOSED_BIT != 0 {
                return Err(SignalError::Closed);
            }
            if observed & IRQ_INFLIGHT_MASK == IRQ_INFLIGHT_MASK {
                return Err(SignalError::Capacity);
            }
            let replaced = observed
                .checked_add(1)
                .expect("IRQ publisher count overflow");
            match run.irq_state.compare_exchange(
                observed,
                replaced,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Self { run }),
                Err(current) => {
                    if current & IRQ_CLOSED_BIT != 0 {
                        return Err(SignalError::Closed);
                    }
                }
            }
        }
    }
}

impl Drop for IrqProducerGuard<'_> {
    fn drop(&mut self) {
        self.run.irq_state.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One activation's wait over a run-owned host wait queue.
pub(crate) struct VcpuWait {
    instance: VcpuInstance,
    signals: Arc<VcpuSignals>,
    run: Arc<RunSignals>,
    queue: HostWaitQueueHandle,
}

impl VcpuWait {
    pub(crate) fn new(
        instance: VcpuInstance,
        signals: Arc<VcpuSignals>,
        run: Arc<RunSignals>,
        queue: HostWaitQueueHandle,
    ) -> Self {
        Self {
            instance,
            signals,
            run,
            queue,
        }
    }

    /// Waits until [`Self::wait_pending`] or the caller's lower predicate is true.
    ///
    /// The predicate never queries a VM, device runtime, lifecycle lock, or a
    /// sleeping lock. While the execution is admitted (entry open) and no
    /// interrupt, work, or sticky wake is published, the park persists; a stop,
    /// a closed admission, or any lower publication ends it.
    ///
    /// A direct pre-bound `ThreadWakeHandle::wake` interrupts this park without
    /// touching `queue`: `wake` reaches `ThreadCore::wake` ->
    /// `TaskSystem::wake_thread` (`components/ax-task/src/sched/system/.../wake/request.rs`),
    /// which publishes a sticky park notification through `core.publish_wake()`
    /// for `Parking|Running|Waking|New|Blocked` states. On the parked side,
    /// `WaitQueue::wait_once_inner` (`components/ax-task/src/sync/wait_queue.rs`)
    /// returns from `park.commit()` with `WaitOutcome::Notified`/`OtherWake` and
    /// its `wait_until` loop re-evaluates this predicate. AxTask park state is
    /// therefore thread-local, not queue-exclusive, so no additional
    /// queue-target wake API is required on the kick path. `queue` remains the
    /// task-side rendezvous for callers that prefer an explicit
    /// `wait_queue_wake` (for example `VcpuPort`).
    pub(crate) fn wait_until(&self, additional_pending: impl Fn() -> bool) {
        crate::host::task::wait_queue_wait_until(&self.queue, || {
            self.wait_pending(&additional_pending)
        });
    }

    /// Evaluates the canonical wait predicate.
    ///
    /// Returning `true` means the vCPU must return to its owner. Admission is
    /// the decisive term: a *parked or stopped* execution (`!entry_is_open`)
    /// returns so the owner can consume its mailbox command, while an admitted
    /// (open) execution keeps waiting for a genuine native interrupt. Only the
    /// sticky wake request is consumed here; the canonical queue publication
    /// stays published for [`RunSignals::drain`], and a source retained across a
    /// `CPU_OFF` is visible through [`RunSignals::has_pending`], so a later
    /// activation observes it even when it starts parked. No term queries a VM,
    /// device runtime, lifecycle state, or sleeping lock.
    fn wait_pending(&self, additional_pending: &dyn Fn() -> bool) -> bool {
        if self.signals.stop_requested() || !self.signals.entry_is_open() {
            return true;
        }
        if self.signals.take_unblock_request() {
            return true;
        }
        let vcpu_id = self.instance.vcpu_id;
        self.run.has_pending(vcpu_id) || self.run.work_pending(vcpu_id) || additional_pending()
    }
}

// Keep the private worker drain methods reachable only from the worker owner.
impl RunSignals {
    pub(crate) fn kick_pending_for_worker(&self) {
        for vcpu_id in SetBits(self.take_irq_pending()) {
            if let Err(error) = self.kick(vcpu_id) {
                trace!(
                    "run {:?} deferred IRQ kick for vCPU {vcpu_id} was not delivered: {error:?}",
                    self.epoch.run()
                );
            }
        }
    }

    pub(crate) fn stop_irq_pending_publication(&self) {
        self.clear_irq_pending();
    }
}

struct SetBits(usize);

impl Iterator for SetBits {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0 == 0 {
            return None;
        }
        let vcpu_id = self.0.trailing_zeros() as usize;
        self.0 &= self.0 - 1;
        Some(vcpu_id)
    }
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use std::sync::Arc;

    use axdevice_base::InterruptControllerId;

    use super::*;
    use crate::{
        HostWaitQueueHandle, InterruptTriggerMode,
        identity::{RunId, VcpuInstance, VmKey},
        irq::model::{
            DeliveryToken, InterruptSourceId, PendingVcpuInterrupt, SourceEvent, VirtualInterruptId,
        },
        vcpu::VcpuSignals,
    };

    fn run_signals(vcpu_count: usize) -> Arc<RunSignals> {
        RunSignals::new(RunId::new(VmKey::new(1, 1), 1), vcpu_count).expect("valid run signals")
    }

    fn instance(run: RunId, vcpu_id: usize, activation: u64) -> VcpuInstance {
        VcpuInstance {
            run,
            vcpu_id,
            activation,
        }
    }

    fn edge(id: u32) -> QueuedVcpuInterrupt {
        PendingVcpuInterrupt {
            id: VirtualInterruptId(id),
            trigger: InterruptTriggerMode::EdgeTriggered,
            source: None,
        }
        .into()
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn eoi_ingress_is_not_consumed_by_ordinary_controller_burst() {
        let run = run_signals(1);
        let source = InterruptSourceId::new(InterruptControllerId::new(0), 4, None);
        for sequence in 0..CONTROLLER_EVENT_CAPACITY {
            run.publish_controller_event(SourceEvent::Pulse {
                epoch: run.epoch(),
                source: InterruptSourceId::new(
                    InterruptControllerId::new(0),
                    sequence as u32,
                    None,
                ),
            })
            .expect("ordinary controller slot should accept its fixed capacity");
        }

        run.publish_controller_event(SourceEvent::Eoi {
            epoch: run.epoch(),
            token: DeliveryToken {
                source,
                target: instance(run.run_id(), 0, 1),
                sequence: 1,
            },
        })
        .expect("EOI has an independent fixed ingress");

        let events = run.drain_controller_events();
        assert_eq!(events.len(), CONTROLLER_EVENT_CAPACITY + 1);
        assert!(matches!(events.last(), Some(SourceEvent::Eoi { .. })));
    }

    /// A parked/stopped execution must return to its owner; an admitted one with
    /// nothing pending keeps waiting.
    #[test]
    fn wfi_wait_keeps_waiting_while_the_execution_is_admitted() {
        let run = run_signals(2);
        let signals = Arc::new(VcpuSignals::new());
        let target = instance(run.run_id(), 0, 7);
        let wait = VcpuWait::new(
            target,
            Arc::clone(&signals),
            Arc::clone(&run),
            HostWaitQueueHandle::new(),
        );
        let none = || false;

        // A fresh execution is parked: it must return so the owner can consume
        // its first command instead of parking in WFI.
        assert!(!signals.entry_is_open());
        assert!(wait.wait_pending(&none));

        // Admitted with nothing pending keeps waiting; the caller's own lower
        // atomics are still honoured.
        assert!(signals.open_entry());
        assert!(signals.take_unblock_request()); // consume `open_entry`'s sticky wake
        assert!(signals.entry_is_open());
        assert!(!wait.wait_pending(&none));
        assert!(wait.wait_pending(&|| true));

        // An accepted source is retained by the run even though no execution
        // target is registered, and the waiting owner observes it.
        assert_eq!(run.publish_queued(0, edge(3)), Ok(()));
        assert!(run.has_pending(0));
        assert!(wait.wait_pending(&none));
        // Nothing drains it until an activation is registered, so the retained
        // source survives until the run retires after quiescence.
        assert!(run.drain(0, target.activation).is_empty());
        assert!(run.has_pending(0));

        // A closed admission (park command) or a stop returns to the owner.
        signals.close_entry();
        assert!(wait.wait_pending(&none));
        signals.request_stop();
        assert!(wait.wait_pending(&none));

        // A source identity outside the architecture's accepted namespaces is
        // rejected instead of being recorded. Arm and RISC-V relay their native
        // controllers' canonical state and accept every virtual identity here.
        #[cfg(any(target_arch = "loongarch64", target_arch = "x86_64"))]
        assert_eq!(
            run.publish_queued(0, edge(u32::MAX)),
            Err(SignalError::InvalidSource)
        );
    }

    /// The closed/in-flight boundary is shared by every publisher, so a closed
    /// run rejects old work notifications and quiescence waits for in-flight ones.
    #[test]
    fn closed_run_rejects_new_work_and_in_flight_publishers_block_quiescence() {
        let run = run_signals(2);
        assert_eq!(run.notify_work(), Ok(()));
        assert!(!run.interrupts_quiet());

        let producer = IrqProducerGuard::new(&run).expect("open run admits one publisher");
        run.close_interrupts();
        assert!(!run.interrupts_quiet());
        assert!(matches!(
            IrqProducerGuard::new(&run),
            Err(SignalError::Closed)
        ));
        drop(producer);
        assert!(run.interrupts_quiet());

        // Every formal publisher now rejects the closed run.
        assert_eq!(run.notify_work(), Err(SignalError::Closed));
        assert_eq!(run.publish_queued(0, edge(1)), Err(SignalError::Closed));
        assert_eq!(run.kick_from_irq(0), Err(SignalError::Closed));
        // `kick` is task-only and is not part of the closed boundary: it still
        // reports the missing target and the out-of-range id.
        assert_eq!(run.kick(0), Err(SignalError::InactiveTarget));
        assert_eq!(
            run.kick(usize::BITS as usize),
            Err(SignalError::InvalidTarget)
        );
    }

    /// Registration identity rejects a foreign run and an out-of-range vCPU
    /// without needing a live host wake handle.
    #[test]
    fn registration_identity_reports_invalid_source_and_target() {
        let run = run_signals(2);
        assert_eq!(run.validate_instance(instance(run.run_id(), 0, 1)), Ok(1));
        assert_eq!(
            run.validate_instance(instance(RunId::new(VmKey::new(2, 1), 1), 0, 1)),
            Err(SignalError::InvalidSource)
        );
        assert_eq!(
            run.validate_instance(instance(run.run_id(), 2, 1)),
            Err(SignalError::InvalidTarget)
        );
    }

    /// Work is canonical and owner independent, so handing the poller over can
    /// never lose work and never lets a non-poller consume it.
    #[test]
    fn poll_owner_transfer_preserves_canonical_work() {
        let run = run_signals(4);

        // Work published before any poller is designated is sticky.
        assert_eq!(run.notify_work(), Ok(()));
        assert!(!run.work_pending(1));
        assert_eq!(run.set_poll_owner(Some(1)), Ok(()));
        assert!(run.work_pending(1));
        assert!(!run.work_pending(2));
        assert!(!run.take_work(2));
        assert!(run.take_work(1));
        assert!(!run.take_work(1));
        assert!(!run.work_pending(1));

        // A hand-over keeps new work for the newly designated poller.
        assert_eq!(run.notify_work(), Ok(()));
        assert!(run.work_pending(1));
        assert_eq!(run.set_poll_owner(Some(2)), Ok(()));
        assert!(!run.work_pending(1));
        assert!(run.work_pending(2));
        assert!(run.take_work(2));

        // Only an out-of-range designation fails, and it changes nothing.
        assert_eq!(run.set_poll_owner(Some(9)), Err(SignalError::InvalidTarget));
        assert_eq!(run.poll_owner(), Some(2));
        assert_eq!(run.set_poll_owner(None), Ok(()));
        assert_eq!(run.poll_owner(), None);
        assert!(!run.work_pending(2));
    }
}
