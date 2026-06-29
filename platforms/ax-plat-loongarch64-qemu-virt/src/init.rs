use ax_plat::init::InitIf;

struct InitIfImpl;

#[impl_plat_interface]
impl InitIf for InitIfImpl {
    /// This function should be called immediately after the kernel has booted,
    /// and performed earliest platform configuration and initialization (e.g.,
    /// early console, clocking).
    fn init_early(_cpu_id: usize, _mbi: usize) {
        crate::console::init_early();
        crate::time::init_early();
    }

    /// Initializes the platform at the early stage for secondary cores.
    #[cfg(feature = "smp")]
    fn init_early_secondary(_cpu_id: usize) {}

    /// Initializes the platform at the later stage for the primary core.
    ///
    /// This function should be called after the kernel has done part of its
    /// initialization (e.g, logging, memory management), and finalized the rest of
    /// platform configuration and initialization.
    fn init_later(_cpu_id: usize, _arg: usize) {
        #[cfg(feature = "irq")]
        crate::irq::init();
        crate::time::init_percpu();
        #[cfg(feature = "smp")]
        {
            let irq = ax_plat::irq::IrqId::new(
                ax_plat::irq::CPU_LOCAL_IRQ_DOMAIN,
                ax_plat::irq::HwIrq(crate::config::devices::IPI_IRQ as u32),
            );
            if let Err(err) = ax_plat::irq::set_enable(irq, true) {
                warn!("failed to enable IPI IRQ: {err:?}");
            }
        }
    }

    /// Initializes the platform at the later stage for secondary cores.
    #[cfg(feature = "smp")]
    fn init_later_secondary(_cpu_id: usize) {
        crate::time::init_percpu();
        let irq = ax_plat::irq::IrqId::new(
            ax_plat::irq::CPU_LOCAL_IRQ_DOMAIN,
            ax_plat::irq::HwIrq(crate::config::devices::IPI_IRQ as u32),
        );
        if let Err(err) = ax_plat::irq::set_enable(irq, true) {
            warn!("failed to enable IPI IRQ: {err:?}");
        }
    }
}
