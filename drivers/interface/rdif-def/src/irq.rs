custom_type!(
    #[doc = "Hardware Interrupt ID"],
    IrqId, usize, "{:#x}");

/// The trigger configuration for an interrupt.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Trigger {
    EdgeBoth,
    EdgeRising,
    EdgeFailling,
    LevelHigh,
    LevelLow,
}

/// The configuration for setup an interrupt.
#[derive(Debug, Clone)]
pub struct IrqConfig {
    pub irq: IrqId,
    pub trigger: Trigger,
    /// Is cpu private interrupt?
    pub is_private: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiIrqTrigger {
    Edge,
    Level,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiIrqPolarity {
    ActiveHigh,
    ActiveLow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiGsiController {
    IoApic,
    PchPic,
}

/// Firmware-owned ACPI GSI routing metadata.
///
/// A CPU vector is deliberately absent: the target interrupt controller owns
/// vector allocation after translating this route to its hardware domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiGsiRoute {
    pub gsi: u32,
    pub controller: AcpiGsiController,
    pub controller_id: u16,
    pub controller_address: u64,
    pub controller_input: u8,
    pub trigger: AcpiIrqTrigger,
    pub polarity: AcpiIrqPolarity,
}
