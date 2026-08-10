//! Host hypervisor task policy for Task 1 mixed-criticality layouts.

use ax_std::os::arceos::api::task::{self, AxCpuMask};

/// Management-domain physical CPU for shell and other host housekeeping.
const HOST_MANAGEMENT_CPU: usize = 0;

/// Pins the current host task to the management pCPU.
///
/// Task 1 QEMU layouts reserve pCPU0 for AxVisor housekeeping while guest
/// vCPUs run on dedicated pCPUs via `phys_cpu_ids`.
pub fn init_host_task_affinity() {
    let mask = AxCpuMask::one_shot(HOST_MANAGEMENT_CPU);
    if task::ax_set_current_affinity(mask).is_err() {
        warn!("Failed to pin AxVisor management task to pCPU{HOST_MANAGEMENT_CPU}");
    } else {
        info!("AxVisor management task pinned to pCPU{HOST_MANAGEMENT_CPU}");
    }
}
