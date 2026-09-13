//! CV181x SDIO1 SoC integration.
//!
//! This module owns only silicon-level pinmux, pull, clock, reset, and
//! card-detect policy. SDIO card enumeration remains in `sdmmc-protocol`.

use core::{ptr::NonNull, time::Duration};

use tock_registers::{
    interfaces::{ReadWriteable, Writeable},
    register_bitfields, register_structs,
    registers::ReadWrite,
};

use crate::{
    Cv181xMmio,
    platform::{MSHC_CTRL, PINMUX32, SD_CTRL_OPT},
};

pub const CV181X_SDIO1_RESET_SETTLE: Duration = Duration::from_millis(1);

const RTC_PULL_REGISTERS: usize = 20;

register_bitfields! [
    u32,

    CLK_EN_0 [
        SD1_ALL OFFSET(21) NUMBITS(3) []
    ],

    CLK_BYP_0 [
        SD1 OFFSET(7) NUMBITS(1) []
    ],

    DIV_CLK [
        RESET_DEASSERT OFFSET(0) NUMBITS(1) []
    ],

    RTCSYS_RST_CTRL [
        SDIO OFFSET(2) NUMBITS(1) []
    ],

    RTCSYS_CLKMUX [
        SDIO OFFSET(0) NUMBITS(4) []
    ],

    RTCSYS_CLKBYP [
        SDIO OFFSET(1) NUMBITS(1) []
    ],

    RTCSYS_CLK_EN [
        SD1_ALL OFFSET(1) NUMBITS(2) []
    ]
];

register_structs! {
    CrgRegisters {
        (0x000 => clk_en_0: ReadWrite<u32, CLK_EN_0::Register>),
        (0x004 => _reserved0),
        (0x030 => clk_byp_0: ReadWrite<u32, CLK_BYP_0::Register>),
        (0x034 => _reserved1),
        (0x07c => div_clk_sd1: ReadWrite<u32, DIV_CLK::Register>),
        (0x080 => _reserved2),
        (0x084 => div_clk_100k_sd1: ReadWrite<u32, DIV_CLK::Register>),
        (0x088 => @END),
    }
}

register_structs! {
    RtcSysCtrlRegisters {
        (0x000 => _reserved0),
        (0x018 => reset_ctrl: ReadWrite<u32, RTCSYS_RST_CTRL::Register>),
        (0x01c => clock_mux: ReadWrite<u32, RTCSYS_CLKMUX::Register>),
        (0x020 => _reserved1),
        (0x030 => clock_bypass: ReadWrite<u32, RTCSYS_CLKBYP::Register>),
        (0x034 => clock_enable: ReadWrite<u32, RTCSYS_CLK_EN::Register>),
        (0x038 => @END),
    }
}

register_structs! {
    RtcSysIoRegisters {
        (0x000 => _reserved0),
        (0x088 => pulls: [ReadWrite<u32>; RTC_PULL_REGISTERS]),
        (0x0d8 => _reserved1),
        (0x0e4 => sdio1_mux: ReadWrite<u32>),
        (0x0e8 => @END),
    }
}

/// Already-mapped resources required by the CV181x SDIO1 instance.
#[derive(Clone, Copy)]
pub struct Cv181xSdio1Mmio {
    host: Cv181xMmio,
    crg: NonNull<u8>,
    rtcsys_ctrl: NonNull<u8>,
    rtcsys_io: NonNull<u8>,
}

impl Cv181xSdio1Mmio {
    pub const fn new(
        host: Cv181xMmio,
        crg: NonNull<u8>,
        rtcsys_ctrl: NonNull<u8>,
        rtcsys_io: NonNull<u8>,
    ) -> Self {
        Self {
            host,
            crg,
            rtcsys_ctrl,
            rtcsys_io,
        }
    }

    pub const fn host(self) -> Cv181xMmio {
        self.host
    }

    /// Apply the SDIO1 silicon setup without sleeping or issuing card
    /// commands. The caller schedules the documented reset-settle deadline.
    pub fn initialize(self) {
        self.host
            .core_registers()
            .mshc_ctrl
            .modify(MSHC_CTRL::SD1_SEL::SET);
        let syscon = self.host.syscon_registers();
        let crg = self.crg_registers();
        let rtcsys_ctrl = self.rtcsys_ctrl_registers();
        let rtcsys_io = self.rtcsys_io_registers();

        for mux in &syscon.sdio1_mux {
            mux.modify(PINMUX32::FUNCTION.val(0));
        }
        for pull in &rtcsys_io.pulls {
            pull.set(0x1111_1111);
        }
        rtcsys_io.sdio1_mux.set(0);

        crg.clk_en_0.modify(CLK_EN_0::SD1_ALL.val(0b111));
        crg.clk_byp_0.modify(CLK_BYP_0::SD1::CLEAR);
        crg.div_clk_sd1.modify(DIV_CLK::RESET_DEASSERT::SET);
        crg.div_clk_100k_sd1.modify(DIV_CLK::RESET_DEASSERT::SET);

        rtcsys_ctrl.clock_mux.modify(RTCSYS_CLKMUX::SDIO.val(0));
        rtcsys_ctrl
            .clock_enable
            .modify(RTCSYS_CLK_EN::SD1_ALL.val(0b11));
        rtcsys_ctrl.clock_bypass.modify(RTCSYS_CLKBYP::SDIO::CLEAR);
        rtcsys_ctrl.reset_ctrl.modify(RTCSYS_RST_CTRL::SDIO::SET);
        syscon
            .sd_ctrl_opt
            .modify(SD_CTRL_OPT::SD1_CARDDET_OVERRIDE.val(0b11));
    }

    fn crg_registers(&self) -> &CrgRegisters {
        // SAFETY: the constructor contract requires the CRG mapping to cover
        // every field in `CrgRegisters` for the controller lifetime.
        unsafe { &*self.crg.as_ptr().cast() }
    }

    fn rtcsys_ctrl_registers(&self) -> &RtcSysCtrlRegisters {
        // SAFETY: the constructor contract requires this mapping to cover the
        // typed RTC control register block for the controller lifetime.
        unsafe { &*self.rtcsys_ctrl.as_ptr().cast() }
    }

    fn rtcsys_io_registers(&self) -> &RtcSysIoRegisters {
        // SAFETY: the constructor contract requires this mapping to cover the
        // typed RTC IO register block for the controller lifetime.
        unsafe { &*self.rtcsys_io.as_ptr().cast() }
    }
}
