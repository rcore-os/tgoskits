use axvisor_api::host::HostIf;

struct HostImpl;

#[axvisor_api::api_impl]
impl HostIf for HostImpl {
    fn get_host_cpu_num() -> usize {
        // std::os::arceos::modules::axconfig::plat::CPU_NUM
        axruntime::cpu_count()
    }
}
