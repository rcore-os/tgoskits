//! CV181x MMIO resources and board policy.

use core::ptr::NonNull;

use sdio_host2::BusWidth;

pub(super) const DEFAULT_SRC_FREQUENCY_HZ: u32 = 375_000_000;
pub(super) const DEFAULT_MIN_FREQUENCY_HZ: u32 = 400_000;
pub(super) const DEFAULT_MAX_FREQUENCY_HZ: u32 = 25_000_000;

/// CV181x TOP syscon physical base used by the SD0 power/pinmux registers.
pub const CV181X_TOP_SYSCON_BASE: u64 = 0x0300_0000;
/// Minimum syscon mapping required by this wrapper (TOP + pinmux/IO window).
pub const CV181X_SYSCON_REQUIRED_SIZE: usize = 0x2000;

pub(super) const SYSCON_PINMUX_OFFSET: usize = 0x1000;

pub(super) const TOP_SD_PWRSW_CTRL: usize = 0x1f4;
pub(super) const TOP_SD_PWRSW_3V3: u32 = 0x9;
pub(super) const TOP_SD_PWRSW_OFF: u32 = 0xe;
pub(super) const TOP_SD_PWRSW_LOW_MASK: u32 = 0xf;

pub(super) const PINMUX_SDIO0_CD: usize = 0x34;
pub(super) const PINMUX_SDIO0_PWR_EN: usize = 0x38;
pub(super) const PINMUX_SDIO0_CLK: usize = 0x1c;
pub(super) const PINMUX_SDIO0_CMD: usize = 0x20;
pub(super) const PINMUX_SDIO0_D0: usize = 0x24;
pub(super) const PINMUX_SDIO0_D1: usize = 0x28;
pub(super) const PINMUX_SDIO0_D2: usize = 0x2c;
pub(super) const PINMUX_SDIO0_D3: usize = 0x30;
pub(super) const PINMUX_FUNC_SDIO0: u8 = 0x0;
pub(super) const PINMUX_FUNC_XGPIO: u8 = 0x3;

pub(super) const IO_SDIO0_CD: usize = 0x900;
pub(super) const IO_SDIO0_PWR_EN: usize = 0x904;
pub(super) const IO_SDIO0_CLK: usize = 0xa00;
pub(super) const IO_SDIO0_CMD: usize = 0xa04;
pub(super) const IO_SDIO0_D0: usize = 0xa08;
pub(super) const IO_SDIO0_D1: usize = 0xa0c;
pub(super) const IO_SDIO0_D2: usize = 0xa10;
pub(super) const IO_SDIO0_D3: usize = 0xa14;
pub(super) const IO_PULL_UP: u8 = 1 << 2;
pub(super) const IO_PULL_DOWN: u8 = 1 << 3;

pub(super) const REG_HOST_CONTROL1: usize = 0x28;
pub(super) const REG_HOST_CONTROL2: usize = 0x3e;
pub(super) const HOST_CTRL1_HIGH_SPEED: u8 = 1 << 2;
pub(super) const HOST_CTRL2_UHS_MODE_MASK: u16 = 0x0007;
pub(super) const HOST_CTRL2_UHS_SDR12: u16 = 0x0000;
pub(super) const HOST_CTRL2_UHS_SDR25: u16 = 0x0001;

pub(super) const CVI_VENDOR_MSHC_CTRL: usize = 0x200;
pub(super) const CVI_PHY_TX_RX_DLY: usize = 0x240;
pub(super) const CVI_PHY_CONFIG: usize = 0x24c;
pub(super) const MSHC_CTRL_DS_HS_BITS: u32 = (1 << 1) | (1 << 8) | (1 << 9);
pub(super) const PHY_TX_RX_DLY_DS_HS: u32 = 0x0100_0100;
pub(super) const PHY_CONFIG_DS_HS: u32 = 1;

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

    pub(super) fn pinmux(self) -> NonNull<u8> {
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

    pub(super) fn clamp_clock(self, hz: u32) -> u32 {
        if hz == 0 {
            return 0;
        }
        hz.clamp(self.min_frequency_hz, self.max_frequency_hz)
    }

    pub(super) fn supports_bus_width(self, width: BusWidth) -> bool {
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

pub(super) fn set_pull(base: NonNull<u8>, off: usize, set: u8, clear: u8) {
    let next = (read_u8(base, off) | set) & !clear;
    write_u8(base, off, next);
}

pub(super) fn read_u8(base: NonNull<u8>, off: usize) -> u8 {
    // SAFETY: caller-provided MMIO base covers the documented byte register.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u8) }
}

pub(super) fn write_u8(base: NonNull<u8>, off: usize, val: u8) {
    // SAFETY: caller-provided MMIO base covers the documented byte register.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off), val) }
}

pub(super) fn read_u16(base: NonNull<u8>, off: usize) -> u16 {
    // SAFETY: caller-provided MMIO base covers the documented 16-bit register.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u16) }
}

pub(super) fn write_u16(base: NonNull<u8>, off: usize, val: u16) {
    // SAFETY: caller-provided MMIO base covers the documented 16-bit register.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u16, val) }
}

pub(super) fn read_u32(base: NonNull<u8>, off: usize) -> u32 {
    // SAFETY: caller-provided MMIO base covers the documented 32-bit register.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(off) as *const u32) }
}

pub(super) fn write_u32(base: NonNull<u8>, off: usize, val: u32) {
    // SAFETY: caller-provided MMIO base covers the documented 32-bit register.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(off) as *mut u32, val) }
}
