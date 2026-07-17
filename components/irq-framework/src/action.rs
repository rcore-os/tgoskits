use core::{
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering},
};

use crate::{
    BoxedIrqHandler, ConcurrentBoxedIrqHandler, CpuId, CpuMask, IrqContext, IrqDrainWake, IrqError,
    IrqExecution, IrqRequest, IrqReturn, IrqScope, types::IrqHandler,
};

const ENABLED_BIT: usize = 1 << (usize::BITS - 1);
const ACTIVE_MASK: usize = !ENABLED_BIT;

struct QuenchState {
    global: AtomicBool,
    cpu_low: AtomicU64,
    cpu_high: AtomicU64,
}

impl QuenchState {
    const fn new() -> Self {
        Self {
            global: AtomicBool::new(false),
            cpu_low: AtomicU64::new(0),
            cpu_high: AtomicU64::new(0),
        }
    }

    fn insert_cpu(&self, cpu: CpuId) -> Result<(), IrqError> {
        let (word, bit) = match cpu.0 {
            0..64 => (&self.cpu_low, cpu.0),
            64..128 => (&self.cpu_high, cpu.0 - 64),
            _ => return Err(IrqError::InvalidCpu),
        };
        word.fetch_or(1 << bit, Ordering::Release);
        Ok(())
    }

    fn contains_cpu(&self, cpu: CpuId) -> bool {
        let (word, bit) = match cpu.0 {
            0..64 => (&self.cpu_low, cpu.0),
            64..128 => (&self.cpu_high, cpu.0 - 64),
            _ => return false,
        };
        word.load(Ordering::Acquire) & (1 << bit) != 0
    }

    fn remove_cpu(&self, cpu: CpuId) -> Result<(), IrqError> {
        let (word, bit) = match cpu.0 {
            0..64 => (&self.cpu_low, cpu.0),
            64..128 => (&self.cpu_high, cpu.0 - 64),
            _ => return Err(IrqError::InvalidCpu),
        };
        word.fetch_and(!(1 << bit), Ordering::AcqRel);
        Ok(())
    }

    fn clear(&self) {
        self.global.store(false, Ordering::Release);
        self.cpu_low.store(0, Ordering::Release);
        self.cpu_high.store(0, Ordering::Release);
    }

    fn is_empty(&self) -> bool {
        !self.global.load(Ordering::Acquire)
            && self.cpu_low.load(Ordering::Acquire) == 0
            && self.cpu_high.load(Ordering::Acquire) == 0
    }
}

pub(crate) enum ActionHandler {
    NonReentrant(UnsafeCell<BoxedIrqHandler>),
    Concurrent(ConcurrentBoxedIrqHandler),
}

unsafe impl Send for ActionHandler {}
unsafe impl Sync for ActionHandler {}

pub(crate) struct Action {
    pub(crate) id: u64,
    pub(crate) handler: ActionHandler,
    pub(crate) scope: IrqScope,
    pub(crate) execution: IrqExecution,
    gate: AtomicUsize,
    drain_epoch: AtomicU64,
    drain_wake: AtomicPtr<IrqDrainWake>,
    drain_notifying: AtomicBool,
    continuation_epoch: AtomicU64,
    continuation_active: AtomicU64,
    pub(crate) detached: AtomicBool,
    pub(crate) running: AtomicBool,
    pending_enable: UnsafeCell<CpuMask>,
    quench: QuenchState,
    pub(crate) next: *mut Action,
}

// Boxed callbacks are owned by the registered action and only called after the
// NonReentrant run guard succeeds, so the handler UnsafeCell is not mutably
// aliased by framework dispatch. `pending_enable` is read or mutated only
// while the registry metadata lock is held, except after the action has been
// detached and the caller has unique `Box<Action>` ownership. `quench` is
// atomic because dispatch must observe CPU-local quench ownership without
// taking the metadata lock.
unsafe impl Send for Action {}
unsafe impl Sync for Action {}

impl Action {
    pub(crate) fn new(id: u64, request: &mut IrqRequest) -> Self {
        let handler = match request
            .handler
            .take()
            .expect("IRQ handler was already consumed")
        {
            IrqHandler::NonReentrant(handler) => {
                ActionHandler::NonReentrant(UnsafeCell::new(handler))
            }
            IrqHandler::Concurrent(handler) => ActionHandler::Concurrent(handler),
        };
        Self {
            id,
            handler,
            scope: request.scope,
            execution: request.execution,
            // Registration publishes the requested enabled state only after
            // affinity and controller-line setup have committed.
            gate: AtomicUsize::new(0),
            drain_epoch: AtomicU64::new(0),
            drain_wake: AtomicPtr::new(ptr::null_mut()),
            drain_notifying: AtomicBool::new(false),
            continuation_epoch: AtomicU64::new(0),
            continuation_active: AtomicU64::new(0),
            detached: AtomicBool::new(false),
            running: AtomicBool::new(false),
            pending_enable: UnsafeCell::new(CpuMask::empty()),
            quench: QuenchState::new(),
            next: ptr::null_mut(),
        }
    }

    pub(crate) fn pending_enable_contains(&self, cpu: CpuId) -> bool {
        unsafe { (&*self.pending_enable.get()).contains(cpu) }
    }

    pub(crate) fn insert_pending_enable(&self, cpu: CpuId) {
        unsafe { (&mut *self.pending_enable.get()).insert(cpu) };
    }

    pub(crate) fn remove_pending_enable(&self, cpu: CpuId) {
        unsafe { (&mut *self.pending_enable.get()).remove(cpu) };
    }

    pub(crate) fn clear_pending_enable_all(&self) {
        unsafe { *self.pending_enable.get() = CpuMask::empty() };
    }

    pub(crate) fn record_quench(&self, cpu: CpuId) -> Result<(), IrqError> {
        match self.scope {
            IrqScope::Global => self.quench.global.store(true, Ordering::Release),
            IrqScope::PerCpu { cpus } if cpus.contains(cpu) => {
                self.quench.insert_cpu(cpu)?;
            }
            IrqScope::PerCpu { .. } => return Err(IrqError::InvalidCpu),
        }
        Ok(())
    }

    pub(crate) fn release_quench_all(&self) {
        self.quench.clear();
    }

    pub(crate) fn release_global_quench(&self) {
        self.quench.global.store(false, Ordering::Release);
    }

    pub(crate) fn release_cpu_quench(&self, cpu: CpuId) -> Result<(), IrqError> {
        self.quench.remove_cpu(cpu)
    }

    pub(crate) fn quench_applies(&self, cpu: Option<CpuId>) -> bool {
        match (self.scope, cpu) {
            (IrqScope::Global, None) => self.quench.global.load(Ordering::Acquire),
            (IrqScope::PerCpu { .. }, Some(cpu)) => self.quench.contains_cpu(cpu),
            _ => false,
        }
    }

    pub(crate) fn has_quench(&self) -> bool {
        !self.quench.is_empty()
    }

    pub(crate) fn begin_continuation(&self) -> Result<u64, IrqError> {
        if !matches!(self.scope, IrqScope::Global)
            || self.continuation_active.load(Ordering::Acquire) != 0
        {
            return Err(IrqError::Busy);
        }
        let epoch = self
            .continuation_epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1)
            })
            .map_err(|_| IrqError::Busy)?
            + 1;
        self.continuation_active
            .compare_exchange(0, epoch, Ordering::Release, Ordering::Acquire)
            .map_err(|_| IrqError::Busy)?;
        Ok(epoch)
    }

    pub(crate) fn finish_continuation(&self, epoch: u64) -> Result<(), IrqError> {
        self.continuation_active
            .compare_exchange(epoch, 0, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| IrqError::NotFound)
    }

    pub(crate) fn has_continuation(&self) -> bool {
        self.continuation_active.load(Ordering::Acquire) != 0
    }

    pub(crate) fn call(&self, ctx: IrqContext) -> IrqReturn {
        match &self.handler {
            ActionHandler::NonReentrant(handler) => {
                let handler = unsafe { &mut *handler.get() };
                handler(ctx)
            }
            ActionHandler::Concurrent(handler) => handler(ctx),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.gate.load(Ordering::Acquire) & ENABLED_BIT != 0
    }

    pub(crate) fn is_detachable(&self) -> bool {
        self.gate.load(Ordering::Acquire) == 0
            && self.drain_wake.load(Ordering::Acquire).is_null()
            && !self.drain_notifying.load(Ordering::Acquire)
            && !self.running.load(Ordering::Acquire)
            && !self.has_quench()
            && !self.has_continuation()
    }

    pub(crate) fn prepare_for_reattach(&mut self, id: u64) {
        debug_assert!(self.is_detachable());
        self.id = id;
        self.gate.store(0, Ordering::Release);
        self.detached.store(false, Ordering::Release);
        self.running.store(false, Ordering::Release);
        self.drain_notifying.store(false, Ordering::Release);
        self.continuation_epoch.store(0, Ordering::Release);
        self.continuation_active.store(0, Ordering::Release);
        self.clear_pending_enable_all();
        self.next = ptr::null_mut();
    }

    pub(crate) fn prepare_for_detached_storage(&mut self) {
        debug_assert!(self.is_detachable());
        self.gate.store(0, Ordering::Release);
        self.detached.store(true, Ordering::Release);
        self.running.store(false, Ordering::Release);
        self.drain_notifying.store(false, Ordering::Release);
        self.continuation_epoch.store(0, Ordering::Release);
        self.continuation_active.store(0, Ordering::Release);
        self.clear_pending_enable_all();
        self.next = ptr::null_mut();
    }

    pub(crate) fn set_enabled(&self, enabled: bool) -> Result<(), IrqError> {
        if enabled {
            if self.gate.load(Ordering::Acquire) & ACTIVE_MASK != 0
                || !self.drain_wake.load(Ordering::Acquire).is_null()
                || self.drain_notifying.load(Ordering::Acquire)
                || self.has_quench()
                || self.has_continuation()
            {
                return Err(IrqError::Busy);
            }
            self.gate.fetch_or(ENABLED_BIT, Ordering::Release);
        } else {
            self.gate.fetch_and(ACTIVE_MASK, Ordering::AcqRel);
        }
        Ok(())
    }

    pub(crate) fn begin_drain(&self, wake: &'static IrqDrainWake) -> Result<u64, IrqError> {
        if self
            .drain_notifying
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(IrqError::Busy);
        }
        if self.has_quench() || self.has_continuation() {
            self.drain_notifying.store(false, Ordering::Release);
            return Err(IrqError::Busy);
        }
        let wake = ptr::from_ref(wake).cast_mut();
        if self
            .drain_wake
            .compare_exchange(ptr::null_mut(), wake, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.drain_notifying.store(false, Ordering::Release);
            return Err(IrqError::Busy);
        }

        let previous_epoch =
            match self
                .drain_epoch
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                    epoch.checked_add(1)
                }) {
                Ok(epoch) => epoch,
                Err(_) => {
                    self.drain_wake.store(ptr::null_mut(), Ordering::Release);
                    self.drain_notifying.store(false, Ordering::Release);
                    return Err(IrqError::Busy);
                }
            };
        let epoch = previous_epoch + 1;
        self.gate.fetch_and(ACTIVE_MASK, Ordering::AcqRel);
        self.drain_notifying.store(false, Ordering::Release);
        Ok(epoch)
    }

    pub(crate) fn drain_complete(&self, epoch: u64) -> bool {
        self.drain_epoch.load(Ordering::Acquire) == epoch
            && self.gate.load(Ordering::Acquire) == 0
            && self.drain_wake.load(Ordering::Acquire).is_null()
            && !self.drain_notifying.load(Ordering::Acquire)
    }

    pub(crate) fn try_enter(&self) -> bool {
        let mut observed = self.gate.load(Ordering::Acquire);
        loop {
            if observed & ENABLED_BIT == 0 {
                return false;
            }
            let active = observed & ACTIVE_MASK;
            assert!(active != ACTIVE_MASK, "IRQ action active count overflowed");
            match self.gate.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => observed = actual,
            }
        }
    }

    pub(crate) fn leave(&self) {
        assert!(
            self.gate
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |gate| {
                    if gate & ACTIVE_MASK != 0 {
                        Some(gate - 1)
                    } else {
                        None
                    }
                })
                .is_ok(),
            "IRQ action active count underflowed"
        );
        self.signal_drain_if_ready();
    }

    pub(crate) fn signal_drain_if_ready(&self) {
        if self.gate.load(Ordering::Acquire) != 0 {
            return;
        }
        if self
            .drain_notifying
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let wake = self.drain_wake.swap(ptr::null_mut(), Ordering::AcqRel);
        if !wake.is_null() {
            unsafe {
                // SAFETY: `begin_drain` accepts only a static target and the
                // atomic swap gives this invocation unique notification
                // ownership for the drain generation.
                (&*wake).notify();
            }
        }
        self.drain_notifying.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;

    unsafe fn ignore_drain_notification(_data: usize) {}

    static NOTIFICATIONS: AtomicUsize = AtomicUsize::new(0);

    unsafe fn count_drain_notification(_data: usize) {
        NOTIFICATIONS.fetch_add(1, Ordering::SeqCst);
    }

    static ENABLE_DURING_NOTIFICATION: AtomicUsize = AtomicUsize::new(0);
    static COMPLETE_DURING_NOTIFICATION: AtomicBool = AtomicBool::new(false);

    unsafe fn try_enable_during_notification(data: usize) {
        let action = unsafe { &*(data as *const Action) };
        let result = match action.set_enabled(true) {
            Ok(()) => 1,
            Err(IrqError::Busy) => 2,
            Err(_) => 3,
        };
        ENABLE_DURING_NOTIFICATION.store(result, Ordering::SeqCst);
        COMPLETE_DURING_NOTIFICATION.store(action.drain_complete(1), Ordering::SeqCst);
    }

    // SAFETY: the callback is a no-op and accepts every integer value. Both
    // the static target and function remain valid for the complete test.
    static DRAIN_WAKE: IrqDrainWake = unsafe { IrqDrainWake::new(0, ignore_drain_notification) };

    // SAFETY: the static counter and callback remain valid for shutdown
    // lifetime, and the callback performs only a lock-free atomic increment.
    static COUNTING_DRAIN_WAKE: IrqDrainWake =
        unsafe { IrqDrainWake::new(0, count_drain_notification) };

    #[test]
    fn exhausted_drain_epoch_never_wraps_into_an_old_generation() {
        let mut request = IrqRequest::new(|_| IrqReturn::Handled);
        let action = Action::new(1, &mut request);
        action.drain_epoch.store(u64::MAX, Ordering::Release);

        assert_eq!(action.begin_drain(&DRAIN_WAKE), Err(IrqError::Busy));
        assert_eq!(action.drain_epoch.load(Ordering::Acquire), u64::MAX);
    }

    #[test]
    fn metadata_transition_never_invokes_drain_notification() {
        NOTIFICATIONS.store(0, Ordering::SeqCst);
        let mut request = IrqRequest::new(|_| IrqReturn::Handled);
        let action = Action::new(1, &mut request);

        action.begin_drain(&COUNTING_DRAIN_WAKE).unwrap();
        action.set_enabled(false).unwrap();
        assert_eq!(NOTIFICATIONS.load(Ordering::SeqCst), 0);

        action.signal_drain_if_ready();
        assert_eq!(NOTIFICATIONS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn action_stays_disabled_until_drain_notification_returns() {
        ENABLE_DURING_NOTIFICATION.store(0, Ordering::SeqCst);
        COMPLETE_DURING_NOTIFICATION.store(true, Ordering::SeqCst);
        let mut request = IrqRequest::new(|_| IrqReturn::Handled);
        let action = Box::leak(Box::new(Action::new(1, &mut request)));
        let wake = Box::leak(Box::new(unsafe {
            // SAFETY: both objects are leaked for shutdown lifetime. The
            // callback performs only one action-state check and atomic store.
            IrqDrainWake::new(
                action as *const Action as usize,
                try_enable_during_notification,
            )
        }));

        action.begin_drain(wake).unwrap();
        action.signal_drain_if_ready();

        assert_eq!(ENABLE_DURING_NOTIFICATION.load(Ordering::SeqCst), 2);
        assert!(!COMPLETE_DURING_NOTIFICATION.load(Ordering::SeqCst));
        assert!(action.drain_complete(1));
        assert!(!action.enabled());
    }
}
