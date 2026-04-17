//! Interrupt management.

use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "ipi")]
pub use ax_config::devices::IPI_IRQ;
use ax_cpu::trap::{irq_handler, set_irq_handler};
#[cfg(feature = "ipi")]
pub use ax_plat::irq::{IpiTarget, send_ipi};
pub use ax_plat::irq::{handle, register, set_enable, unregister};

static IRQ_HOOK: AtomicUsize = AtomicUsize::new(0);

/// Register a hook function called after an IRQ is handled.
///
/// This function can be called only once; subsequent calls will return false.
///
/// TODO: design a better api!
pub fn register_irq_hook(hook: fn(usize)) -> bool {
    IRQ_HOOK
        .compare_exchange(
            0,
            hook as *const () as usize,
            Ordering::Release,
            Ordering::Relaxed,
        )
        .is_ok()
}

/// IRQ handler.
///
/// # Warn
///
/// Make sure called in an interrupt context or hypervisor VM exit handler.
#[irq_handler]
pub fn handle_irq(vector: usize) -> bool {
    let guard = ax_kernel_guard::NoPreempt::new();

    if let Some(irq) = handle(vector) {
        let hook = IRQ_HOOK.load(Ordering::Acquire);
        if hook != 0 {
            let hook = unsafe { core::mem::transmute::<usize, fn(usize)>(hook) };
            hook(irq);
        }
    }

    drop(guard); // rescheduling may occur when preemption is re-enabled.
    true
}

/// Installs the default ArceOS IRQ dispatcher into `ax-cpu`'s runtime hook.
///
/// This is intended for runtimes that dispatch traps through
/// [`ax_cpu::trap::dispatch_irq`] instead of relying on the `#[irq_handler]`
/// link-time override path.
pub fn init_common_irq_handler() {
    let _ = set_irq_handler(handle_irq);
}
