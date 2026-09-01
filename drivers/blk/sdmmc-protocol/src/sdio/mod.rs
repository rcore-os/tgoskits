//! SDIO (Secure Digital Input Output) mode transport layer
//!
//! SDIO mode uses a dedicated host controller with 1-bit or 4-bit data bus.
//! Implement [`sdmmc_host::SdMmcHost`] plus [`SdMmcIrqHost`] for the physical
//! controller; this module owns card-protocol progress.

pub mod host;
pub mod init;
pub mod io;
pub mod native;
pub(crate) mod transport;

use core::num::NonZeroU16;

pub use host::{
    BusWidth, CardIrqControl, ClockSpeed, CompletionIrqRearm, HostEvent, HostEventKind,
    HostEventSource, HostProgressWait, SDMMC_BLOCK_QUEUE_ID, SdMmcBusOp, SdMmcIrqHandle,
    SdMmcIrqHost, SignalVoltage, block_queue_ready_from_host_event,
};
pub use init::{
    CardInitPreference, MmcSwitchRequest, SdMmcInitRequest, SdMmcInitScratch, SdMmcInitWait,
};
#[cfg(test)]
use init::{MmcSwitchTiming, SdMmcInitState, SdMmcInitTiming, sd_acmd6_arg};
pub use io::{
    AddressMode, CisInfo, FunctionNumber, IoAddress, SdioBlockSizeRequest, SdioCard, SdioCardInfo,
    SdioDirectRequest, SdioDmaSubmitError, SdioDmaTransferRequest, SdioFunctionEnableRequest,
    SdioFunctionInfo, SdioInterruptEnableRequest, SdioTransferRequest, TransferMode,
};
pub use native::{
    CardInfo, CardKind, ExtCsdRequest, SdMmcCard, SdMmcCommandRequest, SdMmcDataRequest,
    SdMmcStatusRequest, SwitchFunctionRequest,
};

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
