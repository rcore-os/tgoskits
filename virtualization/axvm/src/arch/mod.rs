//! Target architecture namespaces and compile-time selection.

use crate::{architecture::*, *};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "loongarch64")]
mod loongarch64;
#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

pub(crate) mod current;

/// Host route for one assigned physical interrupt.
///
/// `guest_route` is the guest-visible target, such as a GICv3 redistributor
/// MPIDR. It stays in the virtual controller. The physical controller must
/// target `running_hardware_id`, the hardware id of the host CPU currently
/// executing the owning vCPU. On GICv3 that id is the MPIDR affinity written
/// to IROUTER. On GICv2 it selects the CPU-interface target discovered for
/// that hardware id. Using the guest target delivers the interrupt to a CPU
/// that may not be in guest context, so a single vCPU cannot drain a
/// high-rate device event ring.
#[cfg(any(target_arch = "aarch64", test))]
pub(crate) fn host_interrupt_route(guest_route: u64, running_hardware_id: u64) -> u64 {
    let _ = guest_route;
    running_hardware_id
}

#[cfg(all(test, any(target_arch = "aarch64", target_arch = "x86_64")))]
mod tests {
    use super::host_interrupt_route;

    #[test]
    fn assigned_interrupt_routes_to_the_cpu_running_the_vcpu() {
        // Guest redistributor affinity 0 is a different host CPU from RK3588
        // cpu@100 (MPIDR affinity 0x100). The physical controller must follow
        // the CPU that is about to enter the guest.
        let guest_route = 0;
        let running = 0x100;
        let route = host_interrupt_route(guest_route, running);
        assert_eq!(route, running);
        assert_ne!(route, guest_route);
    }
}
