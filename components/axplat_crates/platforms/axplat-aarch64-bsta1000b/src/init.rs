use ax_plat::init::InitIf;

#[allow(unused_imports)]
use crate::config::devices::{GICC_PADDR, GICD_PADDR, TIMER_IRQ};
use crate::config::plat::PSCI_METHOD;

struct InitIfImpl;

#[impl_plat_interface]
impl InitIf for InitIfImpl {
    /// Initializes the platform at the early stage for the primary core.
    ///
    /// This function should be called immediately after the kernel has booted,
    /// and performed earliest platform configuration and initialization (e.g.,
    /// early console, clocking).
    fn init_early(_cpu_id: usize, _dtb: usize) {
        ax_cpu::init::init_trap();
        ax_plat_aarch64_peripherals::psci::init(PSCI_METHOD);
        super::dw_apb_uart::init_early();
        ax_plat_aarch64_peripherals::generic_timer::init_early();
    }

    /// Initializes the platform at the early stage for secondary cores.
    #[cfg(feature = "smp")]
    fn init_early_secondary(_cpu_id: usize) {
        ax_cpu::init::init_trap();
    }

    /// Initializes the platform at the later stage for the primary core.
    ///
    /// This function should be called after the kernel has done part of its
    /// initialization (e.g, logging, memory management), and finalized the rest of
    /// platform configuration and initialization.
    fn init_later(_cpu_id: usize, _dtb: usize) {
        #[cfg(feature = "irq")]
        {
            use ax_plat::mem::pa;

            use crate::mem::phys_to_virt;
            ax_plat_aarch64_peripherals::gic::init_gic(
                phys_to_virt(pa!(GICD_PADDR)),
                phys_to_virt(pa!(GICC_PADDR)),
            );
            ax_plat_aarch64_peripherals::gic::init_gicc();
            ax_plat_aarch64_peripherals::generic_timer::enable_irqs(TIMER_IRQ);
        }
    }

    /// Initializes the platform at the later stage for secondary cores.
    #[cfg(feature = "smp")]
    fn init_later_secondary(_cpu_id: usize) {
        #[cfg(feature = "irq")]
        {
            ax_plat_aarch64_peripherals::gic::init_gicc();
            ax_plat_aarch64_peripherals::generic_timer::enable_irqs(TIMER_IRQ);
        }
    }
}
