//! Interrupt management.

#[cfg(feature = "ipi")]
pub use ax_config::devices::IPI_IRQ;
use ax_cpu::trap::set_irq_handler;
pub use ax_plat::irq::{
    AutoEnable, CpuId, CpuMask, IrqContext, IrqError, IrqHandle, IrqNumber, IrqOutcome, IrqRequest,
    IrqReturn, IrqScope, IrqStatus, RawIrqHandler, ShareMode, cpu_online, disable_irq,
    dispatch_irq, enable_irq, free_irq, handle, irq_status, request_irq, request_percpu_irq,
    request_shared_irq, set_enable, set_run_on_cpu_sync,
};
#[cfg(feature = "ipi")]
pub use ax_plat::irq::{IpiTarget, send_ipi};

/// IRQ handler.
///
/// # Warn
///
/// Make sure called in an interrupt context or hypervisor VM exit handler.
pub fn handle_irq(vector: usize) -> bool {
    let guard = ax_kernel_guard::NoPreempt::new();
    let handled = handle(vector).is_some();

    drop(guard); // rescheduling may occur when preemption is re-enabled.
    handled
}

/// Installs the default ArceOS IRQ dispatcher into `ax-cpu`'s runtime hook.
///
/// This is intended for runtimes that dispatch traps through
/// [`ax_cpu::trap::dispatch_irq`] instead of relying on the `#[irq_handler]`
/// link-time override path.
pub fn init_common_irq_handler() {
    let _ = set_irq_handler(handle_irq);
}
