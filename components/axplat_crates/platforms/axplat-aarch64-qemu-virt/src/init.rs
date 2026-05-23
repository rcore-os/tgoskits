use ax_plat::{
    init::InitIf,
    mem::{pa, phys_to_virt},
};

#[allow(unused_imports)]
use crate::config::devices::{
    GICC_PADDR, GICD_PADDR, GICR_PADDR, RTC_PADDR, TIMER_IRQ, UART_IRQ, UART_PADDR,
};
use crate::config::plat::PSCI_METHOD;

// Building this platform with `cntv-timer` switches the generic
// timer to CNTV, whose PPI is 11 -> IRQ 27 in the GIC distributor.
// The axconfig must advertise that IRQ instead of the CNTP default
// of 30, otherwise the timer is programmed but never delivered.
// Catch the silent misconfiguration at compile time so a build that
// enables `cntv-timer` without `devices.timer-irq=27` cannot produce
// a boot-image that races on every timer tick. The feature is
// reachable from StarryOS via either the umbrella feature
// `FEATURES=starryos/aarch64-hvf cargo xtask starry build --arch aarch64`
// or the explicit two-feature form
// `FEATURES=starryos/cntv-timer,starryos/gic-v3 cargo xtask starry build --arch aarch64`;
// `cntv-timer` and `gic-v3` are orthogonal and neither composes the
// other.
#[cfg(feature = "cntv-timer")]
const _: () = assert!(
    TIMER_IRQ == 27,
    "cntv-timer feature requires devices.timer-irq=27 (CNTV PPI 11); apply \
     axconfig_overrides=[\"devices.timer-irq=27\"]"
);

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
        ax_plat_aarch64_peripherals::pl011::init_early(phys_to_virt(pa!(UART_PADDR)));
        ax_plat_aarch64_peripherals::psci::init(PSCI_METHOD);
        ax_plat_aarch64_peripherals::generic_timer::init_early();
        #[cfg(feature = "rtc")]
        ax_plat_aarch64_peripherals::pl031::init_early(phys_to_virt(pa!(RTC_PADDR)));
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
            // The GIC backend is selected at compile time by the
            // `gic-v3` Cargo feature on this crate (forwarded from
            // `ax-feat::aarch64-gic-v3` → `starryos::gic-v3`).
            // Default builds keep GICv2 + CNTP for QEMU TCG and
            // physical boards.
            #[cfg(not(feature = "gic-v3"))]
            ax_plat_aarch64_peripherals::gic::init_gic(
                phys_to_virt(pa!(GICD_PADDR)),
                phys_to_virt(pa!(GICC_PADDR)),
            );
            #[cfg(feature = "gic-v3")]
            ax_plat_aarch64_peripherals::gic::init_gic_v3(
                phys_to_virt(pa!(GICD_PADDR)),
                phys_to_virt(pa!(GICR_PADDR)),
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
