//! SDIO (Secure Digital Input Output) mode transport layer.
//!
//! SDIO mode uses a dedicated host controller with 1-bit or 4-bit data bus.
//! Implement [`SdioHost`] for the platform's SDIO peripheral; the host
//! implementation controls command/data progress.

pub mod card;
pub mod host;
pub mod host2;
pub mod init;
mod init_schedule;
mod owned_init;

use core::num::NonZeroU16;

pub use card::{
    CardInfo, CardKind, CardLink, ExtCsdRequest, SdioCommandRequest, SdioDataRequest, SdioSdmmc,
    SdioStatusRequest, SwitchFunctionRequest,
};
pub use host::{
    BusWidth, ClockSpeed, DeferredIrqAck, HostEvent, HostEventKind, HostEventSource,
    ReadyBusRequest, SDMMC_BLOCK_QUEUE_ID, SdioBusOp, SdioHost, SdioIrqHandle, SdioIrqHost,
    SignalVoltage, block_queue_ready_from_host_event, poll_ready_bus_op, submit_ready_bus_op,
};
pub use host2::{
    SdioHost2Adapter, SdioHost2BusRequest, SdioHost2DataRequest, SdioHost2Irq, SdioHost2Timed,
};
#[cfg(feature = "rdif")]
pub use host2::{SdioHost2Lifecycle, SdioHost2Recovery};
pub use init::{CardInitPreference, MmcSwitchRequest, SdioInitRequest, SdioInitScratch};
#[cfg(test)]
use init::{SdioInitState, sd_acmd6_arg};
pub use init_schedule::{InitInput, InitIrqEvent, InitIrqWait, InitPoll, InitSchedule};
pub use owned_init::{InitializedSdioCard, OwnedSdioInit, OwnedSdioInitHost};

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
