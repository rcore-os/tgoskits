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
        use axplat::mem::{va, virt_to_phys};
        unsafe extern "C" {
            fn _start_secondary();
        }
        if sbi_rt::probe_extension(sbi_rt::Hsm).is_unavailable() {
            warn!("HSM SBI extension is not supported for current SEE.");
            return;
        }
        let entry = virt_to_phys(va!(_start_secondary as *const () as usize));
        sbi_rt::hart_start(cpu_id, entry.as_usize(), stack_top_paddr);
    }

    /// Shutdown the whole system.
    fn system_off() -> ! {
        info!("Shutting down...");
        sbi_rt::system_reset(sbi_rt::Shutdown, sbi_rt::NoReason);
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
