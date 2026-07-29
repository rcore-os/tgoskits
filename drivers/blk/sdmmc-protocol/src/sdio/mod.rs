//! SDIO (Secure Digital Input Output) mode transport layer
//!
//! SDIO mode uses a dedicated host controller with 1-bit or 4-bit data bus.
//! Implement [`sdio_host2::SdioHost`] plus [`SdioIrqHost`] for the physical
//! controller; this module owns card-protocol progress.

pub mod card;
pub mod host;
pub(crate) mod host2;
pub mod init;

use core::num::NonZeroU16;

pub use card::{
    CardInfo, CardKind, ExtCsdRequest, SdioCommandRequest, SdioDataRequest, SdioSdmmc,
    SdioStatusRequest, SwitchFunctionRequest,
};
pub use host::{
    BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, HostProgressWait,
    SDMMC_BLOCK_QUEUE_ID, SdioBusOp, SdioIrqHandle, SdioIrqHost, SignalVoltage,
    block_queue_ready_from_host_event,
};
pub use init::{
    CardInitPreference, MmcSwitchRequest, SdioInitRequest, SdioInitScratch, SdioInitWait,
};
#[cfg(test)]
use init::{MmcSwitchTiming, SdioInitState, SdioInitTiming, sd_acmd6_arg};

pub use crate::cmd::DataDirection;
use crate::error::Error;

pub(super) fn nonzero_block_size(block_size: u32) -> Result<NonZeroU16, Error> {
    u16::try_from(block_size)
        .ok()
        .and_then(NonZeroU16::new)
        .ok_or(Error::InvalidArgument)
}

#[cfg(test)]
mod tests;
