//! CV181x Host2 and IRQ capability adapters.

use dma_api::DeviceDma;
use sdhci_host::Sdhci;
use sdio_host2::{ClockHz, ProgressCause, RequestProgress, SignalVoltage};
use sdmmc_protocol::sdio::host::SdioIrqHost;

use super::*;
use crate::platform::*;

impl SdioIrqHost for Cv181xSdhci {
    type Event = sdhci_host::Event;
    type IrqHandle = sdhci_host::SdhciIrqHandle;

    fn completion_irq_enabled(&self) -> bool {
        self.inner.completion_irq_enabled()
    }

    fn enable_completion_irq(&mut self) -> Result<(), ProtocolError> {
        self.inner.enable_completion_irq();
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), ProtocolError> {
        self.inner.disable_completion_irq();
        Ok(())
    }

    fn irq_handle(&mut self) -> Self::IrqHandle {
        self.inner.irq_endpoint()
    }

    fn device_dma(&self) -> Result<&DeviceDma, ProtocolError> {
        <Sdhci as SdioIrqHost>::device_dma(&self.inner)
    }

    fn progress_wait_kind(&self) -> sdmmc_protocol::sdio::HostProgressWait {
        <Sdhci as SdioIrqHost>::progress_wait_kind(&self.inner)
    }
}

impl sdio_host2::SdioHost for Cv181xSdhci {
    type TransactionRequest<'a>
        = <Sdhci as sdio_host2::SdioHost>::TransactionRequest<'a>
    where
        Self: 'a;
    type BusRequest = BusRequest;

    unsafe fn submit_transaction<'a>(
        &mut self,
        transaction: sdio_host2::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdio_host2::Error>
    where
        Self: 'a,
    {
        unsafe { sdio_host2::SdioHost::submit_transaction(&mut self.inner, transaction) }
    }

    unsafe fn submit_transaction_owned<'a>(
        &mut self,
        transaction: sdio_host2::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdio_host2::SubmitTransactionError<'a>>
    where
        Self: 'a,
    {
        unsafe { sdio_host2::SdioHost::submit_transaction_owned(&mut self.inner, transaction) }
    }

    fn advance_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        cause: ProgressCause,
    ) -> Result<RequestProgress<sdio_host2::RawResponse>, sdio_host2::AdvanceRequestError>
    where
        Self: 'a,
    {
        sdio_host2::SdioHost::advance_transaction(&mut self.inner, request, cause)
    }

    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), sdio_host2::Error>
    where
        Self: 'a,
    {
        sdio_host2::SdioHost::abort_transaction(&mut self.inner, request)
    }

    fn take_completed_dma<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Option<dma_api::CompletedDma>
    where
        Self: 'a,
    {
        sdio_host2::SdioHost::take_completed_dma(&mut self.inner, request)
    }

    unsafe fn submit_bus_op(
        &mut self,
        op: sdio_host2::BusOp,
    ) -> Result<Self::BusRequest, sdio_host2::Error> {
        match op {
            sdio_host2::BusOp::PowerOn => {
                let request = unsafe { sdio_host2::SdioHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::PowerOn))
            }
            sdio_host2::BusOp::PowerOff => {
                self.configure_sd_power_off();
                let request = unsafe { sdio_host2::SdioHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::None))
            }
            sdio_host2::BusOp::ResetAll => {
                let request = unsafe { sdio_host2::SdioHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::ResetAll))
            }
            sdio_host2::BusOp::SetClock(speed) => {
                Ok(BusRequest::ready(self.set_clock_speed(speed)))
            }
            sdio_host2::BusOp::SetClockHz(ClockHz(hz)) => Ok(BusRequest::ready(
                self.program_clock(hz, hz > DEFAULT_MAX_FREQUENCY_HZ, HOST_CTRL2_UHS_SDR12),
            )),
            sdio_host2::BusOp::SetBusWidth(width) if !self.config.supports_bus_width(width) => {
                Ok(BusRequest::ready(Err(sdio_host2::Error::Unsupported)))
            }
            sdio_host2::BusOp::SetSignalVoltage(SignalVoltage::V180) if self.config.no_1v8 => {
                Ok(BusRequest::ready(Err(sdio_host2::Error::Unsupported)))
            }
            sdio_host2::BusOp::SetSignalVoltage(SignalVoltage::V330) => {
                self.restore_3v3_power();
                let request = unsafe { sdio_host2::SdioHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::None))
            }
            _ => {
                let request = unsafe { sdio_host2::SdioHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::None))
            }
        }
    }

    fn advance_bus_op(
        &mut self,
        bus_request: &mut Self::BusRequest,
        cause: ProgressCause,
    ) -> Result<RequestProgress<()>, sdio_host2::AdvanceRequestError> {
        match &mut bus_request.state {
            BusRequestState::Ready(result) => {
                if cause == ProgressCause::Submitted {
                    return Ok(RequestProgress::RegisterPending {
                        retry_after: core::time::Duration::from_micros(1),
                    });
                }
                let result = result
                    .take()
                    .ok_or(sdio_host2::AdvanceRequestError::AlreadyCompleted)?;
                bus_request.state = BusRequestState::Done;
                Ok(RequestProgress::Complete(result))
            }
            BusRequestState::Inner {
                request: inner,
                after,
            } => match sdio_host2::SdioHost::advance_bus_op(&mut self.inner, inner, cause)? {
                RequestProgress::WaitingForIrq => Ok(RequestProgress::WaitingForIrq),
                RequestProgress::RegisterPending { retry_after } => {
                    Ok(RequestProgress::RegisterPending { retry_after })
                }
                RequestProgress::Complete(result) => {
                    let result = result.and_then(|()| self.apply_after(*after));
                    bus_request.state = BusRequestState::Done;
                    Ok(RequestProgress::Complete(result))
                }
            },
            BusRequestState::Done => Err(sdio_host2::AdvanceRequestError::AlreadyCompleted),
        }
    }

    fn abort_bus_op(
        &mut self,
        bus_request: &mut Self::BusRequest,
    ) -> Result<(), sdio_host2::Error> {
        let result = match &mut bus_request.state {
            BusRequestState::Inner { request: inner, .. } => {
                sdio_host2::SdioHost::abort_bus_op(&mut self.inner, inner)
            }
            BusRequestState::Ready(_) | BusRequestState::Done => Ok(()),
        };
        bus_request.state = BusRequestState::Done;
        result
    }

    fn now_ms(&self) -> Option<u64> {
        sdio_host2::SdioHost::now_ms(&self.inner)
    }
}

pub struct BusRequest {
    state: BusRequestState,
}

impl BusRequest {
    fn ready(result: Result<(), sdio_host2::Error>) -> Self {
        Self {
            state: BusRequestState::Ready(Some(result)),
        }
    }

    fn inner(request: <Sdhci as sdio_host2::SdioHost>::BusRequest, after: AfterBusOp) -> Self {
        Self {
            state: BusRequestState::Inner { request, after },
        }
    }
}

enum BusRequestState {
    Ready(Option<Result<(), sdio_host2::Error>>),
    Inner {
        request: <Sdhci as sdio_host2::SdioHost>::BusRequest,
        after: AfterBusOp,
    },
    Done,
}

#[derive(Clone, Copy)]
pub(super) enum AfterBusOp {
    None,
    PowerOn,
    ResetAll,
}
