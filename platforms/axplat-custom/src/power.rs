use ax_plat::power::PowerIf;

struct PowerIfImpl;

#[impl_plat_interface]
impl PowerIf for PowerIfImpl {
    #[cfg(feature = "smp")]
    fn cpu_boot(_cpu_id: usize, _stack_top_paddr: usize) {}

    fn system_off() -> ! {
        loop {
            core::hint::spin_loop();
        }
    }

    fn system_reset() -> ! {
        loop {
            core::hint::spin_loop();
        }
    }

    fn cpu_num() -> usize {
        1
    }
}
