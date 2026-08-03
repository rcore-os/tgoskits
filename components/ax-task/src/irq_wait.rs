//! Single-waiter hard-IRQ notification cell.
//!
//! Multi-waiter events should target a fixed service thread through this cell;
//! that thread performs any wait-queue fan-out in ordinary task context.

use alloc::boxed::Box;
#[cfg(test)]
use core::sync::atomic::AtomicBool;
use core::{
    marker::PhantomPinned,
    mem::ManuallyDrop,
    pin::Pin,
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
    Draining  = 3,
}

const fn registration_state(generation: u64, phase: RegistrationPhase) -> u64 {
    (generation << REGISTRATION_PHASE_BITS) | phase as u64
}

const fn registration_generation(state: u64) -> u64 {
    state >> REGISTRATION_PHASE_BITS
}

#[repr(align(8))]
struct PendingWaiterSentinel;

static PENDING_WAITER_SENTINEL: PendingWaiterSentinel = PendingWaiterSentinel;

fn pending_waiter() -> *mut IrqWaitNode {
    ptr::from_ref(&PENDING_WAITER_SENTINEL).cast_mut().cast()
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

/// Test-only direct wake injection for deterministic in-flight notification
/// coverage.
#[cfg(test)]
#[derive(Clone, Copy)]
#[repr(C)]
struct IrqWakeHandle {
    data: usize,
    wake: unsafe fn(usize),
}

#[cfg(test)]
impl IrqWakeHandle {
    /// Creates a direct hard-IRQ wake capability.
    ///
    /// # Safety
    ///
    /// `wake(data)` must remain valid for the registration lifetime. It must be
    /// concurrency-safe, non-blocking, allocation-free, and must not invoke user
    /// code or scan a wait queue.
    const unsafe fn from_raw(data: usize, wake: unsafe fn(usize)) -> Self {
        Self { data, wake }
    }

    fn wake(self) {
        unsafe {
            // Construction requires this fixed runtime operation to remain valid
            // and hard-IRQ-safe for the registration lifetime.
            (self.wake)(self.data);
        }
    }
}

#[cfg(test)]
impl core::fmt::Debug for IrqWakeHandle {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("IrqWakeHandle")
            .field("data", &self.data)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
enum IrqWaitWake {
    Thread(ThreadWakeHandle),
    #[cfg(test)]
    Test(IrqWakeHandle),
}

impl IrqWaitWake {
    fn wake(&self) {
        match self {
            Self::Thread(wake) => {
                let _result = wake.wake();
            }
            #[cfg(test)]
            Self::Test(wake) => wake.wake(),
        }
    }
}

/// Pinned storage published to one [`IrqWaitCell`].
#[derive(Debug)]
struct IrqWaitNode {
    wake: IrqWaitWake,
    state: AtomicU64,
    _pin: PhantomPinned,
}

impl IrqWaitNode {
    fn new(wake: IrqWaitWake) -> Self {
        Self {
            wake,
            state: AtomicU64::new(registration_state(0, RegistrationPhase::Detached)),
            _pin: PhantomPinned,
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

    fn finish_notification(&self, generation: u64) {
        self.state
            .compare_exchange(
                registration_state(generation, RegistrationPhase::Notifying),
                registration_state(generation, RegistrationPhase::Draining),
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

    fn is_detached(&self) -> bool {
        registration_phase(self.state.load(Ordering::Acquire)) == RegistrationPhase::Detached
    }
}

/// Owned, pinned one-shot registration for a fixed scheduler thread.
///
/// The registration owns the direct wake capability; callers cannot publish an
/// arbitrary hard-IRQ callback or separately release its payload. Dropping a
/// correctly detached registration releases that ownership. If a caller
/// abandons a published token or unfinished drain, drop intentionally leaks the
/// pinned allocation so a later IRQ cannot dereference freed storage.
#[derive(Debug)]
pub struct IrqWaitRegistration {
    node: ManuallyDrop<Pin<Box<IrqWaitNode>>>,
}

impl IrqWaitRegistration {
    /// Creates a detached registration reusable across one-shot waits.
    pub fn new(wake: ThreadWakeHandle) -> Self {
        Self::from_wake(IrqWaitWake::Thread(wake))
    }

    #[cfg(test)]
    fn new_test(wake: IrqWakeHandle) -> Self {
        Self::from_wake(IrqWaitWake::Test(wake))
    }

    fn from_wake(wake: IrqWaitWake) -> Self {
        Self {
            node: ManuallyDrop::new(Box::pin(IrqWaitNode::new(wake))),
        }
    }

    fn node(&self) -> Pin<&IrqWaitNode> {
        self.node.as_ref()
    }
}

impl Drop for IrqWaitRegistration {
    fn drop(&mut self) {
        let detached = self.node().is_detached();
        debug_assert!(
            detached,
            "an IRQ wait registration was dropped before token quiescence"
        );
        if !detached {
            return;
        }
        unsafe {
            // SAFETY: detached means no cell or in-flight notifier can retain
            // the pinned node. Otherwise the early return intentionally leaks
            // the ManuallyDrop allocation as the safe failure mode.
            ManuallyDrop::drop(&mut self.node);
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
pub struct IrqWaitToken<'cell, 'registration> {
    registration: Pin<&'registration IrqWaitNode>,
    generation: u64,
    cell: &'cell IrqWaitCell,
}

impl<'cell, 'registration> IrqWaitToken<'cell, 'registration> {
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
    /// the returned drain observes that notifier until its direct wake has
    /// finished. Task context may poll the drain; hard-IRQ teardown must defer
    /// it to a task worker.
    pub fn detach(self) -> IrqWaitDrain<'registration> {
        let cell = self.cell;
        cell.detach(self)
    }

    fn belongs_to(&self, cell: &IrqWaitCell) -> bool {
        ptr::eq(self.cell, cell)
    }
}

impl core::fmt::Debug for IrqWaitToken<'_, '_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("IrqWaitToken")
            .field("generation", &self.generation)
            .field("attached", &self.is_attached())
            .finish()
    }
}

/// Revoked IRQ registration waiting for its in-flight notifier grace period.
///
/// A drain no longer retains the cell: publication admission for its generation
/// is already closed. It only retains the pinned registration until the hard
/// IRQ reader has left the trusted direct-wake operation.
#[must_use = "an IRQ wait drain must finish before registration storage is reused"]
pub struct IrqWaitDrain<'registration> {
    registration: Pin<&'registration IrqWaitNode>,
    generation: u64,
}

impl IrqWaitDrain<'_> {
    /// Returns whether the notifier grace period has completed.
    pub fn is_quiescent(&self) -> bool {
        self.registration.is_quiescent(self.generation)
    }

    /// Consumes a completed drain, or returns it while an IRQ wake is in flight.
    ///
    /// This method never waits. Task-context callers may yield and retry; hard
    /// IRQ paths must defer reclamation instead.
    pub fn try_finish(self) -> Result<(), Self> {
        if self.registration.finish_drain(self.generation) {
            Ok(())
        } else {
            Err(self)
        }
    }
}

impl core::fmt::Debug for IrqWaitDrain<'_> {
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
pub enum IrqRegisterResult<'cell, 'registration> {
    /// The cell owns the sole waiter until notify or unregister.
    Registered(IrqWaitToken<'cell, 'registration>),
    /// An earlier or concurrent interrupt consumed the registration.
    ///
    /// An earlier pending event is returned synchronously without waking the
    /// currently running task. The registration is detached on return.
    ConsumedPending,
    /// A concurrent notifier owns the registration and will release and wake it.
    ///
    /// The task may abort its park once the token is detached, but it must
    /// quiesce the token before reusing the registration or wake payload.
    NotificationInFlight(IrqWaitToken<'cell, 'registration>),
    /// Another waiter is registered, or this registration is still draining.
    Occupied,
}

/// Outcome of one bounded hard-IRQ notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqNotifyResult {
    /// One stable direct waiter was removed and woken.
    Notified,
    /// No waiter was present; one coalesced pending bit was published.
    Pending,
}

/// Pending-bit plus single-waiter hard-IRQ event cell.
#[derive(Debug)]
pub struct IrqWaitCell {
    waiter: AtomicPtr<IrqWaitNode>,
    #[cfg(test)]
    register_published: AtomicBool,
    #[cfg(test)]
    pause_after_register_publish: AtomicBool,
    #[cfg(test)]
    detach_generation_checked: AtomicBool,
    #[cfg(test)]
    pause_after_detach_generation_check: AtomicBool,
}

impl IrqWaitCell {
    /// Creates an empty notification cell.
    pub const fn new() -> Self {
        Self {
            waiter: AtomicPtr::new(ptr::null_mut()),
            #[cfg(test)]
            register_published: AtomicBool::new(false),
            #[cfg(test)]
            pause_after_register_publish: AtomicBool::new(false),
            #[cfg(test)]
            detach_generation_checked: AtomicBool::new(false),
            #[cfg(test)]
            pause_after_detach_generation_check: AtomicBool::new(false),
        }
    }

    /// Registers one stable waiter, consuming an earlier IRQ when present.
    pub fn register<'cell, 'registration>(
        &'cell self,
        registration: &'registration IrqWaitRegistration,
    ) -> IrqRegisterResult<'cell, 'registration> {
        let registration = registration.node();
        let Some(generation) = registration.reserve() else {
            return IrqRegisterResult::Occupied;
        };
        let token = IrqWaitToken {
            registration,
            generation,
            cell: self,
        };
        let registration_ptr = registration.get_ref() as *const IrqWaitNode as *mut _;
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
                        registration.cancel(generation);
                        return IrqRegisterResult::ConsumedPending;
                    }
                    Err(current) => {
                        observed = current;
                        continue;
                    }
                }
            }
            if !observed.is_null() {
                registration.cancel(generation);
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

        #[cfg(test)]
        {
            self.register_published.store(true, Ordering::Release);
            while self.pause_after_register_publish.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }

        if self.waiter.load(Ordering::Acquire) == registration_ptr {
            IrqRegisterResult::Registered(token)
        } else {
            // A concurrent notifier already owns and will wake the registration.
            IrqRegisterResult::NotificationInFlight(token)
        }
    }

    fn detach<'registration>(
        &self,
        token: IrqWaitToken<'_, 'registration>,
    ) -> IrqWaitDrain<'registration> {
        assert!(
            token.belongs_to(self),
            "an IRQ wait token must be detached by its publishing cell"
        );
        let registration = token.registration;
        let state = registration.state.load(Ordering::Acquire);
        if registration_generation(state) == token.generation {
            #[cfg(test)]
            {
                self.detach_generation_checked
                    .store(true, Ordering::Release);
                while self
                    .pause_after_detach_generation_check
                    .load(Ordering::Acquire)
                {
                    core::hint::spin_loop();
                }
            }
            let registration_ptr = registration.get_ref() as *const IrqWaitNode as *mut _;
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
        let pending = pending_waiter();
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
                ptr::null_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(waiter) => {
                    unsafe {
                        // The cell owns one pinned registration until the
                        // successful transition removes it. The pending
                        // sentinel is handled before this branch and is never
                        // dereferenced.
                        Self::notify_registration(&*waiter);
                    }
                    return IrqNotifyResult::Notified;
                }
                Err(current) => observed = current,
            }
        }

        // Hard IRQ work remains wait-free under pathological cross-CPU churn.
        // Keeping the sentinel after displacing a waiter may cause one
        // task-context recheck, but it cannot lose the notification.
        let waiter = self.waiter.swap(pending, Ordering::AcqRel);
        if waiter.is_null() || waiter == pending {
            return IrqNotifyResult::Pending;
        }
        unsafe {
            // The swap owns the displaced pinned registration. The pending
            // sentinel remains installed and is never dereferenced.
            Self::notify_registration(&*waiter);
        }
        IrqNotifyResult::Notified
    }

    /// Reports whether an IRQ is coalesced for the next registration.
    pub fn is_pending(&self) -> bool {
        self.waiter.load(Ordering::Acquire) == pending_waiter()
    }

    fn notify_registration(registration: &IrqWaitNode) {
        let generation = registration.begin_notification();
        registration.wake.wake();
        registration.finish_notification(generation);
    }
}

impl Default for IrqWaitCell {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "irq_wait_tests.rs"]
mod tests;
