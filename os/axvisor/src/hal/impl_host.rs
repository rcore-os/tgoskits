use axvisor_api::host::HostIf;

struct HostImpl;

#[axvisor_api::api_impl]
impl HostIf for HostImpl {
    fn get_host_cpu_num() -> usize {
        ax_hal::cpu_num()
    }

    fn init_percpu() {
        // ArceOS initializes host per-CPU runtime state before AxVisor starts.
    }

    #[cfg(feature = "shell")]
    fn exit(exit_code: i32) -> ! {
        std::process::exit(exit_code)
    }
}
