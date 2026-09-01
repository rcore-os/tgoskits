//! CV181x SD/MMC host and IRQ capability adapters.

use dma_api::DeviceDma;
use sdhci_host::Sdhci;
use sdmmc_host::{ClockHz, ProgressCause, RequestProgress, SignalVoltage};
use sdmmc_protocol::sdio::host::{CompletionIrqRearmHost, SdMmcIrqHost};

use super::*;

impl SdMmcIrqHost for Cv181xSdhci {
    type Event = sdhci_host::Event;
    type IrqHandle = sdhci_host::SdhciIrqHandle;
    type CardIrq = sdhci_host::SdhciCardIrqHandle;

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

    fn into_parts(self) -> sdmmc_host::HostParts<Self, Self::IrqHandle, Self::CardIrq> {
        let Cv181xSdhci {
            inner,
            mmio,
            config,
            controller,
        } = self;
        let parts = <Sdhci as SdMmcIrqHost>::into_parts(inner);
        sdmmc_host::HostParts {
            bus: Cv181xSdhci {
                inner: parts.bus,
                mmio,
                config,
                controller,
            },
            irq: parts.irq,
            card_irq: parts.card_irq,
        }
    }

    fn device_dma(&self) -> Result<&DeviceDma, ProtocolError> {
        <Sdhci as SdMmcIrqHost>::device_dma(&self.inner)
    }

    fn progress_wait_kind(&self) -> sdmmc_protocol::sdio::HostProgressWait {
        <Sdhci as SdMmcIrqHost>::progress_wait_kind(&self.inner)
    }
}

impl CompletionIrqRearmHost for Cv181xSdhci {
    fn rearm_completion_irq_and_check(
        &mut self,
    ) -> Result<sdmmc_protocol::sdio::CompletionIrqRearm, ProtocolError> {
        <Sdhci as CompletionIrqRearmHost>::rearm_completion_irq_and_check(&mut self.inner)
    }
}

impl sdmmc_host::SdMmcHost for Cv181xSdhci {
    type TransactionRequest<'a>
        = <Sdhci as sdmmc_host::SdMmcHost>::TransactionRequest<'a>
    where
        Self: 'a;
    type BusRequest = BusRequest;

    unsafe fn submit_transaction<'a>(
        &mut self,
        transaction: sdmmc_host::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdmmc_host::Error>
    where
        Self: 'a,
    {
        unsafe { sdmmc_host::SdMmcHost::submit_transaction(&mut self.inner, transaction) }
    }

    unsafe fn submit_transaction_owned<'a>(
        &mut self,
        transaction: sdmmc_host::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdmmc_host::SubmitTransactionError<'a>>
    where
        Self: 'a,
    {
        unsafe { sdmmc_host::SdMmcHost::submit_transaction_owned(&mut self.inner, transaction) }
    }

    fn advance_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        cause: ProgressCause,
    ) -> Result<RequestProgress<sdmmc_host::RawResponse>, sdmmc_host::AdvanceRequestError>
    where
        Self: 'a,
    {
        sdmmc_host::SdMmcHost::advance_transaction(&mut self.inner, request, cause)
    }

    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), sdmmc_host::Error>
    where
        Self: 'a,
    {
        sdmmc_host::SdMmcHost::abort_transaction(&mut self.inner, request)
    }

    fn take_completed_dma<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Option<dma_api::CompletedDma>
    where
        Self: 'a,
    {
        sdmmc_host::SdMmcHost::take_completed_dma(&mut self.inner, request)
    }

    unsafe fn submit_bus_op(
        &mut self,
        op: sdmmc_host::BusOp,
    ) -> Result<Self::BusRequest, sdmmc_host::Error> {
        match op {
            sdmmc_host::BusOp::PowerOn => {
                let request = unsafe { sdmmc_host::SdMmcHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::PowerOn))
            }
            sdmmc_host::BusOp::PowerOff => {
                let request = unsafe { sdmmc_host::SdMmcHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::PowerOff))
            }
            sdmmc_host::BusOp::ResetAll => {
                let request = unsafe { sdmmc_host::SdMmcHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::ResetAll))
            }
            sdmmc_host::BusOp::SetClock(speed) => {
                let request = unsafe { sdmmc_host::SdMmcHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::SetClock(speed)))
            }
            sdmmc_host::BusOp::SetClockHz(ClockHz(hz)) => {
                let request = unsafe { sdmmc_host::SdMmcHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::SetClockHz(hz)))
            }
            sdmmc_host::BusOp::SetBusWidth(width) if !self.config.supports_bus_width(width) => {
                Err(sdmmc_host::Error::Unsupported)
            }
            sdmmc_host::BusOp::SetSignalVoltage(SignalVoltage::V180) if self.config.no_1v8 => {
                Err(sdmmc_host::Error::Unsupported)
            }
            sdmmc_host::BusOp::SetSignalVoltage(SignalVoltage::V330) => {
                let request = unsafe { sdmmc_host::SdMmcHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::Restore3v3))
            }
            _ => {
                let request = unsafe { sdmmc_host::SdMmcHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::None))
            }
        }
    }

    fn advance_bus_op(
        &mut self,
        bus_request: &mut Self::BusRequest,
        cause: ProgressCause,
    ) -> Result<RequestProgress<()>, sdmmc_host::AdvanceRequestError> {
        match &mut bus_request.state {
            BusRequestState::Inner {
                request: inner,
                after,
            } => match sdmmc_host::SdMmcHost::advance_bus_op(&mut self.inner, inner, cause)? {
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
            BusRequestState::Done => Err(sdmmc_host::AdvanceRequestError::AlreadyCompleted),
        }
    }

    fn abort_bus_op(
        &mut self,
        bus_request: &mut Self::BusRequest,
    ) -> Result<(), sdmmc_host::Error> {
        let result = match &mut bus_request.state {
            BusRequestState::Inner { request: inner, .. } => {
                sdmmc_host::SdMmcHost::abort_bus_op(&mut self.inner, inner)
            }
            BusRequestState::Done => Ok(()),
        };
        bus_request.state = BusRequestState::Done;
        result
    }

    fn now_ms(&self) -> Option<u64> {
        sdmmc_host::SdMmcHost::now_ms(&self.inner)
    }
}

pub struct BusRequest {
    state: BusRequestState,
}

impl BusRequest {
    fn inner(request: <Sdhci as sdmmc_host::SdMmcHost>::BusRequest, after: AfterBusOp) -> Self {
        Self {
            state: BusRequestState::Inner { request, after },
        }
    }
}

enum BusRequestState {
    Inner {
        request: <Sdhci as sdmmc_host::SdMmcHost>::BusRequest,
        after: AfterBusOp,
    },
    Done,
}

#[derive(Clone, Copy)]
pub(super) enum AfterBusOp {
    None,
    PowerOn,
    PowerOff,
    ResetAll,
    Restore3v3,
    SetClock(sdmmc_host::ClockSpeed),
    SetClockHz(u32),
}
