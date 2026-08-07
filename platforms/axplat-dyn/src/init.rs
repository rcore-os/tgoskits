use ax_plat::init::InitIf;

struct InitIfImpl;

#[impl_plat_interface]
impl InitIf for InitIfImpl {
    /// Initializes the platform at the early stage for the primary core.
    ///
    /// This function should be called immediately after the kernel has booted,
    /// and performed earliest platform configuration and initialization (e.g.,
    /// early console, clocking).
    fn init_early(cpu_id: usize, _dtb: usize) {
        enable_fp_simd();
        somehal::timer::enable();
        // SAFETY: platform entry binds this CPU-local area before ax-runtime
        // calls init_early, and the scheduler and local IRQs are still offline.
        unsafe { ax_plat::time::init_scheduler_clock(cpu_id) }
            .unwrap_or_else(|error| panic!("failed to initialize scheduler clock: {error}"));
    }

    /// Initializes the platform at the early stage for secondary cores.
    #[cfg(feature = "smp")]
    fn init_early_secondary(cpu_id: usize) {
        enable_fp_simd();
        somehal::timer::enable();
        // SAFETY: secondary early initialization precedes scheduler
        // publication and local IRQ enablement on this bound CPU.
        unsafe { ax_plat::time::init_scheduler_clock(cpu_id) }
            .unwrap_or_else(|error| panic!("failed to initialize scheduler clock: {error}"));
    }

    /// Initializes the platform at the later stage for the primary core.
    ///
    /// This function should be called after the kernel has done part of its
    /// initialization (e.g, logging, memory management), and finalized the rest of
    /// platform configuration and initialization.
    fn init_later(_cpu_id: usize, _dtb: usize) {
        somehal::post_paging();
        #[cfg(all(feature = "rtc", target_arch = "loongarch64"))]
        crate::generic_timer::try_init_epoch_offset_from_firmware();
    }

    /// Initializes the platform at the later stage for secondary cores.
    #[cfg(feature = "smp")]
    fn init_later_secondary(_cpu_id: usize) {}
}

#[cfg(all(
    feature = "fp-simd",
    any(target_arch = "aarch64", target_arch = "loongarch64")
))]
fn enable_fp_simd() {
    ax_cpu::asm::enable_fp();
    #[cfg(target_arch = "loongarch64")]
    {
        ax_cpu::asm::enable_lsx();
        ax_cpu::asm::enable_lasx();
    }
    debug!("axplat-dyn: fp/simd enabled");
}

#[cfg(not(all(
    feature = "fp-simd",
    any(target_arch = "aarch64", target_arch = "loongarch64")
)))]
fn enable_fp_simd() {}
