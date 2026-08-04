//! Trigger-configuration routing shared by the GICv2 and GICv3 backends.

/// Routes one trigger update to the register bank that owns the INTID class.
///
/// Private interrupts use the current CPU interface, SPIs use the
/// Distributor, and GICv3 LPIs reject trigger reconfiguration because their
/// properties are owned by the ITS tables.
pub(crate) fn dispatch_trigger_configuration<E>(
    raw_intid: u32,
    lpi_intid_base: Option<u32>,
    configure_private: impl FnOnce(u32) -> Result<(), E>,
    configure_spi: impl FnOnce(u32) -> Result<(), E>,
    unsupported: impl FnOnce() -> E,
) -> Result<(), E> {
    if raw_intid < 32 {
        configure_private(raw_intid)
    } else if lpi_intid_base.is_some_and(|base| raw_intid >= base) {
        Err(unsupported())
    } else {
        configure_spi(raw_intid)
    }
}
