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

/// Steps that move one assigned SPI onto the CPU entering the guest.
///
/// `GICD_IROUTER<n>` and `GICD_ITARGETSR<n>` writes are architecturally
/// UNPREDICTABLE while SPI `n` is enabled. RK3588 drops the IROUTER write, so
/// a guest entry that rewrites an enabled SPI keeps the bind-time target.
/// That target is the control-plane CPU, which is not in guest context while
/// the single vCPU runs. The xHCI event ring then fills and the controller
/// posts Host Controller Event TRBs.
///
/// An enabled SPI is therefore masked, the distributor write is retired, and
/// only then is the route stored. The enable bit the guest left set is
/// restored afterwards. A disabled SPI is left disabled: bind must not enable
/// a source the guest has not claimed, and a later retarget must not undo a
/// guest mask.
#[cfg(any(target_arch = "aarch64", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PhysicalRouteUpdate {
    /// Store the route. The SPI is already disabled.
    WriteWhileDisabled,
    /// Mask, retire the distributor write, store the route, then re-enable.
    DisableWriteThenRestore,
}

#[cfg(any(target_arch = "aarch64", test))]
pub(crate) fn physical_route_update(currently_enabled: bool) -> PhysicalRouteUpdate {
    if currently_enabled {
        PhysicalRouteUpdate::DisableWriteThenRestore
    } else {
        PhysicalRouteUpdate::WriteWhileDisabled
    }
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

    #[test]
    fn enabled_physical_spi_is_disabled_before_its_route_changes() {
        // Writing IROUTER or ITARGETSR while the SPI stays enabled is the
        // sequence that left USB completions on the bind CPU. Both the bind
        // path and every later retarget must mask, write, then restore.
        assert_eq!(
            super::physical_route_update(true),
            super::PhysicalRouteUpdate::DisableWriteThenRestore
        );
        assert_eq!(
            super::physical_route_update(false),
            super::PhysicalRouteUpdate::WriteWhileDisabled
        );
    }
}
