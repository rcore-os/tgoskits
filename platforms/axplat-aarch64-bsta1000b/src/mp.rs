use crate::config::plat::CPU_ID_LIST;
use axplat::mem::{PhysAddr, va, virt_to_phys};

/// Starts the given secondary CPU with its boot stack.
pub fn start_secondary_cpu(cpu_id: usize, stack_top: PhysAddr) {
    if cpu_id >= CPU_ID_LIST.len() {
        error!("No support for bsta1000b core {}", cpu_id);
        return;
    }

    let entry = virt_to_phys(va!(crate::boot::_start_secondary as usize));
    axplat_aarch64_peripherals::psci::cpu_on(
        CPU_ID_LIST[cpu_id],
        entry.as_usize(),
        stack_top.as_usize(),
    );
}
