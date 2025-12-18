use axplat::mem::pa;
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
        crate::mp::start_secondary_cpu(cpu_id, pa!(stack_top_paddr));
    }

    /// Shutdown the whole system.
    fn system_off() -> ! {
        const HALT_ADDR: *mut u8 =
            crate::mem::phys_to_virt(pa!(crate::config::devices::GED_PADDR)).as_mut_ptr();

        info!("Shutting down...");
        unsafe { HALT_ADDR.write_volatile(0x34) };
        axcpu::asm::halt();
        warn!("It should shutdown!");
        loop {
            axcpu::asm::halt();
        }
    }

    /// Get the number of CPU cores available on this platform.
    fn cpu_num() -> usize {
        crate::config::plat::MAX_CPU_NUM
    }
}
