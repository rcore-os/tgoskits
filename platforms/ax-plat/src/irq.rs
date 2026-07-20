//! Interrupt request (IRQ) handling.

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_kspin::{IrqGuard, PreemptGuard};
pub use irq_framework::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, AutoEnable, BoxedIrqHandler,
    CpuId, CpuIpiTarget, CpuMask, DetachedIrqAction, HwIrq, IpiSendStatus, IrqAffinity, IrqContext,
    IrqDomainId, IrqDrainToken, IrqDrainWake, IrqError, IrqExecution, IrqHandle, IrqId,
    IrqLineBinding, IrqLineControl, IrqOps, IrqOutcome, IrqRequest, IrqReturn, IrqScope, IrqSource,
    IrqStatus, PreparedIrqLine, ReattachIrqActionError, Registry, ReleasedIrqLineProof, ShareMode,
    TrapVector,
};
use spin::Once;

#[cfg(target_arch = "riscv64")]
pub mod riscv64_hv;
#[cfg(target_arch = "riscv64")]
pub use riscv64_hv::Riscv64HvIrqIf;

/// Compatibility IRQ domain used while non-domainized platforms migrate.
pub const LEGACY_IRQ_DOMAIN: IrqDomainId = IrqDomainId(0);

/// CPU-local interrupt domain for architecture trap causes such as timers/IPIs.
pub const CPU_LOCAL_IRQ_DOMAIN: IrqDomainId = IrqDomainId(u16::MAX);

/// x86 local APIC interrupt domain.
pub const X86_LAPIC_DOMAIN: IrqDomainId = IrqDomainId(1);

/// x86 I/O APIC interrupt domain.
pub const X86_IOAPIC_DOMAIN: IrqDomainId = IrqDomainId(2);

/// AArch64 GIC interrupt domain.
pub const AARCH64_GIC_DOMAIN: IrqDomainId = IrqDomainId(3);

/// RISC-V PLIC interrupt domain.
pub const RISCV_PLIC_DOMAIN: IrqDomainId = IrqDomainId(4);

/// LoongArch EIOINTC interrupt domain.
pub const LOONGARCH_EIOINTC_DOMAIN: IrqDomainId = IrqDomainId(5);

/// LoongArch PCH-PIC interrupt domain.
pub const LOONGARCH_PCH_PIC_DOMAIN: IrqDomainId = IrqDomainId(6);

/// Creates a legacy IRQ id without truncating the raw IRQ number.
pub fn try_legacy_irq(raw: usize) -> Result<IrqId, IrqError> {
    let hwirq = u32::try_from(raw).map_err(|_| IrqError::InvalidIrq)?;
    Ok(IrqId::new(LEGACY_IRQ_DOMAIN, HwIrq(hwirq)))
}

/// Compatibility constructor for legacy numeric IRQ users.
pub fn legacy_irq(raw: usize) -> Result<IrqId, IrqError> {
    try_legacy_irq(raw)
}

/// Returns the legacy raw IRQ number when this id is in the legacy domain.
pub const fn legacy_irq_raw(irq: IrqId) -> Option<usize> {
    if irq.domain.0 == LEGACY_IRQ_DOMAIN.0 {
        Some(irq.hwirq.0 as usize)
    } else {
        None
    }
}

/// Legacy constructor kept only for upper-layer compatibility.
#[allow(non_snake_case)]
pub fn IrqNumber(raw: usize) -> Result<IrqId, IrqError> {
    legacy_irq(raw)
}

/// Raw synchronous cross-CPU call used after the adapter proves the target is remote.
pub type RunOnCpuSync = unsafe fn(usize, unsafe fn(*mut ()), *mut ()) -> Result<(), IrqError>;

static RUN_ON_CPU_SYNC: AtomicUsize = AtomicUsize::new(0);

/// Installs the runtime-provided synchronous cross-CPU call implementation.
///
/// Reinstalling the same function is idempotent. Installing a different
/// implementation after the first successful call is a fatal initialization
/// error because an IRQ operation may already be executing through the old
/// function pointer.
///
/// # Safety
///
/// The installed function must execute `f(arg)` synchronously on exactly the
/// requested logical CPU with local IRQs disabled. `Ok(())` means the callback
/// completed exactly once. Every error means the callback never began and can
/// never begin later; neither `f` nor `arg` may be retained. The
/// implementation and all state it uses must remain valid until shutdown.
/// Installation must complete before IRQ consumers become reachable.
///
/// # Panics
///
/// Panics if a different implementation was installed previously.
pub unsafe fn set_run_on_cpu_sync(run_on_cpu_sync: RunOnCpuSync) {
    assert!(
        crate::install_runtime_hook_once(&RUN_ON_CPU_SYNC, run_on_cpu_sync as *const () as usize,),
        "attempted to replace the synchronous cross-CPU call implementation"
    );
}

/// Runs a raw thunk synchronously on the requested CPU.
///
/// This is the generic owner-CPU execution bridge used by device runtimes that
/// must keep register access on one non-reentrant CPU context.
/// Local thunks run with local IRQs disabled. Remote calls from IRQ context are
/// rejected with [`IrqError::InIrqContext`].
///
/// # Safety
///
/// `arg` must stay valid until this function returns. `f` must not block and
/// must be safe to execute with local IRQs disabled at the target CPU's
/// IRQ-return safe point.
pub unsafe fn run_on_cpu_sync(
    cpu: CpuId,
    f: unsafe fn(*mut ()),
    arg: *mut (),
) -> Result<(), IrqError> {
    PlatIrqOps.run_on_cpu_sync(cpu, f, arg)
}

struct PlatIrqOps;

// SAFETY: Local thunks run inline under the routing pin. The remote bridge is a
// synchronous rendezvous and guarantees that no callback can execute after it
// returns, including timeout/error paths. PlatIrqOps has no mutable instance
// state and is safe to share between CPUs.
unsafe impl IrqOps for PlatIrqOps {
    type LocalIrqState = IrqGuard;

    fn current_cpu(&self) -> CpuId {
        CpuId(crate::percpu::this_cpu_id())
    }

    fn cpu_online(&self, cpu: CpuId) -> bool {
        cpu.0 < usize::BITS as usize
            && (ONLINE_CPUS.load(Ordering::Acquire) & (1usize << cpu.0)) != 0
    }

    fn in_irq_context(&self) -> bool {
        current_cpu_in_irq_context()
    }

    fn local_irq_save(&self) -> Self::LocalIrqState {
        IrqGuard::new()
    }

    fn local_irq_restore(&self, state: Self::LocalIrqState) {
        drop(state);
    }

    fn run_on_cpu_sync(
        &self,
        cpu: CpuId,
        f: unsafe fn(*mut ()),
        arg: *mut (),
    ) -> Result<(), IrqError> {
        let route_guard = PreemptGuard::new();
        let irq_guard = IrqGuard::new();
        let current_cpu = CpuId(crate::percpu::this_cpu_id_pinned(irq_guard.cpu_pin()));

        if cpu == current_cpu {
            unsafe { f(arg) };
            drop(irq_guard);
            drop(route_guard);
            return Ok(());
        }
        if in_irq_context_on(current_cpu) {
            return Err(IrqError::InIrqContext);
        }
        drop(irq_guard);

        let run_on_cpu_sync = RUN_ON_CPU_SYNC.load(Ordering::Acquire);
        if run_on_cpu_sync == 0 {
            return Err(IrqError::Unsupported);
        }
        let run_on_cpu_sync =
            unsafe { core::mem::transmute::<usize, RunOnCpuSync>(run_on_cpu_sync) };
        let result = unsafe { run_on_cpu_sync(cpu.0, f, arg) };
        drop(route_guard);
        result
    }

    fn prepare_line(
        &self,
        irq: IrqId,
        scope: IrqScope,
        affinity: IrqAffinity,
    ) -> Result<PreparedIrqLine, IrqError> {
        ax_crate_interface::call_interface!(IrqIf::prepare_line, irq, scope, affinity)
    }

    fn set_line_enabled(&self, binding: IrqLineBinding, cpu: Option<CpuId>, enabled: bool) {
        ax_crate_interface::call_interface!(IrqIf::set_line_enabled, binding, cpu, enabled)
    }

    fn release_line(&self, binding: IrqLineBinding) -> Result<(), IrqError> {
        ax_crate_interface::call_interface!(IrqIf::release_line, binding)
    }

    fn relax(&self) {
        core::hint::spin_loop();
    }
}

static IRQ_REGISTRY: Once<Registry<PlatIrqOps>> = Once::new();
static ONLINE_CPUS: AtomicUsize = AtomicUsize::new(0);
static IRQ_CONTEXT_CPUS: AtomicUsize = AtomicUsize::new(0);

fn registry() -> &'static Registry<PlatIrqOps> {
    IRQ_REGISTRY.call_once(|| Registry::new(PlatIrqOps))
}

/// Returns whether the current CPU is dispatching an IRQ action.
pub fn in_irq_context() -> bool {
    current_cpu_in_irq_context()
}

fn current_cpu_in_irq_context() -> bool {
    let guard = IrqGuard::new();
    let cpu = CpuId(crate::percpu::this_cpu_id_pinned(guard.cpu_pin()));
    let result = in_irq_context_on(cpu);
    drop(guard);
    result
}

/// Requests an IRQ action through the dynamic IRQ framework.
pub fn request_irq(irq: IrqId, request: IrqRequest) -> Result<IrqHandle, IrqError> {
    registry().request(irq, request)
}

fn request_enabled_irq(irq: IrqId, request: IrqRequest) -> Result<IrqHandle, IrqError> {
    debug_assert_eq!(request.auto_enable_mode(), AutoEnable::No);
    let handle = registry().request(irq, request)?;
    if let Err(error) = registry().enable(handle) {
        if let Err(rollback_error) = registry().free(handle) {
            panic!(
                "failed to roll back IRQ {irq:?} after enable error {error:?}: {rollback_error:?}"
            );
        }
        return Err(error);
    }
    Ok(handle)
}

/// Requests and enables a shared IRQ action.
pub fn request_shared_irq(
    irq: IrqId,
    handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
) -> Result<IrqHandle, IrqError> {
    let request = IrqRequest::new(handler)
        .share_mode(ShareMode::Shared)
        .auto_enable(AutoEnable::No);
    request_enabled_irq(irq, request)
}

/// Requests and enables a per-CPU IRQ action.
pub fn request_percpu_irq(
    irq: IrqId,
    cpus: CpuMask,
    handler: impl Fn(IrqContext) -> IrqReturn + Send + Sync + 'static,
) -> Result<IrqHandle, IrqError> {
    let request = IrqRequest::new_concurrent(handler)
        .scope(IrqScope::PerCpu { cpus })
        .auto_enable(AutoEnable::No);
    request_enabled_irq(irq, request)
}

/// Frees an IRQ action.
pub fn free_irq(handle: IrqHandle) -> Result<(), IrqError> {
    registry().free(handle)
}

/// Removes a disabled, drained action while retaining its handler ownership.
///
/// # Errors
///
/// Returns an IRQ lifecycle error when the handle is stale, the action is not
/// drained, or the caller is in hard-IRQ context.
pub fn detach_irq_action(handle: IrqHandle) -> Result<DetachedIrqAction, IrqError> {
    registry().detach_action(handle)
}

/// Detaches the sole disabled action and releases its controller line.
///
/// # Errors
///
/// Returns an IRQ lifecycle or platform release error. Every failure leaves
/// the original handle, action, and prepared binding usable.
pub fn detach_irq_action_and_release_line(
    handle: IrqHandle,
) -> Result<(DetachedIrqAction, ReleasedIrqLineProof), IrqError> {
    registry().detach_action_and_release_line(handle)
}

/// Re-registers a detached action under a fresh, disabled handle.
///
/// # Errors
///
/// Returns an error that retains the action when descriptor policy, CPU state,
/// or the interrupt controller prevents registration.
pub fn reattach_irq_action(action: DetachedIrqAction) -> Result<IrqHandle, ReattachIrqActionError> {
    registry().reattach_action(action)
}

/// Enables an IRQ action.
pub fn enable_irq(handle: IrqHandle) -> Result<(), IrqError> {
    registry().enable(handle)
}

/// Disables an IRQ action.
pub fn disable_irq(handle: IrqHandle) -> Result<(), IrqError> {
    registry().disable(handle)
}

/// Acquires fail-closed backing-line containment for an action.
///
/// This task-context operation is used when device activation cannot prove
/// that its exact interrupt source is masked. Recovery must establish that
/// proof before calling [`release_irq_quench`].
pub fn quench_irq(handle: IrqHandle) -> Result<(), IrqError> {
    registry().quench(handle)
}

/// Releases an action-owned emergency line quench after its device source is masked.
///
/// The action remains enabled. On a shared descriptor, every action regains the
/// backing line only after every quench owner has released it.
pub fn release_irq_quench(handle: IrqHandle) -> Result<(), IrqError> {
    registry().release_quench(handle)
}

/// Disables one action and wakes a fixed target after only that action drains.
pub fn disable_irq_async(
    handle: IrqHandle,
    wake: &'static IrqDrainWake,
) -> Result<IrqDrainToken, IrqError> {
    registry().disable_async(handle, wake)
}

/// Checks a generation-bearing action-specific drain token without waiting.
pub fn irq_action_drain_complete(token: IrqDrainToken) -> Result<bool, IrqError> {
    registry().action_drain_complete(token)
}

/// Waits until no handler for this IRQ descriptor is in flight.
pub fn synchronize_irq(handle: IrqHandle) -> Result<(), IrqError> {
    registry().synchronize(handle)
}

/// Returns the status of an IRQ action.
pub fn irq_status(handle: IrqHandle) -> Result<IrqStatus, IrqError> {
    registry().status(handle)
}

/// Marks a CPU online for pending per-CPU IRQ enables.
pub fn cpu_online(cpu: usize) -> Result<(), IrqError> {
    if cpu >= usize::BITS as usize {
        return Err(IrqError::InvalidCpu);
    }
    ONLINE_CPUS.fetch_or(1usize << cpu, Ordering::AcqRel);
    registry().cpu_online(CpuId(cpu))
}

/// Prepares CPU-local runtime state before the common IRQ guard is entered.
pub fn prepare_irq_context(vector: TrapVector) {
    ax_crate_interface::call_interface!(IrqIf::prepare, vector)
}

/// Dispatches a claimed IRQ and completes the platform controller claim.
///
/// `complete_claim` is executed after every shared action returns and before
/// the framework may reopen a fail-closed line. It must perform the matching EOI
/// without allocation, blocking, arbitrary callbacks, or panic.
pub fn dispatch_claimed_irq_on(
    irq: IrqId,
    cpu: CpuId,
    complete_claim: impl FnOnce(),
) -> IrqOutcome {
    let context_bit = irq_context_bit(cpu);
    let was_in_irq = context_bit
        .map(|bit| IRQ_CONTEXT_CPUS.fetch_or(bit, Ordering::AcqRel) & bit != 0)
        .unwrap_or(false);
    let outcome = registry().dispatch(irq, cpu, complete_claim);
    if let Some(bit) = context_bit
        && !was_in_irq
    {
        IRQ_CONTEXT_CPUS.fetch_and(!bit, Ordering::AcqRel);
    }
    outcome
}

fn in_irq_context_on(cpu: CpuId) -> bool {
    irq_context_bit(cpu)
        .map(|bit| IRQ_CONTEXT_CPUS.load(Ordering::Acquire) & bit != 0)
        .unwrap_or(false)
}

fn irq_context_bit(cpu: CpuId) -> Option<usize> {
    (cpu.0 < usize::BITS as usize).then_some(1usize << cpu.0)
}

/// Resolves a firmware/controller interrupt source to a framework IRQ id.
pub fn resolve_irq_source(source: IrqSource) -> Result<IrqId, IrqError> {
    resolve_source(source)
}

/// Resolves an architecture-local/per-CPU hardware interrupt through the
/// platform IRQ domain.
pub fn resolve_percpu_irq(hwirq: HwIrq) -> Result<IrqId, IrqError> {
    resolve_percpu(hwirq)
}

/// IRQ management interface.
#[def_plat_interface]
pub trait IrqIf {
    /// Prepares CPU-local runtime state before the common IRQ handler touches
    /// per-CPU runtime data.
    fn prepare(vector: TrapVector);

    /// Initializes boot-time IRQ controller domains before runtime IRQ handlers
    /// are registered.
    fn init_boot_irqs(cpu_id: usize) -> Result<(), IrqError>;

    /// Initializes early IRQ state for a secondary CPU.
    #[cfg(feature = "smp")]
    fn init_secondary_boot_irqs(cpu_id: usize) -> Result<(), IrqError>;

    /// Resolves and validates a stable IRQ-chip line capability.
    fn prepare_line(
        irq: IrqId,
        scope: IrqScope,
        affinity: IrqAffinity,
    ) -> Result<PreparedIrqLine, IrqError>;

    /// Applies an infallible live transition to a prepared IRQ-chip line.
    fn set_line_enabled(binding: IrqLineBinding, cpu: Option<CpuId>, enabled: bool);

    /// Releases one exact prepared IRQ-chip line generation.
    fn release_line(binding: IrqLineBinding) -> Result<(), IrqError>;

    /// Handles the IRQ.
    ///
    /// It is called by the common interrupt handler. Platform implementations
    /// should claim/ack the controller interrupt, then move the active claim
    /// into [`dispatch_claimed_irq_on`]'s completion closure. This makes EOI a
    /// framework-owned boundary: a masked line cannot reopen between shared
    /// handler return and controller completion.
    ///
    /// Returns the "real" IRQ number. On some platforms, this may differ from
    /// the input `irq` number, for example on AArch64 the input `irq` is
    /// ignored and the real IRQ number is obtained from the GIC. Returns
    /// `None` if the IRQ is spurious.
    fn handle(vector: TrapVector) -> Option<IrqId>;

    /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
    ///
    /// The IRQ guard keeps the caller on one logical CPU and excludes a nested
    /// local sender while implementations validate the target and commit the
    /// controller transaction. This is required by split xAPIC ICR writes.
    fn send_ipi(
        irq_num: IrqId,
        target: CpuIpiTarget,
        irq_guard: &ax_kspin::IrqGuard,
    ) -> IpiSendStatus;

    /// Returns the platform IRQ id used for runtime IPIs.
    fn ipi_irq() -> IrqId;

    /// Resolves a firmware/controller interrupt source to a framework IRQ id.
    fn resolve_source(source: IrqSource) -> Result<IrqId, IrqError>;

    /// Resolves an architecture-local/per-CPU hardware interrupt.
    fn resolve_percpu(hwirq: HwIrq) -> Result<IrqId, IrqError>;
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;
    use crate::impl_plat_interface;

    const TEST_IRQ_COUNT: usize = 6;
    const NO_FAILING_IRQ: usize = usize::MAX;

    static ENABLE_CALLS: [AtomicUsize; TEST_IRQ_COUNT] =
        [const { AtomicUsize::new(0) }; TEST_IRQ_COUNT];
    static FAIL_PREPARE_IRQ: AtomicUsize = AtomicUsize::new(NO_FAILING_IRQ);
    static TIMER_HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);
    static IPI_HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);
    static TEST_CPU_ZERO: Mutex<()> = Mutex::new(());

    fn enable_calls(irq: IrqId) -> usize {
        ENABLE_CALLS[irq.hwirq.0 as usize].load(Ordering::Relaxed)
    }

    fn reset_enable_calls(irq: IrqId) {
        ENABLE_CALLS[irq.hwirq.0 as usize].store(0, Ordering::Relaxed);
    }

    struct TestIrqIf;

    #[impl_plat_interface]
    impl IrqIf for TestIrqIf {
        fn prepare(_vector: TrapVector) {}

        fn init_boot_irqs(_cpu_id: usize) -> Result<(), IrqError> {
            Ok(())
        }

        #[cfg(feature = "smp")]
        fn init_secondary_boot_irqs(_cpu_id: usize) -> Result<(), IrqError> {
            Ok(())
        }

        fn prepare_line(
            irq: IrqId,
            _scope: IrqScope,
            _affinity: IrqAffinity,
        ) -> Result<PreparedIrqLine, IrqError> {
            if FAIL_PREPARE_IRQ.load(Ordering::Relaxed) == irq.hwirq.0 as usize {
                return Err(IrqError::Controller);
            }
            IrqLineBinding::new(irq.hwirq.0, 1)
                .map(|binding| PreparedIrqLine::new(binding, IrqLineControl::Maskable))
                .ok_or(IrqError::InvalidIrq)
        }

        fn set_line_enabled(binding: IrqLineBinding, _cpu: Option<CpuId>, enabled: bool) {
            if enabled {
                ENABLE_CALLS[binding.slot() as usize].fetch_add(1, Ordering::Relaxed);
            }
        }

        fn release_line(_binding: IrqLineBinding) -> Result<(), IrqError> {
            Err(IrqError::Unsupported)
        }

        fn handle(_vector: TrapVector) -> Option<IrqId> {
            None
        }

        fn send_ipi(
            _irq_num: IrqId,
            _target: CpuIpiTarget,
            _irq_guard: &ax_kspin::IrqGuard,
        ) -> IpiSendStatus {
            IpiSendStatus::Invalid
        }

        fn ipi_irq() -> IrqId {
            IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(0))
        }

        fn resolve_source(_source: IrqSource) -> Result<IrqId, IrqError> {
            Err(IrqError::Unsupported)
        }

        fn resolve_percpu(_hwirq: HwIrq) -> Result<IrqId, IrqError> {
            Err(IrqError::Unsupported)
        }
    }

    #[test]
    fn request_irq_auto_enable_no_does_not_enable_line() {
        let _cpu = TEST_CPU_ZERO.lock().unwrap();
        let irq = IrqId::new(IrqDomainId(0xff), HwIrq(1));
        let request = IrqRequest::new(|_| IrqReturn::Handled).auto_enable(AutoEnable::No);

        reset_enable_calls(irq);
        let handle = request_irq(irq, request).unwrap();

        assert_eq!(enable_calls(irq), 0);
        assert!(!irq_status(handle).unwrap().action_enabled);

        free_irq(handle).unwrap();
    }

    #[test]
    fn request_irq_rolls_back_action_when_line_preparation_fails() {
        let _cpu = TEST_CPU_ZERO.lock().unwrap();
        let irq = IrqId::new(IrqDomainId(0xff), HwIrq(2));
        let request = || IrqRequest::new(|_| IrqReturn::Handled).auto_enable(AutoEnable::Yes);

        reset_enable_calls(irq);
        FAIL_PREPARE_IRQ.store(irq.hwirq.0 as usize, Ordering::Relaxed);
        let err = request_irq(irq, request()).unwrap_err();

        assert_eq!(err, IrqError::Controller);
        assert_eq!(enable_calls(irq), 0);

        FAIL_PREPARE_IRQ.store(NO_FAILING_IRQ, Ordering::Relaxed);
        let handle = request_irq(irq, request()).unwrap();
        assert!(irq_status(handle).unwrap().action_enabled);

        free_irq(handle).unwrap();
    }

    #[test]
    fn detached_action_round_trips_through_the_platform_facade() {
        let _cpu = TEST_CPU_ZERO.lock().unwrap();
        let irq = IrqId::new(IrqDomainId(0xff), HwIrq(3));
        let request = IrqRequest::new(|_| IrqReturn::Handled).auto_enable(AutoEnable::No);

        FAIL_PREPARE_IRQ.store(NO_FAILING_IRQ, Ordering::Relaxed);
        let handle = request_irq(irq, request).unwrap();
        let detached = detach_irq_action(handle).unwrap();
        assert_eq!(detached.irq(), irq);

        let handle = reattach_irq_action(detached).unwrap();
        assert!(!irq_status(handle).unwrap().action_enabled);
        free_irq(handle).unwrap();
    }

    #[test]
    fn shared_convenience_request_enables_timer_action_once_before_dispatch() {
        let _cpu = TEST_CPU_ZERO.lock().unwrap();
        let irq = IrqId::new(IrqDomainId(0xff), HwIrq(4));
        reset_enable_calls(irq);
        TIMER_HANDLER_CALLS.store(0, Ordering::Relaxed);

        let handle = request_shared_irq(irq, |_| {
            TIMER_HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
            IrqReturn::Handled
        })
        .unwrap();

        assert_eq!(enable_calls(irq), 1);
        let outcome = dispatch_claimed_irq_on(irq, CpuId(0), || {});
        assert_eq!(outcome.called, 1);
        assert!(outcome.handled);
        assert_eq!(TIMER_HANDLER_CALLS.load(Ordering::Relaxed), 1);

        free_irq(handle).unwrap();
    }

    #[test]
    fn percpu_convenience_request_enables_ipi_action_once_before_dispatch() {
        let _cpu = TEST_CPU_ZERO.lock().unwrap();
        let irq = IrqId::new(IrqDomainId(0xff), HwIrq(5));
        reset_enable_calls(irq);
        IPI_HANDLER_CALLS.store(0, Ordering::Relaxed);
        cpu_online(0).unwrap();

        let handle = request_percpu_irq(irq, CpuMask::from_cpu(CpuId(0)), |_| {
            IPI_HANDLER_CALLS.fetch_add(1, Ordering::Relaxed);
            IrqReturn::Handled
        })
        .unwrap();

        assert_eq!(enable_calls(irq), 1);
        let outcome = dispatch_claimed_irq_on(irq, CpuId(0), || {});
        assert_eq!(outcome.called, 1);
        assert!(outcome.handled);
        assert_eq!(IPI_HANDLER_CALLS.load(Ordering::Relaxed), 1);

        free_irq(handle).unwrap();
    }

    #[test]
    fn claimed_dispatch_completes_an_unhandled_controller_claim_once() {
        let _cpu = TEST_CPU_ZERO.lock().unwrap();
        let irq = IrqId::new(IrqDomainId(0xff), HwIrq(6));
        let completions = AtomicUsize::new(0);

        let outcome = dispatch_claimed_irq_on(irq, CpuId(0), || {
            completions.fetch_add(1, Ordering::Relaxed);
        });

        assert_eq!(outcome.called, 0);
        assert!(!outcome.handled);
        assert_eq!(completions.load(Ordering::Relaxed), 1);
    }
}
