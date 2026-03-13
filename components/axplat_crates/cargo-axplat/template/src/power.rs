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
        todo!()
    }

    /// Shutdown the whole system.
    fn system_off() -> ! {
        todo!()
    }

    /// Get the number of CPU cores available on this platform.
    fn cpu_num() -> usize {
        todo!()
    }
}
