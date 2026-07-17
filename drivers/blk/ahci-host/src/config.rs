use crate::AhciError;

const DEFAULT_RESET_TIMEOUT_NS: u64 = 1_000_000_000;
const DEFAULT_OWNERSHIP_TIMEOUT_NS: u64 = 2_000_000_000;
const DEFAULT_PORT_STOP_TIMEOUT_NS: u64 = 1_000_000_000;
const DEFAULT_COMRESET_ASSERT_NS: u64 = 1_000_000;
const DEFAULT_LINK_TIMEOUT_NS: u64 = 1_000_000_000;
const DEFAULT_COMMAND_TIMEOUT_NS: u64 = 30_000_000_000;
const DEFAULT_STATUS_CHECK_NS: u64 = 1_000_000;

/// Portable AHCI timing and logical interrupt topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AhciConfig {
    pub(crate) irq_source_id: usize,
    pub(crate) ownership_timeout_ns: u64,
    pub(crate) reset_timeout_ns: u64,
    pub(crate) port_stop_timeout_ns: u64,
    pub(crate) comreset_assert_ns: u64,
    pub(crate) link_timeout_ns: u64,
    pub(crate) command_timeout_ns: u64,
    pub(crate) status_check_ns: u64,
}

impl AhciConfig {
    /// Creates a single-source INTx or shared-MSI configuration.
    pub const fn legacy_irq(irq_source_id: usize) -> Self {
        Self {
            irq_source_id,
            ownership_timeout_ns: DEFAULT_OWNERSHIP_TIMEOUT_NS,
            reset_timeout_ns: DEFAULT_RESET_TIMEOUT_NS,
            port_stop_timeout_ns: DEFAULT_PORT_STOP_TIMEOUT_NS,
            comreset_assert_ns: DEFAULT_COMRESET_ASSERT_NS,
            link_timeout_ns: DEFAULT_LINK_TIMEOUT_NS,
            command_timeout_ns: DEFAULT_COMMAND_TIMEOUT_NS,
            status_check_ns: DEFAULT_STATUS_CHECK_NS,
        }
    }

    pub(crate) const fn validate(self) -> Result<Self, AhciError> {
        if self.irq_source_id >= u64::BITS as usize {
            return Err(AhciError::InvalidConfiguration(
                "logical IRQ source ID exceeds RDIF source capacity",
            ));
        }
        if self.ownership_timeout_ns == 0
            || self.reset_timeout_ns == 0
            || self.port_stop_timeout_ns == 0
            || self.comreset_assert_ns == 0
            || self.link_timeout_ns == 0
            || self.command_timeout_ns == 0
            || self.status_check_ns == 0
        {
            return Err(AhciError::InvalidConfiguration(
                "AHCI state-machine deadlines must be nonzero",
            ));
        }
        Ok(self)
    }
}
