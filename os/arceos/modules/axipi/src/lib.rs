//! [ArceOS](https://github.com/arceos-org/arceos) Inter-Processor Interrupt (IPI) primitives.

#![cfg_attr(not(test), no_std)]

#[macro_use]
extern crate log;
extern crate alloc;

use alloc::{sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

pub use ax_hal::irq::CpuId;
use ax_hal::{
    irq::{CpuIpiTarget, IpiSendStatus},
    percpu::this_cpu_id_pinned,
};
use ax_kspin::{IrqGuard, PreemptGuard, SpinNoIrq};
use ax_lazyinit::LazyInit;
use ax_percpu::{BoundCpuPin, CpuPin};

mod event;
mod queue;

mod build_info {
    include!(concat!(env!("OUT_DIR"), "/build_info.rs"));
}

pub use event::{Callback, MulticastCallback};
use queue::{IpiEventNode, IpiEventQueue};

#[ax_percpu::def_percpu]
static IPI_EVENT_QUEUE: LazyInit<SpinNoIrq<IpiEventQueue>> = LazyInit::new();

#[ax_percpu::def_percpu]
static IPI_DEFERRED_PENDING: AtomicBool = AtomicBool::new(false);

const IPI_CPU_NOT_READY: u8 = 0;
const IPI_CPU_BECOMING_READY: u8 = 1;
const IPI_CPU_READY: u8 = 2;

static IPI_CPU_STATE: [AtomicU8; build_info::CPU_CAPACITY] =
    [const { AtomicU8::new(IPI_CPU_NOT_READY) }; build_info::CPU_CAPACITY];

static IPI_READY_CPUS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
const SYNC_IPI_SPIN_LIMIT: usize = 10_000_000;
const DEFERRED_CALLBACK_BATCH: usize = 64;
const CALLBACK_IPI_CLAIMED: u64 = 1;

static CALLBACK_IPI_EPOCH: [AtomicU64; build_info::CPU_CAPACITY] =
    [const { AtomicU64::new(0) }; build_info::CPU_CAPACITY];
static CALLBACK_IPI_RETRY: [AtomicBool; build_info::CPU_CAPACITY] =
    [const { AtomicBool::new(false) }; build_info::CPU_CAPACITY];
static CALLBACK_IPI_RETRY_COUNT: AtomicUsize = AtomicUsize::new(0);
static CALLBACK_IPI_RETRY_CURSOR: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CallbackIpiClaim {
    cpu: usize,
    epoch: u64,
}

fn claim_callback_ipi(cpu: usize) -> Option<CallbackIpiClaim> {
    let state = CALLBACK_IPI_EPOCH.get(cpu)?;
    let mut current = state.load(Ordering::Acquire);
    loop {
        if current & CALLBACK_IPI_CLAIMED != 0 {
            return None;
        }
        let next = current.wrapping_add(2) | CALLBACK_IPI_CLAIMED;
        match state.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(CallbackIpiClaim { cpu, epoch: next }),
            Err(actual) => current = actual,
        }
    }
}

fn mark_callback_ipi_retry(cpu: usize) {
    let Some(retry) = CALLBACK_IPI_RETRY.get(cpu) else {
        return;
    };
    if retry
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        CALLBACK_IPI_RETRY_COUNT.fetch_add(1, Ordering::Release);
    }
}

fn finish_callback_ipi_send(claim: CallbackIpiClaim, status: IpiSendStatus) {
    match status {
        IpiSendStatus::Success => {}
        IpiSendStatus::Retry => {
            let state = &CALLBACK_IPI_EPOCH[claim.cpu];
            if state
                .compare_exchange(
                    claim.epoch,
                    claim.epoch & !CALLBACK_IPI_CLAIMED,
                    Ordering::Release,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                mark_callback_ipi_retry(claim.cpu);
            }
        }
        IpiSendStatus::Invalid => {
            let _ = CALLBACK_IPI_EPOCH[claim.cpu].compare_exchange(
                claim.epoch,
                claim.epoch & !CALLBACK_IPI_CLAIMED,
                Ordering::Release,
                Ordering::Acquire,
            );
            panic!(
                "callback IPI platform rejected validated online CPU {}",
                claim.cpu
            );
        }
    }
}

fn send_callback_ipi_claim(claim: CallbackIpiClaim) {
    let irq_guard = IrqGuard::new();
    let current_cpu = CpuId(this_cpu_id_pinned(irq_guard.cpu_pin()));
    let status = ax_hal::irq::send_ipi(
        ax_hal::irq::ipi_irq(),
        callback_ipi_target(current_cpu, CpuId(claim.cpu)),
        &irq_guard,
    );
    finish_callback_ipi_send(claim, status);
}

fn callback_ipi_target(current: CpuId, destination: CpuId) -> CpuIpiTarget {
    if current == destination {
        CpuIpiTarget::Current { cpu: destination }
    } else {
        CpuIpiTarget::Other { cpu: destination }
    }
}

fn kick_callback_ipi(cpu: usize) {
    if let Some(claim) = claim_callback_ipi(cpu) {
        send_callback_ipi_claim(claim);
    }
}

fn acknowledge_current_callback_ipi() {
    let irq_guard = IrqGuard::new();
    let cpu = this_cpu_id_pinned(irq_guard.cpu_pin());
    let Some(state) = CALLBACK_IPI_EPOCH.get(cpu) else {
        return;
    };
    let mut current = state.load(Ordering::Acquire);
    while current & CALLBACK_IPI_CLAIMED != 0 {
        match state.compare_exchange_weak(
            current,
            current & !CALLBACK_IPI_CLAIMED,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

/// Returns whether callback work owns a persistent outbound IPI retry.
pub fn callback_ipi_retry_pending() -> bool {
    CALLBACK_IPI_RETRY_COUNT.load(Ordering::Acquire) != 0
}

/// Services at most `limit` preallocated callback-doorbell retries.
pub fn service_callback_ipi_retries(limit: usize) -> usize {
    let cpu_count = ax_hal::cpu_num().min(build_info::CPU_CAPACITY);
    if cpu_count == 0 || limit == 0 || !callback_ipi_retry_pending() {
        return 0;
    }
    let limit = limit.min(DEFERRED_CALLBACK_BATCH).min(cpu_count);
    let start = CALLBACK_IPI_RETRY_CURSOR.fetch_add(limit, Ordering::Relaxed) % cpu_count;
    let mut attempted = 0;
    for offset in 0..limit {
        let cpu = (start + offset) % cpu_count;
        if CALLBACK_IPI_RETRY[cpu].swap(false, Ordering::AcqRel) {
            CALLBACK_IPI_RETRY_COUNT.fetch_sub(1, Ordering::AcqRel);
            if let Some(claim) = claim_callback_ipi(cpu) {
                attempted += 1;
                send_callback_ipi_claim(claim);
            }
        }
    }
    attempted
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum SyncCallState {
    Queued,
    Running,
    Cancelled,
    Done,
}

// The destination owns Queued -> Running -> Done. The sole waiting caller may
// instead claim Queued -> Cancelled; after that claim succeeds, a late callback
// cannot read the caller-owned argument.
struct SyncCallLifecycle {
    state: AtomicU8,
}

impl SyncCallLifecycle {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(SyncCallState::Queued as u8),
        }
    }

    fn try_start(&self) -> bool {
        self.state
            .compare_exchange(
                SyncCallState::Queued as u8,
                SyncCallState::Running as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn finish(&self) {
        debug_assert_eq!(self.load(), SyncCallState::Running);
        self.state
            .store(SyncCallState::Done as u8, Ordering::Release);
    }

    fn wait(
        &self,
        initial_spin_limit: usize,
        mut wait_hint: impl FnMut(),
    ) -> Result<(), ax_hal::irq::IrqError> {
        for _ in 0..initial_spin_limit {
            if self.load() == SyncCallState::Done {
                return Ok(());
            }
            wait_hint();
        }

        match self.state.compare_exchange(
            SyncCallState::Queued as u8,
            SyncCallState::Cancelled as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Err(ax_hal::irq::IrqError::Timeout),
            Err(state) => match SyncCallState::from_raw(state) {
                SyncCallState::Running => {
                    while self.load() == SyncCallState::Running {
                        wait_hint();
                    }
                    debug_assert_eq!(self.load(), SyncCallState::Done);
                    Ok(())
                }
                SyncCallState::Done => Ok(()),
                SyncCallState::Queued => {
                    unreachable!("strong queued-to-cancelled CAS cannot observe queued on failure")
                }
                SyncCallState::Cancelled => {
                    unreachable!("only this waiter may cancel a synchronous IPI call")
                }
            },
        }
    }

    fn load(&self) -> SyncCallState {
        SyncCallState::from_raw(self.state.load(Ordering::Acquire))
    }
}

impl SyncCallState {
    fn from_raw(state: u8) -> Self {
        match state {
            state if state == Self::Queued as u8 => Self::Queued,
            state if state == Self::Running as u8 => Self::Running,
            state if state == Self::Cancelled as u8 => Self::Cancelled,
            state if state == Self::Done as u8 => Self::Done,
            _ => unreachable!("synchronous IPI lifecycle contains an invalid state"),
        }
    }
}

struct SyncPayload {
    function: unsafe fn(*mut ()),
    argument: usize,
}

impl SyncPayload {
    unsafe fn invoke_and_clear(mut self) {
        let function = core::mem::replace(&mut self.function, cleared_sync_payload);
        let argument = core::mem::replace(&mut self.argument, 0);
        // SAFETY: the synchronous-call contract keeps the argument valid while
        // Running. The original function and argument live only in this stack
        // frame, which returns before the lifecycle publishes Done.
        unsafe { function(argument as *mut ()) };
    }
}

unsafe fn cleared_sync_payload(_argument: *mut ()) {}

struct SyncCall {
    lifecycle: SyncCallLifecycle,
    payload: UnsafeCell<Option<SyncPayload>>,
}

// SAFETY: the payload is initialized before the SyncCall is published through
// an Arc. Moving an Arc between CPUs does not access it; exactly one successful
// Queued -> Running or Queued -> Cancelled transition later takes the payload.
unsafe impl Send for SyncCall {}

// SAFETY: the Running and Cancelled transitions are mutually exclusive. The
// destination accesses the payload only after winning Running, while the sole
// waiter accesses it only after winning Cancelled. Both take the Option, so a
// Done or returned Cancelled call retains neither the raw argument nor thunk.
unsafe impl Sync for SyncCall {}

impl SyncCall {
    fn new(function: unsafe fn(*mut ()), argument: *mut ()) -> Self {
        Self {
            lifecycle: SyncCallLifecycle::new(),
            payload: UnsafeCell::new(Some(SyncPayload {
                function,
                argument: argument as usize,
            })),
        }
    }

    fn execute(&self) {
        if !self.lifecycle.try_start() {
            return;
        }

        let payload = self.take_running_payload();
        // SAFETY: `run_on_cpu_sync_raw` keeps the argument alive until this
        // execution publishes `Done`. Winning Queued -> Running gives this
        // destination exclusive ownership. `invoke_and_clear` returns only
        // after its stack-local raw payload is dead, before Done is visible.
        unsafe { payload.invoke_and_clear() };
        self.lifecycle.finish();
    }

    fn wait(&self) -> Result<(), ax_hal::irq::IrqError> {
        self.wait_with(SYNC_IPI_SPIN_LIMIT, || {
            service_callback_ipi_retries(DEFERRED_CALLBACK_BATCH);
            core::hint::spin_loop();
        })
    }

    fn wait_with(
        &self,
        initial_spin_limit: usize,
        wait_hint: impl FnMut(),
    ) -> Result<(), ax_hal::irq::IrqError> {
        match self.lifecycle.wait(initial_spin_limit, wait_hint) {
            Err(ax_hal::irq::IrqError::Timeout) => {
                self.clear_cancelled_payload();
                Err(ax_hal::irq::IrqError::Timeout)
            }
            result => result,
        }
    }

    fn take_running_payload(&self) -> SyncPayload {
        debug_assert_eq!(self.lifecycle.load(), SyncCallState::Running);
        // SAFETY: only the destination that won Queued -> Running calls this
        // method. The waiter cannot win Queued -> Cancelled afterwards and no
        // other destination can win the same transition.
        unsafe { &mut *self.payload.get() }
            .take()
            .expect("a running synchronous IPI call must own its payload")
    }

    fn clear_cancelled_payload(&self) {
        debug_assert_eq!(self.lifecycle.load(), SyncCallState::Cancelled);
        // SAFETY: only the sole waiter that won Queued -> Cancelled calls this
        // method. A destination can no longer enter Running or read the payload.
        let payload = unsafe { &mut *self.payload.get() }.take();
        debug_assert!(payload.is_some());
    }

    #[cfg(test)]
    fn payload_is_cleared(&self) -> bool {
        debug_assert!(matches!(
            self.lifecycle.load(),
            SyncCallState::Cancelled | SyncCallState::Done
        ));
        // SAFETY: tests call this only after the exclusive transition owner has
        // completed its payload access and no concurrent executor is running.
        unsafe { &*self.payload.get() }.is_none()
    }
}

/// Initialize the per-CPU IPI event queue.
pub fn init() {
    let route_guard = PreemptGuard::new();
    let cpu_pin = bound_cpu_pin(route_guard.cpu_pin());
    IPI_EVENT_QUEUE.with_current_ref(&cpu_pin, |ipi_queue| {
        ipi_queue.init_once(SpinNoIrq::new(IpiEventQueue::default()));
    });
}

/// Marks the current CPU ready to receive and handle queued IPI callbacks.
///
/// The runtime should call this after the local IPI event queue is initialized,
/// the IPI handler is installed, and local IRQs are enabled.
pub fn mark_current_cpu_ready() {
    let route_guard = PreemptGuard::new();
    let cpu_id = this_cpu_id_pinned(route_guard.cpu_pin());
    IPI_CPU_STATE[cpu_id].store(IPI_CPU_BECOMING_READY, Ordering::Release);
    ax_hal::asm::flush_tlb(None);
    IPI_CPU_STATE[cpu_id].store(IPI_CPU_READY, Ordering::Release);
    IPI_READY_CPUS.fetch_add(1, Ordering::Release);
}

/// Waits until every online CPU has completed [`mark_current_cpu_ready`].
pub fn wait_for_all_cpus_ready() {
    let cpu_num = ax_hal::cpu_num();
    while IPI_READY_CPUS.load(Ordering::Acquire) < cpu_num {
        core::hint::spin_loop();
    }
}

/// Returns whether `cpu_id` is ready to receive and handle queued IPI callbacks.
pub fn is_cpu_ready(cpu_id: usize) -> bool {
    cpu_id < build_info::CPU_CAPACITY
        && IPI_CPU_STATE[cpu_id].load(Ordering::Acquire) == IPI_CPU_READY
}

/// Waits while `cpu_id` is becoming ready, and returns whether it is ready.
///
/// If a page-table update races with a CPU publishing IPI readiness, the caller
/// must not skip the CPU after it has already started its final local TLB flush.
/// Waiting for the transition to complete lets the caller send a conservative
/// follow-up IPI after the CPU can receive callbacks.
pub fn wait_until_cpu_ready(cpu_id: usize) -> bool {
    if cpu_id >= build_info::CPU_CAPACITY {
        return false;
    }

    loop {
        match IPI_CPU_STATE[cpu_id].load(Ordering::Acquire) {
            IPI_CPU_READY => return true,
            IPI_CPU_NOT_READY => return false,
            _ => core::hint::spin_loop(),
        }
    }
}

/// Executes a callback on the specified destination CPU via IPI.
///
/// Local and remote callbacks execute with local IRQs disabled.
/// They must not block, allocate, fault, or acquire non-IRQ-safe locks.
///
/// # Errors
///
/// Returns an IRQ error for an invalid/offline destination or a hard-IRQ
/// caller. Validation completes before this function takes callback ownership.
///
/// # Panics
///
/// Panics if the platform rejects a destination after the runtime published it
/// as online and callback-ready, because the queued callback cannot be rolled
/// back without racing the destination consumer.
pub fn run_on_cpu<T: Into<Callback>>(
    dest_cpu: CpuId,
    callback: T,
) -> Result<(), ax_hal::irq::IrqError> {
    validate_callback_routing_context()?;
    let dest_cpu = validate_callback_destination(dest_cpu)?;
    debug!("Send IPI event to CPU {dest_cpu}");
    // Callback erasure and node allocation must finish before CPU pinning,
    // IRQ masking, or destination queue locking begins.
    let mut node = IpiEventNode::prepare(callback.into());
    let route_guard = PreemptGuard::new();
    let irq_guard = IrqGuard::new();
    let current_cpu = this_cpu_id_pinned(irq_guard.cpu_pin());
    if dest_cpu == current_cpu {
        let callback = node.take_callback();
        callback.call();
        drop(irq_guard);
        drop(route_guard);
        drop(node);
        return Ok(());
    }
    drop(irq_guard);

    let remote_queue = unsafe { IPI_EVENT_QUEUE.remote_ref_raw(cpu_index(dest_cpu)) }
        .map_err(|_| ax_hal::irq::IrqError::CpuOffline)?;
    let remote_queue = remote_queue
        .get()
        .ok_or(ax_hal::irq::IrqError::CpuOffline)?;
    remote_queue.lock().push(current_cpu, node);
    drop(route_guard);
    kick_callback_ipi(dest_cpu);
    service_callback_ipi_retries(DEFERRED_CALLBACK_BATCH);
    Ok(())
}

/// Executes a raw thunk synchronously on the specified CPU via IPI.
///
/// # Errors
///
/// Returns an IRQ error for invalid/offline destinations, hard-IRQ callers, or
/// a destination that does not acknowledge the call before the bounded wait
/// expires.
///
/// # Safety
///
/// `arg` must remain valid until this function returns. `f` executes with local
/// IRQs disabled and must not block, allocate, fault, or acquire non-IRQ-safe locks.
pub unsafe fn run_on_cpu_sync_raw(
    dest_cpu: CpuId,
    f: unsafe fn(*mut ()),
    arg: *mut (),
) -> Result<(), ax_hal::irq::IrqError> {
    if ax_hal::irq::in_irq_context() {
        return Err(ax_hal::irq::IrqError::InIrqContext);
    }
    let dest_cpu = validate_callback_destination(dest_cpu)?;
    let route_guard = PreemptGuard::new();
    let irq_guard = IrqGuard::new();
    if dest_cpu == this_cpu_id_pinned(irq_guard.cpu_pin()) {
        unsafe { f(arg) };
        drop(irq_guard);
        drop(route_guard);
        return Ok(());
    }
    drop(irq_guard);
    drop(route_guard);

    let call = Arc::new(SyncCall::new(f, arg));
    let remote_call = Arc::clone(&call);
    run_on_cpu(CpuId(dest_cpu), move || {
        remote_call.execute();
    })?;
    call.wait()
}

/// Executes a callback on all other CPUs via IPI.
///
/// Local and remote callbacks execute with local IRQs disabled.
/// They must not block, allocate, fault, or acquire non-IRQ-safe locks.
///
/// # Errors
///
/// Returns an IRQ error before publication when called from hard IRQ or when
/// any configured destination is not callback-ready.
///
/// # Panics
///
/// Panics if the platform rejects a destination after the runtime published it
/// as online and callback-ready.
pub fn run_on_each_cpu<T: Into<MulticastCallback>>(
    callback: T,
) -> Result<(), ax_hal::irq::IrqError> {
    validate_callback_routing_context()?;
    info!("Send IPI event to all other CPUs");
    let cpu_num = ax_hal::cpu_num();
    for cpu_id in 0..cpu_num {
        validate_callback_destination(CpuId(cpu_id))?;
    }
    // Allocate one node for every possible destination before pinning. The
    // node for the eventual local CPU is reused for the local invocation.
    let callback = callback.into();
    let mut nodes = Vec::with_capacity(cpu_num);
    for _ in 0..cpu_num {
        nodes.push(IpiEventNode::prepare(callback.clone().into_unicast()));
    }
    drop(callback);
    // Allocate the destination table before acquiring the migration pin. Its
    // exact capacity guarantees that filling it below cannot reallocate.
    let mut destinations: Vec<Option<&SpinNoIrq<IpiEventQueue>>> = Vec::with_capacity(cpu_num);

    let route_guard = PreemptGuard::new();
    let irq_guard = IrqGuard::new();
    let current_cpu_id = this_cpu_id_pinned(irq_guard.cpu_pin());
    drop(irq_guard);

    // Preflight every remote queue before publishing the first node.
    for cpu_id in 0..cpu_num {
        if cpu_id == current_cpu_id {
            destinations.push(None);
        } else {
            let remote_queue = unsafe { IPI_EVENT_QUEUE.remote_ref_raw(cpu_index(cpu_id)) }
                .map_err(|_| ax_hal::irq::IrqError::CpuOffline)?;
            let remote_queue = remote_queue
                .get()
                .ok_or(ax_hal::irq::IrqError::CpuOffline)?;
            destinations.push(Some(remote_queue));
        }
    }

    // Preemption keeps the source CPU stable across multicast publication.
    // Each SpinNoIrq queue acquisition and each hardware kick owns only a
    // short IRQ-disabled section, bounding local interrupt latency by one
    // destination rather than the complete CPU set.
    let mut local_node = None;
    for (cpu_id, (node, destination)) in nodes.into_iter().zip(destinations).enumerate() {
        match destination {
            Some(remote_queue) => remote_queue.lock().push(current_cpu_id, node),
            None => {
                debug_assert_eq!(cpu_id, current_cpu_id);
                local_node = Some(node);
            }
        }
    }
    // Each destination owns an independent generation and persistent retry;
    // this avoids one partial broadcast failure stranding unrelated queues.
    for cpu_id in 0..cpu_num {
        if cpu_id != current_cpu_id {
            kick_callback_ipi(cpu_id);
        }
    }
    service_callback_ipi_retries(DEFERRED_CALLBACK_BATCH);
    let mut local_node = local_node.expect("current CPU must be inside the runtime CPU set");
    let callback = local_node.take_callback();
    let callback_irq_guard = IrqGuard::new();
    callback.call();
    drop(callback_irq_guard);
    drop(route_guard);
    drop(local_node);
    Ok(())
}

/// Publishes pending IPI work from the hard-IRQ handler.
///
/// This entry point neither removes events from the queue nor invokes or drops
/// callbacks. The runtime must call [`drain_deferred_callbacks`] after the IRQ
/// framework has cleared its hard-IRQ marker.
pub fn ipi_handler() {
    // Any IPI arriving after callback publication is an equivalent doorbell:
    // it forces this CPU through the deferred IRQ-return drain. Generation CAS
    // prevents a stale sender completion from clearing a newer epoch.
    acknowledge_current_callback_ipi();
    mark_deferred_pending();
}

/// Executes one bounded batch of callbacks at the IRQ-return safe point.
///
/// Local IRQs must remain disabled and the caller must have left the IRQ
/// framework's hard-IRQ marker. Residual callbacks remain explicitly pending
/// for the next real IRQ-return safe point. The consumer must not raise an
/// immediate self-IPI: doing so would turn a bounded batch into an unbounded
/// high-priority interrupt chain and could starve device IRQs.
///
/// # Panics
///
/// Panics if called with local IRQs enabled or while the IRQ framework still
/// reports hard-IRQ context.
pub fn drain_deferred_callbacks() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "deferred IPI callbacks require local IRQs disabled"
    );
    service_callback_ipi_retries(DEFERRED_CALLBACK_BATCH);
    if !take_deferred_pending() {
        return;
    }
    assert!(
        !ax_hal::irq::in_irq_context(),
        "deferred IPI callbacks cannot run inside the hard-IRQ marker"
    );

    if execute_callback_batch(DEFERRED_CALLBACK_BATCH, pop_deferred_callback)
        < DEFERRED_CALLBACK_BATCH
    {
        return;
    }

    if deferred_callbacks_remain() {
        mark_deferred_pending();
    }
}

fn execute_callback_batch(
    limit: usize,
    mut pop: impl FnMut() -> Option<(usize, Callback)>,
) -> usize {
    let mut executed = 0;
    while executed < limit {
        let Some((src_cpu_id, callback)) = pop() else {
            break;
        };
        debug!("Received IPI event from CPU {src_cpu_id}");
        callback.call();
        executed += 1;
    }
    executed
}

fn mark_deferred_pending() {
    let irq_guard = ax_kspin::IrqGuard::new();
    let cpu_pin = bound_cpu_pin(irq_guard.cpu_pin());
    IPI_DEFERRED_PENDING.with_current_ref(&cpu_pin, mark_deferred_pending_flag);
}

fn mark_deferred_pending_flag(pending: &AtomicBool) {
    pending.store(true, Ordering::Release);
}

fn take_deferred_pending() -> bool {
    let irq_guard = IrqGuard::new();
    let cpu_pin = bound_cpu_pin(irq_guard.cpu_pin());
    IPI_DEFERRED_PENDING.with_current_ref(&cpu_pin, |pending| pending.swap(false, Ordering::AcqRel))
}

fn pop_deferred_callback() -> Option<(usize, Callback)> {
    let irq_guard = IrqGuard::new();
    let cpu_pin = bound_cpu_pin(irq_guard.cpu_pin());
    let node = IPI_EVENT_QUEUE.with_current_ref(&cpu_pin, |ipi_queue| ipi_queue.lock().pop_node());
    drop(irq_guard);
    node.map(|mut node| node.take_parts())
}

fn deferred_callbacks_remain() -> bool {
    let irq_guard = IrqGuard::new();
    let cpu_pin = bound_cpu_pin(irq_guard.cpu_pin());
    IPI_EVENT_QUEUE.with_current_ref(&cpu_pin, |ipi_queue| !ipi_queue.lock().is_empty())
}

fn bound_cpu_pin(pin: &CpuPin) -> BoundCpuPin<'_> {
    ax_percpu::bound_current(pin).expect("IPI access requires a bound CPU-local area")
}

fn validate_callback_routing_context() -> Result<(), ax_hal::irq::IrqError> {
    if ax_hal::irq::in_irq_context() {
        Err(ax_hal::irq::IrqError::InIrqContext)
    } else {
        Ok(())
    }
}

fn validate_callback_destination(dest_cpu: CpuId) -> Result<usize, ax_hal::irq::IrqError> {
    let cpu_count = ax_hal::cpu_num();
    let ready = dest_cpu.0 < cpu_count && is_cpu_ready(dest_cpu.0);
    validate_callback_destination_state(dest_cpu, cpu_count, ready)
}

fn validate_callback_destination_state(
    dest_cpu: CpuId,
    cpu_count: usize,
    ready: bool,
) -> Result<usize, ax_hal::irq::IrqError> {
    if dest_cpu.0 >= cpu_count {
        return Err(ax_hal::irq::IrqError::InvalidCpu);
    }
    if !ready {
        return Err(ax_hal::irq::IrqError::CpuOffline);
    }
    Ok(dest_cpu.0)
}

fn cpu_index(cpu_id: usize) -> ax_percpu::CpuIndex {
    ax_percpu::CpuIndex::try_from(cpu_id).expect("logical CPU ID must fit the CPU-local ABI")
}

#[cfg(test)]
mod test_lock_runtime {
    use ax_kspin::{LockRuntime, LockdepEvent, impl_trait};

    struct TestLockRuntime;

    impl_trait! {
        impl LockRuntime for TestLockRuntime {
            fn irq_enter() {}
            fn irq_exit() {}
            fn preempt_enter() {}
            fn preempt_exit() {}
            unsafe fn preempt_exit_irq_return() {}
            fn current_thread_id() -> u64 { 1 }
            fn lockdep_acquire(_event: LockdepEvent) {}
            fn lockdep_release(_event: LockdepEvent) {}
            fn lockdep_set_trace_enabled(_enabled: bool) {}
            fn lockdep_dump_trace() {}
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::{cell::Cell, sync::atomic::AtomicUsize};
    use std::sync::Mutex;

    use super::*;

    static CALLBACK_IPI_STATE_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset_callback_ipi_state(cpu: usize) {
        CALLBACK_IPI_EPOCH[cpu].store(0, Ordering::Relaxed);
        CALLBACK_IPI_RETRY[cpu].store(false, Ordering::Relaxed);
        CALLBACK_IPI_RETRY_COUNT.store(0, Ordering::Relaxed);
        CALLBACK_IPI_RETRY_CURSOR.store(0, Ordering::Relaxed);
    }

    unsafe fn increment_counter(arg: *mut ()) {
        // SAFETY: each test passes a live `AtomicUsize` for the complete call.
        let counter = unsafe { &*arg.cast::<AtomicUsize>() };
        counter.fetch_add(1, Ordering::Release);
    }

    #[test]
    fn hard_irq_publication_neither_invokes_nor_drops_queued_callback() {
        struct DropProbe(Arc<AtomicUsize>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Release);
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let callback_calls = Arc::clone(&calls);
        let drop_probe = DropProbe(Arc::clone(&drops));
        let mut queue = IpiEventQueue::new();
        queue.push(
            0,
            IpiEventNode::prepare(
                (move || {
                    let _drop_probe = drop_probe;
                    callback_calls.fetch_add(1, Ordering::Release);
                })
                .into(),
            ),
        );
        let pending = AtomicBool::new(false);

        mark_deferred_pending_flag(&pending);

        assert!(pending.load(Ordering::Acquire));
        assert!(!queue.is_empty());
        assert_eq!(calls.load(Ordering::Acquire), 0);
        assert_eq!(drops.load(Ordering::Acquire), 0);

        let (_, callback) = queue
            .pop_node()
            .map(|mut node| node.take_parts())
            .expect("queued callback");
        callback.call();
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn queue_publication_reuses_the_caller_allocated_node() {
        let mut queue = IpiEventQueue::new();
        let node = IpiEventNode::prepare((|| {}).into());
        let allocation = (&*node) as *const IpiEventNode;

        queue.push(7, node);
        let mut node = queue.pop_node().expect("published callback node");

        assert_eq!((&*node) as *const IpiEventNode, allocation);
        assert_eq!(node.take_parts().0, 7);
    }

    #[test]
    fn cancelled_sync_call_skips_a_late_remote_callback() {
        let calls = AtomicUsize::new(0);
        let arg = (&calls as *const AtomicUsize).cast_mut().cast();
        let call = SyncCall::new(increment_counter, arg);

        assert_eq!(
            call.wait_with(0, core::hint::spin_loop),
            Err(ax_hal::irq::IrqError::Timeout)
        );
        assert!(call.payload_is_cleared());
        call.execute();

        assert_eq!(calls.load(Ordering::Acquire), 0);
    }

    #[test]
    fn running_sync_call_waits_for_done_instead_of_timing_out() {
        let lifecycle = SyncCallLifecycle::new();
        assert!(lifecycle.try_start());
        let wait_hints = Cell::new(0);

        let result = lifecycle.wait(0, || {
            wait_hints.set(wait_hints.get() + 1);
            lifecycle.finish();
        });

        assert_eq!(result, Ok(()));
        assert_eq!(wait_hints.get(), 1);
    }

    #[test]
    fn completed_sync_call_releases_its_raw_payload_before_done() {
        let calls = AtomicUsize::new(0);
        let arg = (&calls as *const AtomicUsize).cast_mut().cast();
        let call = SyncCall::new(increment_counter, arg);

        call.execute();

        assert_eq!(call.lifecycle.load(), SyncCallState::Done);
        assert!(call.payload_is_cleared());
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn sync_call_wait_returns_timeout_when_remote_cpu_does_not_complete() {
        let lifecycle = SyncCallLifecycle::new();

        assert_eq!(
            lifecycle.wait(0, core::hint::spin_loop),
            Err(ax_hal::irq::IrqError::Timeout)
        );
    }

    #[test]
    fn sync_call_wait_returns_ok_after_completion() {
        let lifecycle = SyncCallLifecycle::new();
        assert!(lifecycle.try_start());
        lifecycle.finish();

        assert_eq!(lifecycle.wait(0, core::hint::spin_loop), Ok(()));
    }

    #[test]
    fn safe_callback_api_rejects_invalid_or_offline_cpu() {
        assert_eq!(
            validate_callback_destination_state(CpuId(usize::MAX), 4, false),
            Err(ax_hal::irq::IrqError::InvalidCpu)
        );
        assert_eq!(
            validate_callback_destination_state(CpuId(3), 4, false),
            Err(ax_hal::irq::IrqError::CpuOffline)
        );
        assert_eq!(
            validate_callback_destination_state(CpuId(3), 4, true),
            Ok(3)
        );
    }

    #[test]
    fn sixty_five_callbacks_drain_as_bounded_sixty_four_plus_one() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut queue = IpiEventQueue::new();
        for _ in 0..(DEFERRED_CALLBACK_BATCH + 1) {
            let calls = Arc::clone(&calls);
            queue.push(
                0,
                IpiEventNode::prepare(
                    (move || {
                        calls.fetch_add(1, Ordering::Release);
                    })
                    .into(),
                ),
            );
        }

        assert_eq!(
            execute_callback_batch(DEFERRED_CALLBACK_BATCH, || {
                queue.pop_node().map(|mut node| node.take_parts())
            }),
            DEFERRED_CALLBACK_BATCH
        );
        assert_eq!(calls.load(Ordering::Acquire), DEFERRED_CALLBACK_BATCH);
        assert!(!queue.is_empty());

        // Residual work remains explicitly pending for a later real IRQ-return
        // safe point. The bounded consumer must not synthesize an immediate
        // high-priority self-IPI, which could starve lower-priority device IRQs.
        let pending = AtomicBool::new(false);
        mark_deferred_pending_flag(&pending);
        assert!(pending.swap(false, Ordering::AcqRel));

        assert_eq!(
            execute_callback_batch(DEFERRED_CALLBACK_BATCH, || {
                queue.pop_node().map(|mut node| node.take_parts())
            }),
            1
        );
        assert_eq!(calls.load(Ordering::Acquire), DEFERRED_CALLBACK_BATCH + 1);
        assert!(queue.is_empty());
    }

    #[test]
    fn callback_ipi_target_distinguishes_self_from_remote_destinations() {
        assert_eq!(
            callback_ipi_target(CpuId(2), CpuId(2)),
            CpuIpiTarget::Current { cpu: CpuId(2) }
        );
        assert_eq!(
            callback_ipi_target(CpuId(2), CpuId(3)),
            CpuIpiTarget::Other { cpu: CpuId(3) }
        );
    }

    #[test]
    fn callback_ipi_retry_survives_without_a_new_producer() {
        let _lock = CALLBACK_IPI_STATE_TEST_LOCK.lock().unwrap();
        let cpu = 0;
        reset_callback_ipi_state(cpu);

        let first = claim_callback_ipi(cpu).expect("first callback IPI claim");
        finish_callback_ipi_send(first, IpiSendStatus::Retry);

        assert!(CALLBACK_IPI_RETRY[cpu].load(Ordering::Acquire));
        assert_eq!(CALLBACK_IPI_RETRY_COUNT.load(Ordering::Acquire), 1);
        assert_eq!(
            CALLBACK_IPI_EPOCH[cpu].load(Ordering::Acquire) & CALLBACK_IPI_CLAIMED,
            0
        );

        // Model the bounded retry scanner without touching host IPI hardware.
        assert!(CALLBACK_IPI_RETRY[cpu].swap(false, Ordering::AcqRel));
        CALLBACK_IPI_RETRY_COUNT.fetch_sub(1, Ordering::AcqRel);
        let retry = claim_callback_ipi(cpu).expect("retry must own a fresh generation");
        assert_ne!(retry.epoch, first.epoch);
        assert_eq!(CALLBACK_IPI_RETRY_COUNT.load(Ordering::Acquire), 0);

        reset_callback_ipi_state(cpu);
    }

    #[test]
    fn stale_callback_ipi_failure_cannot_clear_a_new_generation() {
        let _lock = CALLBACK_IPI_STATE_TEST_LOCK.lock().unwrap();
        let cpu = 0;
        reset_callback_ipi_state(cpu);

        let stale = claim_callback_ipi(cpu).expect("stale callback IPI claim");
        CALLBACK_IPI_EPOCH[cpu]
            .compare_exchange(
                stale.epoch,
                stale.epoch & !CALLBACK_IPI_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("model target acknowledgement");
        let current = claim_callback_ipi(cpu).expect("new callback IPI generation");

        finish_callback_ipi_send(stale, IpiSendStatus::Retry);

        assert_eq!(
            CALLBACK_IPI_EPOCH[cpu].load(Ordering::Acquire),
            current.epoch
        );
        assert!(!CALLBACK_IPI_RETRY[cpu].load(Ordering::Acquire));
        assert_eq!(CALLBACK_IPI_RETRY_COUNT.load(Ordering::Acquire), 0);

        reset_callback_ipi_state(cpu);
    }
}
