//! Interrupt request (IRQ) handling.

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_kernel_guard::BaseGuard;
pub use irq_framework::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, AutoEnable, BoxedIrqHandler,
    CpuId, CpuMask, HwIrq, IrqAffinity, IrqContext, IrqDomainId, IrqError, IrqExecution, IrqHandle,
    IrqId, IrqOps, IrqOutcome, IrqRequest, IrqReturn, IrqScope, IrqSource, IrqStatus,
    RawIrqHandler, Registry, ShareMode, TrapVector,
};
use spin::Once;

#[cfg(target_arch = "loongarch64")]
pub mod loongarch64_hv;
#[cfg(target_arch = "loongarch64")]
pub use loongarch64_hv::LoongArchHvIrqIf;

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

/// Raw synchronous cross-CPU call used by the IRQ registry.
pub type RunOnCpuSync = unsafe fn(usize, unsafe fn(*mut ()), *mut ()) -> Result<(), IrqError>;

static RUN_ON_CPU_SYNC: AtomicUsize = AtomicUsize::new(0);

/// Installs the runtime-provided synchronous cross-CPU call implementation.
pub fn set_run_on_cpu_sync(run_on_cpu_sync: RunOnCpuSync) {
    RUN_ON_CPU_SYNC.store(run_on_cpu_sync as usize, Ordering::Release);
}

/// Runs a raw thunk synchronously on the requested CPU.
///
/// This is the generic owner-CPU execution bridge used by device runtimes that
/// must keep register access on one non-reentrant CPU context.
///
/// # Safety
///
/// `arg` must stay valid until this function returns, and `f` must be safe to
/// execute in the target CPU's IRQ/IPI context.
pub unsafe fn run_on_cpu_sync(
    cpu: CpuId,
    f: unsafe fn(*mut ()),
    arg: *mut (),
) -> Result<(), IrqError> {
    PlatIrqOps.run_on_cpu_sync(cpu, f, arg)
}

struct PlatIrqOps;

impl IrqOps for PlatIrqOps {
    type LocalIrqState = <ax_kernel_guard::IrqSave as BaseGuard>::State;

    fn current_cpu(&self) -> CpuId {
        CpuId(crate::percpu::this_cpu_id())
    }

    fn cpu_online(&self, cpu: CpuId) -> bool {
        cpu.0 < usize::BITS as usize
            && (ONLINE_CPUS.load(Ordering::Acquire) & (1usize << cpu.0)) != 0
    }

    fn in_irq_context(&self) -> bool {
        IN_IRQ_CONTEXT.with_current(|in_irq| *in_irq)
    }

    fn local_irq_save(&self) -> Self::LocalIrqState {
        ax_kernel_guard::IrqSave::acquire()
    }

    fn local_irq_restore(&self, state: Self::LocalIrqState) {
        ax_kernel_guard::IrqSave::release(state);
    }

    fn run_on_cpu_sync(
        &self,
        cpu: CpuId,
        f: unsafe fn(*mut ()),
        arg: *mut (),
    ) -> Result<(), IrqError> {
        if cpu == self.current_cpu() {
            unsafe { f(arg) };
            Ok(())
        } else {
            let run_on_cpu_sync = RUN_ON_CPU_SYNC.load(Ordering::Acquire);
            if run_on_cpu_sync == 0 {
                return Err(IrqError::Unsupported);
            }
            let run_on_cpu_sync =
                unsafe { core::mem::transmute::<usize, RunOnCpuSync>(run_on_cpu_sync) };
            unsafe { run_on_cpu_sync(cpu.0, f, arg) }
        }
    }

    fn set_enabled(&self, irq: IrqId, _cpu: Option<CpuId>, enabled: bool) -> Result<(), IrqError> {
        set_enable(irq, enabled)
    }

    fn set_affinity(&self, irq: IrqId, affinity: IrqAffinity) -> Result<(), IrqError> {
        set_affinity(irq, affinity)
    }

    fn is_enabled(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn is_pending(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn is_in_service(&self, _irq: IrqId, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn relax(&self) {
        core::hint::spin_loop();
    }
}

static IRQ_REGISTRY: Once<Registry<PlatIrqOps>> = Once::new();
static ONLINE_CPUS: AtomicUsize = AtomicUsize::new(0);

#[ax_percpu::def_percpu]
static IN_IRQ_CONTEXT: bool = false;

fn registry() -> &'static Registry<PlatIrqOps> {
    IRQ_REGISTRY.call_once(|| Registry::new(PlatIrqOps))
}

/// Requests an IRQ action through the dynamic IRQ framework.
pub fn request_irq(irq: IrqId, request: IrqRequest) -> Result<IrqHandle, IrqError> {
    let auto_enable = request.auto_enable_mode();
    let handle = registry().request(irq, request)?;
    if auto_enable == AutoEnable::Yes
        && let Err(err) = registry().enable(handle)
    {
        let _ = registry().free(handle);
        return Err(err);
    }
    Ok(handle)
}

/// Requests a shared IRQ action.
pub fn request_shared_irq(
    irq: IrqId,
    handler: RawIrqHandler,
    data: core::ptr::NonNull<()>,
) -> Result<IrqHandle, IrqError> {
    request_irq(
        irq,
        IrqRequest::new(handler, data).share_mode(ShareMode::Shared),
    )
}

/// Requests a boxed IRQ action.
pub fn request_boxed_irq(irq: IrqId, request: IrqRequest) -> Result<IrqHandle, IrqError> {
    request_irq(irq, request)
}

/// Requests a boxed shared IRQ action.
pub fn request_boxed_shared_irq(
    irq: IrqId,
    handler: BoxedIrqHandler,
) -> Result<IrqHandle, IrqError> {
    request_irq(
        irq,
        IrqRequest::new_boxed(handler).share_mode(ShareMode::Shared),
    )
}

/// Requests a per-CPU IRQ action.
pub fn request_percpu_irq(
    irq: IrqId,
    cpus: CpuMask,
    handler: RawIrqHandler,
    data: core::ptr::NonNull<()>,
) -> Result<IrqHandle, IrqError> {
    request_irq(
        irq,
        IrqRequest::new(handler, data).scope(IrqScope::PerCpu { cpus }),
    )
}

/// Frees an IRQ action.
pub fn free_irq(handle: IrqHandle) -> Result<(), IrqError> {
    registry().free(handle)
}

/// Enables an IRQ action.
pub fn enable_irq(handle: IrqHandle) -> Result<(), IrqError> {
    registry().enable(handle)
}

/// Disables an IRQ action.
pub fn disable_irq(handle: IrqHandle) -> Result<(), IrqError> {
    registry().disable(handle)
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

/// Dispatches actions registered in the dynamic IRQ framework.
pub fn dispatch_irq(irq: IrqId) -> IrqOutcome {
    let cpu = CpuId(crate::percpu::this_cpu_id());
    IN_IRQ_CONTEXT.with_current(|in_irq| {
        let was_in_irq = *in_irq;
        *in_irq = true;
        let outcome = registry().dispatch(irq, cpu);
        *in_irq = was_in_irq;
        outcome
    })
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

/// Target specification for inter-processor interrupts (IPIs).
pub enum IpiTarget {
    /// Send to the current CPU.
    Current {
        /// The CPU ID of the current CPU.
        cpu_id: usize,
    },
    /// Send to a specific CPU.
    Other {
        /// The CPU ID of the target CPU.
        cpu_id: usize,
    },
    /// Send to all other CPUs.
    AllExceptCurrent {
        /// The CPU ID of the current CPU.
        cpu_id: usize,
        /// The total number of CPUs.
        cpu_num: usize,
    },
}

/// IRQ management interface.
#[def_plat_interface]
pub trait IrqIf {
    /// Enables or disables the given IRQ.
    fn set_enable(irq: IrqId, enabled: bool) -> Result<(), IrqError>;

    /// Routes a global IRQ to a fixed CPU when supported.
    fn set_affinity(irq: IrqId, affinity: IrqAffinity) -> Result<(), IrqError>;

    /// Handles the IRQ.
    ///
    /// It is called by the common interrupt handler. Platform implementations
    /// should claim/ack the controller interrupt, dispatch the real IRQ through
    /// [`dispatch_irq`], and perform the matching EOI/complete operation.
    ///
    /// Returns the "real" IRQ number. On some platforms, this may differ from
    /// the input `irq` number, for example on AArch64 the input `irq` is
    /// ignored and the real IRQ number is obtained from the GIC. Returns
    /// `None` if the IRQ is spurious.
    fn handle(vector: TrapVector) -> Option<IrqId>;

    /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
    fn send_ipi(irq_num: IrqId, target: IpiTarget);

    /// Returns the platform IRQ id used for runtime IPIs.
    fn ipi_irq() -> IrqId;

    /// Resolves a firmware/controller interrupt source to a framework IRQ id.
    fn resolve_source(source: IrqSource) -> Result<IrqId, IrqError>;

    /// Resolves an architecture-local/per-CPU hardware interrupt.
    fn resolve_percpu(hwirq: HwIrq) -> Result<IrqId, IrqError>;
}

#[cfg(test)]
mod tests {
    use core::{
        ptr::NonNull,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::impl_plat_interface;

    static ENABLE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static FAIL_ENABLE: AtomicUsize = AtomicUsize::new(0);

    struct TestIrqIf;

    #[impl_plat_interface]
    impl IrqIf for TestIrqIf {
        fn set_enable(_irq: IrqId, _enabled: bool) -> Result<(), IrqError> {
            ENABLE_CALLS.fetch_add(1, Ordering::Relaxed);
            if FAIL_ENABLE.load(Ordering::Relaxed) != 0 {
                return Err(IrqError::Controller);
            }
            Ok(())
        }

        fn set_affinity(_irq: IrqId, _affinity: IrqAffinity) -> Result<(), IrqError> {
            Err(IrqError::Unsupported)
        }

        fn handle(_vector: TrapVector) -> Option<IrqId> {
            None
        }

        fn send_ipi(_irq_num: IrqId, _target: IpiTarget) {}

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

    unsafe fn test_irq_handler(_ctx: IrqContext, _data: NonNull<()>) -> IrqReturn {
        IrqReturn::Handled
    }

    #[test]
    fn request_irq_auto_enable_no_does_not_enable_line() {
        let irq = IrqId::new(IrqDomainId(0xff), HwIrq(1));
        let request =
            IrqRequest::new(test_irq_handler, NonNull::dangling()).auto_enable(AutoEnable::No);

        ENABLE_CALLS.store(0, Ordering::Relaxed);
        let handle = request_irq(irq, request).unwrap();

        assert_eq!(ENABLE_CALLS.load(Ordering::Relaxed), 0);
        assert_eq!(irq_status(handle).unwrap().action_enabled, false);

        free_irq(handle).unwrap();
    }

    #[test]
    fn request_irq_rolls_back_action_when_auto_enable_fails() {
        let irq = IrqId::new(IrqDomainId(0xff), HwIrq(2));
        let request = || IrqRequest::new(test_irq_handler, NonNull::dangling());

        ENABLE_CALLS.store(0, Ordering::Relaxed);
        FAIL_ENABLE.store(1, Ordering::Relaxed);
        let err = request_irq(irq, request()).unwrap_err();

        assert_eq!(err, IrqError::Controller);
        assert_eq!(ENABLE_CALLS.load(Ordering::Relaxed), 1);

        FAIL_ENABLE.store(0, Ordering::Relaxed);
        let handle = request_irq(irq, request()).unwrap();
        assert_eq!(irq_status(handle).unwrap().action_enabled, true);

        free_irq(handle).unwrap();
    }
}
