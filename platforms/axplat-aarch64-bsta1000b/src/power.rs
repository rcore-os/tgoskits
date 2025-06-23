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
            crate::mp::start_secondary_cpu(_cpu_id, pa!(_stack_top_paddr));
        }
    }

    /// Shutdown the whole system.
    fn system_off() -> ! {
        axplat_aarch64_peripherals::psci::system_off()
    }
}
