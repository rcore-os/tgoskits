#![no_std]

#[cfg(test)]
extern crate std;

use core::ptr::NonNull;

use dma_api::{CompletedDma, DeviceDma};
use dwmmc_host::{DwMmc, DwMmcIrq, Event, FifoConfig, FifoDataWidth};
use sdmmc_host::{
    AdvanceRequestError, BusOp, BusWidth, Error as Host2Error, ProgressCause, RawResponse,
    RequestProgress, SdMmcHost, SignalVoltage, SubmitTransactionError, Transaction,
};
use sdmmc_protocol::{Error, sdio::host::SdMmcIrqHost};

pub const JH7110_STABLE_REFERENCE_CLOCK_HZ: u32 = 50_000_000;
pub const JH7110_FIFO_DEPTH_WORDS: u16 = 32;
pub const JH7110_FIFO_CONFIG: FifoConfig =
    match FifoConfig::new(JH7110_FIFO_DEPTH_WORDS, FifoDataWidth::Bits32) {
        Some(config) => config,
        None => panic!("JH7110 FIFO capability is invalid"),
    };
pub const DEVICE_NAME: &str = "starfive-jh7110-mmc";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Jh7110DwMmcConfig {
    reference_clock_hz: u32,
    fifo_config: FifoConfig,
    max_bus_width: BusWidth,
    supports_1v8: bool,
}

impl Default for Jh7110DwMmcConfig {
    fn default() -> Self {
        Self {
            reference_clock_hz: JH7110_STABLE_REFERENCE_CLOCK_HZ,
            fifo_config: JH7110_FIFO_CONFIG,
            max_bus_width: BusWidth::Bit4,
            supports_1v8: false,
        }
    }
}

impl Jh7110DwMmcConfig {
    pub const fn new() -> Self {
        Self {
            reference_clock_hz: JH7110_STABLE_REFERENCE_CLOCK_HZ,
            fifo_config: JH7110_FIFO_CONFIG,
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

    pub const fn with_fifo_config(mut self, fifo_config: FifoConfig) -> Self {
        self.fifo_config = fifo_config;
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

    pub const fn fifo_config(&self) -> FifoConfig {
        self.fifo_config
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
            .with_fifo_config(config.fifo_config())
            .with_max_bus_width(config.max_bus_width())
            .with_1v8_support(config.supports_1v8());
        let mut inner = unsafe { DwMmc::new(base) };
        inner.set_reference_clock(normalized_config.reference_clock_hz());
        inner.set_fifo_config(normalized_config.fifo_config());
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

    pub fn reset_and_init(&mut self) -> Result<(), Error> {
        self.inner.reset_and_init()
    }
}

impl SdMmcHost for Jh7110DwMmc {
    type TransactionRequest<'a>
        = <DwMmc as SdMmcHost>::TransactionRequest<'a>
    where
        Self: 'a;

    type BusRequest = <DwMmc as SdMmcHost>::BusRequest;

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

    fn advance_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        cause: ProgressCause,
    ) -> Result<RequestProgress<RawResponse>, AdvanceRequestError>
    where
        Self: 'a,
    {
        self.inner.advance_transaction(request, cause)
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

    fn advance_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
        cause: ProgressCause,
    ) -> Result<RequestProgress<()>, AdvanceRequestError> {
        self.inner.advance_bus_op(request, cause)
    }

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), Host2Error> {
        self.inner.abort_bus_op(request)
    }

    fn now_ms(&self) -> Option<u64> {
        self.inner.now_ms()
    }
}

impl SdMmcIrqHost for Jh7110DwMmc {
    type Event = Event;
    type IrqHandle = DwMmcIrq;
    type CardIrq = ();

    fn completion_irq_enabled(&self) -> bool {
        self.inner.completion_irq_enabled()
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        self.inner.enable_completion_irq();
        Ok(())
    }

    fn rearm_completion_irq_and_check(
        &mut self,
    ) -> Result<sdmmc_protocol::sdio::CompletionIrqRearm, Error> {
        <DwMmc as SdMmcIrqHost>::rearm_completion_irq_and_check(&mut self.inner)
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        self.inner.disable_completion_irq();
        Ok(())
    }

    fn into_parts(self) -> sdmmc_host::HostParts<Self, Self::IrqHandle, Self::CardIrq> {
        let Jh7110DwMmc { inner, config } = self;
        let parts = <DwMmc as SdMmcIrqHost>::into_parts(inner);
        sdmmc_host::HostParts {
            bus: Jh7110DwMmc {
                inner: parts.bus,
                config,
            },
            irq: parts.irq,
            card_irq: None,
        }
    }

    fn device_dma(&self) -> Result<&DeviceDma, Error> {
        <DwMmc as SdMmcIrqHost>::device_dma(&self.inner)
    }

    fn progress_wait_kind(&self) -> sdmmc_protocol::sdio::HostProgressWait {
        <DwMmc as SdMmcIrqHost>::progress_wait_kind(&self.inner)
    }
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;
    use std::{vec, vec::Vec};

    use sdmmc_host::{BusOp, BusWidth, SdMmcHost, SignalVoltage};
    use sdmmc_protocol::sdio::host::SdMmcIrqHost;

    use super::*;

    fn fake_mmio() -> (Vec<u32>, NonNull<u8>) {
        let mut regs = vec![0_u32; 256];
        let ptr = NonNull::new(regs.as_mut_ptr().cast::<u8>()).unwrap();
        (regs, ptr)
    }

    #[test]
    fn default_config_keeps_jh7110_slot_constraints() {
        let config = Jh7110DwMmcConfig::default();

        assert_eq!(JH7110_STABLE_REFERENCE_CLOCK_HZ, 50_000_000);
        assert_eq!(JH7110_FIFO_DEPTH_WORDS, 32);
        assert_eq!(DEVICE_NAME, "starfive-jh7110-mmc");
        assert_eq!(
            config.reference_clock_hz(),
            JH7110_STABLE_REFERENCE_CLOCK_HZ
        );
        assert_eq!(config.fifo_config(), JH7110_FIFO_CONFIG);
        assert_eq!(config.max_bus_width(), BusWidth::Bit4);
        assert!(!config.supports_1v8());
    }

    #[test]
    fn constructor_applies_reference_clock_policy() {
        let (_regs, mmio) = fake_mmio();
        let host = unsafe { Jh7110DwMmc::new(mmio, Jh7110DwMmcConfig::default()) };

        assert_eq!(
            host.inner().reference_clock(),
            JH7110_STABLE_REFERENCE_CLOCK_HZ
        );
        assert_eq!(host.inner().fifo_config().depth_words(), 32);
        assert_eq!(
            host.inner().fifo_config().data_width(),
            FifoDataWidth::Bits32
        );
    }

    #[test]
    fn bus_policy_rejects_8bit_and_1v8_requests() {
        let (_regs, mmio) = fake_mmio();
        let mut host = unsafe { Jh7110DwMmc::new(mmio, Jh7110DwMmcConfig::default()) };

        assert!(matches!(
            unsafe { host.submit_bus_op(BusOp::SetBusWidth(BusWidth::Bit8)) },
            Err(sdmmc_host::Error::Unsupported)
        ));
        assert!(matches!(
            unsafe { host.submit_bus_op(BusOp::SetSignalVoltage(SignalVoltage::V180)) },
            Err(sdmmc_host::Error::Unsupported)
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
    fn host_exposes_explicit_progress_cause_api() {
        fn assert_progress_api<H: SdMmcHost>() {}

        assert_progress_api::<Jh7110DwMmc>();
    }

    #[test]
    fn device_dma_is_explicitly_unavailable_before_configuration() {
        let (_regs, mmio) = fake_mmio();
        let host = unsafe { Jh7110DwMmc::new(mmio, Jh7110DwMmcConfig::default()) };

        assert!(matches!(host.device_dma(), Err(Error::UnsupportedCommand)));
    }
}
