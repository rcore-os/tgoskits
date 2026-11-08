//! SD/MMC IRQ capability layered on the portable `sdmmc-host` bus contract.

use core::num::NonZeroU16;

pub use sdmmc_host::{
    BusWidth, CardIrqControl, ClockSpeed, CompletionIrqRearm, CompletionIrqRearmHost, HostEvent,
    HostEventKind, HostEventSource, HostProgressWait, SdMmcIrqHandle, SdMmcIrqHost, SignalVoltage,
};

use crate::cmd::Command;

/// Queue identifier used by single-queue SD/MMC block adapters.
pub const SDMMC_BLOCK_QUEUE_ID: usize = 0;

pub fn block_queue_ready_from_host_event(event: &impl HostEvent) -> Option<usize> {
    match event.kind() {
        HostEventKind::None | HostEventKind::CardInterrupt => None,
        _ => Some(SDMMC_BLOCK_QUEUE_ID),
    }
}

/// Protocol-level naming for portable host bus operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdMmcBusOp {
    ResetAll,
    PowerOn,
    PowerOff,
    SetBusWidth(BusWidth),
    SetClock(ClockSpeed),
    SwitchVoltage(SignalVoltage),
    SwitchSdVoltage,
    ExecuteTuning {
        cmd_index: u8,
        block_size: NonZeroU16,
    },
}

impl SdMmcBusOp {
    pub(super) fn into_host_op(self) -> sdmmc_host::BusOp {
        match self {
            Self::ResetAll => sdmmc_host::BusOp::ResetAll,
            Self::PowerOn => sdmmc_host::BusOp::PowerOn,
            Self::PowerOff => sdmmc_host::BusOp::PowerOff,
            Self::SetBusWidth(width) => sdmmc_host::BusOp::SetBusWidth(width),
            Self::SetClock(speed) => sdmmc_host::BusOp::SetClock(speed),
            Self::SwitchVoltage(voltage) => sdmmc_host::BusOp::SetSignalVoltage(voltage),
            Self::SwitchSdVoltage => sdmmc_host::BusOp::SwitchSdVoltage,
            Self::ExecuteTuning {
                cmd_index,
                block_size,
            } => sdmmc_host::BusOp::ExecuteTuning {
                command: Command::new(cmd_index, 0, crate::response::ResponseType::R1),
                block_size,
            },
        }
    }
}
