//! Dynamic platform implementation of the value-only CPU-local ABI.

use ax_cpu_local::{CpuBindingResultV1, CpuLocalPlatformV1, CpuLocalStatus, CpuPin};

struct CpuLocalPlatform;

#[ax_cpu_local::abi::impl_extern_trait(name = "ax-cpu-local_0_1", abi = "rust")]
impl CpuLocalPlatformV1 for CpuLocalPlatform {
    fn current_cpu_binding() -> CpuBindingResultV1 {
        // SAFETY: callers enter this static platform operation only while an
        // IRQ/preemption guard or the offline boot boundary prevents migration.
        let pin = unsafe { CpuPin::new_unchecked() };
        match ax_cpu_local::raw::current_binding(&pin) {
            Ok(binding) => CpuBindingResultV1::ok(binding),
            Err(ax_cpu_local::CpuLocalError::NotInitialized) => {
                CpuBindingResultV1::error(CpuLocalStatus::NotInitialized)
            }
            Err(_) => CpuBindingResultV1::error(CpuLocalStatus::InvalidBinding),
        }
    }

    fn get_tp() -> usize {
        // SAFETY: CpuLocalPlatformV1 callers hold a CPU pin or an offline boot
        // invariant; the raw backend only reads the mode-owned register.
        unsafe { ax_cpu_local::raw::get_task_pointer() }
    }

    unsafe fn set_tp(value: usize) -> CpuLocalStatus {
        // SAFETY: the trait method contract is the raw backend's ownership
        // contract in both LinuxCurrent and UnikernelTls images.
        unsafe { ax_cpu_local::raw::set_task_pointer(value) };
        CpuLocalStatus::Ok
    }

    fn current_thread() -> usize {
        // SAFETY: see current_cpu_binding; the platform ABI itself never
        // manufactures a Rust reference from the returned value.
        let pin = unsafe { CpuPin::new_unchecked() };
        ax_cpu_local::raw::current_thread(&pin).map_or(0, |header| header.as_ptr() as usize)
    }
}
