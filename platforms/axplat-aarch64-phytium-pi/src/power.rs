use axplat::power::PowerIf;

struct PowerImpl;

#[impl_plat_interface]
impl PowerIf for PowerImpl {
    /// Bootstraps the given CPU core with the given initial stack (in physical
    /// address).
    ///
    /// Where `cpu_id` is the logical CPU ID (0, 1, ..., N-1, N is the number of
    /// CPU cores on the platform).
    #[cfg(feature = "smp")]
    fn cpu_boot(cpu_id: usize, stack_top_paddr: usize) {
        use crate::config::plat::CPU_ID_LIST;
        use axplat::mem::{va, virt_to_phys};

        let entry = virt_to_phys(va!(crate::boot::_start_secondary as *const () as usize));
        axplat_aarch64_peripherals::psci::cpu_on(
            CPU_ID_LIST[cpu_id],
            entry.as_usize(),
            stack_top_paddr,
        );
    }

    /// Shutdown the whole system.
    fn system_off() -> ! {
        info!("Shutting down...");
        axplat_aarch64_peripherals::psci::system_off()
    }

    /// Get the number of CPU cores available on this platform.
    fn cpu_num() -> usize {
        crate::config::plat::MAX_CPU_NUM
    }
}
