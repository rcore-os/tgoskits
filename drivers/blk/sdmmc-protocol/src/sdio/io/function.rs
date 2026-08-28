use core::num::NonZeroU16;

use sdmmc_host::ProgressCause;

use super::{
    FunctionNumber, IoAddress, SdioCard,
    response::{bad_response, check_r5},
};
use crate::{
    block::{CommandResponseProgress, OperationProgress},
    cmd,
    error::{Error, ErrorContext, Phase},
    response::Response,
    sdio::host::SdMmcIrqHost,
};

const CCCR_IO_ENABLE: u32 = 0x02;
const CCCR_IO_READY: u32 = 0x03;
const CCCR_INT_ENABLE: u32 = 0x04;
const CCCR_INT_PENDING: u32 = 0x05;
const FBR_BLOCK_SIZE: u32 = 0x10;

/// Submitted CMD52 direct-access request.
pub struct SdioDirectRequest {
    request_id: u64,
}

/// Non-blocking function-enable request.
pub struct SdioFunctionEnableRequest {
    request_id: u64,
    function: FunctionNumber,
    state: FunctionEnableState,
    polls: u16,
}

/// Non-blocking per-function block-size configuration request.
pub struct SdioBlockSizeRequest {
    request_id: u64,
    function: FunctionNumber,
    block_size: NonZeroU16,
    state: BlockSizeState,
    readback: [u8; 2],
}

/// Non-blocking function-interrupt enable request.
pub struct SdioInterruptEnableRequest {
    request_id: u64,
    function: FunctionNumber,
    state: InterruptEnableState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FunctionEnableState {
    ReadEnable,
    WriteEnable,
    PollReady,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockSizeState {
    WriteLow,
    WriteHigh,
    ReadLow,
    ReadHigh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InterruptEnableState {
    Read,
    Write,
}

impl<H: SdMmcIrqHost + 'static> SdioCard<H> {
    /// Submit a checked CMD52 read.
    pub fn submit_read_byte(
        &mut self,
        function: FunctionNumber,
        address: IoAddress,
    ) -> Result<SdioDirectRequest, Error> {
        let request_id = self.reserve_io_request()?;
        if let Err(error) =
            self.host
                .submit_command(&cmd::cmd52(false, function.get(), false, address.get(), 0))
        {
            self.finish_io_request(request_id)?;
            return Err(error);
        }
        Ok(SdioDirectRequest { request_id })
    }

    /// Submit a checked CMD52 write. `read_after_write` selects the RAW bit.
    pub fn submit_write_byte(
        &mut self,
        function: FunctionNumber,
        address: IoAddress,
        value: u8,
        read_after_write: bool,
    ) -> Result<SdioDirectRequest, Error> {
        let request_id = self.reserve_io_request()?;
        if let Err(error) = self.host.submit_command(&cmd::cmd52(
            true,
            function.get(),
            read_after_write,
            address.get(),
            value,
        )) {
            self.finish_io_request(request_id)?;
            return Err(error);
        }
        Ok(SdioDirectRequest { request_id })
    }

    /// Advance a CMD52 request.
    pub fn advance_direct_request(
        &mut self,
        request: &mut SdioDirectRequest,
        cause: ProgressCause,
    ) -> Result<OperationProgress<u8>, Error> {
        self.ensure_io_request(request.request_id)?;
        let progress = match self.host.advance_command_response(cause) {
            Err(error) => {
                self.finish_io_request(request.request_id)?;
                return Err(error);
            }
            Ok(CommandResponseProgress::Pending) => Ok(OperationProgress::Pending),
            Ok(CommandResponseProgress::Complete(Response::R5(response))) => {
                response.checked_data().map(OperationProgress::Complete)
            }
            Ok(CommandResponseProgress::Complete(_)) => Err(bad_response(52)),
        };
        if !matches!(progress, Ok(OperationProgress::Pending)) {
            self.finish_io_request(request.request_id)?;
        }
        progress
    }

    /// Abort a pending CMD52 request.
    pub fn abort_direct_request(&mut self, request: &mut SdioDirectRequest) -> Result<(), Error> {
        self.ensure_io_request(request.request_id)?;
        let result = self.host.abort_command_request();
        self.finish_io_request(request.request_id)?;
        result
    }

    /// Begin enabling one enumerated I/O function.
    pub fn submit_enable_function(
        &mut self,
        function: FunctionNumber,
    ) -> Result<SdioFunctionEnableRequest, Error> {
        ensure_io_function(function, self.info)?;
        let request_id = self.reserve_io_request()?;
        if let Err(error) = self.submit_common_read(CCCR_IO_ENABLE) {
            self.finish_io_request(request_id)?;
            return Err(error);
        }
        Ok(SdioFunctionEnableRequest {
            request_id,
            function,
            state: FunctionEnableState::ReadEnable,
            polls: 0,
        })
    }

    /// Advance function enable/readiness without sleeping or polling in a loop.
    pub fn advance_enable_function(
        &mut self,
        request: &mut SdioFunctionEnableRequest,
        cause: ProgressCause,
    ) -> Result<OperationProgress<()>, Error> {
        let value = match self.advance_owned_common_direct(request.request_id, cause)? {
            OperationProgress::Pending => return Ok(OperationProgress::Pending),
            OperationProgress::Complete(value) => value,
        };
        let mask = 1u8 << request.function.get();
        match request.state {
            FunctionEnableState::ReadEnable => {
                self.continue_common_write(request.request_id, CCCR_IO_ENABLE, value | mask)?;
                request.state = FunctionEnableState::WriteEnable;
                Ok(OperationProgress::Pending)
            }
            FunctionEnableState::WriteEnable => {
                self.continue_common_read(request.request_id, CCCR_IO_READY)?;
                request.state = FunctionEnableState::PollReady;
                Ok(OperationProgress::Pending)
            }
            FunctionEnableState::PollReady if value & mask != 0 => {
                if let Some(function) =
                    self.functions[usize::from(request.function.get() - 1)].as_mut()
                {
                    function.enabled = true;
                }
                self.finish_io_request(request.request_id)?;
                Ok(OperationProgress::Complete(()))
            }
            FunctionEnableState::PollReady => {
                request.polls = request.polls.saturating_add(1);
                if request.polls >= 1_000 {
                    self.finish_io_request(request.request_id)?;
                    return Err(Error::SdioFunctionNotReady);
                }
                self.continue_common_read(request.request_id, CCCR_IO_READY)?;
                Ok(OperationProgress::Pending)
            }
        }
    }

    /// Abort a pending function-enable request.
    pub fn abort_enable_function(
        &mut self,
        request: &mut SdioFunctionEnableRequest,
    ) -> Result<(), Error> {
        self.abort_owned_common(request.request_id)
    }

    /// Begin programming and verifying one function's block size.
    pub fn submit_set_block_size(
        &mut self,
        function: FunctionNumber,
        block_size: NonZeroU16,
    ) -> Result<SdioBlockSizeRequest, Error> {
        ensure_io_function(function, self.info)?;
        let address = fbr_base(function.get()) + FBR_BLOCK_SIZE;
        let request_id = self.reserve_io_request()?;
        if let Err(error) = self.submit_common_write(address, block_size.get().to_le_bytes()[0]) {
            self.finish_io_request(request_id)?;
            return Err(error);
        }
        Ok(SdioBlockSizeRequest {
            request_id,
            function,
            block_size,
            state: BlockSizeState::WriteLow,
            readback: [0; 2],
        })
    }

    /// Advance block-size programming and readback verification.
    pub fn advance_set_block_size(
        &mut self,
        request: &mut SdioBlockSizeRequest,
        cause: ProgressCause,
    ) -> Result<OperationProgress<()>, Error> {
        let value = match self.advance_owned_common_direct(request.request_id, cause)? {
            OperationProgress::Pending => return Ok(OperationProgress::Pending),
            OperationProgress::Complete(value) => value,
        };
        let address = fbr_base(request.function.get()) + FBR_BLOCK_SIZE;
        match request.state {
            BlockSizeState::WriteLow => {
                self.continue_common_write(
                    request.request_id,
                    address + 1,
                    request.block_size.get().to_le_bytes()[1],
                )?;
                request.state = BlockSizeState::WriteHigh;
                Ok(OperationProgress::Pending)
            }
            BlockSizeState::WriteHigh => {
                self.continue_common_read(request.request_id, address)?;
                request.state = BlockSizeState::ReadLow;
                Ok(OperationProgress::Pending)
            }
            BlockSizeState::ReadLow => {
                request.readback[0] = value;
                self.continue_common_read(request.request_id, address + 1)?;
                request.state = BlockSizeState::ReadHigh;
                Ok(OperationProgress::Pending)
            }
            BlockSizeState::ReadHigh => {
                request.readback[1] = value;
                if u16::from_le_bytes(request.readback) != request.block_size.get() {
                    self.finish_io_request(request.request_id)?;
                    return Err(Error::BadResponse(ErrorContext::for_cmd(
                        Phase::ResponseWait,
                        52,
                    )));
                }
                if request.function.is_io()
                    && let Some(function) =
                        self.functions[usize::from(request.function.get() - 1)].as_mut()
                {
                    function.block_size = Some(request.block_size);
                }
                self.finish_io_request(request.request_id)?;
                Ok(OperationProgress::Complete(()))
            }
        }
    }

    /// Abort a pending block-size request.
    pub fn abort_set_block_size(
        &mut self,
        request: &mut SdioBlockSizeRequest,
    ) -> Result<(), Error> {
        self.abort_owned_common(request.request_id)
    }

    /// Begin enabling the card interrupt master and one function bit.
    pub fn submit_enable_function_interrupt(
        &mut self,
        function: FunctionNumber,
    ) -> Result<SdioInterruptEnableRequest, Error> {
        ensure_io_function(function, self.info)?;
        let request_id = self.reserve_io_request()?;
        if let Err(error) = self.submit_common_read(CCCR_INT_ENABLE) {
            self.finish_io_request(request_id)?;
            return Err(error);
        }
        Ok(SdioInterruptEnableRequest {
            request_id,
            function,
            state: InterruptEnableState::Read,
        })
    }

    /// Advance function interrupt enable.
    pub fn advance_enable_function_interrupt(
        &mut self,
        request: &mut SdioInterruptEnableRequest,
        cause: ProgressCause,
    ) -> Result<OperationProgress<()>, Error> {
        let value = match self.advance_owned_common_direct(request.request_id, cause)? {
            OperationProgress::Pending => return Ok(OperationProgress::Pending),
            OperationProgress::Complete(value) => value,
        };
        match request.state {
            InterruptEnableState::Read => {
                let value = value | 1 | (1 << request.function.get());
                self.continue_common_write(request.request_id, CCCR_INT_ENABLE, value)?;
                request.state = InterruptEnableState::Write;
                Ok(OperationProgress::Pending)
            }
            InterruptEnableState::Write => {
                if let Some(function) =
                    self.functions[usize::from(request.function.get() - 1)].as_mut()
                {
                    function.interrupt_enabled = true;
                }
                self.finish_io_request(request.request_id)?;
                Ok(OperationProgress::Complete(()))
            }
        }
    }

    /// Abort a pending function-interrupt enable request.
    pub fn abort_enable_function_interrupt(
        &mut self,
        request: &mut SdioInterruptEnableRequest,
    ) -> Result<(), Error> {
        self.abort_owned_common(request.request_id)
    }

    /// Submit a read of the CCCR function-interrupt pending bitmap.
    pub fn submit_read_interrupt_pending(&mut self) -> Result<SdioDirectRequest, Error> {
        let request_id = self.reserve_io_request()?;
        if let Err(error) = self.submit_common_read(CCCR_INT_PENDING) {
            self.finish_io_request(request_id)?;
            return Err(error);
        }
        Ok(SdioDirectRequest { request_id })
    }

    pub(super) fn submit_common_read(&mut self, address: u32) -> Result<(), Error> {
        self.host.submit_command(&read_common(address))
    }

    fn submit_common_write(&mut self, address: u32, value: u8) -> Result<(), Error> {
        self.host
            .submit_command(&cmd::cmd52(true, 0, true, address, value))
    }

    fn continue_common_read(&mut self, request_id: u64, address: u32) -> Result<(), Error> {
        self.ensure_io_request(request_id)?;
        if let Err(error) = self.submit_common_read(address) {
            self.finish_io_request(request_id)?;
            return Err(error);
        }
        Ok(())
    }

    fn continue_common_write(
        &mut self,
        request_id: u64,
        address: u32,
        value: u8,
    ) -> Result<(), Error> {
        self.ensure_io_request(request_id)?;
        if let Err(error) = self.submit_common_write(address, value) {
            self.finish_io_request(request_id)?;
            return Err(error);
        }
        Ok(())
    }

    fn advance_owned_common_direct(
        &mut self,
        request_id: u64,
        cause: ProgressCause,
    ) -> Result<OperationProgress<u8>, Error> {
        self.ensure_io_request(request_id)?;
        match self.advance_common_direct(cause) {
            Ok(progress) => Ok(progress),
            Err(error) => {
                self.finish_io_request(request_id)?;
                Err(error)
            }
        }
    }

    fn abort_owned_common(&mut self, request_id: u64) -> Result<(), Error> {
        self.ensure_io_request(request_id)?;
        let result = self.host.abort_command_request();
        self.finish_io_request(request_id)?;
        result
    }

    fn advance_common_direct(
        &mut self,
        cause: ProgressCause,
    ) -> Result<OperationProgress<u8>, Error> {
        match self.host.advance_command_response(cause)? {
            CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
            CommandResponseProgress::Complete(Response::R5(response)) => {
                check_r5(response, 52).map(OperationProgress::Complete)
            }
            CommandResponseProgress::Complete(_) => Err(bad_response(52)),
        }
    }
}

pub(super) fn read_common(address: u32) -> crate::cmd::Command {
    cmd::cmd52(false, 0, false, address, 0)
}

fn ensure_known_function(
    function: FunctionNumber,
    info: Option<super::SdioCardInfo>,
) -> Result<(), Error> {
    let info = info.ok_or(Error::InvalidArgument)?;
    if function.get() <= info.io_functions {
        Ok(())
    } else {
        Err(Error::InvalidArgument)
    }
}

pub(super) fn ensure_io_function(
    function: FunctionNumber,
    info: Option<super::SdioCardInfo>,
) -> Result<(), Error> {
    if !function.is_io() {
        return Err(Error::InvalidArgument);
    }
    ensure_known_function(function, info)
}

pub(super) const fn fbr_base(function: u8) -> u32 {
    (function as u32) * 0x100
}
