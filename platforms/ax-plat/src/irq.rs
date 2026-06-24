//! Interrupt request (IRQ) handling.

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_kernel_guard::BaseGuard;
pub use irq_framework::{
    AutoEnable, CpuId, CpuMask, IrqAffinity, IrqContext, IrqError, IrqExecution, IrqHandle,
    IrqNumber, IrqOps, IrqOutcome, IrqRequest, IrqReturn, IrqScope, IrqStatus, RawIrqHandler,
    Registry, ShareMode,
};
use spin::Once;

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

    fn set_enabled(
        &self,
        irq: IrqNumber,
        _cpu: Option<CpuId>,
        enabled: bool,
    ) -> Result<(), IrqError> {
        set_enable(irq.0, enabled);
        Ok(())
    }

    fn set_affinity(&self, irq: IrqNumber, affinity: IrqAffinity) -> Result<(), IrqError> {
        set_affinity(irq.0, affinity)
    }

    fn is_enabled(&self, _irq: IrqNumber, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn is_pending(&self, _irq: IrqNumber, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn is_in_service(&self, _irq: IrqNumber, _cpu: Option<CpuId>) -> Result<bool, IrqError> {
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
pub fn request_irq(irq: usize, request: IrqRequest) -> Result<IrqHandle, IrqError> {
    let auto_enable = request.auto_enable_mode();
    let handle = registry().request(IrqNumber(irq), request)?;
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
    irq: usize,
    handler: RawIrqHandler,
    data: core::ptr::NonNull<()>,
) -> Result<IrqHandle, IrqError> {
    request_irq(
        irq,
        IrqRequest::new(handler, data).share_mode(ShareMode::Shared),
    )
}

/// Requests a per-CPU IRQ action.
pub fn request_percpu_irq(
    irq: usize,
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
pub fn dispatch_irq(irq: usize) -> IrqOutcome {
    let cpu = CpuId(crate::percpu::this_cpu_id());
    IN_IRQ_CONTEXT.with_current(|in_irq| {
        let was_in_irq = *in_irq;
        *in_irq = true;
        let outcome = registry().dispatch(IrqNumber(irq), cpu);
        *in_irq = was_in_irq;
        outcome
    })
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
    fn set_enable(irq: usize, enabled: bool);

    /// Routes a global IRQ to a fixed CPU when supported.
    fn set_affinity(irq: usize, affinity: IrqAffinity) -> Result<(), IrqError>;

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
    fn handle(irq: usize) -> Option<usize>;

    /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
    fn send_ipi(irq_num: usize, target: IpiTarget);
}
