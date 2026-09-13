use alloc::boxed::Box;
use core::fmt;

use dma_api::{CompletedDma, DmaDirection, PreparedDma};
use sdmmc_host::ProgressCause;

use super::{
    AddressMode, FunctionNumber, IoAddress, SdioCard, TransferMode,
    function::ensure_io_function,
    response::{bad_response, check_r5},
};
use crate::{
    block::{DataCommandProgress, OperationProgress},
    cmd,
    error::Error,
    response::Response,
    sdio::{host::SdMmcIrqHost, transport::ProtocolDataRequest},
};

/// Submitted borrowed-buffer CMD53 request.
pub struct SdioTransferRequest<'a, H: SdMmcIrqHost + 'static> {
    inner: ProtocolDataRequest<'a, H>,
}

/// Submitted owned-DMA CMD53 request.
pub struct SdioDmaTransferRequest<H: SdMmcIrqHost + 'static> {
    inner: ProtocolDataRequest<'static, H>,
}

/// CMD53 submission failure that returns the caller's prepared DMA buffer.
pub struct SdioDmaSubmitError {
    error: Error,
    buffer: Box<PreparedDma>,
}

impl SdioDmaSubmitError {
    /// Returns the protocol error.
    pub const fn error(&self) -> Error {
        self.error
    }

    /// Returns both the protocol error and the unsubmitted DMA buffer.
    pub fn into_parts(self) -> (Error, PreparedDma) {
        (self.error, *self.buffer)
    }
}

impl fmt::Debug for SdioDmaSubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SdioDmaSubmitError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for SdioDmaSubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl core::error::Error for SdioDmaSubmitError {}

impl<H: SdMmcIrqHost + 'static> SdioCard<H> {
    /// Submit a borrowed-buffer CMD53 read.
    pub fn submit_read<'a>(
        &mut self,
        function: FunctionNumber,
        address: IoAddress,
        address_mode: AddressMode,
        transfer_mode: TransferMode,
        buffer: &'a mut [u8],
    ) -> Result<SdioTransferRequest<'a, H>, Error>
    where
        H: 'a,
    {
        self.validate_transfer_target(function, transfer_mode)?;
        let layout = transfer_layout(buffer.len(), transfer_mode)?;
        let command = cmd53_command(false, function, address, address_mode, layout);
        let inner = self.host.submit_read_data(
            &command,
            buffer,
            u32::from(layout.block_size),
            layout.block_count,
        )?;
        Ok(SdioTransferRequest { inner })
    }

    /// Submit a borrowed-buffer CMD53 write.
    pub fn submit_write<'a>(
        &mut self,
        function: FunctionNumber,
        address: IoAddress,
        address_mode: AddressMode,
        transfer_mode: TransferMode,
        buffer: &'a [u8],
    ) -> Result<SdioTransferRequest<'a, H>, Error>
    where
        H: 'a,
    {
        self.validate_transfer_target(function, transfer_mode)?;
        let layout = transfer_layout(buffer.len(), transfer_mode)?;
        let command = cmd53_command(true, function, address, address_mode, layout);
        let inner = self.host.submit_write_data(
            &command,
            buffer,
            u32::from(layout.block_size),
            layout.block_count,
        )?;
        Ok(SdioTransferRequest { inner })
    }

    /// Advance a CMD53 request.
    pub fn advance_transfer_request(
        &mut self,
        request: &mut SdioTransferRequest<'_, H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<()>, Error> {
        match self.host.advance_data_request(&mut request.inner, cause)? {
            DataCommandProgress::Pending => Ok(OperationProgress::Pending),
            DataCommandProgress::Complete(Response::R5(response)) => {
                check_r5(response, 53)?;
                Ok(OperationProgress::Complete(()))
            }
            DataCommandProgress::Complete(_) => Err(bad_response(53)),
        }
    }

    /// Abort a pending CMD53 request.
    pub fn abort_transfer_request(
        &mut self,
        request: &mut SdioTransferRequest<'_, H>,
    ) -> Result<(), Error> {
        self.host.abort_data_request(&mut request.inner)
    }

    /// Submit an owned-DMA CMD53 read.
    pub fn submit_read_dma(
        &mut self,
        function: FunctionNumber,
        address: IoAddress,
        address_mode: AddressMode,
        transfer_mode: TransferMode,
        buffer: PreparedDma,
    ) -> Result<SdioDmaTransferRequest<H>, SdioDmaSubmitError> {
        self.submit_dma(
            false,
            function,
            address,
            address_mode,
            transfer_mode,
            buffer,
        )
    }

    /// Submit an owned-DMA CMD53 write.
    pub fn submit_write_dma(
        &mut self,
        function: FunctionNumber,
        address: IoAddress,
        address_mode: AddressMode,
        transfer_mode: TransferMode,
        buffer: PreparedDma,
    ) -> Result<SdioDmaTransferRequest<H>, SdioDmaSubmitError> {
        self.submit_dma(true, function, address, address_mode, transfer_mode, buffer)
    }

    /// Advance an owned-DMA CMD53 request.
    pub fn advance_dma_transfer_request(
        &mut self,
        request: &mut SdioDmaTransferRequest<H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<()>, Error> {
        match self.host.advance_data_request(&mut request.inner, cause)? {
            DataCommandProgress::Pending => Ok(OperationProgress::Pending),
            DataCommandProgress::Complete(Response::R5(response)) => {
                check_r5(response, 53)?;
                Ok(OperationProgress::Complete(()))
            }
            DataCommandProgress::Complete(_) => Err(bad_response(53)),
        }
    }

    /// Abort an owned-DMA CMD53 request.
    pub fn abort_dma_transfer_request(
        &mut self,
        request: &mut SdioDmaTransferRequest<H>,
    ) -> Result<(), Error> {
        self.host.abort_data_request(&mut request.inner)
    }

    /// Returns the DMA buffer after terminal completion or abort.
    pub fn take_completed_dma(
        &mut self,
        request: &mut SdioDmaTransferRequest<H>,
    ) -> Option<CompletedDma> {
        request.inner.take_completed_dma()
    }

    fn submit_dma(
        &mut self,
        write: bool,
        function: FunctionNumber,
        address: IoAddress,
        address_mode: AddressMode,
        transfer_mode: TransferMode,
        buffer: PreparedDma,
    ) -> Result<SdioDmaTransferRequest<H>, SdioDmaSubmitError> {
        if let Err(error) = self.validate_transfer_target(function, transfer_mode) {
            return Err(SdioDmaSubmitError {
                error,
                buffer: Box::new(buffer),
            });
        }
        let layout = match transfer_layout(buffer.len().get(), transfer_mode) {
            Ok(layout) => layout,
            Err(error) => {
                return Err(SdioDmaSubmitError {
                    error,
                    buffer: Box::new(buffer),
                });
            }
        };
        let dma_direction = if write {
            DmaDirection::ToDevice
        } else {
            DmaDirection::FromDevice
        };
        if !matches!(
            (buffer.direction(), dma_direction),
            (DmaDirection::ToDevice, DmaDirection::ToDevice)
                | (DmaDirection::FromDevice, DmaDirection::FromDevice)
                | (DmaDirection::Bidirectional, _)
        ) {
            return Err(SdioDmaSubmitError {
                error: Error::InvalidArgument,
                buffer: Box::new(buffer),
            });
        }
        let command = cmd53_command(write, function, address, address_mode, layout);
        let data_direction = if write {
            sdmmc_host::DataDirection::Write
        } else {
            sdmmc_host::DataDirection::Read
        };
        self.host
            .submit_dma_data(
                &command,
                data_direction,
                buffer,
                u32::from(layout.block_size),
                layout.block_count,
            )
            .map(|inner| SdioDmaTransferRequest { inner })
            .map_err(|error| SdioDmaSubmitError {
                error: error.error,
                buffer: Box::new(error.into_buffer()),
            })
    }

    fn validate_transfer_target(
        &self,
        function: FunctionNumber,
        transfer_mode: TransferMode,
    ) -> Result<(), Error> {
        ensure_io_function(function, self.info)?;
        let function_info = self.function(function).ok_or(Error::InvalidArgument)?;
        if !function_info.enabled {
            return Err(Error::InvalidArgument);
        }
        if let TransferMode::Block { block_size } = transfer_mode
            && function_info.block_size != Some(block_size)
        {
            return Err(Error::InvalidArgument);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct TransferLayout {
    block_mode: bool,
    block_size: u16,
    block_count: u32,
    command_count: u16,
}

fn transfer_layout(length: usize, mode: TransferMode) -> Result<TransferLayout, Error> {
    match mode {
        TransferMode::Byte => {
            if !(1..=512).contains(&length) {
                return Err(Error::InvalidArgument);
            }
            Ok(TransferLayout {
                block_mode: false,
                block_size: u16::try_from(length).map_err(|_| Error::InvalidArgument)?,
                block_count: 1,
                command_count: if length == 512 { 0 } else { length as u16 },
            })
        }
        TransferMode::Block { block_size } => {
            let block_size_usize = usize::from(block_size.get());
            if length == 0 || !length.is_multiple_of(block_size_usize) {
                return Err(Error::Misaligned);
            }
            let block_count = length / block_size_usize;
            if !(1..=511).contains(&block_count) {
                return Err(Error::InvalidArgument);
            }
            Ok(TransferLayout {
                block_mode: true,
                block_size: block_size.get(),
                block_count: block_count as u32,
                command_count: block_count as u16,
            })
        }
    }
}

fn cmd53_command(
    write: bool,
    function: FunctionNumber,
    address: IoAddress,
    address_mode: AddressMode,
    layout: TransferLayout,
) -> crate::cmd::Command {
    cmd::cmd53(
        write,
        function.get(),
        layout.block_mode,
        address.get(),
        matches!(address_mode, AddressMode::Incrementing),
        layout.command_count,
    )
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU16;

    use super::*;

    #[test]
    fn byte_mode_encodes_512_bytes_as_zero_count() {
        let layout = transfer_layout(512, TransferMode::Byte).unwrap();
        assert!(!layout.block_mode);
        assert_eq!(layout.command_count, 0);
        assert_eq!(layout.block_size, 512);
        assert_eq!(layout.block_count, 1);
    }

    #[test]
    fn block_mode_rejects_partial_and_oversized_transfers() {
        let block_size = NonZeroU16::new(512).unwrap();
        assert_eq!(
            transfer_layout(513, TransferMode::Block { block_size }).err(),
            Some(Error::Misaligned)
        );
        assert_eq!(
            transfer_layout(512 * 512, TransferMode::Block { block_size }).err(),
            Some(Error::InvalidArgument)
        );
    }
}
