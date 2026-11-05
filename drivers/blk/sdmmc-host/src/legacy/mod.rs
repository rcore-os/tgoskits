//! Shared response and progress representations for physical controller engines.
pub mod block;
pub mod error;
pub mod response;
/// Direction of the data phase that follows a command, if any.
///
/// Marked `#[non_exhaustive]`: bidirectional / control-stream variants may be
/// added before 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataDirection {
    /// No data phase follows this command.
    None,
    /// The host reads data from the card after the command response.
    Read,
    /// The host writes data to the card after the command response.
    Write,
}

impl DataDirection {
    /// Returns true if this command has no data phase.
    pub const fn is_none(self) -> bool {
        matches!(self, DataDirection::None)
    }
}

pub use block::{CommandProgress, CommandResponseProgress, DataCommandProgress};

pub use crate::{BusWidth, ClockSpeed, Command, SignalVoltage};
pub mod irq_capabilities {
    pub use crate::{
        BusWidth, CardIrqControl, ClockSpeed, CompletionIrqRearm, CompletionIrqRearmHost,
        HostEvent, HostEventKind, HostEventSource, HostProgressWait, SdMmcIrqHandle, SdMmcIrqHost,
        SignalVoltage,
    };
}
pub const CMD12: crate::Command = crate::Command::new(12, 0, crate::ResponseType::R1b);
