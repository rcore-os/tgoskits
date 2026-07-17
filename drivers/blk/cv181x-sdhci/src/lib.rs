//! CV181x/SG2002 SD-card host wrapper for the generic SDHCI backend.
//!
//! This crate owns only the Cvitek-specific top/pinmux/PHY programming around
//! the controller. Command, data, interrupt-status caching, and RDIF queue
//! semantics are delegated to [`sdhci_host::Sdhci`].

#![no_std]

use core::{num::NonZeroU32, ptr::NonNull};

use dma_api::DeviceDma;
use sdhci_host::Sdhci;
use sdio_host2::{BusWidth, ClockHz, ClockSpeed, RequestPoll, SignalVoltage};
use sdmmc_protocol::{
    Error as ProtocolError,
    sdio::host2::{SdioHost2Irq, SdioHost2Lifecycle, SdioHost2Timed},
};

pub mod rdif;

const DEFAULT_SRC_FREQUENCY_HZ: u32 = 375_000_000;
const DEFAULT_MIN_FREQUENCY_HZ: u32 = 400_000;
const DEFAULT_MAX_FREQUENCY_HZ: u32 = 25_000_000;

/// CV181x TOP syscon physical base used by the SD0 power/pinmux registers.
pub const CV181X_TOP_SYSCON_BASE: u64 = 0x0300_0000;
/// Minimum syscon mapping required by this wrapper (TOP + pinmux/IO window).
pub const CV181X_SYSCON_REQUIRED_SIZE: usize = 0x2000;

const SYSCON_PINMUX_OFFSET: usize = 0x1000;

const TOP_SD_PWRSW_CTRL: usize = 0x1f4;
const TOP_SD_PWRSW_3V3: u32 = 0x9;
const TOP_SD_PWRSW_OFF: u32 = 0xe;
const TOP_SD_PWRSW_LOW_MASK: u32 = 0xf;

const PINMUX_SDIO0_CD: usize = 0x34;
const PINMUX_SDIO0_PWR_EN: usize = 0x38;
const PINMUX_SDIO0_CLK: usize = 0x1c;
const PINMUX_SDIO0_CMD: usize = 0x20;
const PINMUX_SDIO0_D0: usize = 0x24;
const PINMUX_SDIO0_D1: usize = 0x28;
const PINMUX_SDIO0_D2: usize = 0x2c;
const PINMUX_SDIO0_D3: usize = 0x30;
const PINMUX_FUNC_SDIO0: u8 = 0x0;
const PINMUX_FUNC_XGPIO: u8 = 0x3;

const IO_SDIO0_CD: usize = 0x900;
const IO_SDIO0_PWR_EN: usize = 0x904;
const IO_SDIO0_CLK: usize = 0xa00;
const IO_SDIO0_CMD: usize = 0xa04;
const IO_SDIO0_D0: usize = 0xa08;
const IO_SDIO0_D1: usize = 0xa0c;
const IO_SDIO0_D2: usize = 0xa10;
const IO_SDIO0_D3: usize = 0xa14;
const IO_PULL_UP: u8 = 1 << 2;
const IO_PULL_DOWN: u8 = 1 << 3;

const REG_HOST_CONTROL1: usize = 0x28;
const REG_HOST_CONTROL2: usize = 0x3e;
const HOST_CTRL1_HIGH_SPEED: u8 = 1 << 2;
const HOST_CTRL2_UHS_MODE_MASK: u16 = 0x0007;
const HOST_CTRL2_UHS_SDR12: u16 = 0x0000;
const HOST_CTRL2_UHS_SDR25: u16 = 0x0001;

const CVI_VENDOR_MSHC_CTRL: usize = 0x200;
const CVI_PHY_TX_RX_DLY: usize = 0x240;
const CVI_PHY_CONFIG: usize = 0x24c;
const MSHC_CTRL_DS_HS_BITS: u32 = (1 << 1) | (1 << 8) | (1 << 9);
const PHY_TX_RX_DLY_DS_HS: u32 = 0x0100_0100;
const PHY_CONFIG_DS_HS: u32 = 1;

/// Already-mapped MMIO regions required by the portable CV181x wrapper.
///
/// The capability is move-only so one mapping cannot be installed into two
/// independently movable host objects:
///
/// ```compile_fail
/// use cv181x_sdhci::Cv181xMmio;
///
/// fn duplicate_mapping(mmio: Cv181xMmio) {
///     let first_owner = mmio;
///     let second_owner = mmio;
///     drop((first_owner, second_owner));
/// }
/// ```
pub struct Cv181xMmio {
    core: NonNull<u8>,
    syscon: NonNull<u8>,
}

impl Cv181xMmio {
    /// Create an exclusive mapped-register capability.
    ///
    /// # Safety
    ///
    /// `core` must point to a naturally aligned, exclusively owned CV181x
    /// SDHCI register block. `syscon` must be naturally aligned and cover
    /// TOP_BASE including the pinmux block. Both mappings must remain valid and
    /// accessible from every CPU to which the resulting host may move, until
    /// the host and its registered IRQ endpoint have been destroyed. The
    /// caller must not access either mapping through another pointer while the
    /// capability is alive.
    pub const unsafe fn new(core: NonNull<u8>, syscon: NonNull<u8>) -> Self {
        Self { core, syscon }
    }

    const fn core(&self) -> NonNull<u8> {
        self.core
    }

    const fn syscon(&self) -> NonNull<u8> {
        self.syscon
    }

    fn pinmux(&self) -> NonNull<u8> {
        // SAFETY: OS glue maps the CV181x syscon window. The documented
        // pinmux block lives at TOP_BASE + 0x1000 inside that mapping.
        unsafe { NonNull::new_unchecked(self.syscon.as_ptr().add(SYSCON_PINMUX_OFFSET)) }
    }
}

/// Board/device policy for the CV181x SD-card controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cv181xConfig {
    pub src_frequency_hz: u32,
    pub min_frequency_hz: u32,
    pub max_frequency_hz: u32,
    pub max_bus_width: BusWidth,
    pub no_1v8: bool,
    pub has_card_detect_gpio: bool,
    pub touch_power_enable_pin: bool,
}

impl Default for Cv181xConfig {
    fn default() -> Self {
        Self {
            src_frequency_hz: DEFAULT_SRC_FREQUENCY_HZ,
            min_frequency_hz: DEFAULT_MIN_FREQUENCY_HZ,
            max_frequency_hz: DEFAULT_MAX_FREQUENCY_HZ,
            max_bus_width: BusWidth::Bit4,
            no_1v8: true,
            has_card_detect_gpio: false,
            touch_power_enable_pin: false,
        }
    }
}

impl Cv181xConfig {
    pub fn normalized(mut self) -> Self {
        if self.src_frequency_hz == 0 {
            self.src_frequency_hz = DEFAULT_SRC_FREQUENCY_HZ;
        }
        if self.min_frequency_hz == 0 {
            self.min_frequency_hz = DEFAULT_MIN_FREQUENCY_HZ;
        }
        if self.max_frequency_hz == 0 {
            self.max_frequency_hz = DEFAULT_MAX_FREQUENCY_HZ;
        }
        if self.max_frequency_hz < self.min_frequency_hz {
            self.max_frequency_hz = self.min_frequency_hz;
        }
        self
    }

    fn clamp_clock(self, hz: u32) -> u32 {
        if hz == 0 {
            return 0;
        }
        hz.clamp(self.min_frequency_hz, self.max_frequency_hz)
    }

    fn supports_bus_width(self, width: BusWidth) -> bool {
        matches!(
            (self.max_bus_width, width),
            (BusWidth::Bit1, BusWidth::Bit1)
                | (BusWidth::Bit4, BusWidth::Bit1 | BusWidth::Bit4)
                | (
                    BusWidth::Bit8,
                    BusWidth::Bit1 | BusWidth::Bit4 | BusWidth::Bit8
                )
        )
    }
}

/// CV181x SD-card host endpoint.
pub struct Cv181xSdhci {
    inner: Sdhci,
    mmio: Cv181xMmio,
    config: Cv181xConfig,
}

// SAFETY: `Cv181xMmio::new` requires both mappings to remain valid and
// accessible after a move to another CPU. The wrapper does not expose its
// mutable register endpoints; IRQ extraction uses the pre-registered SDHCI IRQ
// core.
unsafe impl Send for Cv181xSdhci {}

impl Cv181xSdhci {
    /// Construct a discovery-stage CV181x SD-card host over mapped MMIO.
    ///
    /// Construction does not touch controller or board registers. The staged
    /// initializer binds and enables the IRQ endpoint before ResetAll or
    /// PowerOn applies the platform configuration.
    pub fn new(mmio: Cv181xMmio, config: Cv181xConfig) -> Self {
        let config = config.normalized();
        // SAFETY: `Cv181xMmio` can only be constructed under the mapping,
        // alignment, exclusivity, and lifetime contract required by SDHCI.
        let mut inner = unsafe { Sdhci::new(mmio.core()) };
        inner.set_base_clock_hz(
            NonZeroU32::new(config.src_frequency_hz)
                .expect("normalized CV181x source frequency is non-zero"),
        );
        Self {
            inner,
            mmio,
            config,
        }
    }

    pub const fn config(&self) -> Cv181xConfig {
        self.config
    }

    pub fn set_dma(&mut self, dma: DeviceDma) {
        self.inner.set_dma(dma);
    }

    fn configure_sd_power_on(&mut self) {
        self.restore_3v3_power();
        self.setup_sd_pad(false);
        self.setup_sd_io(false);
        self.restore_ds_hs_phy();
    }

    fn configure_sd_power_off(&mut self) {
        self.setup_sd_pad(true);
        self.setup_sd_io(true);
        self.close_power();
    }

    fn restore_3v3_power(&mut self) {
        self.update_top_power(TOP_SD_PWRSW_3V3);
    }

    fn close_power(&mut self) {
        self.update_top_power(TOP_SD_PWRSW_OFF);
    }

    fn setup_sd_pad(&mut self, unplug: bool) {
        let pinmux = self.mmio.pinmux();
        let active_cd_func = if self.config.has_card_detect_gpio {
            PINMUX_FUNC_XGPIO
        } else {
            PINMUX_FUNC_SDIO0
        };
        write_u8(pinmux, PINMUX_SDIO0_CD, active_cd_func);

        if self.config.touch_power_enable_pin {
            write_u8(pinmux, PINMUX_SDIO0_PWR_EN, PINMUX_FUNC_SDIO0);
        }

        let func = if unplug {
            PINMUX_FUNC_XGPIO
        } else {
            PINMUX_FUNC_SDIO0
        };
        for off in [
            PINMUX_SDIO0_CLK,
            PINMUX_SDIO0_CMD,
            PINMUX_SDIO0_D0,
            PINMUX_SDIO0_D1,
            PINMUX_SDIO0_D2,
            PINMUX_SDIO0_D3,
        ] {
            write_u8(pinmux, off, func);
        }
    }

    fn setup_sd_io(&mut self, reset: bool) {
        let pinmux = self.mmio.pinmux();
        set_pull(pinmux, IO_SDIO0_CD, IO_PULL_UP, IO_PULL_DOWN);
        set_pull(pinmux, IO_SDIO0_PWR_EN, IO_PULL_DOWN, IO_PULL_UP);
        set_pull(pinmux, IO_SDIO0_CLK, IO_PULL_DOWN, IO_PULL_UP);

        let (set, clear) = if reset {
            (IO_PULL_DOWN, IO_PULL_UP)
        } else {
            (IO_PULL_UP, IO_PULL_DOWN)
        };
        for off in [
            IO_SDIO0_CMD,
            IO_SDIO0_D0,
            IO_SDIO0_D1,
            IO_SDIO0_D2,
            IO_SDIO0_D3,
        ] {
            set_pull(pinmux, off, set, clear);
        }
    }

    fn restore_ds_hs_phy(&mut self) {
        let core = self.mmio.core();
        let mshc = read_u32(core, CVI_VENDOR_MSHC_CTRL) | MSHC_CTRL_DS_HS_BITS;
        write_u32(core, CVI_VENDOR_MSHC_CTRL, mshc);
        write_u32(core, CVI_PHY_TX_RX_DLY, PHY_TX_RX_DLY_DS_HS);
        write_u32(core, CVI_PHY_CONFIG, PHY_CONFIG_DS_HS);
    }

    fn update_top_power(&mut self, low_bits: u32) {
        let cur = read_u32(self.mmio.syscon(), TOP_SD_PWRSW_CTRL);
        write_u32(
            self.mmio.syscon(),
            TOP_SD_PWRSW_CTRL,
            (cur & !TOP_SD_PWRSW_LOW_MASK) | low_bits,
        );
    }

    fn clock_plan(&self, speed: ClockSpeed) -> Result<Cv181xClockPlan, sdio_host2::Error> {
        match speed {
            ClockSpeed::Identification => Ok(Cv181xClockPlan::new(
                self.config.clamp_clock(self.config.min_frequency_hz),
                false,
                HOST_CTRL2_UHS_SDR12,
            )),
            ClockSpeed::Default | ClockSpeed::Sdr12 => Ok(Cv181xClockPlan::new(
                self.config.clamp_clock(25_000_000),
                false,
                HOST_CTRL2_UHS_SDR12,
            )),
            ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => Ok(Cv181xClockPlan::new(
                self.config.clamp_clock(50_000_000),
                true,
                HOST_CTRL2_UHS_SDR25,
            )),
            ClockSpeed::Sdr50 | ClockSpeed::Sdr104 | ClockSpeed::Ddr50 | ClockSpeed::Hs200
                if self.config.no_1v8 =>
            {
                Err(sdio_host2::Error::Unsupported)
            }
            ClockSpeed::Sdr50 | ClockSpeed::Sdr104 | ClockSpeed::Ddr50 | ClockSpeed::Hs200 => {
                Err(sdio_host2::Error::Unsupported)
            }
            _ => Err(sdio_host2::Error::Unsupported),
        }
    }

    fn set_host_timing_bits(&mut self, high_speed: bool, uhs_mode: u16) {
        let core = self.mmio.core();
        let mut ctrl1 = read_u8(core, REG_HOST_CONTROL1);
        if high_speed {
            ctrl1 |= HOST_CTRL1_HIGH_SPEED;
        } else {
            ctrl1 &= !HOST_CTRL1_HIGH_SPEED;
        }
        write_u8(core, REG_HOST_CONTROL1, ctrl1);

        let ctrl2 = (read_u16(core, REG_HOST_CONTROL2) & !HOST_CTRL2_UHS_MODE_MASK)
            | (uhs_mode & HOST_CTRL2_UHS_MODE_MASK);
        write_u16(core, REG_HOST_CONTROL2, ctrl2);
    }

    fn apply_after(&mut self, after: AfterBusOp) -> Result<(), sdio_host2::Error> {
        match after {
            AfterBusOp::None => Ok(()),
            AfterBusOp::PowerOn | AfterBusOp::ResetAll => {
                self.configure_sd_power_on();
                Ok(())
            }
        }
    }

    unsafe fn submit_clock_plan(
        &mut self,
        plan: Cv181xClockPlan,
    ) -> Result<BusRequest, sdio_host2::Error> {
        let request = unsafe {
            sdio_host2::SdioHost::submit_bus_op(
                &mut self.inner,
                sdio_host2::BusOp::SetClockHz(ClockHz(plan.target_hz)),
            )?
        };
        self.set_host_timing_bits(plan.high_speed, plan.uhs_mode);
        Ok(BusRequest::inner(request, AfterBusOp::None))
    }
}

impl SdioHost2Irq for Cv181xSdhci {
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

    fn poll_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<sdio_host2::RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
    where
        Self: 'a,
    {
        sdio_host2::SdioHost::poll_transaction(&mut self.inner, request)
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
                let request = unsafe { sdio_host2::SdioHost::submit_bus_op(&mut self.inner, op)? };
                self.configure_sd_power_off();
                Ok(BusRequest::inner(request, AfterBusOp::None))
            }
            sdio_host2::BusOp::ResetAll => {
                let request = unsafe { sdio_host2::SdioHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::ResetAll))
            }
            sdio_host2::BusOp::SetClock(speed) => {
                let plan = self.clock_plan(speed)?;
                unsafe { self.submit_clock_plan(plan) }
            }
            sdio_host2::BusOp::SetClockHz(ClockHz(hz)) => unsafe {
                self.submit_clock_plan(Cv181xClockPlan::new(
                    self.config.clamp_clock(hz),
                    hz > DEFAULT_MAX_FREQUENCY_HZ,
                    HOST_CTRL2_UHS_SDR12,
                ))
            },
            sdio_host2::BusOp::SetBusWidth(width) if !self.config.supports_bus_width(width) => {
                Ok(BusRequest::ready(Err(sdio_host2::Error::Unsupported)))
            }
            sdio_host2::BusOp::SetSignalVoltage(SignalVoltage::V180) if self.config.no_1v8 => {
                Ok(BusRequest::ready(Err(sdio_host2::Error::Unsupported)))
            }
            sdio_host2::BusOp::SetSignalVoltage(SignalVoltage::V330) => {
                let request = unsafe { sdio_host2::SdioHost::submit_bus_op(&mut self.inner, op)? };
                self.restore_3v3_power();
                Ok(BusRequest::inner(request, AfterBusOp::None))
            }
            _ => {
                let request = unsafe { sdio_host2::SdioHost::submit_bus_op(&mut self.inner, op)? };
                Ok(BusRequest::inner(request, AfterBusOp::None))
            }
        }
    }

    fn poll_bus_op(
        &mut self,
        bus_request: &mut Self::BusRequest,
    ) -> Result<RequestPoll<()>, sdio_host2::PollRequestError> {
        match &mut bus_request.state {
            BusRequestState::Ready(result) => {
                let result = result
                    .take()
                    .ok_or(sdio_host2::PollRequestError::AlreadyCompleted)?;
                bus_request.state = BusRequestState::Done;
                Ok(RequestPoll::Ready(result))
            }
            BusRequestState::Inner {
                request: inner,
                after,
            } => match sdio_host2::SdioHost::poll_bus_op(&mut self.inner, inner)? {
                RequestPoll::Pending => Ok(RequestPoll::Pending),
                RequestPoll::Ready(result) => {
                    let result = result.and_then(|()| self.apply_after(*after));
                    bus_request.state = BusRequestState::Done;
                    Ok(RequestPoll::Ready(result))
                }
            },
            BusRequestState::Done => Err(sdio_host2::PollRequestError::AlreadyCompleted),
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

impl SdioHost2Timed for Cv181xSdhci {
    fn poll_transaction_at<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        now_ns: u64,
    ) -> Result<RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
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
        bus_request: &mut Self::BusRequest,
        now_ns: u64,
    ) -> Result<RequestPoll<()>, sdio_host2::PollRequestError> {
        match &mut bus_request.state {
            BusRequestState::Ready(result) => {
                let result = result
                    .take()
                    .ok_or(sdio_host2::PollRequestError::AlreadyCompleted)?;
                bus_request.state = BusRequestState::Done;
                Ok(RequestPoll::Ready(result))
            }
            BusRequestState::Inner {
                request: inner,
                after,
            } => match SdioHost2Timed::poll_bus_op_at(&mut self.inner, inner, now_ns)? {
                RequestPoll::Pending => Ok(RequestPoll::Pending),
                RequestPoll::Ready(result) => {
                    let result = result.and_then(|()| self.apply_after(*after));
                    bus_request.state = BusRequestState::Done;
                    Ok(RequestPoll::Ready(result))
                }
            },
            BusRequestState::Done => Err(sdio_host2::PollRequestError::AlreadyCompleted),
        }
    }

    fn bus_op_wake_at(&self, request: &Self::BusRequest) -> Option<u64> {
        match &request.state {
            BusRequestState::Inner { request, .. } => {
                SdioHost2Timed::bus_op_wake_at(&self.inner, request)
            }
            BusRequestState::Ready(_) | BusRequestState::Done => None,
        }
    }
}

/// Recovery state retained while the CV181x controller is detached from I/O.
pub struct Cv181xRecoveryState {
    inner: sdhci_host::SdhciRecoveryState,
}

impl SdioHost2Lifecycle for Cv181xSdhci {
    type RecoveryState = Cv181xRecoveryState;

    fn begin_recovery(
        &mut self,
        cause: rdif_block::RecoveryCause,
    ) -> Result<Self::RecoveryState, ProtocolError> {
        SdioHost2Lifecycle::begin_recovery(&mut self.inner, cause)
            .map(|inner| Cv181xRecoveryState { inner })
    }

    fn poll_dma_quiesce(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        SdioHost2Lifecycle::poll_dma_quiesce(&mut self.inner, &mut state.inner, input)
    }

    fn begin_reinitialize(&mut self, state: &mut Self::RecoveryState) -> Result<(), ProtocolError> {
        SdioHost2Lifecycle::begin_reinitialize(&mut self.inner, &mut state.inner)
    }

    fn poll_reinitialize(
        &mut self,
        state: &mut Self::RecoveryState,
        input: rdif_block::InitInput,
    ) -> rdif_block::InitPoll<()> {
        let progress =
            SdioHost2Lifecycle::poll_reinitialize(&mut self.inner, &mut state.inner, input);
        if matches!(progress, rdif_block::InitPoll::Ready(())) {
            self.configure_sd_power_on();
        }
        progress
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
enum AfterBusOp {
    None,
    PowerOn,
    ResetAll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cv181xClockPlan {
    target_hz: u32,
    high_speed: bool,
    uhs_mode: u16,
}

impl Cv181xClockPlan {
    const fn new(target_hz: u32, high_speed: bool, uhs_mode: u16) -> Self {
        Self {
            target_hz,
            high_speed,
            uhs_mode,
        }
    }
}

fn set_pull(base: NonNull<u8>, off: usize, set: u8, clear: u8) {
    let next = (read_u8(base, off) | set) & !clear;
    write_u8(base, off, next);
}

fn read_u8(base: NonNull<u8>, off: usize) -> u8 {
    // SAFETY: caller-provided MMIO base covers the documented byte register.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u8) }
}

fn write_u8(base: NonNull<u8>, off: usize, val: u8) {
    // SAFETY: caller-provided MMIO base covers the documented byte register.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off), val) }
}

fn read_u16(base: NonNull<u8>, off: usize) -> u16 {
    // SAFETY: caller-provided MMIO base covers the documented 16-bit register.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u16) }
}

fn write_u16(base: NonNull<u8>, off: usize, val: u16) {
    // SAFETY: caller-provided MMIO base covers the documented 16-bit register.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u16, val) }
}

fn read_u32(base: NonNull<u8>, off: usize) -> u32 {
    // SAFETY: caller-provided MMIO base covers the documented 32-bit register.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u32) }
}

fn write_u32(base: NonNull<u8>, off: usize, val: u32) {
    // SAFETY: caller-provided MMIO base covers the documented 32-bit register.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u32, val) }
}

#[cfg(test)]
mod tests;
