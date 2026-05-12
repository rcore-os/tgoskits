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
