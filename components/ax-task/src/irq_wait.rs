//! Single-waiter hard-IRQ notification cell.
//!
//! Multi-waiter events should target a fixed service thread through this cell;
//! that thread performs any wait-queue fan-out in ordinary task context.
//!
//! The registration lifecycle follows Linux v7.1 `irq_work` ownership: the
//! `Notifying` phase is the executor's `IRQ_WORK_BUSY` claim. It covers the
//! direct wake, the cell-sentinel cleanup, and every other access to the
//! registration and its wake payload, and the release publication back to
//! `Detached` is the notifier's final action. A waiter that observes
//! `Detached` for its generation owns the registration and its wake payload
//! again, exactly like `irq_work_sync()` observing a cleared BUSY bit.

use alloc::sync::Arc;
use core::{
    hint::spin_loop,
    ptr,
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
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
        _ => unreachable!("registration phase exceeds its bit mask"),
    }
}

#[derive(Debug)]
enum IrqWaitWake {
    Thread(ThreadWakeHandle),
}

impl IrqWaitWake {
    fn wake(&self) -> crate::WakeResult {
        match self {
            Self::Thread(wake) => wake.wake(),
        }
    }
}

/// Pinned storage published to one [`IrqWaitCell`].
#[derive(Debug)]
struct IrqWaitNode {
    wake: IrqWaitWake,
    state: AtomicU64,
}

impl IrqWaitNode {
    fn new(wake: IrqWaitWake) -> Self {
        Self {
            wake,
            state: AtomicU64::new(registration_state(0, RegistrationPhase::Detached)),
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

    /// Releases the notification claim as the notifier's final node access.
    ///
    /// Linux `irq_work_single()` clears `IRQ_WORK_BUSY` only after the whole
    /// callback has returned, and `irq_work_sync()` treats that clear as the
    /// executor's last access to the work item. This transition carries the
    /// same contract: a waiter that observes `Detached` for its generation
    /// may reuse the registration and its wake payload because no notifier
    /// touches them afterwards.
    fn finish_notification(&self, generation: u64) {
        self.state
            .compare_exchange(
                registration_state(generation, RegistrationPhase::Notifying),
                registration_state(generation, RegistrationPhase::Detached),
                Ordering::Release,
                Ordering::Acquire,
            )
            .expect("IRQ wait notification generation changed while in flight");
    }

    fn is_attached(&self, generation: u64) -> bool {
        self.state.load(Ordering::Acquire)
            == registration_state(generation, RegistrationPhase::Attached)
    }

    fn is_quiescent(&self, generation: u64) -> bool {
        let state = self.state.load(Ordering::Acquire);
        registration_generation(state) != generation
            || registration_phase(state) == RegistrationPhase::Detached
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
        Self {
            node: Arc::new(IrqWaitNode::new(IrqWaitWake::Thread(wake))),
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
    /// imply that an IRQ notifier has finished reading the wake payload; only
    /// the drain returned by [`Self::detach`] publishes that guarantee.
    pub fn is_attached(&self) -> bool {
        self.registration.is_attached(self.generation)
    }

    /// Stops publication of this generation and enters its drain lifetime.
    ///
    /// This operation never waits. If a notifier already removed the waiter,
    /// the returned drain observes that notifier until it publishes its claim
    /// release; [`IrqWaitDrain::finish`] performs that bounded wait. Hard-IRQ
    /// teardown must defer the drain to a task-context worker.
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

/// Revoked IRQ registration waiting out an in-flight notification claim.
///
/// A drain never writes registration state. The notifier's `Notifying` claim
/// covers every access to the node and its wake payload, and publishes
/// `Detached` as its final action, so this type only needs to observe that
/// publication before the registration may be reused.
#[must_use = "an IRQ wait drain must finish before registration storage is reused"]
pub struct IrqWaitDrain {
    registration: Arc<IrqWaitNode>,
    generation: u64,
}

impl IrqWaitDrain {
    /// Reports whether the in-flight notifier has published its release.
    pub fn is_quiescent(&self) -> bool {
        self.registration.is_quiescent(self.generation)
    }

    /// Waits until the in-flight notifier has published its release.
    ///
    /// Every notifier section runs in hard IRQ or with preemption disabled,
    /// so the claim always ends in bounded time. This is Linux's hard-path
    /// `irq_work_sync()` shape (`while (irq_work_is_busy(work)) cpu_relax();`):
    /// a park would require exactly the post-release completion wake that the
    /// `Notifying`-covers-everything ownership rule removes.
    pub fn finish(self) {
        while !self.is_quiescent() {
            spin_loop();
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
    /// Another waiter is registered, or this registration is still in use.
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
    ///
    /// The whole claim-to-release section runs non-preemptible, mirroring
    /// Linux's irq_work execution boundary: hard IRQ context is inherently
    /// non-preemptible, and ordinary task context enters a preemption scope,
    /// matching how `irq_workd` disables migration around the claim. This
    /// bounds how long a draining waiter can observe a `Notifying` claim.
    pub fn notify(&self) -> IrqNotifyResult {
        // SAFETY-free context probe: the runtime hook only reports the current
        // interrupt state; the preemption scope below never nests into it.
        let _preempt =
            (!crate::runtime::task_runtime::in_hard_irq()).then(crate::lock::PreemptScope::enter);
        self.notify_claimed()
    }

    fn notify_claimed(&self) -> IrqNotifyResult {
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
                    let (generation, result) = Self::wake_registration(&registration);
                    self.finish_notification(result);
                    registration.finish_notification(generation);
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
        let (generation, result) = Self::wake_registration(&registration);
        registration.finish_notification(generation);
        Self::notification_result(result)
    }

    /// Reports whether an IRQ is coalesced for the next registration.
    pub fn is_pending(&self) -> bool {
        matches!(
            self.waiter.load(Ordering::Acquire),
            waiter if waiter == pending_waiter() || waiter == notifying_pending_waiter()
        )
    }

    fn wake_registration(registration: &IrqWaitNode) -> (u64, crate::WakeResult) {
        let generation = registration.begin_notification();
        let result = registration.wake.wake();
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
    const DETACHED_GENERATION_2: usize = 2 << 2;
    const ATTACHED_GENERATION_2: usize = DETACHED_GENERATION_2 | 1;
    const NOTIFYING_GENERATION_2: usize = DETACHED_GENERATION_2 | 2;

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
                                // finish_notification(): the release is the
                                // notifier's final node access.
                                registration
                                    .compare_exchange(
                                        NOTIFYING_GENERATION_1,
                                        DETACHED_GENERATION_1,
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
            assert_eq!(
                wakes.load(Ordering::Acquire) + synchronous_consumes.load(Ordering::Acquire),
                1
            );
            assert_eq!(waiter.load(Ordering::Acquire), EMPTY);
            assert_eq!(registration.load(Ordering::Acquire), DETACHED_GENERATION_1);
        });
    }

    fn model_generation_release_closes_pointer_aba() {
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
                        let (attached, notifying, detached) =
                            match registration.load(Ordering::Acquire) {
                                ATTACHED_GENERATION_1 => (
                                    ATTACHED_GENERATION_1,
                                    NOTIFYING_GENERATION_1,
                                    DETACHED_GENERATION_1,
                                ),
                                ATTACHED_GENERATION_2 => (
                                    ATTACHED_GENERATION_2,
                                    NOTIFYING_GENERATION_2,
                                    DETACHED_GENERATION_2,
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
                                detached,
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
                    // detach(): removing the cell publication proves the
                    // registration is still `Attached` for this generation,
                    // so the cancel cannot observe a reused registration.
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
                    // The drain only observes the notifier's release; it
                    // never writes registration state itself.
                    while registration.load(Ordering::Acquire) == NOTIFYING_GENERATION_1 {
                        thread::yield_now();
                    }
                    // Registration reuse happens strictly after the drain
                    // finishes, on the same owner thread.
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
                })
            };

            notifier.join().unwrap();
            old_owner.join().unwrap();
            match registration.load(Ordering::Acquire) {
                ATTACHED_GENERATION_2 => assert_eq!(waiter.load(Ordering::Acquire), WAITER),
                DETACHED_GENERATION_1 => assert_eq!(waiter.load(Ordering::Acquire), EMPTY),
                state => panic!("registration ended in invalid state {state}"),
            }
        });
    }

    /// Guards the Linux v7.1 `irq_work` ownership rule: the executor must not
    /// touch the work item after publishing that it is no longer busy.
    ///
    /// The ghost `payload_epoch` stands in for the reusable
    /// `ThreadWakeHandle` payload: the waiter may re-arm it (advance the
    /// epoch) only after observing the notifier's release publication
    /// (`Detached`, or a newer generation). Because the notifier reads the
    /// payload strictly before publishing that release, no interleaving can
    /// hand the payload to a new generation underneath an in-flight read.
    fn model_notification_owns_payload_until_release_publication() {
        loom::model(|| {
            let slot = Arc::new(AtomicUsize::new(WAITER));
            let registration = Arc::new(AtomicUsize::new(ATTACHED_GENERATION_1));
            let payload_epoch = Arc::new(AtomicUsize::new(1));

            let notifier = {
                let slot = Arc::clone(&slot);
                let registration = Arc::clone(&registration);
                let payload_epoch = Arc::clone(&payload_epoch);
                thread::spawn(move || {
                    // notify(): claim the published waiter out of the cell.
                    slot.compare_exchange(WAITER, EMPTY, Ordering::AcqRel, Ordering::Acquire)
                        .unwrap();
                    // begin_notification(): ATTACHED -> NOTIFIING (BUSY).
                    registration
                        .compare_exchange(
                            ATTACHED_GENERATION_1,
                            NOTIFYING_GENERATION_1,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .unwrap();
                    // The direct wake reads its generation-owned payload while
                    // the claim is held.
                    assert_eq!(
                        payload_epoch.load(Ordering::Acquire),
                        1,
                        "the direct wake must read its own generation's payload"
                    );
                    // finish_notification(): publishes Detached as the final
                    // node access; the notifier touches nothing afterwards.
                    registration
                        .compare_exchange(
                            NOTIFYING_GENERATION_1,
                            DETACHED_GENERATION_1,
                            Ordering::Release,
                            Ordering::Acquire,
                        )
                        .unwrap();
                })
            };
            let waiter = {
                let registration = Arc::clone(&registration);
                let payload_epoch = Arc::clone(&payload_epoch);
                thread::spawn(move || {
                    // quiesce_irq_wait(): reuse is permitted only after the
                    // release publication is observed.
                    while registration.load(Ordering::Acquire) == NOTIFYING_GENERATION_1 {
                        thread::yield_now();
                    }
                    if registration.load(Ordering::Acquire) == DETACHED_GENERATION_1 {
                        payload_epoch.store(2, Ordering::Release);
                    }
                })
            };

            notifier.join().unwrap();
            waiter.join().unwrap();
        });
    }

    #[test]
    fn notification_owns_payload_until_release_publication() {
        model_notification_owns_payload_until_release_publication();
    }

    #[test]
    fn registration_notify_and_generation_release_are_race_safe() {
        model_register_notify_winner();
        model_generation_release_closes_pointer_aba();
    }
}
