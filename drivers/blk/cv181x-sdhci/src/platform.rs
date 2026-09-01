//! CV181x MMIO resources and board policy.

use core::ptr::NonNull;

use sdmmc_host::BusWidth;
use tock_registers::{register_bitfields, register_structs, registers::ReadWrite};

pub(super) const DEFAULT_SRC_FREQUENCY_HZ: u32 = 375_000_000;
pub(super) const DEFAULT_MIN_FREQUENCY_HZ: u32 = 400_000;
pub(super) const DEFAULT_MAX_FREQUENCY_HZ: u32 = 25_000_000;

pub(super) const TOP_SD_PWRSW_3V3: u32 = 0x9;
pub(super) const TOP_SD_PWRSW_OFF: u32 = 0xe;

pub(super) const PINMUX_FUNC_SDIO0: u8 = 0x0;
pub(super) const PINMUX_FUNC_XGPIO: u8 = 0x3;

pub(super) const PHY_TX_RX_DLY_DS_HS: u32 = 0x0100_0100;
pub(super) const PHY_CONFIG_DS_HS: u32 = 1;

register_bitfields! [
    u8,

    pub HOST_CONTROL1 [
        HIGH_SPEED OFFSET(2) NUMBITS(1) []
    ],

    pub PINMUX [
        FUNCTION OFFSET(0) NUMBITS(3) []
    ],

    pub PAD_PULL [
        UP OFFSET(2) NUMBITS(1) [],
        DOWN OFFSET(3) NUMBITS(1) []
    ]
];

register_bitfields! [
    u16,

    pub HOST_CONTROL2 [
        UHS_MODE OFFSET(0) NUMBITS(3) [
            SDR12 = 0,
            SDR25 = 1
        ]
    ]
];

register_bitfields! [
    u32,

    pub TOP_SD_PWRSW_CTRL [
        LOW_BITS OFFSET(0) NUMBITS(4) []
    ],

    pub SD_CTRL_OPT [
        SD1_CARDDET_OVERRIDE OFFSET(8) NUMBITS(2) []
    ],

    pub MSHC_CTRL [
        DS_HS_BIT_1 OFFSET(1) NUMBITS(1) [],
        DS_HS_BIT_8 OFFSET(8) NUMBITS(1) [],
        DS_HS_BIT_9 OFFSET(9) NUMBITS(1) [],
        SD1_SEL OFFSET(16) NUMBITS(1) []
    ],

    pub PINMUX32 [
        FUNCTION OFFSET(0) NUMBITS(3) []
    ]
];

register_structs! {
    pub Cv181xCoreRegisters {
        (0x000 => _reserved0),
        (0x028 => pub host_control1: ReadWrite<u8, HOST_CONTROL1::Register>),
        (0x029 => _reserved1),
        (0x03e => pub host_control2: ReadWrite<u16, HOST_CONTROL2::Register>),
        (0x040 => _reserved2),
        (0x200 => pub mshc_ctrl: ReadWrite<u32, MSHC_CTRL::Register>),
        (0x204 => _reserved3),
        (0x240 => pub phy_tx_rx_dly: ReadWrite<u32>),
        (0x244 => _reserved4),
        (0x24c => pub phy_config: ReadWrite<u32>),
        (0x250 => @END),
    }
}

register_structs! {
    pub Cv181xSysconRegisters {
        (0x000 => _reserved0),
        (0x1f4 => pub sd_powersw_ctrl: ReadWrite<u32, TOP_SD_PWRSW_CTRL::Register>),
        (0x1f8 => _reserved1),
        (0x294 => pub sd_ctrl_opt: ReadWrite<u32, SD_CTRL_OPT::Register>),
        (0x298 => _reserved2),
        (0x101c => pub sdio0_clk_mux: ReadWrite<u8, PINMUX::Register>),
        (0x101d => _reserved3),
        (0x1020 => pub sdio0_cmd_mux: ReadWrite<u8, PINMUX::Register>),
        (0x1021 => _reserved4),
        (0x1024 => pub sdio0_d0_mux: ReadWrite<u8, PINMUX::Register>),
        (0x1025 => _reserved5),
        (0x1028 => pub sdio0_d1_mux: ReadWrite<u8, PINMUX::Register>),
        (0x1029 => _reserved6),
        (0x102c => pub sdio0_d2_mux: ReadWrite<u8, PINMUX::Register>),
        (0x102d => _reserved7),
        (0x1030 => pub sdio0_d3_mux: ReadWrite<u8, PINMUX::Register>),
        (0x1031 => _reserved8),
        (0x1034 => pub sdio0_cd_mux: ReadWrite<u8, PINMUX::Register>),
        (0x1035 => _reserved9),
        (0x1038 => pub sdio0_power_enable_mux: ReadWrite<u8, PINMUX::Register>),
        (0x1039 => _reserved10),
        (0x10d0 => pub sdio1_mux: [ReadWrite<u32, PINMUX32::Register>; 6]),
        (0x10e8 => _reserved_sdio1),
        (0x1900 => pub sdio0_cd_pull: ReadWrite<u8, PAD_PULL::Register>),
        (0x1901 => _reserved11),
        (0x1904 => pub sdio0_power_enable_pull: ReadWrite<u8, PAD_PULL::Register>),
        (0x1905 => _reserved12),
        (0x1a00 => pub sdio0_clk_pull: ReadWrite<u8, PAD_PULL::Register>),
        (0x1a01 => _reserved13),
        (0x1a04 => pub sdio0_cmd_pull: ReadWrite<u8, PAD_PULL::Register>),
        (0x1a05 => _reserved14),
        (0x1a08 => pub sdio0_d0_pull: ReadWrite<u8, PAD_PULL::Register>),
        (0x1a09 => _reserved15),
        (0x1a0c => pub sdio0_d1_pull: ReadWrite<u8, PAD_PULL::Register>),
        (0x1a0d => _reserved16),
        (0x1a10 => pub sdio0_d2_pull: ReadWrite<u8, PAD_PULL::Register>),
        (0x1a11 => _reserved17),
        (0x1a14 => pub sdio0_d3_pull: ReadWrite<u8, PAD_PULL::Register>),
        (0x1a15 => @END),
    }
}

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

    pub(super) fn core_registers(&self) -> &Cv181xCoreRegisters {
        // SAFETY: `Cv181xSdhci::new` requires this mapping to cover the
        // controller register file for the wrapper's whole lifetime.
        unsafe { &*self.core.as_ptr().cast() }
    }

    pub(super) fn syscon_registers(&self) -> &Cv181xSysconRegisters {
        // SAFETY: `Cv181xSdhci::new` requires this mapping to cover TOP and
        // the pinmux/IO window for the wrapper's whole lifetime.
        unsafe { &*self.syscon.as_ptr().cast() }
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

#[derive(Clone, Copy)]
pub(super) enum PullMode {
    Up,
    Down,
}

pub(super) fn set_pull(register: &ReadWrite<u8, PAD_PULL::Register>, mode: PullMode) {
    use tock_registers::interfaces::ReadWriteable;

    match mode {
        PullMode::Up => register.modify(PAD_PULL::UP::SET + PAD_PULL::DOWN::CLEAR),
        PullMode::Down => register.modify(PAD_PULL::UP::CLEAR + PAD_PULL::DOWN::SET),
    }
}
