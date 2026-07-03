//! CV181x/SG2002 SD-card host wrapper for the generic SDHCI backend.
//!
//! This crate owns only the Cvitek-specific top/pinmux/PHY programming around
//! the controller. Command, data, interrupt-status caching, and RDIF queue
//! semantics are delegated to [`sdhci_host::Sdhci`].

#![no_std]

use core::ptr::NonNull;

use sdhci_host::Sdhci;
use sdio_host2::{BusWidth, ClockHz, ClockSpeed, RequestPoll, SignalVoltage};
use sdmmc_protocol::{Error as ProtocolError, sdio::SdioHost2Irq};

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
#[derive(Clone, Copy)]
pub struct Cv181xMmio {
    core: NonNull<u8>,
    syscon: NonNull<u8>,
}

impl Cv181xMmio {
    pub const fn new(core: NonNull<u8>, syscon: NonNull<u8>) -> Self {
        Self { core, syscon }
    }

    pub const fn core(self) -> NonNull<u8> {
        self.core
    }

    pub const fn syscon(self) -> NonNull<u8> {
        self.syscon
    }

    fn pinmux(self) -> NonNull<u8> {
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

// SAFETY: The wrapper owns exclusive access to one SDHCI register file and the
// board-level syscon/pinmux window for the controller lifetime. It does not
// expose shared mutable access; IRQ extraction uses the cloned SDHCI IRQ core.
unsafe impl Send for Cv181xSdhci {}

impl Cv181xSdhci {
    /// Construct a CV181x SD-card host over already-mapped MMIO.
    ///
    /// # Safety
    ///
    /// `mmio.core` must point to an exclusively-owned CV181x SDHCI register
    /// block and `mmio.syscon` must cover TOP_BASE including the pinmux block.
    pub unsafe fn new(mmio: Cv181xMmio, config: Cv181xConfig) -> Self {
        let inner = unsafe { Sdhci::new(mmio.core()) };
        let mut this = Self {
            inner,
            mmio,
            config: config.normalized(),
        };
        this.restore_ds_hs_phy();
        this
    }

    pub const fn config(&self) -> Cv181xConfig {
        self.config
    }

    pub fn inner(&self) -> &Sdhci {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut Sdhci {
        &mut self.inner
    }

    pub fn into_inner(self) -> Sdhci {
        self.inner
    }

    pub fn configure_sd_power_on(&mut self) {
        self.restore_3v3_power();
        self.setup_sd_pad(false);
        self.setup_sd_io(false);
        self.restore_ds_hs_phy();
    }

    pub fn configure_sd_power_off(&mut self) {
        self.setup_sd_pad(true);
        self.setup_sd_io(true);
        self.close_power();
    }

    pub fn restore_3v3_power(&mut self) {
        self.update_top_power(TOP_SD_PWRSW_3V3);
    }

    pub fn close_power(&mut self) {
        self.update_top_power(TOP_SD_PWRSW_OFF);
    }

    pub fn setup_sd_pad(&mut self, unplug: bool) {
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

    pub fn setup_sd_io(&mut self, reset: bool) {
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

    pub fn restore_ds_hs_phy(&mut self) {
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

    fn program_clock(
        &mut self,
        target_hz: u32,
        high_speed: bool,
        uhs_mode: u16,
    ) -> Result<(), sdio_host2::Error> {
        let target_hz = self.config.clamp_clock(target_hz);
        self.set_host_timing_bits(high_speed, uhs_mode);
        self.inner
            .enable_clock(self.config.src_frequency_hz, target_hz)
            .map_err(map_protocol_error)
    }

    fn set_clock_speed(&mut self, speed: ClockSpeed) -> Result<(), sdio_host2::Error> {
        match speed {
            ClockSpeed::Identification => {
                self.program_clock(self.config.min_frequency_hz, false, HOST_CTRL2_UHS_SDR12)
            }
            ClockSpeed::Default | ClockSpeed::Sdr12 => {
                self.program_clock(25_000_000, false, HOST_CTRL2_UHS_SDR12)
            }
            ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => {
                self.program_clock(50_000_000, true, HOST_CTRL2_UHS_SDR25)
            }
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

fn map_protocol_error(err: ProtocolError) -> sdio_host2::Error {
    match err {
        ProtocolError::Timeout(_) => sdio_host2::Error::Timeout,
        ProtocolError::Crc(_) => sdio_host2::Error::Crc,
        ProtocolError::NoCard => sdio_host2::Error::NoCard,
        ProtocolError::Busy => sdio_host2::Error::Busy,
        ProtocolError::UnsupportedCommand => sdio_host2::Error::Unsupported,
        ProtocolError::Misaligned => sdio_host2::Error::Misaligned,
        ProtocolError::InvalidArgument => sdio_host2::Error::InvalidArgument,
        ProtocolError::BusError(_) => sdio_host2::Error::Bus,
        ProtocolError::ReadError(_)
        | ProtocolError::WriteError(_)
        | ProtocolError::BadResponse(_) => sdio_host2::Error::Bus,
        ProtocolError::CardError(_) | ProtocolError::CardLocked => sdio_host2::Error::Controller,
        _ => sdio_host2::Error::Controller,
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
mod tests {
    extern crate std;

    use super::*;

    #[repr(align(4))]
    struct FakeMmio<const N: usize>([u8; N]);

    impl<const N: usize> FakeMmio<N> {
        fn new() -> Self {
            Self([0; N])
        }

        fn base(&mut self) -> NonNull<u8> {
            NonNull::new(self.0.as_mut_ptr()).unwrap()
        }
    }

    fn new_host<'a>(
        core: &'a mut FakeMmio<0x400>,
        syscon: &'a mut FakeMmio<0x2000>,
        config: Cv181xConfig,
    ) -> Cv181xSdhci {
        let mmio = Cv181xMmio::new(core.base(), syscon.base());
        unsafe { Cv181xSdhci::new(mmio, config) }
    }

    fn poll_ready_bus_op(
        host: &mut Cv181xSdhci,
        request: &mut BusRequest,
    ) -> Result<(), sdio_host2::Error> {
        match sdio_host2::SdioHost::poll_bus_op(host, request).unwrap() {
            RequestPoll::Ready(result) => result,
            RequestPoll::Pending => panic!("test bus op should complete synchronously"),
        }
    }

    #[test]
    fn power_on_sequence_configures_3v3_pads_io_and_ds_hs_phy() {
        let mut core = FakeMmio::new();
        let mut syscon = FakeMmio::new();
        write_u32(syscon.base(), TOP_SD_PWRSW_CTRL, 0xa5a5_a5a0);
        write_u8(
            unsafe { NonNull::new_unchecked(syscon.base().as_ptr().add(SYSCON_PINMUX_OFFSET)) },
            PINMUX_SDIO0_PWR_EN,
            0x7,
        );

        let mut host = new_host(
            &mut core,
            &mut syscon,
            Cv181xConfig {
                has_card_detect_gpio: true,
                ..Cv181xConfig::default()
            },
        );
        host.configure_sd_power_on();

        let pinmux =
            unsafe { NonNull::new_unchecked(syscon.base().as_ptr().add(SYSCON_PINMUX_OFFSET)) };
        assert_eq!(
            read_u32(syscon.base(), TOP_SD_PWRSW_CTRL),
            0xa5a5_a5a0 | TOP_SD_PWRSW_3V3
        );
        assert_eq!(read_u8(pinmux, PINMUX_SDIO0_CD), PINMUX_FUNC_XGPIO);
        assert_eq!(read_u8(pinmux, PINMUX_SDIO0_CLK), PINMUX_FUNC_SDIO0);
        assert_eq!(read_u8(pinmux, PINMUX_SDIO0_CMD), PINMUX_FUNC_SDIO0);
        assert_eq!(read_u8(pinmux, PINMUX_SDIO0_D3), PINMUX_FUNC_SDIO0);
        assert_eq!(read_u8(pinmux, PINMUX_SDIO0_PWR_EN), 0x7);
        assert_eq!(read_u8(pinmux, IO_SDIO0_CMD) & IO_PULL_UP, IO_PULL_UP);
        assert_eq!(read_u8(pinmux, IO_SDIO0_CMD) & IO_PULL_DOWN, 0);
        assert_eq!(
            read_u32(core.base(), CVI_PHY_TX_RX_DLY),
            PHY_TX_RX_DLY_DS_HS
        );
        assert_eq!(read_u32(core.base(), CVI_PHY_CONFIG), PHY_CONFIG_DS_HS);
        assert_eq!(
            read_u32(core.base(), CVI_VENDOR_MSHC_CTRL) & MSHC_CTRL_DS_HS_BITS,
            MSHC_CTRL_DS_HS_BITS
        );
    }

    #[test]
    fn power_off_switches_sd_pads_to_gpio_and_closes_power() {
        let mut core = FakeMmio::new();
        let mut syscon = FakeMmio::new();
        let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());

        host.configure_sd_power_off();

        let pinmux =
            unsafe { NonNull::new_unchecked(syscon.base().as_ptr().add(SYSCON_PINMUX_OFFSET)) };
        assert_eq!(read_u8(pinmux, PINMUX_SDIO0_CLK), PINMUX_FUNC_XGPIO);
        assert_eq!(read_u8(pinmux, PINMUX_SDIO0_D0), PINMUX_FUNC_XGPIO);
        assert_eq!(read_u8(pinmux, IO_SDIO0_D0) & IO_PULL_DOWN, IO_PULL_DOWN);
        assert_eq!(
            read_u32(syscon.base(), TOP_SD_PWRSW_CTRL) & TOP_SD_PWRSW_LOW_MASK,
            TOP_SD_PWRSW_OFF
        );
    }

    #[test]
    fn config_normalization_keeps_clock_bounds_valid() {
        let config = Cv181xConfig {
            src_frequency_hz: 0,
            min_frequency_hz: 50_000_000,
            max_frequency_hz: 25_000_000,
            ..Cv181xConfig::default()
        }
        .normalized();

        assert_eq!(config.src_frequency_hz, DEFAULT_SRC_FREQUENCY_HZ);
        assert_eq!(config.max_frequency_hz, 50_000_000);
    }

    #[test]
    fn bus_width_limit_rejects_width_above_board_wiring() {
        let mut core = FakeMmio::new();
        let mut syscon = FakeMmio::new();
        let mut host = new_host(
            &mut core,
            &mut syscon,
            Cv181xConfig {
                max_bus_width: BusWidth::Bit1,
                ..Cv181xConfig::default()
            },
        );

        let mut request = unsafe {
            sdio_host2::SdioHost::submit_bus_op(
                &mut host,
                sdio_host2::BusOp::SetBusWidth(BusWidth::Bit4),
            )
        }
        .unwrap();

        assert_eq!(
            poll_ready_bus_op(&mut host, &mut request),
            Err(sdio_host2::Error::Unsupported)
        );
    }

    #[test]
    fn no_1v8_rejects_uhs_clock_and_voltage_paths() {
        let mut core = FakeMmio::new();
        let mut syscon = FakeMmio::new();
        let mut host = new_host(
            &mut core,
            &mut syscon,
            Cv181xConfig {
                no_1v8: true,
                ..Cv181xConfig::default()
            },
        );

        assert_eq!(
            host.set_clock_speed(ClockSpeed::Sdr50),
            Err(sdio_host2::Error::Unsupported)
        );

        let mut request = unsafe {
            sdio_host2::SdioHost::submit_bus_op(
                &mut host,
                sdio_host2::BusOp::SetSignalVoltage(SignalVoltage::V180),
            )
        }
        .unwrap();

        assert_eq!(
            poll_ready_bus_op(&mut host, &mut request),
            Err(sdio_host2::Error::Unsupported)
        );
    }

    #[test]
    fn high_speed_mode_sets_host_timing_even_when_clock_is_capped() {
        let mut core = FakeMmio::new();
        let mut syscon = FakeMmio::new();
        let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());

        let _ = host.set_clock_speed(ClockSpeed::HighSpeed);

        assert_eq!(
            read_u8(core.base(), REG_HOST_CONTROL1) & HOST_CTRL1_HIGH_SPEED,
            HOST_CTRL1_HIGH_SPEED
        );
        assert_eq!(
            read_u16(core.base(), REG_HOST_CONTROL2) & HOST_CTRL2_UHS_MODE_MASK,
            HOST_CTRL2_UHS_SDR25
        );
    }
}
