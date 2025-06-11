use axplat::power::PowerIf;

struct PowerImpl;

#[impl_plat_interface]
impl PowerIf for PowerImpl {
    /// Bootstraps the given CPU core with the given initial stack (in physical
    /// address).
    ///
    /// Where `cpu_id` is the logical CPU ID (0, 1, ..., N-1, N is the number of
    /// CPU cores on the platform).
    fn cpu_boot(_cpu_id: usize, _stack_top_paddr: usize) {
        #[cfg(feature = "smp")]
        {
            use crate::config::plat::CPU_ID_LIST;
            use crate::mem::virt_to_phys;

            let entry = virt_to_phys(va!(crate::boot::_start_secondary as usize));
            axplat_aarch64_common::psci::cpu_on(
                CPU_ID_LIST[_cpu_id],
                entry.as_usize(),
                _stack_top_paddr,
            );
        }
    }

    /// Shutdown the whole system.
    fn system_off() -> ! {
        info!("Shutting down...");
        axplat_aarch64_common::psci::system_off()
    }
}
