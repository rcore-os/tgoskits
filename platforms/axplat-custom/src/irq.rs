struct IrqIfImpl;

#[impl_plat_interface]
impl ax_plat::irq::IrqIf for IrqIfImpl {
    fn set_enable(_irq: ax_plat::irq::IrqId, _enabled: bool) -> Result<(), ax_plat::irq::IrqError> {
        Ok(())
    }

    fn set_affinity(
        _irq: ax_plat::irq::IrqId,
        _affinity: ax_plat::irq::IrqAffinity,
    ) -> Result<(), ax_plat::irq::IrqError> {
        Err(ax_plat::irq::IrqError::Unsupported)
    }

    fn handle(vector: ax_plat::irq::TrapVector) -> Option<ax_plat::irq::IrqId> {
        let irq = ax_plat::irq::IrqNumber(vector.0).ok()?;
        let _ = ax_plat::irq::dispatch_irq(irq);
        Some(irq)
    }

    fn send_ipi(_irq_num: ax_plat::irq::IrqId, _target: ax_plat::irq::IpiTarget) {}

    fn ipi_irq() -> ax_plat::irq::IrqId {
        ax_plat::irq::IrqNumber(0).expect("example IPI IRQ is in range")
    }

    fn resolve_source(
        _source: ax_plat::irq::IrqSource,
    ) -> Result<ax_plat::irq::IrqId, ax_plat::irq::IrqError> {
        Err(ax_plat::irq::IrqError::Unsupported)
    }

    fn resolve_percpu(
        hwirq: ax_plat::irq::HwIrq,
    ) -> Result<ax_plat::irq::IrqId, ax_plat::irq::IrqError> {
        Ok(ax_plat::irq::IrqId::new(
            ax_plat::irq::CPU_LOCAL_IRQ_DOMAIN,
            hwirq,
        ))
    }
}
