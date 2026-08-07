use core::{
    cell::UnsafeCell,
    marker::PhantomPinned,
    pin::Pin,
    ptr,
    sync::atomic::{AtomicPtr, AtomicU8, Ordering},
};

const CALL_NEW: u8 = 0;
const CALL_QUEUED: u8 = 1;
const CALL_EXECUTING: u8 = 2;
const CALL_CANCELLED: u8 = 3;
const CALL_COMPLETE: u8 = 4;

/// One caller-owned, pinned cross-CPU hard-call request.
///
/// The target hard-IRQ handler owns neither the node nor its argument. It only
/// invokes the immutable raw thunk and publishes completion back to the caller.
pub(crate) struct HardCall {
    next: AtomicPtr<Self>,
    state: AtomicU8,
    operation: unsafe fn(*mut ()),
    argument: *mut (),
    _pin: PhantomPinned,
}

impl HardCall {
    pub(crate) const fn new(operation: unsafe fn(*mut ()), argument: *mut ()) -> Self {
        Self {
            next: AtomicPtr::new(ptr::null_mut()),
            state: AtomicU8::new(CALL_NEW),
            operation,
            argument,
            _pin: PhantomPinned,
        }
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.state.load(Ordering::Acquire) == CALL_COMPLETE
    }

    pub(crate) fn wait(&self) {
        while !self.is_complete() {
            core::hint::spin_loop();
        }
    }

    fn cancel(&self) -> bool {
        self.state
            .compare_exchange(
                CALL_QUEUED,
                CALL_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    unsafe fn execute(request: *mut Self) {
        // SAFETY: publishers keep every request pinned until CALL_COMPLETE and
        // the single consumer removes each pointer at most once.
        let request = unsafe { &*request };
        match request.state.compare_exchange(
            CALL_QUEUED,
            CALL_EXECUTING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // SAFETY: call_on_cpu requires the raw argument and operation
                // to remain valid and hard-IRQ safe until completion.
                unsafe { (request.operation)(request.argument) };
                request.state.store(CALL_COMPLETE, Ordering::Release);
            }
            Err(CALL_CANCELLED) => request.state.store(CALL_COMPLETE, Ordering::Release),
            Err(state) => panic!("invalid hard-call state while draining: {state}"),
        }
    }
}

// SAFETY: remotely mutable fields are atomic. The immutable thunk and argument
// are published through the queue Release/Acquire edge, and the caller retains
// ownership until completion.
unsafe impl Send for HardCall {}
// SAFETY: see the Send implementation; no non-atomic mutable alias is exposed.
unsafe impl Sync for HardCall {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HardCallDrain {
    pub(crate) completed: usize,
    pub(crate) more_work: bool,
}

/// Per-CPU MPSC transport with one non-reentrant owner-CPU consumer.
pub(crate) struct HardCallQueue {
    head: AtomicPtr<HardCall>,
    pending: UnsafeCell<*mut HardCall>,
}

impl HardCallQueue {
    pub(crate) const fn new() -> Self {
        Self {
            head: AtomicPtr::new(ptr::null_mut()),
            pending: UnsafeCell::new(ptr::null_mut()),
        }
    }

    /// Publishes one pinned request and reports whether the producer list was
    /// empty before publication.
    ///
    /// # Safety
    ///
    /// `call` must remain pinned and alive until it becomes complete and must
    /// be published exactly once.
    pub(crate) unsafe fn publish(&self, call: Pin<&HardCall>) -> bool {
        let call = call.get_ref();
        call.state
            .compare_exchange(CALL_NEW, CALL_QUEUED, Ordering::Relaxed, Ordering::Relaxed)
            .expect("hard-call request must be published exactly once");
        let call_ptr = ptr::from_ref(call).cast_mut();
        let mut observed = self.head.load(Ordering::Acquire);
        loop {
            call.next.store(observed, Ordering::Relaxed);
            match self.head.compare_exchange_weak(
                observed,
                call_ptr,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return observed.is_null(),
                Err(actual) => observed = actual,
            }
        }
    }

    /// Cancels a request after its physical notification failed.
    ///
    /// Returns `true` when the operation was cancelled and therefore must be
    /// reported as an error. If execution already started, this waits through
    /// the normal completion path and returns `false`.
    pub(crate) fn cancel_after_delivery_error(&self, call: Pin<&HardCall>) -> bool {
        let call = call.get_ref();
        let cancelled = call.cancel();
        if cancelled {
            self.complete_cancelled_head();
        }
        call.wait();
        cancelled
    }

    pub(crate) fn drain(&self, budget: usize) -> HardCallDrain {
        let mut detached = unsafe {
            // SAFETY: only the owner CPU drains this queue with local IRQ
            // re-entry excluded, so pending has exactly one consumer.
            self.pending.get().replace(ptr::null_mut())
        };
        if detached.is_null() {
            detached = reverse_list(self.head.swap(ptr::null_mut(), Ordering::Acquire));
        }

        let mut completed = 0;
        while completed < budget && !detached.is_null() {
            let call = detached;
            // SAFETY: the detached FIFO list belongs exclusively to this
            // consumer until every node is completed or saved as remainder.
            detached = unsafe { (*call).next.load(Ordering::Relaxed) };
            // SAFETY: see above.
            unsafe { HardCall::execute(call) };
            completed += 1;
        }
        unsafe {
            // SAFETY: only the owner CPU mutates the local remainder.
            self.pending.get().write(detached);
        }
        HardCallDrain {
            completed,
            more_work: !detached.is_null() || !self.head.load(Ordering::Acquire).is_null(),
        }
    }

    fn complete_cancelled_head(&self) {
        let mut observed = self.head.load(Ordering::Acquire);
        loop {
            if observed.is_null() {
                return;
            }
            // SAFETY: an observed head remains alive while it is QUEUED or
            // CANCELLED because its caller cannot return before completion.
            let request = unsafe { &*observed };
            if request.state.load(Ordering::Acquire) != CALL_CANCELLED {
                return;
            }
            let next = request.next.load(Ordering::Relaxed);
            match self.head.compare_exchange_weak(
                observed,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    request.state.store(CALL_COMPLETE, Ordering::Release);
                    observed = next;
                }
                Err(actual) => observed = actual,
            }
        }
    }
}

// SAFETY: producers access only head. pending is confined to the target CPU's
// non-reentrant hard-IRQ consumer.
unsafe impl Sync for HardCallQueue {}
// SAFETY: moving an unshared queue during BSP endpoint-table construction is
// harmless. Once shared, access follows the same producer/owner rules as Sync.
unsafe impl Send for HardCallQueue {}

fn reverse_list(mut head: *mut HardCall) -> *mut HardCall {
    let mut reversed = ptr::null_mut();
    while !head.is_null() {
        // SAFETY: head was detached from the producer list and is exclusively
        // traversed by the owner CPU.
        let next = unsafe { (*head).next.load(Ordering::Relaxed) };
        // SAFETY: rewriting next restores FIFO order in the detached list.
        unsafe { (*head).next.store(reversed, Ordering::Relaxed) };
        reversed = head;
        head = next;
    }
    reversed
}

impl Default for HardCallQueue {
    fn default() -> Self {
        Self::new()
    }
}
