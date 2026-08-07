//! Synopsys DesignWare Mobile Storage Host Controller (DW_mshc) backend
//! for the [`sdmmc-protocol`](sdmmc_protocol) driver crate.
//!
//! Implements [`sdio_host2::SdioHost`] for the IP block known
//! variously as DWC_mobile_storage, dw_mshc, dw_mmc (Linux), or simply
//! the "Synopsys SD/MMC controller" — the same core used in Rockchip
//! RK33xx/RK35xx, Allwinner A-series, StarFive JH7110, and a long
//! tail of mid-range SoCs. Block I/O uses the internal DMAC (IDMAC)
//! exclusively and completes through acknowledged IRQ events.
//!
//! # Scope
//!
//! - **Implemented**: IDMAC descriptor transfers,
//!   1-bit / 4-bit / 8-bit bus selection,
//!   default / high-speed / UHS-I / HS200 clocking, DW_mshc UHS DDR
//!   and 1.8 V signaling bits, R1/R1b/R2/R3/R4/R5/R6/R7 response
//!   decoding, software reset.
//! - **Out of scope (for now)**: external-DMA path, controller-specific
//!   DLL/strobe/tuning window setup (CMD19/CMD21).
//!
//! # Usage
//!
//! ```rust,no_run
//! use core::ptr::NonNull;
//!
//! use dma_api::DeviceDma;
//! use dwmmc_host::DwMmc;
//! use sdmmc_protocol::sdio::card::SdioSdmmc;
//!
//! // SAFETY: 0xFE2B_0000 must point at a valid DW_mshc register file
//! // the caller has exclusive access to.
//! let mmio = NonNull::new(0xFE2B_0000 as *mut u8).unwrap();
//! let mut host = unsafe { DwMmc::new(mmio) };
//! host.set_reference_clock(50_000_000);
//! let dma: DeviceDma = todo!("install the platform DMA capability");
//! host.configure_dma(dma)?;
//!
//! let mut card = SdioSdmmc::new(host);
//! let mut request = card.submit_init()?;
//! // Advance `request` only for acknowledged IRQs or bounded register retries.
//! # Ok::<(), sdmmc_protocol::Error>(())
//! ```
//!
//! The runtime block queue adapter belongs in OS/platform glue. The reusable
//! driver crate exposes request state and event-driven advance primitives instead:
//!
//! ```compile_fail
//! use dwmmc_host::BlockQueue;
//! ```
//!
//! Construction is `unsafe` because the caller must guarantee that
//! the supplied address is a valid, exclusively-owned DW_mshc
//! register file.

#![no_std]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

use alloc::sync::Arc;
use core::{marker::PhantomData, ptr::NonNull, time::Duration};

use log::warn;

mod command;
mod dma;
mod fifo;
mod host;
mod regs;

pub use sdmmc_protocol::block::{
    BlockRequestId, BlockTransferDirection, BlockTransferMode, BlockTransferState,
};
use sdmmc_protocol::{
    CommandResponseProgress, DataCommandProgress,
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    sdio::host::{
        BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, SdioIrqHandle,
        SdioIrqHost, SignalVoltage,
    },
};

use crate::regs::RegisterBlockVolatileFieldAccess;
pub use crate::{
    dma::{
        BlockRequest, BlockRequestSlot, IDMAC_DESC_ALIGN, IDMAC_DESC_SIZE, IDMAC_MAX_BLOCKS,
        IDMAC_MAX_TRANSFER_SIZE, RequestId,
    },
    fifo::{FifoConfig, FifoDataWidth},
    host::{CardDetect, DwMmc, HostClock},
};

/// Stable controller event extracted from DW_mshc raw interrupt status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Event {
    /// No status bit requiring runtime action is currently pending.
    #[default]
    None,
    /// A command response has completed.
    CommandComplete,
    /// A data transfer has completed.
    TransferComplete,
    /// One or more controller error bits are pending.
    Error { raw_status: u32 },
    /// Status bits are pending but do not map to a high-level event yet.
    Other { raw_status: u32 },
}

mod host2;
pub use host2::{BusRequest, DataRequest, DwMmcIrq, TransactionRequest};
pub(crate) use host2::{
    DWMMC_INT_COMMAND_DONE, DWMMC_INT_DATA_TRANSFER_OVER, DWMMC_INT_ERROR_MASK,
    DWMMC_LATCH_IDMAC_COMPLETE, DWMMC_LATCH_IDMAC_ERROR,
};

fn clock_hz_for_speed(speed: ClockSpeed) -> u32 {
    match speed {
        ClockSpeed::Identification => 400_000,
        ClockSpeed::Default | ClockSpeed::Sdr12 => 25_000_000,
        ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => 50_000_000,
        ClockSpeed::Sdr50 | ClockSpeed::Ddr50 => 50_000_000,
        ClockSpeed::Sdr104 => 104_000_000,
        ClockSpeed::Hs200 => 200_000_000,
        // Future ClockSpeed variants: unknown frequency, signal 0.
        _ => 0,
    }
}

fn dwmmc_clock_divisor(ref_clock_hz: u32, target_hz: u32) -> u8 {
    if ref_clock_hz == 0 || target_hz == 0 || target_hz >= ref_clock_hz {
        0
    } else {
        ref_clock_hz.div_ceil(2 * target_hz).min(0xFF) as u8
    }
}

pub(crate) fn ddr_mask_for_speed(speed: ClockSpeed) -> u16 {
    match speed {
        ClockSpeed::Ddr50 => 1,
        _ => 0,
    }
}

pub(crate) fn volt_mask_for_signal(voltage: SignalVoltage) -> Result<u16, Error> {
    match voltage {
        SignalVoltage::V330 => Ok(0),
        SignalVoltage::V180 => Ok(1),
        SignalVoltage::V120 => Err(Error::UnsupportedCommand),
        // Future SignalVoltage variants are not supported by this controller.
        _ => Err(Error::UnsupportedCommand),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UhsBits {
    pub ddr: u16,
    pub volt: u16,
}

pub(crate) fn uhs_bits_after_speed(cur: UhsBits, speed: ClockSpeed) -> UhsBits {
    UhsBits {
        ddr: ddr_mask_for_speed(speed),
        ..cur
    }
}

pub(crate) fn uhs_bits_after_voltage(
    cur: UhsBits,
    voltage: SignalVoltage,
) -> Result<UhsBits, Error> {
    Ok(UhsBits {
        volt: volt_mask_for_signal(voltage)?,
        ..cur
    })
}

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU16, NonZeroU32};

    use sdio_host2::ResponseType;

    use super::*;
    use crate::host2::{DWMMC_REGISTER_RETRY_DELAY, event_from_raw_status};

    #[test]
    fn bus_width_contract_is_closed_and_exhaustive() {
        fn width_bits(width: sdio_host2::BusWidth) -> u8 {
            match width {
                sdio_host2::BusWidth::Bit1 => 1,
                sdio_host2::BusWidth::Bit4 => 4,
                sdio_host2::BusWidth::Bit8 => 8,
            }
        }

        assert_eq!(width_bits(sdio_host2::BusWidth::Bit1), 1);
        assert_eq!(width_bits(sdio_host2::BusWidth::Bit4), 4);
        assert_eq!(width_bits(sdio_host2::BusWidth::Bit8), 8);
    }

    #[test]
    fn irq_capability_trait_controls_hardware_interrupt_mask() {
        const CTRL_WORD: usize = 0;
        const INTMASK_WORD: usize = 9;

        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };

        assert!(!sdmmc_protocol::sdio::SdioIrqHost::completion_irq_enabled(
            &host
        ));
        sdmmc_protocol::sdio::SdioIrqHost::enable_completion_irq(&mut host).unwrap();
        assert!(sdmmc_protocol::sdio::SdioIrqHost::completion_irq_enabled(
            &host
        ));
        assert_ne!(mmio[INTMASK_WORD], 0);
        assert_ne!(mmio[CTRL_WORD] & (1 << 4), 0);

        sdmmc_protocol::sdio::SdioIrqHost::disable_completion_irq(&mut host).unwrap();
        assert!(!sdmmc_protocol::sdio::SdioIrqHost::completion_irq_enabled(
            &host
        ));
        assert_eq!(mmio[INTMASK_WORD], 0);
        assert_eq!(mmio[CTRL_WORD] & (1 << 4), 0);
    }

    #[test]
    fn event_reports_command_completion_without_os_wakeup_policy() {
        let raw = crate::regs::RIntSts::new()
            .with_command_done(true)
            .into_bits();

        assert_eq!(event_from_raw_status(raw), Event::CommandComplete);
    }

    #[test]
    fn event_reports_transfer_completion_without_os_wakeup_policy() {
        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .into_bits();

        assert_eq!(event_from_raw_status(raw), Event::TransferComplete);
    }

    #[test]
    fn event_reports_error_status_without_translating_to_os_action() {
        let raw = crate::regs::RIntSts::new()
            .with_response_timeout(true)
            .into_bits();

        assert_eq!(event_from_raw_status(raw), Event::Error { raw_status: raw });
    }

    #[test]
    fn event_reports_data_completion_source_for_runtime_wakeup() {
        use sdmmc_protocol::sdio::host::{HostEvent, HostEventKind, HostEventSource};

        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .into_bits();
        let event = event_from_raw_status(raw);

        assert_eq!(event.kind(), HostEventKind::TransferComplete);
        assert_eq!(event.source(), HostEventSource::Data);
        assert_eq!(event.queue_id(), Some(BlockRequestId::new(0)));
    }

    #[test]
    fn host2_data_submit_reports_busy_without_dirtying_pending_data() {
        let mut host = unsafe { DwMmc::new_from_addr(0x1000_0000) };
        host.command_state = command::CommandState::Issued {
            cmd: Command::new(0, 0, ResponseType::None),
            polls: 0,
        };
        let mut buf = [0u8; 512];
        let data = sdio_host2::DataPhase::read(
            NonZeroU16::new(512).unwrap(),
            NonZeroU32::new(1).unwrap(),
            &mut buf,
        )
        .unwrap();
        let tx = sdio_host2::Transaction::with_data(Command::new(17, 0, ResponseType::R1), data);

        let err =
            match unsafe { <DwMmc as sdio_host2::SdioHost>::submit_transaction(&mut host, tx) } {
                Ok(_) => panic!("busy host accepted a second transaction"),
                Err(err) => err,
            };

        assert_eq!(err, sdio_host2::Error::Busy);
        assert!(host.pending_data.is_none());
        assert_eq!(host.data_blocks_remaining, 0);
    }

    #[test]
    fn acknowledged_command_irq_advances_waiting_start_and_consumes_event() {
        use sdio_host2::SdioHost;

        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        let command = Command::new(13, 0, ResponseType::R1);
        let request_id = 7;
        host.host2_active_id = Some(request_id);
        host.command_state = command::CommandState::WaitingStart {
            cmd: command,
            polls: 0,
        };
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq
            .state
            .cache_if_current(generation, DWMMC_INT_COMMAND_DONE);
        let mut request = TransactionRequest::command(
            host.host2_owner(),
            request_id,
            sdio_host2::ResponseType::R1,
        );

        assert_eq!(
            host.advance_transaction(&mut request, sdio_host2::ProgressCause::AcknowledgedIrq,),
            Ok(sdio_host2::RequestProgress::Complete(Ok(
                sdio_host2::RawResponse::new(sdio_host2::ResponseType::R1, [0; 4])
            )))
        );
        assert!(request.done);
    }

    #[test]
    fn r1b_completion_waits_for_busy_release_after_command_irq() {
        use sdio_host2::SdioHost;

        const STATUS_WORD: usize = 18;
        let mut mmio = [0u32; 256];
        mmio[STATUS_WORD] = crate::regs::Status::new().with_data_busy(true).into_bits();
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        let command = Command::new(12, 0, ResponseType::R1b);
        let request_id = 8;
        host.host2_active_id = Some(request_id);
        host.command_state = command::CommandState::Issued {
            cmd: command,
            polls: 0,
        };
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq
            .state
            .cache_if_current(generation, DWMMC_INT_COMMAND_DONE);
        let mut request = TransactionRequest::command(
            host.host2_owner(),
            request_id,
            sdio_host2::ResponseType::R1b,
        );

        assert_eq!(
            host.advance_transaction(&mut request, sdio_host2::ProgressCause::AcknowledgedIrq,),
            Ok(sdio_host2::RequestProgress::RegisterPending {
                retry_after: DWMMC_REGISTER_RETRY_DELAY,
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
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        host.command_state = command::CommandState::Issued {
            cmd: sdmmc_protocol::cmd::CMD12,
            polls: 0,
        };
        host.irq.state.begin_request();
        let generation = host.irq.state.generation();
        host.irq
            .state
            .cache_if_current(generation, DWMMC_INT_COMMAND_DONE | DWMMC_LATCH_IDMAC_ERROR);

        assert!(matches!(
            host.advance_command_for_cause(true),
            Err(Error::BusError(context))
                if context.phase == Phase::BusyWait && context.cmd == Some(12)
        ));
    }

    #[test]
    fn owned_irq_endpoint_acks_and_caches_status() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        host.irq.state.begin_request();
        host.enable_completion_irq();
        let old_generation = host.irq.state.generation();
        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .into_bits();
        const MINTSTS_WORD: usize = 16;
        unsafe {
            mmio.as_mut_ptr().add(MINTSTS_WORD).write_volatile(raw);
        }

        let mut irq = host.irq_endpoint();

        assert_eq!(irq.handle_irq(), Event::TransferComplete);
        assert_eq!(host.irq.state.pending(), raw);
        unsafe {
            mmio.as_mut_ptr().add(MINTSTS_WORD).write_volatile(0);
        }
        assert_eq!(host.irq_endpoint().handle_irq(), Event::None);

        host.irq.state.end_request();
        host.irq.state.begin_request();
        assert_ne!(host.irq.state.generation(), old_generation);
        host.irq
            .state
            .cache_if_current(old_generation, crate::DWMMC_INT_DATA_TRANSFER_OVER);
        assert_eq!(host.irq.state.pending(), 0);
    }

    #[test]
    fn masked_controller_status_is_acked_without_publishing_an_event() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        host.irq.state.begin_request();
        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .into_bits();
        const MINTSTS_WORD: usize = 16;
        unsafe {
            mmio.as_mut_ptr().add(MINTSTS_WORD).write_volatile(raw);
        }

        let mut irq = host.irq_endpoint();

        assert_eq!(irq.handle_irq(), Event::None);
        assert_eq!(host.irq.state.pending(), 0);
    }

    #[test]
    fn idmac_irq_completion_is_latched_separately_from_controller_data_over() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const IDSTS_WORD: usize = 35;
        const IDINTEN_WORD: usize = 36;
        host.irq.state.begin_request();
        unsafe {
            mmio.as_mut_ptr()
                .add(IDINTEN_WORD)
                .write_volatile(dma::IDMAC_INT_TI);
            mmio.as_mut_ptr()
                .add(IDSTS_WORD)
                .write_volatile(dma::IDMAC_INT_TI);
        }

        let mut irq = host.irq_endpoint();

        assert_eq!(
            irq.handle_irq(),
            Event::Other {
                raw_status: DWMMC_LATCH_IDMAC_COMPLETE
            }
        );
        assert_eq!(host.irq.state.pending(), DWMMC_LATCH_IDMAC_COMPLETE);
        let cleared = unsafe { mmio.as_ptr().add(IDSTS_WORD).read_volatile() };
        assert_eq!(cleared, dma::IDMAC_INT_TI);
    }

    #[test]
    fn disabled_idmac_completion_is_acked_without_runtime_wakeup() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const IDSTS_WORD: usize = 35;
        host.irq.state.begin_request();
        unsafe {
            mmio.as_mut_ptr()
                .add(IDSTS_WORD)
                .write_volatile(dma::IDMAC_INT_TI);
        }

        let mut irq = host.irq_endpoint();

        assert_eq!(irq.handle_irq(), Event::None);
        assert_eq!(host.irq.state.pending(), 0);
        assert_eq!(
            unsafe { mmio.as_ptr().add(IDSTS_WORD).read_volatile() },
            dma::IDMAC_INT_TI
        );
    }

    #[test]
    fn idmac_error_irq_is_acked_and_cached_as_error() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const IDSTS_WORD: usize = 35;
        const IDINTEN_WORD: usize = 36;
        let raw = dma::IDMAC_INT_FBE | dma::IDMAC_INT_AI;
        host.irq.state.begin_request();
        unsafe {
            mmio.as_mut_ptr().add(IDINTEN_WORD).write_volatile(raw);
            mmio.as_mut_ptr().add(IDSTS_WORD).write_volatile(raw);
        }

        let mut irq = host.irq_endpoint();

        assert_eq!(
            irq.handle_irq(),
            Event::Error {
                raw_status: DWMMC_LATCH_IDMAC_ERROR
            }
        );
        assert_eq!(host.irq.state.pending(), DWMMC_LATCH_IDMAC_ERROR);
        assert_eq!(
            unsafe { mmio.as_ptr().add(IDSTS_WORD).read_volatile() },
            raw
        );
    }

    #[test]
    fn clear_all_int_status_matches_linux_w1c_all_bits() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let host = unsafe { DwMmc::new(base) };
        const RINTSTS_WORD: usize = 17;
        unsafe {
            mmio.as_mut_ptr()
                .add(RINTSTS_WORD)
                .write_volatile(crate::DWMMC_INT_COMMAND_DONE);
        }

        host.clear_all_int_status();

        let written = unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() };
        assert_eq!(written, u32::MAX);
    }

    #[test]
    fn host2_reset_programs_linux_baseline_without_clock_update() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        let mut request = unsafe {
            <DwMmc as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
        }
        .unwrap();
        const CTRL_WORD: usize = 0;
        const TMOUT_WORD: usize = 5;
        const CMD_WORD: usize = 11;
        const RINTSTS_WORD: usize = 17;
        const FIFOTH_WORD: usize = 19;
        const EXPECTED_FIFOTH: u32 = (0x2 << 28) | (0x7f << 16) | 0x80;

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::advance_bus_op(
                &mut host,
                &mut request,
                sdio_host2::ProgressCause::Submitted,
            )
            .unwrap(),
            sdio_host2::RequestProgress::RegisterPending { .. }
        ));
        unsafe {
            mmio.as_mut_ptr().add(CTRL_WORD).write_volatile(0);
        }
        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::advance_bus_op(
                &mut host,
                &mut request,
                sdio_host2::ProgressCause::RegisterRetry,
            )
            .unwrap(),
            sdio_host2::RequestProgress::Complete(Ok(()))
        ));

        assert_eq!(
            unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() },
            u32::MAX
        );
        assert_eq!(
            unsafe { mmio.as_ptr().add(TMOUT_WORD).read_volatile() },
            u32::MAX
        );
        assert_eq!(
            unsafe { mmio.as_ptr().add(FIFOTH_WORD).read_volatile() },
            EXPECTED_FIFOTH
        );
        assert_eq!(unsafe { mmio.as_ptr().add(CMD_WORD).read_volatile() }, 0);
    }

    #[test]
    fn host2_reset_restores_the_enabled_completion_irq_contract() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const CTRL_WORD: usize = 0;
        const INTMASK_WORD: usize = 9;

        sdmmc_protocol::sdio::SdioIrqHost::enable_completion_irq(&mut host).unwrap();
        let mut request = unsafe {
            <DwMmc as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
        }
        .unwrap();

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::advance_bus_op(
                &mut host,
                &mut request,
                sdio_host2::ProgressCause::Submitted,
            )
            .unwrap(),
            sdio_host2::RequestProgress::RegisterPending { .. }
        ));
        unsafe {
            mmio.as_mut_ptr().add(CTRL_WORD).write_volatile(0);
        }
        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::advance_bus_op(
                &mut host,
                &mut request,
                sdio_host2::ProgressCause::RegisterRetry,
            )
            .unwrap(),
            sdio_host2::RequestProgress::Complete(Ok(()))
        ));

        assert!(sdmmc_protocol::sdio::SdioIrqHost::completion_irq_enabled(
            &host
        ));
        assert_ne!(mmio[CTRL_WORD] & (1 << 4), 0);
        assert_ne!(mmio[INTMASK_WORD], 0);
    }

    #[test]
    fn host2_power_on_resets_after_enabling_pwren() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        let mut request = unsafe {
            <DwMmc as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::PowerOn)
        }
        .unwrap();
        const CTRL_WORD: usize = 0;
        const PWREN_WORD: usize = 1;
        const TMOUT_WORD: usize = 5;
        const RINTSTS_WORD: usize = 17;

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::advance_bus_op(
                &mut host,
                &mut request,
                sdio_host2::ProgressCause::Submitted,
            )
            .unwrap(),
            sdio_host2::RequestProgress::RegisterPending { .. }
        ));
        assert_eq!(unsafe { mmio.as_ptr().add(PWREN_WORD).read_volatile() }, 1);
        let ctrl =
            crate::regs::Ctrl::from_bits(unsafe { mmio.as_ptr().add(CTRL_WORD).read_volatile() });
        assert!(ctrl.controller_reset());
        assert!(ctrl.fifo_reset());
        assert!(ctrl.dma_reset());

        unsafe {
            mmio.as_mut_ptr().add(CTRL_WORD).write_volatile(0);
        }
        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::advance_bus_op(
                &mut host,
                &mut request,
                sdio_host2::ProgressCause::RegisterRetry,
            )
            .unwrap(),
            sdio_host2::RequestProgress::Complete(Ok(()))
        ));
        assert_eq!(
            unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() },
            u32::MAX
        );
        assert_eq!(
            unsafe { mmio.as_ptr().add(TMOUT_WORD).read_volatile() },
            u32::MAX
        );
    }

    #[test]
    fn absent_controller_card_detect_rejects_command_before_issue() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const CMD_WORD: usize = 11;
        const CDETECT_WORD: usize = 20;
        unsafe {
            mmio.as_mut_ptr().add(CDETECT_WORD).write_volatile(1);
        }

        let err = host
            .submit_command(&Command::new(8, 0x1aa, ResponseType::R7))
            .expect_err("absent card must not issue a command");

        assert_eq!(err, Error::NoCard);
        assert_eq!(unsafe { mmio.as_ptr().add(CMD_WORD).read_volatile() }, 0);
        assert!(matches!(host.command_state, command::CommandState::Idle));
    }

    #[test]
    fn host2_set_clock_rewrites_clksrc_like_linux_setup_bus() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        host.set_reference_clock(50_000_000);
        const CLKSRC_WORD: usize = 3;
        unsafe {
            mmio.as_mut_ptr()
                .add(CLKSRC_WORD)
                .write_volatile(0xdead_beef);
        }
        let mut request = unsafe {
            <DwMmc as sdio_host2::SdioHost>::submit_bus_op(
                &mut host,
                sdio_host2::BusOp::SetClock(sdio_host2::ClockSpeed::Identification),
            )
        }
        .unwrap();

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::advance_bus_op(
                &mut host,
                &mut request,
                sdio_host2::ProgressCause::Submitted,
            )
            .unwrap(),
            sdio_host2::RequestProgress::RegisterPending { .. }
        ));

        assert_eq!(unsafe { mmio.as_ptr().add(CLKSRC_WORD).read_volatile() }, 0);
    }

    #[test]
    fn host2_external_clock_returned_bus_hz_feeds_dwmmc_divider() {
        struct Clock;

        impl HostClock for Clock {
            fn set_clock(&self, target_hz: u32) -> Result<u32, Error> {
                assert_eq!(target_hz, 400_000);
                Ok(400_000)
            }
        }

        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        host.set_reference_clock(50_000_000);
        host.set_external_clock(Clock);
        let mut request = unsafe {
            <DwMmc as sdio_host2::SdioHost>::submit_bus_op(
                &mut host,
                sdio_host2::BusOp::SetClock(sdio_host2::ClockSpeed::Identification),
            )
        }
        .unwrap();
        const CMD_WORD: usize = 11;
        const CLKDIV_WORD: usize = 2;

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::advance_bus_op(
                &mut host,
                &mut request,
                sdio_host2::ProgressCause::Submitted,
            )
            .unwrap(),
            sdio_host2::RequestProgress::RegisterPending { .. }
        ));
        assert_eq!(host.reference_clock(), 50_000_000);

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::advance_bus_op(
                &mut host,
                &mut request,
                sdio_host2::ProgressCause::RegisterRetry,
            )
            .unwrap(),
            sdio_host2::RequestProgress::RegisterPending { .. }
        ));
        assert_eq!(host.reference_clock(), 400_000);
        unsafe {
            mmio.as_mut_ptr().add(CMD_WORD).write_volatile(0);
        }

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::advance_bus_op(
                &mut host,
                &mut request,
                sdio_host2::ProgressCause::RegisterRetry,
            )
            .unwrap(),
            sdio_host2::RequestProgress::RegisterPending { .. }
        ));
        assert_eq!(unsafe { mmio.as_ptr().add(CLKDIV_WORD).read_volatile() }, 0);
    }

    #[test]
    fn rintsts_error_includes_host_timeout_and_fifo_overrun() {
        assert!(crate::regs::RIntSts::new().with_host_timeout(true).error());
        assert!(
            crate::regs::RIntSts::new()
                .with_fifo_under_over_run(true)
                .error()
        );
    }

    #[test]
    fn uhs_i_sdr_modes_keep_ddr_disabled() {
        let cur = UhsBits { ddr: 1, volt: 1 };

        assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Sdr50).ddr, 0);
        assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Sdr104).ddr, 0);
        assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Hs200).ddr, 0);
    }

    #[test]
    fn ddr50_enables_ddr_mode_for_card0() {
        let cur = UhsBits { ddr: 0, volt: 1 };

        assert_eq!(
            uhs_bits_after_speed(cur, ClockSpeed::Ddr50),
            UhsBits { ddr: 1, volt: 1 }
        );
    }

    #[test]
    fn uhs_i_voltage_switch_selects_1v8_for_card0() {
        let cur = UhsBits { ddr: 1, volt: 0 };

        assert_eq!(
            uhs_bits_after_voltage(cur, SignalVoltage::V180).unwrap(),
            UhsBits { ddr: 1, volt: 1 }
        );
        assert_eq!(
            uhs_bits_after_voltage(cur, SignalVoltage::V330).unwrap(),
            UhsBits { ddr: 1, volt: 0 }
        );
    }

    #[test]
    fn unsupported_1v2_voltage_is_rejected() {
        assert_eq!(
            volt_mask_for_signal(SignalVoltage::V120).unwrap_err(),
            Error::UnsupportedCommand
        );
    }

    #[test]
    fn data_command_index_is_recorded_for_diagnostics() {
        let mut host = unsafe { DwMmc::new_from_addr(0x1000_0000) };
        host.data_cmd_index = 6;

        assert_eq!(host.data_cmd_index, 6);
    }
}
