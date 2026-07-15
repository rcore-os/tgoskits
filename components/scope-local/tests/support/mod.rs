use ax_cpu_local::{CpuBindingResultV1, CpuLocalError, CpuLocalPlatformV1, CpuLocalStatus, CpuPin};

struct HostCpuLocalPlatform;

#[ax_cpu_local::impl_extern_trait(name = "ax-cpu-local_0_1", abi = "rust")]
impl CpuLocalPlatformV1 for HostCpuLocalPlatform {
    fn current_cpu_binding() -> CpuBindingResultV1 {
        // SAFETY: every integration test models a non-migrating host thread.
        let pin = unsafe { CpuPin::new_unchecked() };
        match ax_cpu_local::raw::current_binding(&pin) {
            Ok(binding) => CpuBindingResultV1::ok(binding),
            Err(CpuLocalError::NotInitialized) => {
                CpuBindingResultV1::error(CpuLocalStatus::NotInitialized)
            }
            Err(_) => CpuBindingResultV1::error(CpuLocalStatus::InvalidBinding),
        }
    }

    fn get_tp() -> usize {
        if !matches!(
            Self::current_cpu_binding(),
            CpuBindingResultV1 {
                status: CpuLocalStatus::Ok,
                ..
            }
        ) {
            return 0;
        }
        // SAFETY: successful binding validation and the modeled non-migrating
        // host thread satisfy the raw task-pointer read contract.
        unsafe { ax_cpu_local::raw::get_task_pointer() }
    }

    unsafe fn set_tp(value: usize) -> CpuLocalStatus {
        let result = Self::current_cpu_binding();
        if result.status != CpuLocalStatus::Ok {
            return result.status;
        }
        unsafe { ax_cpu_local::raw::set_task_pointer(value) };
        CpuLocalStatus::Ok
    }

    fn current_thread() -> usize {
        // SAFETY: every integration test models a non-migrating host thread.
        let pin = unsafe { CpuPin::new_unchecked() };
        ax_cpu_local::raw::current_thread(&pin).map_or(0, |pointer| pointer.as_ptr() as usize)
    }
}

pub fn bind_test_area(area: ax_percpu::PerCpuArea) {
    let expected = area.binding();
    // SAFETY: the test thread cannot migrate while executing this helper.
    let pin = unsafe { CpuPin::new_unchecked() };
    match ax_cpu_local::raw::current_binding(&pin) {
        Ok(installed) => assert_eq!(
            installed, expected,
            "a host test thread cannot be rebound to another modeled CPU"
        ),
        Err(CpuLocalError::NotInitialized) => {
            // SAFETY: the caller assigns one initialized shutdown-lifetime
            // host area before this modeled CPU thread performs local access.
            unsafe { ax_cpu_local::raw::install_binding(expected) }
                .expect("host CPU-local binding must be valid");
        }
        Err(error) => panic!("host CPU-local binding is corrupt: {error}"),
    }
}
