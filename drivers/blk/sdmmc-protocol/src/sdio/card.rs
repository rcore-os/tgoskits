//! Card-facing SD/MMC command and block I/O wrapper.

use core::num::NonZeroU16;

use dma_api::{CompletedDma, CpuDmaBuffer};
use log::warn;
use sdio_host2::ProgressCause;

use super::{
    host::{BusWidth, SdioIrqHost},
    host2::{DmaSubmitError, ProtocolDataRequest, ProtocolHost},
    init::{MmcSwitchRequest, MmcSwitchRequestState, mmc_switch_deadline_passed},
    nonzero_block_size,
};
use crate::{
    block::{CommandResponseProgress, DataCommandProgress, OperationProgress},
    cmd::Command,
    common::block_addr_of,
    error::{Error, ErrorContext, Phase},
    response::{CardState, CidResponse, Response},
};

pub struct SdioSdmmc<H: SdioIrqHost + 'static> {
    pub(super) host: ProtocolHost<H>,
    pub(super) rca: u16,
    pub(super) high_capacity: bool,
    pub(super) bus_width: BusWidth,
    pub(super) kind: CardKind,
    pub(super) sd_speed_selection_enabled: bool,
    pub(super) sd_uhs_selection_enabled: bool,
}

pub struct SdioDataRequest<'a, H: SdioIrqHost + 'static> {
    pub(super) inner: ProtocolDataRequest<'a, H>,
}

impl<H: SdioIrqHost + 'static> SdioDataRequest<'static, H> {
    pub(super) fn take_completed_dma(&mut self) -> Option<CompletedDma> {
        self.inner.take_completed_dma()
    }
}

/// Submitted SDIO command transaction.
pub struct SdioCommandRequest;

/// Submitted `CMD13 SEND_STATUS` transaction.
pub struct SdioStatusRequest {
    pub(super) inner: SdioCommandRequest,
}

/// Submitted MMC `CMD8 SEND_EXT_CSD` data transaction.
pub struct ExtCsdRequest<'a, H: SdioIrqHost + 'static> {
    pub(super) inner: SdioDataRequest<'a, H>,
}

/// Submitted SD `CMD6 SWITCH_FUNC` data transaction.
pub struct SwitchFunctionRequest<'a, H: SdioIrqHost + 'static> {
    pub(super) inner: SdioDataRequest<'a, H>,
}

impl<H: SdioIrqHost + 'static> SdioSdmmc<H> {
    pub fn new(host: H) -> Self {
        Self {
            host: ProtocolHost::new(host),
            rca: 0,
            high_capacity: false,
            bus_width: BusWidth::Bit1,
            kind: CardKind::Sd,
            sd_speed_selection_enabled: true,
            sd_uhs_selection_enabled: true,
        }
    }

    /// Returns mutable access to the underlying SDIO host controller.
    pub fn host_mut(&mut self) -> &mut H {
        self.host.inner_mut()
    }

    /// Returns shared access to the underlying SDIO host controller.
    pub fn host(&self) -> &H {
        self.host.inner()
    }

    #[cfg(any(feature = "rdif", test))]
    pub(crate) fn protocol_host_mut(&mut self) -> &mut ProtocolHost<H> {
        &mut self.host
    }

    #[cfg(any(feature = "rdif", test))]
    pub(crate) const fn protocol_progress_wait(&self) -> super::host::HostProgressWait {
        self.host.progress_wait()
    }

    /// Returns whether the initialized card uses sector addressing.
    pub fn is_high_capacity(&self) -> bool {
        self.high_capacity
    }

    /// Enable or disable optional SD CMD6 speed-mode selection.
    ///
    /// When disabled, SD cards still leave identification mode and run at
    /// default speed, but the driver does not switch the card to HighSpeed or
    /// UHS-I timing.
    pub fn set_sd_speed_selection_enabled(&mut self, enabled: bool) {
        self.sd_speed_selection_enabled = enabled;
    }

    /// Enable or disable UHS-I SD access-mode selection.
    ///
    /// When disabled while SD speed selection remains enabled, initialization
    /// still uses CMD6 to select legacy HighSpeed when the card supports it,
    /// but it does not try CMD11 voltage switching, SDR50, SDR104, DDR50, or
    /// tuning.
    pub fn set_sd_uhs_selection_enabled(&mut self, enabled: bool) {
        self.sd_uhs_selection_enabled = enabled;
    }

    pub(super) fn mmc_tuning_block_size(&self) -> Result<NonZeroU16, Error> {
        let bytes = if matches!(self.bus_width, BusWidth::Bit8) {
            crate::cmd::MMC_TUNING_BLOCK_SIZE_8BIT
        } else {
            crate::cmd::SD_TUNING_BLOCK_SIZE
        };
        nonzero_block_size(bytes)
    }

    pub(super) fn sd_tuning_block_size(&self) -> Result<NonZeroU16, Error> {
        nonzero_block_size(crate::cmd::SD_TUNING_BLOCK_SIZE)
    }

    /// Which card family the driver detected. Meaningful only after a
    /// successful [`init`](Self::init); defaults to [`CardKind::Sd`].
    pub fn kind(&self) -> CardKind {
        self.kind
    }

    /// Currently published Relative Card Address. `0` until [`init`](Self::init)
    /// has run successfully.
    pub fn rca(&self) -> u16 {
        self.rca
    }

    pub fn submit_read_blocks_into<'a>(
        &mut self,
        addr: u32,
        buf: &'a mut [u8],
    ) -> Result<SdioDataRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let count = block_count_from_len(buf.len())?;
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = if count == 1 {
            crate::cmd::cmd17(block_addr)
        } else {
            crate::cmd::cmd18(block_addr)
        };
        let inner = self.host.submit_read_data(&cmd, buf, 512, count)?;
        Ok(SdioDataRequest { inner })
    }

    pub fn submit_write_blocks_from<'a>(
        &mut self,
        addr: u32,
        buf: &'a [u8],
    ) -> Result<SdioDataRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let count = block_count_from_len(buf.len())?;
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = if count == 1 {
            crate::cmd::cmd24(block_addr)
        } else {
            crate::cmd::cmd25(block_addr)
        };
        let inner = self.host.submit_write_data(&cmd, buf, 512, count)?;
        Ok(SdioDataRequest { inner })
    }

    pub fn advance_data_request<'a>(
        &mut self,
        request: &mut SdioDataRequest<'a, H>,
        cause: ProgressCause,
    ) -> Result<DataCommandProgress, Error>
    where
        H: 'a,
    {
        self.host.advance_data_request(&mut request.inner, cause)
    }

    pub(crate) fn abort_data_request(
        &mut self,
        request: &mut SdioDataRequest<'_, H>,
    ) -> (Result<(), Error>, Option<CompletedDma>) {
        let result = self.host.abort_data_request(&mut request.inner);
        let completed = request.inner.take_completed_dma();
        (result, completed)
    }

    pub fn submit_command_request(&mut self, cmd: &Command) -> Result<SdioCommandRequest, Error> {
        self.host.submit_command(cmd)?;
        Ok(SdioCommandRequest)
    }

    pub fn advance_command_request(
        &mut self,
        _request: &mut SdioCommandRequest,
        cause: ProgressCause,
    ) -> Result<CommandResponseProgress, Error> {
        self.host.advance_command_response(cause)
    }

    pub fn submit_status(&mut self) -> Result<SdioStatusRequest, Error> {
        let cmd = crate::cmd::cmd13(self.rca);
        let inner = self.submit_command_request(&cmd)?;
        Ok(SdioStatusRequest { inner })
    }

    pub fn advance_status_request(
        &mut self,
        request: &mut SdioStatusRequest,
        cause: ProgressCause,
    ) -> Result<OperationProgress<CardState>, Error> {
        match self.advance_command_request(&mut request.inner, cause)? {
            CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
            CommandResponseProgress::Complete(Response::R1(r1)) => {
                Ok(OperationProgress::Complete(r1.current_state()))
            }
            CommandResponseProgress::Complete(_) => Err(Error::BadResponse(ErrorContext::for_cmd(
                Phase::ResponseWait,
                13,
            ))),
        }
    }

    #[cfg(any(feature = "rdif", test))]
    pub(crate) fn abort_status_request(
        &mut self,
        _request: &mut SdioStatusRequest,
    ) -> Result<(), Error> {
        self.host.abort_command_request()
    }

    pub fn submit_read_data_command<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<SdioDataRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let inner = self
            .host
            .submit_read_data(cmd, buf, block_size, block_count)?;
        Ok(SdioDataRequest { inner })
    }

    pub fn submit_read_ext_csd<'a>(
        &mut self,
        buf: &'a mut [u8; 512],
    ) -> Result<ExtCsdRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let inner = self.submit_read_data_command(&crate::cmd::CMD8_MMC, buf, 512, 1)?;
        Ok(ExtCsdRequest { inner })
    }

    pub(super) fn submit_read_ext_csd_dma(
        &mut self,
        buffer: CpuDmaBuffer,
    ) -> Result<ExtCsdRequest<'static, H>, DmaSubmitError> {
        let inner = self.host.submit_dma_data(
            &crate::cmd::CMD8_MMC,
            sdio_host2::DataDirection::Read,
            buffer.prepare_for_device(),
            512,
            1,
        )?;
        Ok(ExtCsdRequest {
            inner: SdioDataRequest { inner },
        })
    }

    pub fn advance_ext_csd_request<'a>(
        &mut self,
        request: &mut ExtCsdRequest<'a, H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<()>, Error>
    where
        H: 'a,
    {
        match self.advance_data_request(&mut request.inner, cause)? {
            DataCommandProgress::Pending => Ok(OperationProgress::Pending),
            DataCommandProgress::Complete(_) => Ok(OperationProgress::Complete(())),
        }
    }

    pub fn submit_switch_function<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8; 64],
    ) -> Result<SwitchFunctionRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let inner = self.submit_read_data_command(cmd, buf, 64, 1)?;
        Ok(SwitchFunctionRequest { inner })
    }

    pub(super) fn submit_switch_function_dma(
        &mut self,
        cmd: &Command,
        buffer: CpuDmaBuffer,
    ) -> Result<SwitchFunctionRequest<'static, H>, DmaSubmitError> {
        let inner = self.host.submit_dma_data(
            cmd,
            sdio_host2::DataDirection::Read,
            buffer.prepare_for_device(),
            64,
            1,
        )?;
        Ok(SwitchFunctionRequest {
            inner: SdioDataRequest { inner },
        })
    }

    pub fn advance_switch_function_request<'a>(
        &mut self,
        request: &mut SwitchFunctionRequest<'a, H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<()>, Error>
    where
        H: 'a,
    {
        match self.advance_data_request(&mut request.inner, cause)? {
            DataCommandProgress::Pending => Ok(OperationProgress::Pending),
            DataCommandProgress::Complete(_) => Ok(OperationProgress::Complete(())),
        }
    }

    pub fn submit_mmc_switch(
        &mut self,
        access: u8,
        index: u8,
        value: u8,
    ) -> Result<MmcSwitchRequest, Error> {
        let cmd = crate::cmd::cmd6_mmc_switch(access, index, value);
        let started_ms = self.host.now_ms();
        self.host.submit_command(&cmd)?;
        Ok(MmcSwitchRequest {
            rca: self.rca,
            index,
            value,
            polls: 0,
            started_ms,
            state: MmcSwitchRequestState::PollSwitch,
        })
    }

    pub fn advance_mmc_switch_request(
        &mut self,
        request: &mut MmcSwitchRequest,
        cause: ProgressCause,
    ) -> Result<OperationProgress<()>, Error> {
        match request.state {
            MmcSwitchRequestState::PollSwitch => {
                match self.host.advance_command_response(cause)? {
                    CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
                    CommandResponseProgress::Complete(_) => {
                        let cmd = crate::cmd::cmd13(request.rca);
                        self.host.submit_command(&cmd)?;
                        request.state = MmcSwitchRequestState::PollStatus;
                        Ok(OperationProgress::Pending)
                    }
                }
            }
            MmcSwitchRequestState::PollStatus => {
                match self.host.advance_command_response(cause)? {
                    CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
                    CommandResponseProgress::Complete(Response::R1(r1)) => {
                        if r1.switch_error() {
                            warn!(
                                "sdio: SWITCH_ERROR after CMD6 idx={} val={}",
                                request.index, request.value
                            );
                            return Err(Error::CardError(crate::error::CardError::IllegalCommand));
                        }
                        if r1.ready_for_data() && matches!(r1.current_state(), CardState::Transfer)
                        {
                            return Ok(OperationProgress::Complete(()));
                        }
                        if mmc_switch_deadline_passed(self.host.inner(), request) {
                            return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 6)));
                        }
                        request.polls = request.polls.saturating_add(1);
                        let cmd = crate::cmd::cmd13(request.rca);
                        self.host.submit_command(&cmd)?;
                        Ok(OperationProgress::Pending)
                    }
                    CommandResponseProgress::Complete(_) => {
                        if mmc_switch_deadline_passed(self.host.inner(), request) {
                            return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 6)));
                        }
                        request.polls = request.polls.saturating_add(1);
                        let cmd = crate::cmd::cmd13(request.rca);
                        self.host.submit_command(&cmd)?;
                        Ok(OperationProgress::Pending)
                    }
                }
            }
        }
    }

    #[cfg(any(feature = "rdif", test))]
    pub(crate) fn abort_mmc_switch_request(
        &mut self,
        _request: &mut MmcSwitchRequest,
    ) -> Result<(), Error> {
        self.host.abort_command_request()
    }
}

#[derive(Debug, Clone)]
pub struct CardInfo {
    /// Which physical-layer protocol the card speaks. SD vs eMMC matters
    /// for follow-up steps the protocol layer can't generalize over —
    /// e.g. EXT_CSD reads, 8-bit bus switching, HS200 tuning.
    pub kind: CardKind,
    /// True when the card responded to CMD8 with a valid R7 echo
    /// (SD physical layer 2.0+). Always `false` for eMMC.
    pub sd_v2: bool,
    pub high_capacity: bool,
    pub ocr: u32,
    pub rca: u16,
    /// User-data capacity in 512-byte blocks, parsed from the CSD.
    /// `None` if the CSD reports a structure version we do not yet support.
    pub capacity_blocks: Option<u64>,
    /// Card identification register (manufacturer / OEM / serial / date).
    /// `None` if the host returned an unexpected response type to CMD2.
    pub cid: Option<CidResponse>,
    /// Decoded EXT_CSD register, present only for [`CardKind::Mmc`]
    /// after a successful init. Lets callers introspect HS200/HS400
    /// support, partition geometry, etc., without re-reading the card.
    pub ext_csd: Option<crate::ext_csd::ExtCsd>,
}

/// Which physical-layer family the card belongs to.
///
/// The SD vs MMC split is decided during `init()`:
///
/// - CMD8 echoes a valid R7 → SD v2 (SDHC/SDXC)
/// - CMD8 has no response, but ACMD41 succeeds → SD v1 (legacy SDSC)
/// - CMD8 has no response and ACMD41 also fails, but CMD1 reports
///   power-up → eMMC / MMC
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CardKind {
    /// SD memory card (SDSC / SDHC / SDXC).
    Sd,
    /// Embedded MMC or removable MMC card.
    Mmc,
}

pub(super) fn block_count_from_len(len: usize) -> Result<u32, Error> {
    if len == 0 || !len.is_multiple_of(512) {
        return Err(Error::Misaligned);
    }
    u32::try_from(len / 512).map_err(|_| Error::InvalidArgument)
}
