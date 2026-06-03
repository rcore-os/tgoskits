use alloc::boxed::Box;
use std::os::arceos;

#[cfg(feature = "fs")]
use ax_errno::AxResult;
use axvisor_api::host::HostIf;

struct HostImpl;

#[axvisor_api::api_impl]
impl HostIf for HostImpl {
    fn prepare_virtualization() {
        crate::hal::arch::prepare_virtualization();
    }

    fn get_host_cpu_num() -> usize {
        ax_hal::cpu_num()
    }

    fn spawn_cpu_init_task(cpu_id: usize, task: Box<dyn FnOnce() + Send + 'static>) {
        use std::thread;

        use arceos::api::task::{AxCpuMask, ax_set_current_affinity};

        thread::spawn(move || {
            assert!(
                ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_ok(),
                "Initialize CPU affinity failed!"
            );
            task();
        });
    }

    fn yield_now() {
        std::thread::yield_now();
    }

    #[cfg(feature = "fs")]
    fn release_host_filesystems() -> AxResult {
        arceos::modules::ax_fs::shutdown_filesystems()
    }

    #[cfg(feature = "shell")]
    fn exit(exit_code: i32) -> ! {
        std::process::exit(exit_code)
    }
}
