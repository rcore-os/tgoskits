//! Single-waiter hard-IRQ notification cell.
//!
//! Multi-waiter events should target a fixed service thread through this cell;
//! that thread performs any wait-queue fan-out in ordinary task context.

use alloc::sync::Arc;
use core::{
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering},
};

use crate::ThreadWakeHandle;

const REGISTRATION_PHASE_BITS: u32 = 2;
const REGISTRATION_PHASE_MASK: u64 = (1 << REGISTRATION_PHASE_BITS) - 1;
const REGISTRATION_GENERATION_MAX: u64 = u64::MAX >> REGISTRATION_PHASE_BITS;
const IRQ_NOTIFY_CAS_BUDGET: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u64)]
enum RegistrationPhase {
    Detached  = 0,
    Attached  = 1,
    Notifying = 2,
    Draining  = 3,
}

const fn registration_state(generation: u64, phase: RegistrationPhase) -> u64 {
    (generation << REGISTRATION_PHASE_BITS) | phase as u64
}

const fn registration_generation(state: u64) -> u64 {
    state >> REGISTRATION_PHASE_BITS
}

#[repr(align(8))]
struct WaiterSentinel {
    _tag: u8,
}

static PENDING_WAITER_SENTINEL: WaiterSentinel = WaiterSentinel { _tag: 1 };
static NOTIFYING_WAITER_SENTINEL: WaiterSentinel = WaiterSentinel { _tag: 2 };
static NOTIFYING_PENDING_WAITER_SENTINEL: WaiterSentinel = WaiterSentinel { _tag: 3 };

fn waiter_sentinel(sentinel: &'static WaiterSentinel) -> *mut IrqWaitNode {
    ptr::from_ref(sentinel).cast_mut().cast()
}

fn pending_waiter() -> *mut IrqWaitNode {
    waiter_sentinel(&PENDING_WAITER_SENTINEL)
}

fn notifying_waiter() -> *mut IrqWaitNode {
    waiter_sentinel(&NOTIFYING_WAITER_SENTINEL)
}

fn notifying_pending_waiter() -> *mut IrqWaitNode {
    waiter_sentinel(&NOTIFYING_PENDING_WAITER_SENTINEL)
}

fn is_notification_sentinel(waiter: *mut IrqWaitNode) -> bool {
    waiter == notifying_waiter() || waiter == notifying_pending_waiter()
}

fn registration_phase(state: u64) -> RegistrationPhase {
    match state & REGISTRATION_PHASE_MASK {
        0 => RegistrationPhase::Detached,
        1 => RegistrationPhase::Attached,
        2 => RegistrationPhase::Notifying,
        3 => RegistrationPhase::Draining,
        _ => unreachable!("registration phase exceeds its bit mask"),
    }
}

#[derive(Debug)]
enum IrqWaitWake {
    Thread(ThreadWakeHandle),
}

#[derive(Clone, Copy)]
enum WakeContext {
    HardIrq,
    Task,
}

impl IrqWaitWake {
    fn wake(&self, context: WakeContext) -> crate::WakeResult {
        match self {
            Self::Thread(wake) => match context {
                WakeContext::HardIrq => wake.wake(),
                WakeContext::Task => wake.wake_from_task(),
            },
        }
    }
}

/// Pinned storage published to one [`IrqWaitCell`].
#[derive(Debug)]
struct IrqWaitNode {
    wake: IrqWaitWake,
    state: AtomicU64,
    drain_wake_requested: AtomicBool,
}

impl IrqWaitNode {
    fn new(wake: IrqWaitWake) -> Self {
        Self {
            wake,
            state: AtomicU64::new(registration_state(0, RegistrationPhase::Detached)),
            drain_wake_requested: AtomicBool::new(false),
        }
    }

    fn reserve(&self) -> Option<u64> {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if registration_phase(state) != RegistrationPhase::Detached {
                return None;
            }
            let generation = registration_generation(state)
                .checked_add(1)
                .filter(|generation| *generation <= REGISTRATION_GENERATION_MAX)
                .expect("IRQ wait registration generation exhausted");
            let attached = registration_state(generation, RegistrationPhase::Attached);
            match self.state.compare_exchange_weak(
                state,
                attached,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(generation),
                Err(observed) => state = observed,
            }
        }
    }

    fn cancel(&self, generation: u64) {
        self.state
            .compare_exchange(
                registration_state(generation, RegistrationPhase::Attached),
                registration_state(generation, RegistrationPhase::Detached),
                Ordering::Release,
                Ordering::Acquire,
            )
            .expect("only an attached IRQ wait registration can be cancelled");
    }

    fn begin_notification(&self) -> u64 {
        let state = self.state.load(Ordering::Acquire);
        let generation = registration_generation(state);
        assert_eq!(
            registration_phase(state),
            RegistrationPhase::Attached,
            "an IRQ wait cell took a registration it no longer owned"
        );
        self.state
            .compare_exchange(
                state,
                registration_state(generation, RegistrationPhase::Notifying),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("IRQ wait registration ownership changed after cell removal");
        generation
    }

    fn finish_notification(&self, generation: u64, context: WakeContext) {
        self.state
            .compare_exchange(
                registration_state(generation, RegistrationPhase::Notifying),
                registration_state(generation, RegistrationPhase::Draining),
                Ordering::Release,
                Ordering::Acquire,
            )
            .expect("IRQ wait notification generation changed while in flight");
        if self.drain_wake_requested.swap(false, Ordering::AcqRel) {
            let _ = self.wake.wake(context);
        }
    }

    fn is_attached(&self, generation: u64) -> bool {
        self.state.load(Ordering::Acquire)
            == registration_state(generation, RegistrationPhase::Attached)
    }

    fn is_quiescent(&self, generation: u64) -> bool {
        let state = self.state.load(Ordering::Acquire);
        registration_generation(state) != generation
            || matches!(
                registration_phase(state),
                RegistrationPhase::Detached | RegistrationPhase::Draining
            )
    }

    fn finish_drain(&self, generation: u64) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if registration_generation(state) != generation {
                return true;
            }
            match registration_phase(state) {
                RegistrationPhase::Detached => return true,
                RegistrationPhase::Attached | RegistrationPhase::Notifying => return false,
                RegistrationPhase::Draining => {
                    match self.state.compare_exchange_weak(
                        state,
                        registration_state(generation, RegistrationPhase::Detached),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => return true,
                        Err(observed) => state = observed,
                    }
                }
            }
        }
    }

    fn request_drain_wake(&self, generation: u64) {
        debug_assert_eq!(
            registration_generation(self.state.load(Ordering::Acquire)),
            generation,
            "only the active IRQ wait generation may request its drain wake"
        );
        self.drain_wake_requested.store(true, Ordering::Release);
    }
}

fn publish_cell_owner(node: Arc<IrqWaitNode>) -> *mut IrqWaitNode {
    Arc::into_raw(node).cast_mut()
}

unsafe fn take_cell_owner(node: *mut IrqWaitNode) -> Arc<IrqWaitNode> {
    unsafe {
        // SAFETY: callers invoke this exactly once after atomically removing a
        // real node pointer previously produced by publish_cell_owner().
        Arc::from_raw(node)
    }
}

/// Task-context owner of a reusable one-shot registration.
///
/// The node is reference-owned. Publishing it transfers one owning reference
/// to the cell, while tokens and drains retain their own references. Dropping
/// this task-context handle therefore never relies on a destructor leak and
/// cannot invalidate an in-flight hard-IRQ reader.
#[derive(Debug)]
pub struct IrqWaitRegistration {
    node: Arc<IrqWaitNode>,
}

impl IrqWaitRegistration {
    /// Creates a detached registration reusable across one-shot waits.
    pub fn new(wake: ThreadWakeHandle) -> Self {
        Self::from_wake(IrqWaitWake::Thread(wake))
    }

    fn from_wake(wake: IrqWaitWake) -> Self {
        Self {
            node: Arc::new(IrqWaitNode::new(wake)),
        }
    }
}

/// Published identity for one IRQ waiter registration generation.
///
/// A token remains attached while its cell owns the waiter. Once
/// [`is_attached`](Self::is_attached) becomes false, the task may avoid or
/// abort its park. It must then be consumed by [`detach`](Self::detach) before
/// the backing registration or wake payload is reused.
#[must_use = "an IRQ wait token must enter its drain lifetime before storage is reused"]
pub struct IrqWaitToken<'cell> {
    registration: Arc<IrqWaitNode>,
    generation: u64,
    cell: &'cell IrqWaitCell,
}

impl IrqWaitToken<'_> {
    /// Returns this one-shot registration generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns whether the cell still owns this generation.
    ///
    /// Once this becomes false, a waiter may safely avoid sleeping. It does not
    /// imply that an IRQ notifier has finished reading the wake payload.
    pub fn is_attached(&self) -> bool {
        self.registration.is_attached(self.generation)
    }

    /// Stops publication of this generation and enters its drain lifetime.
    ///
    /// This operation never waits. If a notifier already removed the waiter,
    /// the returned drain observes that notifier until its direct wake and cell
    /// ownership publication have both finished. Task context may poll the
    /// drain; hard-IRQ teardown must defer it to a task worker.
    pub fn detach(self) -> IrqWaitDrain {
        let cell = self.cell;
        cell.detach(self)
    }

    fn belongs_to(&self, cell: &IrqWaitCell) -> bool {
        ptr::eq(self.cell, cell)
    }
}

impl core::fmt::Debug for IrqWaitToken<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("IrqWaitToken")
            .field("generation", &self.generation)
            .field("attached", &self.is_attached())
            .finish()
    }
}

/// Revoked IRQ registration waiting for its in-flight notification transaction.
///
/// A drain no longer retains the cell: publication admission for its generation
/// is already closed. It retains the reference-owned node until the hard-IRQ
/// reader has left the trusted direct-wake operation and completed the cell's
/// notification ownership transition.
#[must_use = "an IRQ wait drain must finish before registration storage is reused"]
pub struct IrqWaitDrain {
    registration: Arc<IrqWaitNode>,
    generation: u64,
}

impl IrqWaitDrain {
    /// Returns whether the notifier grace period has completed.
    pub fn is_quiescent(&self) -> bool {
        self.registration.is_quiescent(self.generation)
    }

    /// Consumes a completed drain, or requests a completion wake and returns it
    /// while an IRQ notification is in flight.
    ///
    /// This method never waits. A caller that receives the drain back must use
    /// a generation-checked task park before retrying. Hard-IRQ paths must
    /// defer reclamation instead.
    pub fn try_finish(self) -> Result<(), Self> {
        if self.registration.finish_drain(self.generation) {
            Ok(())
        } else {
            self.registration.request_drain_wake(self.generation);
            if self.registration.finish_drain(self.generation) {
                self.registration
                    .drain_wake_requested
                    .store(false, Ordering::Release);
                Ok(())
            } else {
                Err(self)
            }
        }
    }
}

impl core::fmt::Debug for IrqWaitDrain {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("IrqWaitDrain")
            .field("generation", &self.generation)
            .field("quiescent", &self.is_quiescent())
            .finish()
    }
}

/// Outcome of task-context waiter registration.
#[derive(Debug)]
pub enum IrqRegisterResult<'cell> {
    /// The cell owns the sole waiter until notify or unregister.
    Registered(IrqWaitToken<'cell>),
    /// An earlier or concurrent interrupt consumed the registration.
    ///
    /// An earlier pending event is returned synchronously without waking the
    /// currently running task. The registration is detached on return.
    ConsumedPending,
    /// A concurrent notifier owns the registration and will release and wake it.
    ///
    /// The task may abort its park once the token is detached, but it must
    /// quiesce the token before reusing the registration or wake payload.
    NotificationInFlight(IrqWaitToken<'cell>),
    /// Another waiter is registered, or this registration is still draining.
    Occupied,
}

/// Outcome of one bounded hard-IRQ notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqNotifyResult {
    /// One stable direct waiter was removed and woken.
    ///
    /// A scheduler wake already retained by the target park transition also
    /// counts as delivered.
    Notified,
    /// No waiter was present, or direct delivery failed; one coalesced pending
    /// bit was published.
    Pending,
}

/// Pending-bit plus single-waiter hard-IRQ event cell.
#[derive(Debug)]
pub struct IrqWaitCell {
    waiter: AtomicPtr<IrqWaitNode>,
}

impl IrqWaitCell {
    /// Creates an empty notification cell.
    pub const fn new() -> Self {
        Self {
            waiter: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Registers one stable waiter, consuming an earlier IRQ when present.
    pub fn register<'cell>(
        &'cell self,
        registration: &IrqWaitRegistration,
    ) -> IrqRegisterResult<'cell> {
        let registration = Arc::clone(&registration.node);
        let Some(generation) = registration.reserve() else {
            return IrqRegisterResult::Occupied;
        };
        let registration_ptr = publish_cell_owner(Arc::clone(&registration));
        let token = IrqWaitToken {
            registration,
            generation,
            cell: self,
        };
        let pending = pending_waiter();
        let mut observed = self.waiter.load(Ordering::Acquire);
        loop {
            if observed == pending {
                match self.waiter.compare_exchange(
                    pending,
                    ptr::null_mut(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        token.registration.cancel(generation);
                        // SAFETY: this raw reference was never published, so
                        // the registering task still exclusively owns it.
                        unsafe { drop(take_cell_owner(registration_ptr)) };
                        return IrqRegisterResult::ConsumedPending;
                    }
                    Err(current) => {
                        observed = current;
                        continue;
                    }
                }
            }
            if !observed.is_null() {
                token.registration.cancel(generation);
                // SAFETY: this raw reference was never published, so the
                // registering task still exclusively owns it.
                unsafe { drop(take_cell_owner(registration_ptr)) };
                return IrqRegisterResult::Occupied;
            }
            match self.waiter.compare_exchange(
                ptr::null_mut(),
                registration_ptr,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => observed = current,
            }
        }

        if self.waiter.load(Ordering::Acquire) == registration_ptr {
            IrqRegisterResult::Registered(token)
        } else {
            // A concurrent notifier already owns and will wake the registration.
            IrqRegisterResult::NotificationInFlight(token)
        }
    }

    fn detach(&self, token: IrqWaitToken<'_>) -> IrqWaitDrain {
        assert!(
            token.belongs_to(self),
            "an IRQ wait token must be detached by its publishing cell"
        );
        let registration = token.registration;
        let state = registration.state.load(Ordering::Acquire);
        if registration_generation(state) == token.generation {
            let registration_ptr = Arc::as_ptr(&registration).cast_mut();
            if self
                .waiter
                .compare_exchange(
                    registration_ptr,
                    ptr::null_mut(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                registration.cancel(token.generation);
                // SAFETY: the successful CAS transferred the cell-owned raw
                // reference to this task-context detach operation.
                unsafe { drop(take_cell_owner(registration_ptr)) };
            }
        }
        IrqWaitDrain {
            registration,
            generation: token.generation,
        }
    }

    /// Wakes the sole registered thread or publishes one coalesced pending bit.
    ///
    /// This operation performs a bounded number of atomics and at most one
    /// trusted direct wake. After repeated contention, it atomically installs
    /// the sticky pending state and may retain one harmless extra service pass
    /// after waking the displaced waiter. It never scans a wait queue or
    /// allocates.
    pub fn notify(&self) -> IrqNotifyResult {
        self.notify_with_context(WakeContext::HardIrq)
    }

    fn notify_with_context(&self, context: WakeContext) -> IrqNotifyResult {
        let pending = pending_waiter();
        let notifying = notifying_waiter();
        let notifying_pending = notifying_pending_waiter();
        let mut observed = self.waiter.load(Ordering::Acquire);
        for _ in 0..IRQ_NOTIFY_CAS_BUDGET {
            if observed == pending {
                match self.waiter.compare_exchange(
                    pending,
                    pending,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return IrqNotifyResult::Pending,
                    Err(current) => {
                        observed = current;
                        continue;
                    }
                }
            }
            if observed == notifying_pending {
                return IrqNotifyResult::Pending;
            }
            if observed == notifying {
                match self.waiter.compare_exchange(
                    notifying,
                    notifying_pending,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return IrqNotifyResult::Pending,
                    Err(current) => {
                        observed = current;
                        continue;
                    }
                }
            }
            if observed.is_null() {
                match self.waiter.compare_exchange(
                    ptr::null_mut(),
                    pending,
                    Ordering::Release,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return IrqNotifyResult::Pending,
                    Err(current) => {
                        observed = current;
                        continue;
                    }
                }
            }
            match self.waiter.compare_exchange(
                observed,
                notifying,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(waiter) => {
                    // SAFETY: the successful CAS transferred the cell-owned
                    // raw reference to this notifier.
                    let registration = unsafe { take_cell_owner(waiter) };
                    let (generation, result) = Self::wake_registration(&registration, context);
                    // The direct wake may make the service thread runnable
                    // immediately. Keep its registration in Notifying until
                    // the cell sentinel is gone, so quiescence is the single
                    // edge after which that thread may register again.
                    self.finish_notification(result);
                    registration.finish_notification(generation, context);
                    return Self::notification_result(result);
                }
                Err(current) => observed = current,
            }
        }

        // Hard IRQ work remains wait-free under pathological cross-CPU churn.
        // Keeping the sentinel after displacing a waiter may cause one
        // task-context recheck, but it cannot lose the notification.
        let waiter = self.waiter.swap(pending, Ordering::AcqRel);
        if waiter.is_null() || waiter == pending || is_notification_sentinel(waiter) {
            return IrqNotifyResult::Pending;
        }
        // SAFETY: swap transferred the displaced cell-owned raw reference to
        // this notifier; null and the sentinel were rejected above.
        let registration = unsafe { take_cell_owner(waiter) };
        let (generation, result) = Self::wake_registration(&registration, context);
        registration.finish_notification(generation, context);
        Self::notification_result(result)
    }

    /// Wakes the sole registered thread from ordinary task context.
    ///
    /// This preserves the same pending and registration lifetime protocol as
    /// [`Self::notify`], while allowing the scheduler to activate a same-CPU
    /// waiter directly. Callers must not invoke it from hard IRQ context.
    pub fn notify_from_task(&self) -> IrqNotifyResult {
        assert!(
            !crate::runtime::task_runtime::in_hard_irq(),
            "task IRQ-cell notification is not valid in hard IRQ context"
        );
        // Linux keeps the waiter metadata owner non-preemptible until the
        // wake callback and wake-queue publication are both complete. Without
        // the same boundary here, a same-CPU direct wake can run the waiter
        // while this registration is still `Notifying`; that waiter then spins
        // in its drain path waiting for the notifier it just preempted.
        let _preempt = crate::lock::PreemptScope::enter();
        self.notify_with_context(WakeContext::Task)
    }

    /// Reports whether an IRQ is coalesced for the next registration.
    pub fn is_pending(&self) -> bool {
        matches!(
            self.waiter.load(Ordering::Acquire),
            waiter if waiter == pending_waiter() || waiter == notifying_pending_waiter()
        )
    }

    fn wake_registration(
        registration: &IrqWaitNode,
        context: WakeContext,
    ) -> (u64, crate::WakeResult) {
        let generation = registration.begin_notification();
        let result = registration.wake.wake(context);
        (generation, result)
    }

    fn finish_notification(&self, result: crate::WakeResult) {
        let pending = pending_waiter();
        let notifying = notifying_waiter();
        let notifying_pending = notifying_pending_waiter();
        let delivered = matches!(
            result,
            crate::WakeResult::Notified | crate::WakeResult::AlreadyPending
        );
        let mut observed = self.waiter.load(Ordering::Acquire);
        loop {
            let next = if observed == notifying {
                if delivered { ptr::null_mut() } else { pending }
            } else if observed == notifying_pending {
                pending
            } else if observed == pending {
                return;
            } else {
                panic!("IRQ wait cell notification ownership changed while wake was in flight");
            };
            match self.waiter.compare_exchange_weak(
                observed,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(current) => observed = current,
            }
        }
    }

    const fn notification_result(result: crate::WakeResult) -> IrqNotifyResult {
        match result {
            crate::WakeResult::Notified | crate::WakeResult::AlreadyPending => {
                IrqNotifyResult::Notified
            }
            crate::WakeResult::Exited | crate::WakeResult::Unavailable => IrqNotifyResult::Pending,
        }
    }
}

impl Drop for IrqWaitCell {
    fn drop(&mut self) {
        let waiter = core::mem::replace(self.waiter.get_mut(), ptr::null_mut());
        if waiter.is_null() || waiter == pending_waiter() {
            return;
        }
        assert!(
            !is_notification_sentinel(waiter),
            "exclusive IRQ wait cell teardown found an in-flight notifier",
        );

        // SAFETY: exclusive cell teardown removes the sole cell-owned raw
        // reference, and safe Rust prevents a concurrent notifier borrow.
        let registration = unsafe { take_cell_owner(waiter) };
        let state = registration.state.load(Ordering::Acquire);
        assert_eq!(
            registration_phase(state),
            RegistrationPhase::Attached,
            "exclusive IRQ wait cell teardown found an in-flight notifier",
        );
        registration.cancel(registration_generation(state));
    }
}

impl Default for IrqWaitCell {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, not(miri)))]
mod loom_tests {
    use loom::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    const EMPTY: usize = 0;
    const WAITER: usize = 1;
    const PENDING: usize = 2;
    const DETACHED_GENERATION_0: usize = 0;
    const DETACHED_GENERATION_1: usize = 1 << 2;
    const ATTACHED_GENERATION_1: usize = DETACHED_GENERATION_1 | 1;
    const NOTIFYING_GENERATION_1: usize = DETACHED_GENERATION_1 | 2;
    const DRAINING_GENERATION_1: usize = DETACHED_GENERATION_1 | 3;
    const ATTACHED_GENERATION_2: usize = (2 << 2) | 1;
    const NOTIFYING_GENERATION_2: usize = (2 << 2) | 2;
    const DRAINING_GENERATION_2: usize = (2 << 2) | 3;

    fn model_register_notify_winner() {
        loom::model(|| {
            let waiter = Arc::new(AtomicUsize::new(EMPTY));
            let registration = Arc::new(AtomicUsize::new(DETACHED_GENERATION_0));
            let wakes = Arc::new(AtomicUsize::new(0));
            let synchronous_consumes = Arc::new(AtomicUsize::new(0));

            let register = {
                let waiter = Arc::clone(&waiter);
                let registration = Arc::clone(&registration);
                let synchronous_consumes = Arc::clone(&synchronous_consumes);
                thread::spawn(move || {
                    registration
                        .compare_exchange(
                            DETACHED_GENERATION_0,
                            ATTACHED_GENERATION_1,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .unwrap();
                    let mut observed = waiter.load(Ordering::Acquire);
                    loop {
                        if observed == PENDING {
                            match waiter.compare_exchange(
                                PENDING,
                                EMPTY,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            ) {
                                Ok(_) => {
                                    registration
                                        .compare_exchange(
                                            ATTACHED_GENERATION_1,
                                            DETACHED_GENERATION_1,
                                            Ordering::Release,
                                            Ordering::Acquire,
                                        )
                                        .unwrap();
                                    synchronous_consumes.fetch_add(1, Ordering::Release);
                                    return;
                                }
                                Err(current) => {
                                    observed = current;
                                    continue;
                                }
                            }
                        }
                        assert_eq!(observed, EMPTY);
                        match waiter.compare_exchange(
                            EMPTY,
                            WAITER,
                            Ordering::Release,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => return,
                            Err(current) => observed = current,
                        }
                    }
                })
            };
            let notify = {
                let waiter = Arc::clone(&waiter);
                let registration = Arc::clone(&registration);
                let wakes = Arc::clone(&wakes);
                thread::spawn(move || {
                    let mut observed = waiter.load(Ordering::Acquire);
                    loop {
                        if observed == PENDING {
                            match waiter.compare_exchange(
                                PENDING,
                                PENDING,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            ) {
                                Ok(_) => return,
                                Err(current) => {
                                    observed = current;
                                    continue;
                                }
                            }
                        }
                        if observed == EMPTY {
                            match waiter.compare_exchange(
                                EMPTY,
                                PENDING,
                                Ordering::Release,
                                Ordering::Acquire,
                            ) {
                                Ok(_) => return,
                                Err(current) => {
                                    observed = current;
                                    continue;
                                }
                            }
                        }
                        assert_eq!(observed, WAITER);
                        match waiter.compare_exchange(
                            WAITER,
                            EMPTY,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => {
                                registration
                                    .compare_exchange(
                                        ATTACHED_GENERATION_1,
                                        NOTIFYING_GENERATION_1,
                                        Ordering::AcqRel,
                                        Ordering::Acquire,
                                    )
                                    .unwrap();
                                wakes.fetch_add(1, Ordering::Release);
                                registration
                                    .compare_exchange(
                                        NOTIFYING_GENERATION_1,
                                        DRAINING_GENERATION_1,
                                        Ordering::Release,
                                        Ordering::Acquire,
                                    )
                                    .unwrap();
                                return;
                            }
                            Err(current) => observed = current,
                        }
                    }
                })
            };

            register.join().unwrap();
            notify.join().unwrap();
            let _ = registration.compare_exchange(
                DRAINING_GENERATION_1,
                DETACHED_GENERATION_1,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            assert_eq!(
                wakes.load(Ordering::Acquire) + synchronous_consumes.load(Ordering::Acquire),
                1
            );
            assert_eq!(waiter.load(Ordering::Acquire), EMPTY);
            assert_eq!(registration.load(Ordering::Acquire), DETACHED_GENERATION_1);
        });
    }

    fn model_generation_drain_closes_pointer_aba() {
        loom::model(|| {
            let waiter = Arc::new(AtomicUsize::new(WAITER));
            let registration = Arc::new(AtomicUsize::new(ATTACHED_GENERATION_1));

            let notifier = {
                let waiter = Arc::clone(&waiter);
                let registration = Arc::clone(&registration);
                thread::spawn(move || {
                    if waiter
                        .compare_exchange(WAITER, EMPTY, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        let (attached, notifying, draining) =
                            match registration.load(Ordering::Acquire) {
                                ATTACHED_GENERATION_1 => (
                                    ATTACHED_GENERATION_1,
                                    NOTIFYING_GENERATION_1,
                                    DRAINING_GENERATION_1,
                                ),
                                ATTACHED_GENERATION_2 => (
                                    ATTACHED_GENERATION_2,
                                    NOTIFYING_GENERATION_2,
                                    DRAINING_GENERATION_2,
                                ),
                                state => panic!("IRQ removed a waiter in invalid state {state}"),
                            };
                        registration
                            .compare_exchange(
                                attached,
                                notifying,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .unwrap();
                        thread::yield_now();
                        registration
                            .compare_exchange(
                                notifying,
                                draining,
                                Ordering::Release,
                                Ordering::Acquire,
                            )
                            .unwrap();
                    }
                })
            };
            let old_owner = {
                let waiter = Arc::clone(&waiter);
                let registration = Arc::clone(&registration);
                thread::spawn(move || {
                    let observed = registration.load(Ordering::Acquire);
                    thread::yield_now();
                    if observed == ATTACHED_GENERATION_1
                        && waiter
                            .compare_exchange(WAITER, EMPTY, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                    {
                        registration
                            .compare_exchange(
                                ATTACHED_GENERATION_1,
                                DETACHED_GENERATION_1,
                                Ordering::Release,
                                Ordering::Acquire,
                            )
                            .unwrap();
                    }
                    let _ = registration.compare_exchange(
                        DRAINING_GENERATION_1,
                        DETACHED_GENERATION_1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                })
            };

            if registration
                .compare_exchange(
                    DETACHED_GENERATION_1,
                    ATTACHED_GENERATION_2,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                waiter
                    .compare_exchange(EMPTY, WAITER, Ordering::Release, Ordering::Acquire)
                    .unwrap();
            }

            notifier.join().unwrap();
            old_owner.join().unwrap();
            let _ = registration.compare_exchange(
                DRAINING_GENERATION_1,
                DETACHED_GENERATION_1,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            if registration
                .compare_exchange(
                    DETACHED_GENERATION_1,
                    ATTACHED_GENERATION_2,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                waiter
                    .compare_exchange(EMPTY, WAITER, Ordering::Release, Ordering::Acquire)
                    .unwrap();
            }
            match registration.load(Ordering::Acquire) {
                ATTACHED_GENERATION_2 => assert_eq!(waiter.load(Ordering::Acquire), WAITER),
                DRAINING_GENERATION_2 => assert_eq!(waiter.load(Ordering::Acquire), EMPTY),
                state => panic!("new generation ended in invalid state {state}"),
            }
        });
    }

    #[test]
    fn registration_notify_and_generation_drain_are_race_safe() {
        model_register_notify_winner();
        model_generation_drain_closes_pointer_aba();
    }
}
