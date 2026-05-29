use ax_std::os::arceos::{api, modules};

pub type HostCpuMask = api::task::AxCpuMask;

pub fn cpu_count() -> usize {
    modules::ax_hal::cpu_num()
}

pub fn this_cpu_id() -> usize {
    modules::ax_hal::percpu::this_cpu_id()
}

pub fn bind_current_to_cpu(cpu_id: usize) -> ax_errno::AxResult {
    api::task::ax_set_current_affinity(HostCpuMask::one_shot(cpu_id))
}
