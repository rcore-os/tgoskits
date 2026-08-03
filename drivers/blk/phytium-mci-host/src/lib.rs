//! Phytium MCI/FSDIF host controller backend for `sdmmc-protocol`.
//!
//! The register layout is the Phytium Memory Card Interface found on E2000
//! class SoCs. It is close to the DesignWare MSHC programming model, with
//! Phytium-specific clock-source and timing registers.
//!
//! # Scope
//!
//! - **Implemented**: controller recovery, power and clock setup, Phytium
//!   timing tables, 1-bit / 4-bit / 8-bit bus selection, command response
//!   decoding, persistent-ring IDMAC block transfers, and stable IRQ event
//!   extraction.
//! - **Out of scope for this crate**: FDT/ACPI probe, MMIO remapping, IRQ
//!   registration, pad-controller programming, OS sleeps/wakeups, and rdif-block
//!   registration.
//! - **Implemented for block I/O**: owned DMA, IRQ-only completion, and a
//!   controller-lifetime 4 KiB IDMAC descriptor ring. There is no PIO or
//!   completion-polling fallback.

#![no_std]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

use alloc::sync::Arc;
use core::{marker::PhantomData, num::NonZeroUsize, ptr::NonNull, time::Duration};

mod command;
mod dma;
mod host;
mod regs;
mod timing;

pub use dma::{
    BlockRequest, BlockRequestSlot, IDMAC_DESC_ALIGN, IDMAC_DESC_SIZE, IDMAC_MAX_BLOCKS,
    IDMAC_MAX_TRANSFER_SIZE, RequestId,
};
pub use host::PhytiumMci;
use host::uhs_bits_after_voltage;
use regs::RegisterBlockVolatileFieldAccess;
pub use sdmmc_protocol::block::{
    BlockRequestId, BlockTransferDirection, BlockTransferMode, BlockTransferState,
};
use sdmmc_protocol::{
    CommandResponseProgress, DataCommandProgress,
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    sdio::host::{
        BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, SdioIrqHost, SignalVoltage,
    },
};

/// Stable controller event extracted from Phytium MCI raw interrupt status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Event {
    /// No status bit requiring runtime action is currently pending.
    #[default]
    None,
    /// A command response has completed.
    CommandComplete,
    /// A data transfer has completed.
    TransferComplete,
    /// Receive FIFO can be drained.
    ReceiveReady,
    /// Transmit FIFO can accept more data.
    TransmitReady,
    /// One or more controller error bits are pending.
    Error { raw_status: u32 },
    /// Status bits are pending but do not map to a high-level event yet.
    Other { raw_status: u32 },
}

impl HostEvent for Event {
    fn kind(&self) -> HostEventKind {
        match self {
            Event::None => HostEventKind::None,
            Event::CommandComplete => HostEventKind::CommandComplete,
            Event::TransferComplete => HostEventKind::TransferComplete,
            Event::ReceiveReady => HostEventKind::ReceiveReady,
            Event::TransmitReady => HostEventKind::TransmitReady,
            Event::Error { .. } => HostEventKind::Error,
            Event::Other { .. } => HostEventKind::Other,
        }
    }

    fn source(&self) -> HostEventSource {
        match self {
            Event::CommandComplete => HostEventSource::Command,
            Event::TransferComplete | Event::ReceiveReady | Event::TransmitReady => {
                HostEventSource::Data
            }
            Event::None | Event::Error { .. } | Event::Other { .. } => HostEventSource::Controller,
        }
    }
}

pub(crate) const MCI_INT_RESPONSE_ERROR: u32 = 1 << 1;
pub(crate) const MCI_INT_COMMAND_DONE: u32 = 1 << 2;
pub(crate) const MCI_INT_DATA_TRANSFER_OVER: u32 = 1 << 3;
pub(crate) const MCI_INT_TXDR: u32 = 1 << 4;
pub(crate) const MCI_INT_RXDR: u32 = 1 << 5;
pub(crate) const MCI_INT_RESPONSE_CRC_ERROR: u32 = 1 << 6;
pub(crate) const MCI_INT_DATA_CRC_ERROR: u32 = 1 << 7;
pub(crate) const MCI_INT_RESPONSE_TIMEOUT: u32 = 1 << 8;
pub(crate) const MCI_INT_DATA_READ_TIMEOUT: u32 = 1 << 9;
pub(crate) const MCI_INT_HOST_TIMEOUT: u32 = 1 << 10;
pub(crate) const MCI_INT_FIFO_UNDER_OVER_RUN: u32 = 1 << 11;
pub(crate) const MCI_INT_HARDWARE_LOCKED_WRITE: u32 = 1 << 12;
pub(crate) const MCI_INT_START_BIT_ERROR: u32 = 1 << 13;
pub(crate) const MCI_INT_END_BIT_ERROR: u32 = 1 << 15;
pub(crate) const MCI_INT_ERROR_MASK: u32 = MCI_INT_RESPONSE_ERROR
    | MCI_INT_RESPONSE_CRC_ERROR
    | MCI_INT_DATA_CRC_ERROR
    | MCI_INT_RESPONSE_TIMEOUT
    | MCI_INT_DATA_READ_TIMEOUT
    | MCI_INT_HOST_TIMEOUT
    | MCI_INT_FIFO_UNDER_OVER_RUN
    | MCI_INT_HARDWARE_LOCKED_WRITE
    | MCI_INT_START_BIT_ERROR
    | MCI_INT_END_BIT_ERROR;

pub(crate) const MCI_IDSTS_FATAL_BUS_ERROR: u32 = 1 << 2;
pub(crate) const MCI_IDSTS_DESCRIPTOR_UNAVAILABLE: u32 = (1 << 3) | (1 << 4);
pub(crate) const MCI_IDSTS_CARD_ERROR_SUMMARY: u32 = 1 << 5;
pub(crate) const MCI_IDSTS_ABNORMAL_SUMMARY: u32 = 1 << 9;
pub(crate) const MCI_IDSTS_ERROR_MASK: u32 =
    MCI_IDSTS_FATAL_BUS_ERROR | MCI_IDSTS_DESCRIPTOR_UNAVAILABLE | MCI_IDSTS_CARD_ERROR_SUMMARY;
pub(crate) const MCI_IDSTS_LATCH_ERROR_MASK: u32 =
    MCI_IDSTS_ERROR_MASK | MCI_IDSTS_ABNORMAL_SUMMARY;

mod host2;
pub use host2::{BusRequest, DataRequest, PhytiumMciIrqHandle, TransactionRequest};

#[cfg(test)]
mod tests {
    use core::{
        num::{NonZeroU16, NonZeroU32},
        ptr::NonNull,
    };

    use sdmmc_protocol::{
        cmd::{CMD0, cmd6_sd_access_mode},
        response::ResponseType,
        sdio::host::{ClockSpeed, SignalVoltage},
    };

    use crate::{
        DataDirection, PhytiumMci,
        command::encode_command,
        host2::{PHYTIUM_REGISTER_RETRY_DELAY, supports_owned_dma_transaction},
        regs::{Ctrl, RegisterBlockVolatileFieldAccess, Uhs},
        timing::{MediaKind, TimingTable},
    };

    #[test]
    fn controller_card_detect_matches_linux_active_low_semantics() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let host = unsafe { PhytiumMci::new(base) };
        const CDETECT_WORD: usize = 0x50 / size_of::<u32>();

        unsafe {
            mmio.as_mut_ptr().add(CDETECT_WORD).write_volatile(0);
        }
        assert!(host.card_present());

        unsafe {
            mmio.as_mut_ptr().add(CDETECT_WORD).write_volatile(1);
        }
        assert!(!host.card_present());
    }

    #[test]
    fn sd_timing_table_matches_linux_phytium_clock_source_rules() {
        let init = TimingTable::for_speed(ClockSpeed::Identification, MediaKind::Sd).unwrap();
        assert_eq!(init.clk_div, 0x7e7dfa);
        assert_eq!(init.clk_src, 0x000502);
        assert!(init.use_hold);

        let default = TimingTable::for_speed(ClockSpeed::Default, MediaKind::Sd).unwrap();
        assert_eq!(default.clk_div, 0x030204);
        assert_eq!(default.clk_src, 0x000102);
        assert!(default.use_hold);

        let hs = TimingTable::for_speed(ClockSpeed::HighSpeed, MediaKind::Sd).unwrap();
        assert_eq!(hs.clk_div, 0x030204);
        assert_eq!(hs.clk_src, 0x000102);
        assert!(hs.use_hold);
    }

    #[test]
    fn mmc_timing_table_uses_mmc_specific_rates() {
        let default = TimingTable::for_speed(ClockSpeed::Default, MediaKind::Mmc).unwrap();
        assert_eq!(default.target_hz, 26_000_000);

        let high = TimingTable::for_speed(ClockSpeed::HighSpeed, MediaKind::Mmc).unwrap();
        assert_eq!(high.target_hz, 52_000_000);
    }

    #[test]
    fn unsupported_sd_clock_modes_are_rejected() {
        assert!(TimingTable::sd_for_speed(ClockSpeed::Sdr104).is_err());
    }

    #[test]
    fn ctrl_register_bits_match_phytium_mci_layout() {
        let reg = Ctrl::new()
            .with_int_enable(true)
            .with_dma_enable(true)
            .with_read_wait(true)
            .with_use_internal_dmac(true);

        assert_eq!(reg.into_bits(), (1 << 4) | (1 << 5) | (1 << 6) | (1 << 25));
    }

    #[test]
    fn r3_command_encoding_does_not_enable_crc_check() {
        let cmd = sdmmc_protocol::cmd::Command::new(1, 0, ResponseType::R3);
        let reg = encode_command(&cmd, None);
        assert!(reg.response_expect());
        assert!(!reg.check_response_crc());
    }

    #[test]
    fn cmd0_encoding_sends_initialization_clocks() {
        let reg = encode_command(&CMD0, None);
        assert!(reg.send_initialization());
        assert!(!reg.response_expect());
    }

    #[test]
    fn cmd12_encoding_marks_stop_abort() {
        let reg = encode_command(&sdmmc_protocol::cmd::CMD12, None);
        assert!(reg.stop_abort_cmd());
    }

    #[test]
    fn native_dma_accepts_sd_switch_function_payload() {
        let command = cmd6_sd_access_mode(false, 0);

        let _ = command;
        assert!(supports_owned_dma_transaction(
            64,
            1,
            64,
            DataDirection::Read
        ));
    }

    #[test]
    fn uhs_voltage_bit_tracks_signal_voltage() {
        let v180 = crate::host::uhs_bits_after_voltage(Uhs::new(), SignalVoltage::V180).unwrap();
        assert_eq!(v180.volt(), 1);

        let v330 = crate::host::uhs_bits_after_voltage(v180, SignalVoltage::V330).unwrap();
        assert_eq!(v330.volt(), 0);
    }

    #[test]
    fn command_register_keeps_hold_register_optional() {
        let cmd = sdmmc_protocol::cmd::Command::new(17, 0, ResponseType::R1);
        let without_hold = encode_command(&cmd, None).with_use_hold_reg(false);
        assert!(!without_hold.use_hold_reg());
        assert_eq!(without_hold.cmd_index(), 17);
    }

    #[test]
    fn host2_data_submit_reports_busy_without_dirtying_pending_data() {
        let mut host = unsafe { PhytiumMci::new_from_addr(0x1000_0000) };
        host.command_state = crate::command::CommandState::Issued {
            cmd: sdmmc_protocol::cmd::Command::new(0, 0, ResponseType::None),
            polls: 0,
        };
        let mut buf = [0u8; 512];
        let data = sdio_host2::DataPhase::read(
            NonZeroU16::new(512).unwrap(),
            NonZeroU32::new(1).unwrap(),
            &mut buf,
        )
        .unwrap();
        let tx = sdio_host2::Transaction::with_data(
            sdmmc_protocol::cmd::Command::new(17, 0, ResponseType::R1),
            data,
        );

        let err = match unsafe {
            <PhytiumMci as sdio_host2::SdioHost>::submit_transaction(&mut host, tx)
        } {
            Ok(_) => panic!("busy host accepted a second transaction"),
            Err(err) => err,
        };

        assert_eq!(err, sdio_host2::Error::Busy);
        assert!(host.pending_data.is_none());
        assert_eq!(host.data_blocks_remaining, 0);
    }

    #[test]
    fn command_completion_requires_acknowledged_irq_cause() {
        use sdio_host2::SdioHost;

        let mut mmio = [0u32; 256];
        let base = core::ptr::NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        let command = sdmmc_protocol::cmd::Command::new(13, 0, ResponseType::R1);
        let request_id = 7;
        host.host2_active_id = Some(request_id);
        host.command_state = crate::command::CommandState::Issued {
            cmd: command,
            polls: 0,
        };
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq
            .state
            .cache_if_current(generation, crate::MCI_INT_COMMAND_DONE, 0);
        let mut request = crate::TransactionRequest::command(
            host.host2_owner(),
            request_id,
            sdio_host2::ResponseType::R1,
        );

        assert_eq!(
            host.advance_transaction(&mut request, sdio_host2::ProgressCause::Submitted),
            Ok(sdio_host2::RequestProgress::WaitingForIrq)
        );
        assert!(!request.done);
        assert_eq!(
            host.advance_transaction(&mut request, sdio_host2::ProgressCause::AcknowledgedIrq,),
            Ok(sdio_host2::RequestProgress::Complete(Ok(
                sdio_host2::RawResponse::new(sdio_host2::ResponseType::R1, [0; 4])
            )))
        );
        assert!(request.done);
    }

    #[test]
    fn acknowledged_command_irq_advances_waiting_start_and_consumes_event() {
        use sdio_host2::SdioHost;

        let mut mmio = [0u32; 256];
        let base = core::ptr::NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        let command = sdmmc_protocol::cmd::Command::new(12, 0, ResponseType::R1b);
        let request_id = 8;
        host.host2_active_id = Some(request_id);
        host.command_state = crate::command::CommandState::WaitingStart {
            cmd: command,
            polls: 0,
        };
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq
            .state
            .cache_if_current(generation, crate::MCI_INT_COMMAND_DONE, 0);
        let mut request = crate::TransactionRequest::command(
            host.host2_owner(),
            request_id,
            sdio_host2::ResponseType::R1b,
        );

        assert_eq!(
            host.advance_transaction(&mut request, sdio_host2::ProgressCause::AcknowledgedIrq,),
            Ok(sdio_host2::RequestProgress::Complete(Ok(
                sdio_host2::RawResponse::new(sdio_host2::ResponseType::R1b, [0; 4])
            )))
        );
        assert!(request.done);
    }

    #[test]
    fn acknowledged_command_irq_survives_start_register_retry() {
        use sdio_host2::SdioHost;

        const CMD_WORD: usize = 11;
        let mut mmio = [0u32; 256];
        mmio[CMD_WORD] = crate::regs::Cmd::new().with_start_cmd(true).into_bits();
        let base = core::ptr::NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        let command = sdmmc_protocol::cmd::Command::new(0, 0, ResponseType::None);
        let request_id = 10;
        host.host2_active_id = Some(request_id);
        host.command_state = crate::command::CommandState::WaitingStart {
            cmd: command,
            polls: 0,
        };
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq
            .state
            .cache_if_current(generation, crate::MCI_INT_COMMAND_DONE, 0);
        let mut request = crate::TransactionRequest::command(
            host.host2_owner(),
            request_id,
            sdio_host2::ResponseType::None,
        );

        assert_eq!(
            host.advance_transaction(&mut request, sdio_host2::ProgressCause::AcknowledgedIrq,),
            Ok(sdio_host2::RequestProgress::RegisterPending {
                retry_after: PHYTIUM_REGISTER_RETRY_DELAY,
            })
        );
        assert_eq!(host.irq.state.pending_status(), crate::MCI_INT_COMMAND_DONE);
        unsafe {
            mmio.as_mut_ptr().add(CMD_WORD).write_volatile(0);
        }
        assert_eq!(
            host.advance_transaction(&mut request, sdio_host2::ProgressCause::RegisterRetry,),
            Ok(sdio_host2::RequestProgress::Complete(Ok(
                sdio_host2::RawResponse::new(sdio_host2::ResponseType::None, [0; 4])
            )))
        );
        assert_eq!(host.irq.state.pending_status(), 0);
        assert!(request.done);
    }

    #[test]
    fn r1b_completion_waits_for_busy_release_after_command_irq() {
        use sdio_host2::SdioHost;

        const STATUS_WORD: usize = 18;
        let mut mmio = [0u32; 256];
        mmio[STATUS_WORD] = crate::regs::Status::new().with_data_busy(true).into_bits();
        let base = core::ptr::NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        let command = sdmmc_protocol::cmd::Command::new(12, 0, ResponseType::R1b);
        let request_id = 9;
        host.host2_active_id = Some(request_id);
        host.command_state = crate::command::CommandState::Issued {
            cmd: command,
            polls: 0,
        };
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq
            .state
            .cache_if_current(generation, crate::MCI_INT_COMMAND_DONE, 0);
        let mut request = crate::TransactionRequest::command(
            host.host2_owner(),
            request_id,
            sdio_host2::ResponseType::R1b,
        );

        assert_eq!(
            host.advance_transaction(&mut request, sdio_host2::ProgressCause::AcknowledgedIrq,),
            Ok(sdio_host2::RequestProgress::RegisterPending {
                retry_after: PHYTIUM_REGISTER_RETRY_DELAY,
            })
        );
        assert!(!request.done);

        unsafe {
            mmio.as_mut_ptr()
                .add(STATUS_WORD)
                .write_volatile(crate::regs::Status::new().into_bits());
        }
        assert_eq!(
            host.advance_transaction(&mut request, sdio_host2::ProgressCause::RegisterRetry,),
            Ok(sdio_host2::RequestProgress::Complete(Ok(
                sdio_host2::RawResponse::new(sdio_host2::ResponseType::R1b, [0; 4])
            )))
        );
        assert!(request.done);
    }

    #[test]
    fn stop_command_rejects_a_latched_idmac_error() {
        let mut mmio = [0u32; 256];
        let base = core::ptr::NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        host.command_state = crate::command::CommandState::Issued {
            cmd: sdmmc_protocol::cmd::CMD12,
            polls: 0,
        };
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(
            generation,
            crate::MCI_INT_COMMAND_DONE,
            crate::MCI_IDSTS_FATAL_BUS_ERROR,
        );

        assert!(matches!(
            host.advance_command_for_cause(true),
            Err(sdmmc_protocol::Error::BusError(context))
                if context.phase == sdmmc_protocol::Phase::BusyWait
                    && context.cmd == Some(12)
        ));
    }

    #[test]
    fn reset_all_restores_the_enabled_completion_irq_contract() {
        use sdio_host2::{BusOp, ProgressCause, RequestProgress, SdioHost};

        const CTRL_WORD: usize = 0;
        const CMD_WORD: usize = 11;
        const CLOCK_STATUS_WORD: usize = 22;
        const BMOD_WORD: usize = 32;
        const EXT_CLOCK_DIVIDER_WORD: usize = 0x114 / size_of::<u32>();

        let mut mmio = [0u32; 256];
        mmio[CLOCK_STATUS_WORD] = crate::regs::ClockStatus::new().with_ready(true).into_bits();
        let base = core::ptr::NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        host.enable_completion_irq();
        let mut request = unsafe { host.submit_bus_op(BusOp::ResetAll) }.unwrap();

        assert_eq!(
            host.advance_bus_op(&mut request, ProgressCause::Submitted),
            Ok(RequestProgress::RegisterPending {
                retry_after: PHYTIUM_REGISTER_RETRY_DELAY,
            })
        );

        // Model the controller and IDMAC self-clearing their reset bits.
        unsafe {
            mmio.as_mut_ptr().add(CTRL_WORD).write_volatile(0);
        }
        assert!(matches!(
            host.advance_bus_op(&mut request, ProgressCause::RegisterRetry),
            Ok(RequestProgress::RegisterPending { .. })
        ));
        unsafe {
            mmio.as_mut_ptr().add(BMOD_WORD).write_volatile(0);
        }
        assert!(matches!(
            host.advance_bus_op(&mut request, ProgressCause::RegisterRetry),
            Ok(RequestProgress::RegisterPending { .. })
        ));

        // Advance the register-only identification-clock sequence. Hardware
        // clears CMD.START after each update-clock command.
        assert!(matches!(
            host.advance_bus_op(&mut request, ProgressCause::RegisterRetry),
            Ok(RequestProgress::RegisterPending { .. })
        ));
        assert!(matches!(
            host.advance_bus_op(&mut request, ProgressCause::RegisterRetry),
            Ok(RequestProgress::RegisterPending { .. })
        ));
        unsafe {
            mmio.as_mut_ptr().add(CMD_WORD).write_volatile(0);
        }
        assert!(matches!(
            host.advance_bus_op(&mut request, ProgressCause::RegisterRetry),
            Ok(RequestProgress::RegisterPending { .. })
        ));
        assert!(matches!(
            host.advance_bus_op(&mut request, ProgressCause::RegisterRetry),
            Ok(RequestProgress::RegisterPending { .. })
        ));
        unsafe {
            mmio.as_mut_ptr().add(CMD_WORD).write_volatile(0);
        }
        assert_eq!(
            host.advance_bus_op(&mut request, ProgressCause::RegisterRetry),
            Ok(RequestProgress::Complete(Ok(())))
        );

        assert!(host.completion_irq_enabled());
        assert!(host.regs.ctrl().read().int_enable());
        assert_ne!(host.regs.intmask().read(), 0);
        assert_eq!(mmio[EXT_CLOCK_DIVIDER_WORD], 500);
    }
}
