//! Adapter from `bare-task` OS hooks to ArceOS task runtime services.

use ax_kernel_guard::BaseGuard;
use bare_task::{BareTaskOs, CpuId, impl_trait};

struct BareTaskOsImpl;

impl_trait! {
    impl BareTaskOs for BareTaskOsImpl {
        fn cpu_num() -> usize {
            ax_hal::cpu_num()
        }

        fn this_cpu_id() -> CpuId {
            CpuId(ax_hal::percpu::this_cpu_id())
        }

        fn current_task_ptr() -> *const () {
            ax_hal::percpu::current_task_ptr::<()>()
        }

        unsafe fn set_current_task_ptr(ptr: *const ()) {
            unsafe { ax_hal::percpu::set_current_task_ptr(ptr) };
        }

        fn irq_save_and_disable() -> usize {
            ax_kernel_guard::IrqSave::acquire()
        }

        unsafe fn irq_restore(state: usize) {
            ax_kernel_guard::IrqSave::release(state);
        }

        fn irqs_enabled() -> bool {
            ax_hal::asm::irqs_enabled()
        }

        fn in_irq_context() -> bool {
            ax_hal::irq::in_irq_context()
        }

        fn wait_for_irqs() {
            ax_hal::asm::wait_for_irqs();
        }

        fn monotonic_time_nanos() -> u64 {
            ax_hal::time::monotonic_time_nanos()
        }

        fn set_oneshot_timer(deadline_nanos: u64) {
            ax_hal::time::set_oneshot_timer(deadline_nanos);
        }

        fn request_reschedule(cpu: CpuId) {
            #[cfg(feature = "ipi")]
            if cpu.0 != ax_hal::percpu::this_cpu_id() {
                ax_ipi::run_on_cpu(cpu.0, || {});
            }
            #[cfg(not(feature = "ipi"))]
            let _ = cpu;
        }

        fn request_irq_wake(cpu: CpuId) {
            #[cfg(any(feature = "ipi", feature = "irq-wake-ipi"))]
            if cpu.0 != ax_hal::percpu::this_cpu_id() {
                ax_hal::irq::send_ipi(
                    ax_hal::irq::ipi_irq(),
                    ax_hal::irq::IpiTarget::Other { cpu_id: cpu.0 },
                );
            }
            #[cfg(not(any(feature = "ipi", feature = "irq-wake-ipi")))]
            let _ = cpu;
        }

        fn wait_until_cpu_ready(cpu: CpuId) -> bool {
            #[cfg(feature = "ipi")]
            {
                ax_ipi::wait_until_cpu_ready(cpu.0)
            }
            #[cfg(not(feature = "ipi"))]
            {
                cpu.0 == ax_hal::percpu::this_cpu_id()
            }
        }

        fn on_sched_switch(_prev: usize, _next: usize, _next_state: u8) {}
    }
}
