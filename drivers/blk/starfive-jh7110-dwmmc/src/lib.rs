#![no_std]

#[cfg(test)]
extern crate std;

use core::ptr::NonNull;

use dma_api::{CompletedDma, DeviceDma};
use dwmmc_host::{DwMmc, DwMmcIrq, Event};
use sdio_host2::{
    BusOp, BusWidth, Error as Host2Error, PollRequestError, RawResponse, RequestPoll, SdioHost,
    SignalVoltage, SubmitTransactionError, Transaction,
};
use sdmmc_protocol::{
    Error,
    sdio::host2::{SdioHost2Irq, SdioHost2Lifecycle, SdioHost2Timed},
};

pub const JH7110_DWMMC_FIFO_OFFSET: usize = 0x200;
pub const JH7110_STABLE_REFERENCE_CLOCK_HZ: u32 = 50_000_000;
pub const DEVICE_NAME: &str = "starfive-jh7110-mmc";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Jh7110DwMmcConfig {
    reference_clock_hz: u32,
    max_bus_width: BusWidth,
    supports_1v8: bool,
}

impl Default for Jh7110DwMmcConfig {
    fn default() -> Self {
        Self {
            reference_clock_hz: JH7110_STABLE_REFERENCE_CLOCK_HZ,
            max_bus_width: BusWidth::Bit4,
            supports_1v8: false,
        }
    }
}

impl Jh7110DwMmcConfig {
    pub const fn new() -> Self {
        Self {
            reference_clock_hz: JH7110_STABLE_REFERENCE_CLOCK_HZ,
            max_bus_width: BusWidth::Bit4,
            supports_1v8: false,
        }
    }

    pub const fn with_reference_clock_hz(mut self, reference_clock_hz: u32) -> Self {
        self.reference_clock_hz = reference_clock_hz;
        self
    }

    pub const fn with_max_bus_width(mut self, max_bus_width: BusWidth) -> Self {
        self.max_bus_width = max_bus_width;
        self
    }

    pub const fn with_1v8_support(mut self, supports_1v8: bool) -> Self {
        self.supports_1v8 = supports_1v8;
        self
    }

    pub const fn reference_clock_hz(&self) -> u32 {
        if self.reference_clock_hz == 0 {
            JH7110_STABLE_REFERENCE_CLOCK_HZ
        } else {
            self.reference_clock_hz
        }
    }

    pub const fn max_bus_width(&self) -> BusWidth {
        self.max_bus_width
    }

    pub const fn supports_1v8(&self) -> bool {
        self.supports_1v8
    }

    fn validate_bus_op(self, op: BusOp) -> Result<(), Host2Error> {
        match op {
            BusOp::SetBusWidth(BusWidth::Bit8) if !matches!(self.max_bus_width, BusWidth::Bit8) => {
                Err(Host2Error::Unsupported)
            }
            BusOp::SetSignalVoltage(SignalVoltage::V180 | SignalVoltage::V120)
                if !self.supports_1v8 =>
            {
                Err(Host2Error::Unsupported)
            }
            _ => Ok(()),
        }
    }
}

pub struct Jh7110DwMmc {
    inner: DwMmc,
    config: Jh7110DwMmcConfig,
}

impl Jh7110DwMmc {
    /// Construct a JH7110 DWMMC host over an already mapped MMIO register file.
    ///
    /// # Safety
    ///
    /// `base` must point to a valid JH7110 DWMMC register file that the caller
    /// has mapped and owns exclusively for the host lifetime.
    pub unsafe fn new(base: NonNull<u8>, config: Jh7110DwMmcConfig) -> Self {
        let normalized_config = config
            .with_reference_clock_hz(config.reference_clock_hz())
            .with_max_bus_width(config.max_bus_width())
            .with_1v8_support(config.supports_1v8());
        let mut inner = unsafe { DwMmc::new_with_fifo_offset(base, JH7110_DWMMC_FIFO_OFFSET) };
        inner.set_reference_clock(normalized_config.reference_clock_hz());
        Self {
            inner,
            config: normalized_config,
        }
    }

    pub const fn config(&self) -> Jh7110DwMmcConfig {
        self.config
    }

    pub const fn inner(&self) -> &DwMmc {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut DwMmc {
        &mut self.inner
    }

    pub fn into_inner(self) -> DwMmc {
        self.inner
    }

    pub fn set_dma(&mut self, dma: DeviceDma) {
        self.inner.set_dma(dma);
    }
}

impl SdioHost for Jh7110DwMmc {
    type TransactionRequest<'a>
        = <DwMmc as SdioHost>::TransactionRequest<'a>
    where
        Self: 'a;

    type BusRequest = <DwMmc as SdioHost>::BusRequest;

    unsafe fn submit_transaction<'a>(
        &mut self,
        transaction: Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, Host2Error>
    where
        Self: 'a,
    {
        unsafe { self.inner.submit_transaction(transaction) }
    }

    unsafe fn submit_transaction_owned<'a>(
        &mut self,
        transaction: Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, SubmitTransactionError<'a>>
    where
        Self: 'a,
    {
        unsafe { self.inner.submit_transaction_owned(transaction) }
    }

    fn poll_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<RequestPoll<RawResponse>, PollRequestError>
    where
        Self: 'a,
    {
        self.inner.poll_transaction(request)
    }

    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), Host2Error>
    where
        Self: 'a,
    {
        self.inner.abort_transaction(request)
    }

    fn take_completed_dma<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Option<CompletedDma>
    where
        Self: 'a,
    {
        self.inner.take_completed_dma(request)
    }

    unsafe fn submit_bus_op(&mut self, op: BusOp) -> Result<Self::BusRequest, Host2Error> {
        self.config.validate_bus_op(op)?;
        unsafe { self.inner.submit_bus_op(op) }
    }

    fn poll_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
    ) -> Result<RequestPoll<()>, PollRequestError> {
        self.inner.poll_bus_op(request)
    }

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), Host2Error> {
        self.inner.abort_bus_op(request)
    }

    fn now_ms(&self) -> Option<u64> {
        self.inner.now_ms()
    }
}

impl SdioHost2Irq for Jh7110DwMmc {
    type Event = Event;
    type IrqHandle = DwMmcIrq;

    fn completion_irq_enabled(&self) -> bool {
        self.inner.completion_irq_enabled()
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        self.inner.enable_completion_irq();
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        self.inner.disable_completion_irq();
        Ok(())
    }

    fn irq_handle(&mut self) -> Self::IrqHandle {
        self.inner.irq_endpoint()
    }
}

impl SdioHost2Timed for Jh7110DwMmc {
    fn poll_transaction_at<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        now_ns: u64,
    ) -> Result<RequestPoll<RawResponse>, PollRequestError>
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
    ) -> Result<RequestPoll<()>, PollRequestError> {
        SdioHost2Timed::poll_bus_op_at(&mut self.inner, request, now_ns)
    }

    fn bus_op_wake_at(&self, request: &Self::BusRequest) -> Option<u64> {
        SdioHost2Timed::bus_op_wake_at(&self.inner, request)
    }
}

impl SdioHost2Lifecycle for Jh7110DwMmc {
    type RecoveryState = dwmmc_host::DwMmcRecoveryState;

    fn begin_recovery(
        &mut self,
        cause: rdif_block::RecoveryCause,
    ) -> Result<Self::RecoveryState, Error> {
        SdioHost2Lifecycle::begin_recovery(&mut self.inner, cause)
    }

    fn poll_dma_quiesce(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        SdioHost2Lifecycle::poll_dma_quiesce(&mut self.inner, state, input)
    }

    fn begin_reinitialize(&mut self, state: &mut Self::RecoveryState) -> Result<(), Error> {
        SdioHost2Lifecycle::begin_reinitialize(&mut self.inner, state)
    }

    fn poll_reinitialize(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        SdioHost2Lifecycle::poll_reinitialize(&mut self.inner, state, input)
    }
}

pub mod rdif {
    use dma_api::DeviceDma;
    pub use rdif_block::{
        BInterface, BIrqHandler, BQueue, BlkError, CompletedRequest, CompletionHint,
        CompletionSink, DispatchMode, IQueue, Interface, OwnedRequest, QueueEventBatch,
        QueueHandle, QueueKind, RequestId as RdifRequestId, ServiceProgress, SubmitError,
        SubmitOutcome,
    };
    pub use sdmmc_protocol::rdif::{config::BlockConfig, device::BlockDevice, queue::BlockQueue};
    use sdmmc_protocol::sdio::{InitializedSdioCard, host2::SdioHost2Adapter};

    use crate::{DEVICE_NAME, Jh7110DwMmc};

    pub fn device(
        card: InitializedSdioCard<SdioHost2Adapter<Jh7110DwMmc>>,
        config: BlockConfig,
    ) -> BlockDevice<SdioHost2Adapter<Jh7110DwMmc>> {
        BlockDevice::from_initialized(card, config)
    }

    pub fn dma_config(capacity_blocks: u64, dma: DeviceDma) -> BlockConfig {
        dwmmc_host::rdif::dma_config(DEVICE_NAME, capacity_blocks, dma)
    }

    /// Build the FIFO-only configuration used while the card initializes.
    ///
    /// FIFO is confined to controller/card initialization and cannot publish
    /// an RDIF runtime queue.
    pub const fn initialization_config(capacity_blocks: u64) -> BlockConfig {
        BlockConfig::fifo(DEVICE_NAME, capacity_blocks)
    }
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;
    use std::{vec, vec::Vec};

    use sdio_host2::{BusOp, BusWidth, SdioHost, SignalVoltage};
    use sdmmc_protocol::sdio::host2::SdioHost2Irq;

    use super::*;

    fn fake_mmio() -> (Vec<u32>, NonNull<u8>) {
        let mut regs = vec![0_u32; 256];
        let ptr = NonNull::new(regs.as_mut_ptr().cast::<u8>()).unwrap();
        (regs, ptr)
    }

    #[test]
    fn default_config_keeps_jh7110_slot_constraints() {
        let config = Jh7110DwMmcConfig::default();

        assert_eq!(JH7110_DWMMC_FIFO_OFFSET, 0x200);
        assert_eq!(JH7110_STABLE_REFERENCE_CLOCK_HZ, 50_000_000);
        assert_eq!(DEVICE_NAME, "starfive-jh7110-mmc");
        assert_eq!(
            config.reference_clock_hz(),
            JH7110_STABLE_REFERENCE_CLOCK_HZ
        );
        assert_eq!(config.max_bus_width(), BusWidth::Bit4);
        assert!(!config.supports_1v8());
    }

    #[test]
    fn constructor_applies_reference_clock_and_fifo_offset_policy() {
        let (_regs, mmio) = fake_mmio();
        let host = unsafe { Jh7110DwMmc::new(mmio, Jh7110DwMmcConfig::default()) };

        assert_eq!(
            host.inner().reference_clock(),
            JH7110_STABLE_REFERENCE_CLOCK_HZ
        );
    }

    #[test]
    fn bus_policy_rejects_8bit_and_1v8_requests() {
        let (_regs, mmio) = fake_mmio();
        let mut host = unsafe { Jh7110DwMmc::new(mmio, Jh7110DwMmcConfig::default()) };

        assert!(matches!(
            unsafe { host.submit_bus_op(BusOp::SetBusWidth(BusWidth::Bit8)) },
            Err(sdio_host2::Error::Unsupported)
        ));
        assert!(matches!(
            unsafe { host.submit_bus_op(BusOp::SetSignalVoltage(SignalVoltage::V180)) },
            Err(sdio_host2::Error::Unsupported)
        ));
    }

    #[test]
    fn completion_irq_methods_delegate_to_inner_dwmmc() {
        let (_regs, mmio) = fake_mmio();
        let mut host = unsafe { Jh7110DwMmc::new(mmio, Jh7110DwMmcConfig::default()) };

        assert!(!host.completion_irq_enabled());
        host.enable_completion_irq().unwrap();
        assert!(host.completion_irq_enabled());
        host.disable_completion_irq().unwrap();
        assert!(!host.completion_irq_enabled());
    }

    #[test]
    fn rdif_fifo_config_cannot_publish_runtime_queue() {
        let config = rdif::initialization_config(16);

        assert_eq!(config.name, DEVICE_NAME);
        assert_eq!(config.capacity_blocks, 16);
        assert!(!config.uses_dma());
        assert!(!config.supports_runtime_queue());
    }

    #[test]
    fn wrapper_exposes_timed_initialization_and_typed_recovery() {
        fn assert_runtime_contract<T>()
        where
            T: sdmmc_protocol::sdio::host2::SdioHost2Timed
                + sdmmc_protocol::sdio::host2::SdioHost2Lifecycle,
        {
        }

        assert_runtime_contract::<Jh7110DwMmc>();
    }
}
