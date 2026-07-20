//! CV1800/SG2002 SDIO1 platform wrapper for the generic SDHCI host.
//!
//! This crate owns only SoC clock/reset/pinmux policy. Command, data, status
//! acknowledgement, request generations, and absolute-time progress are all
//! delegated to `sdhci-host`; no OS runtime or blocking wait is injected.

#![no_std]

extern crate alloc;

use alloc::sync::Arc;

use sdhci_host::Sdhci;
use sdmmc_protocol::{
    Error,
    sdio::{
        SdioIrqSource,
        host2::{SdioHost2Irq, SdioHost2Timed},
    },
};

pub mod hw_init;
pub mod irq;

use hw_init::{Sdio1MappedResources, Sdio1PlatformInit, Sdio1Policy};

/// Discovered SDIO1 controller transferred to one CPU-pinned owner.
pub struct CviSdhci {
    inner: Sdhci,
    base: usize,
    card_irq: Arc<irq::CardIrqState>,
    resources: Arc<Sdio1MappedResources>,
}

// SAFETY: construction requires exclusive ownership of every mapped register
// window. Moving the discovery object transfers that ownership to the final
// maintenance thread; the live object is never shared or migrated afterwards.
unsafe impl Send for CviSdhci {}

impl CviSdhci {
    /// Creates a side-effect-free discovered controller.
    ///
    /// The final maintenance owner must retire both split IRQ capabilities
    /// before dropping the discovered controller.
    pub fn discover(resources: Sdio1MappedResources, policy: Sdio1Policy) -> Result<Self, Error> {
        let resources = Arc::new(resources);
        let pointer = resources.controller_base();
        let base = pointer.as_ptr() as usize;
        // SAFETY: the consumed resource aggregate owns the controller mapping.
        let mut inner = unsafe { Sdhci::new(pointer) };
        inner.set_base_clock_hz(policy.base_clock_hz());
        inner.set_reset_hook(Sdio1PlatformInit::new(resources.register_views(), policy));
        Ok(Self {
            inner,
            base,
            card_irq: Arc::new(irq::CardIrqState::new()),
            resources,
        })
    }
}

impl SdioHost2Irq for CviSdhci {
    type Event = irq::CviSdhciIrqEvent;
    type IrqEndpoint = irq::CviSdhciIrqEndpoint;
    type IrqControl = irq::CviSdhciIrqControl;

    fn completion_irq_enabled(&self) -> bool {
        self.inner.completion_irq_enabled()
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        sdmmc_protocol::sdio::SdioHost::enable_completion_irq(&mut self.inner)?;
        self.card_irq.activate();
        irq::enable_card_status(self.base);
        irq::enable_card_signal(self.base);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        let result = sdmmc_protocol::sdio::SdioHost::disable_completion_irq(&mut self.inner);
        self.card_irq.deactivate();
        result
    }

    fn take_irq_source(&mut self) -> Option<SdioIrqSource<Self::IrqEndpoint, Self::IrqControl>> {
        let source = self.inner.take_irq_source()?;
        let (endpoint, control) = source.into_parts();
        Some(SdioIrqSource::new(
            irq::CviSdhciIrqEndpoint {
                inner: endpoint,
                card: Arc::clone(&self.card_irq),
                base: self.base,
                _resources: Arc::clone(&self.resources),
            },
            irq::CviSdhciIrqControl {
                inner: control,
                card: Arc::clone(&self.card_irq),
                base: self.base,
                _resources: Arc::clone(&self.resources),
            },
        ))
    }
}

impl sdio_host2::SdioHost for CviSdhci {
    type TransactionRequest<'a>
        = <Sdhci as sdio_host2::SdioHost>::TransactionRequest<'a>
    where
        Self: 'a;
    type BusRequest = <Sdhci as sdio_host2::SdioHost>::BusRequest;

    unsafe fn submit_transaction<'a>(
        &mut self,
        transaction: sdio_host2::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdio_host2::Error>
    where
        Self: 'a,
    {
        // SAFETY: the request lifetime remains tied to the caller and the
        // delegated host preserves the same ownership contract.
        unsafe { self.inner.submit_transaction(transaction) }
    }

    unsafe fn submit_transaction_owned<'a>(
        &mut self,
        transaction: sdio_host2::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdio_host2::SubmitTransactionError<'a>>
    where
        Self: 'a,
    {
        // SAFETY: identical ownership/lifetime contract to the inner host.
        unsafe { self.inner.submit_transaction_owned(transaction) }
    }

    fn poll_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<sdio_host2::RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
    where
        Self: 'a,
    {
        self.inner.poll_transaction(request)
    }

    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), sdio_host2::Error>
    where
        Self: 'a,
    {
        self.inner.abort_transaction(request)
    }

    fn take_completed_dma<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Option<dma_api::CompletedDma>
    where
        Self: 'a,
    {
        self.inner.take_completed_dma(request)
    }

    fn take_completed_cpu<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Option<dma_api::CpuDmaBuffer>
    where
        Self: 'a,
    {
        self.inner.take_completed_cpu(request)
    }

    unsafe fn submit_bus_op(
        &mut self,
        op: sdio_host2::BusOp,
    ) -> Result<Self::BusRequest, sdio_host2::Error> {
        // SAFETY: delegated request is retained and polled by the same owner.
        unsafe { self.inner.submit_bus_op(op) }
    }

    fn poll_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::PollRequestError> {
        self.inner.poll_bus_op(request)
    }

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), sdio_host2::Error> {
        self.inner.abort_bus_op(request)
    }

    fn now_ms(&self) -> Option<u64> {
        self.inner.now_ms()
    }
}

impl SdioHost2Timed for CviSdhci {
    fn poll_transaction_at<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
    where
        Self: 'a,
    {
        SdioHost2Timed::poll_transaction_at(&mut self.inner, request, now_ns)
    }

    fn transaction_wake_at<'a>(&self, request: &Self::TransactionRequest<'a>) -> Option<u64>
    where
        Self: 'a,
    {
        SdioHost2Timed::transaction_wake_at(&self.inner, request)
    }

    fn poll_bus_op_at(
        &mut self,
        request: &mut Self::BusRequest,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::PollRequestError> {
        SdioHost2Timed::poll_bus_op_at(&mut self.inner, request, now_ns)
    }

    fn bus_op_wake_at(&self, request: &Self::BusRequest) -> Option<u64> {
        SdioHost2Timed::bus_op_wake_at(&self.inner, request)
    }
}
