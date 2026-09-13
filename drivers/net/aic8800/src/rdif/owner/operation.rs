use core::num::NonZeroUsize;

use dma_api::{CpuDmaBuffer, DmaDirection};
use sdmmc_host::{ClockHz, ProgressCause, RequestProgress};
use sdmmc_protocol::{
    Error, ErrorContext, OperationProgress, Phase,
    sdio::{
        CompletionIrqRearmHost, SdioBlockSizeRequest, SdioCard, SdioDirectRequest,
        SdioDmaTransferRequest, SdioFunctionEnableRequest, SdioInterruptEnableRequest,
    },
};

use crate::{SdioRequest, SdioRequestKind, SdioResponse};

/// Terminal response from one core-requested SDIO operation.
pub(crate) struct OperationCompletion {
    pub(crate) request_id: u64,
    pub(crate) response: SdioResponse,
}

pub(crate) enum ActiveOperation<H: CompletionIrqRearmHost + 'static> {
    Direct {
        request_id: u64,
        request: SdioDirectRequest,
    },
    Enable {
        request_id: u64,
        request: SdioFunctionEnableRequest,
    },
    BlockSize {
        request_id: u64,
        request: SdioBlockSizeRequest,
    },
    Interrupt {
        request_id: u64,
        request: SdioInterruptEnableRequest,
    },
    Dma {
        request_id: u64,
        read_length: Option<usize>,
        request: SdioDmaTransferRequest<H>,
    },
    Bus {
        request_id: u64,
        request: H::BusRequest,
    },
}

impl<H: CompletionIrqRearmHost + Send + 'static> ActiveOperation<H> {
    pub(crate) fn submit(card: &mut SdioCard<H>, operation: SdioRequest) -> Result<Self, Error> {
        let request_id = operation.id;
        match operation.kind {
            SdioRequestKind::EnableFunction(function) => Ok(Self::Enable {
                request_id,
                request: card.submit_enable_function(function)?,
            }),
            SdioRequestKind::SetBlockSize {
                function,
                block_size,
            } => Ok(Self::BlockSize {
                request_id,
                request: card.submit_set_block_size(function, block_size)?,
            }),
            SdioRequestKind::EnableFunctionInterrupt(function) => Ok(Self::Interrupt {
                request_id,
                request: card.submit_enable_function_interrupt(function)?,
            }),
            SdioRequestKind::ReadByte { function, address } => Ok(Self::Direct {
                request_id,
                request: card.submit_read_byte(function, address)?,
            }),
            SdioRequestKind::WriteByte {
                function,
                address,
                value,
                read_after_write,
            } => Ok(Self::Direct {
                request_id,
                request: card.submit_write_byte(function, address, value, read_after_write)?,
            }),
            SdioRequestKind::Read {
                function,
                address,
                address_mode,
                transfer_mode,
                length,
            } => {
                let length = NonZeroUsize::new(length).ok_or(Error::InvalidArgument)?;
                let buffer = CpuDmaBuffer::new_zero(
                    card.host().device_dma()?,
                    length,
                    4,
                    DmaDirection::FromDevice,
                )
                .map_err(|_| bus_error())?
                .prepare_for_device();
                let request = card
                    .submit_read_dma(function, address, address_mode, transfer_mode, buffer)
                    .map_err(|error| error.error())?;
                Ok(Self::Dma {
                    request_id,
                    read_length: Some(length.get()),
                    request,
                })
            }
            SdioRequestKind::Write {
                function,
                address,
                address_mode,
                transfer_mode,
                bytes,
            } => {
                let length = NonZeroUsize::new(bytes.len()).ok_or(Error::InvalidArgument)?;
                let mut buffer = CpuDmaBuffer::new_zero(
                    card.host().device_dma()?,
                    length,
                    4,
                    DmaDirection::ToDevice,
                )
                .map_err(|_| bus_error())?;
                buffer.copy_from_slice_cpu(&bytes);
                let request = card
                    .submit_write_dma(
                        function,
                        address,
                        address_mode,
                        transfer_mode,
                        buffer.prepare_for_device(),
                    )
                    .map_err(|error| error.error())?;
                Ok(Self::Dma {
                    request_id,
                    read_length: None,
                    request,
                })
            }
            SdioRequestKind::SetClockHz(hz) => {
                let request = unsafe {
                    card.host_mut()
                        .submit_bus_op(sdmmc_host::BusOp::SetClockHz(ClockHz(hz)))
                }
                .map_err(map_host_error)?;
                Ok(Self::Bus {
                    request_id,
                    request,
                })
            }
        }
    }

    pub(crate) fn advance(
        &mut self,
        card: &mut SdioCard<H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<OperationCompletion>, Error> {
        match self {
            Self::Direct {
                request_id,
                request,
            } => card
                .advance_direct_request(request, cause)
                .map(|progress| progress.map_complete(*request_id, SdioResponse::Byte)),
            Self::Enable {
                request_id,
                request,
            } => card
                .advance_enable_function(request, cause)
                .map(|progress| progress.map_complete(*request_id, |_| SdioResponse::Unit)),
            Self::BlockSize {
                request_id,
                request,
            } => card
                .advance_set_block_size(request, cause)
                .map(|progress| progress.map_complete(*request_id, |_| SdioResponse::Unit)),
            Self::Interrupt {
                request_id,
                request,
            } => card
                .advance_enable_function_interrupt(request, cause)
                .map(|progress| progress.map_complete(*request_id, |_| SdioResponse::Unit)),
            Self::Dma {
                request_id,
                read_length,
                request,
            } => match card.advance_dma_transfer_request(request, cause)? {
                OperationProgress::Pending => Ok(OperationProgress::Pending),
                OperationProgress::Complete(()) => {
                    let completed = card
                        .take_completed_dma(request)
                        .ok_or(Error::InvalidArgument)?;
                    let response = if let Some(length) = *read_length {
                        let mut bytes = alloc::vec![0; length];
                        completed.copy_to_slice_cpu(&mut bytes);
                        SdioResponse::Data(bytes)
                    } else {
                        drop(completed);
                        SdioResponse::Unit
                    };
                    Ok(OperationProgress::Complete(OperationCompletion {
                        request_id: *request_id,
                        response,
                    }))
                }
            },
            Self::Bus {
                request_id,
                request,
            } => match card.host_mut().advance_bus_op(request, cause) {
                Ok(RequestProgress::RegisterPending { .. } | RequestProgress::WaitingForIrq) => {
                    Ok(OperationProgress::Pending)
                }
                Ok(RequestProgress::Complete(Ok(()))) => {
                    Ok(OperationProgress::Complete(OperationCompletion {
                        request_id: *request_id,
                        response: SdioResponse::Unit,
                    }))
                }
                Ok(RequestProgress::Complete(Err(error))) => Err(map_host_error(error)),
                Err(_) => Err(bus_error()),
            },
        }
    }

    pub(crate) fn abort(&mut self, card: &mut SdioCard<H>) -> Result<(), Error> {
        match self {
            Self::Direct { request, .. } => card.abort_direct_request(request),
            Self::Enable { request, .. } => card.abort_enable_function(request),
            Self::BlockSize { request, .. } => card.abort_set_block_size(request),
            Self::Interrupt { request, .. } => card.abort_enable_function_interrupt(request),
            Self::Dma { request, .. } => card.abort_dma_transfer_request(request),
            Self::Bus { request, .. } => card
                .host_mut()
                .abort_bus_op(request)
                .map_err(map_host_error),
        }
    }
}

trait MapComplete<T> {
    fn map_complete(
        self,
        request_id: u64,
        response: impl FnOnce(T) -> SdioResponse,
    ) -> OperationProgress<OperationCompletion>;
}

impl<T> MapComplete<T> for OperationProgress<T> {
    fn map_complete(
        self,
        request_id: u64,
        response: impl FnOnce(T) -> SdioResponse,
    ) -> OperationProgress<OperationCompletion> {
        match self {
            OperationProgress::Pending => OperationProgress::Pending,
            OperationProgress::Complete(value) => {
                OperationProgress::Complete(OperationCompletion {
                    request_id,
                    response: response(value),
                })
            }
        }
    }
}

fn map_host_error(error: sdmmc_host::Error) -> Error {
    match error {
        sdmmc_host::Error::Busy => Error::Busy,
        sdmmc_host::Error::Timeout => Error::Timeout(ErrorContext::new(Phase::Switch)),
        sdmmc_host::Error::Crc => Error::Crc(ErrorContext::new(Phase::Switch)),
        sdmmc_host::Error::NoCard => Error::NoCard,
        sdmmc_host::Error::Unsupported => Error::UnsupportedCommand,
        sdmmc_host::Error::InvalidArgument => Error::InvalidArgument,
        sdmmc_host::Error::Misaligned => Error::Misaligned,
        sdmmc_host::Error::Bus | sdmmc_host::Error::Controller => bus_error(),
        _ => bus_error(),
    }
}

fn bus_error() -> Error {
    Error::BusError(ErrorContext::new(Phase::Switch))
}
