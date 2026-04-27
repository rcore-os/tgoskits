use tock_registers::{register_bitfields, registers::ReadWrite};

// Start Address: 0x0A00
#[repr(C)]
pub struct SoftRstRegisters {
    _reserved0: u32,
    pub cru_softrst_con01: ReadWrite<u32, CRU_SOFTRST_CON01::Register>,
    pub cru_softrst_con02: ReadWrite<u32, CRU_SOFTRST_CON02::Register>,
    pub cru_softrst_con03: ReadWrite<u32, CRU_SOFTRST_CON03::Register>,
    pub cru_softrst_con04: ReadWrite<u32, CRU_SOFTRST_CON04::Register>,
    pub cru_softrst_con05: ReadWrite<u32, CRU_SOFTRST_CON05::Register>,
    pub cru_softrst_con06: ReadWrite<u32, CRU_SOFTRST_CON06::Register>,
    pub cru_softrst_con07: ReadWrite<u32, CRU_SOFTRST_CON07::Register>,
    pub cru_softrst_con08: ReadWrite<u32, CRU_SOFTRST_CON08::Register>,
    pub cru_softrst_con09: ReadWrite<u32, CRU_SOFTRST_CON09::Register>,
    pub cru_softrst_con10: ReadWrite<u32, CRU_SOFTRST_CON10::Register>,
    pub cru_softrst_con11: ReadWrite<u32, CRU_SOFTRST_CON11::Register>,
    pub cru_softrst_con12: ReadWrite<u32, CRU_SOFTRST_CON12::Register>,
    pub cru_softrst_con13: ReadWrite<u32, CRU_SOFTRST_CON13::Register>,
    pub cru_softrst_con14: ReadWrite<u32, CRU_SOFTRST_CON14::Register>,
    pub cru_softrst_con15: ReadWrite<u32, CRU_SOFTRST_CON15::Register>,
    pub cru_softrst_con16: ReadWrite<u32, CRU_SOFTRST_CON16::Register>,
    pub cru_softrst_con17: ReadWrite<u32, CRU_SOFTRST_CON17::Register>,
    pub cru_softrst_con18: ReadWrite<u32, CRU_SOFTRST_CON18::Register>,
    pub cru_softrst_con19: ReadWrite<u32, CRU_SOFTRST_CON19::Register>,
    pub cru_softrst_con20: ReadWrite<u32, CRU_SOFTRST_CON20::Register>,
    pub cru_softrst_con21: ReadWrite<u32, CRU_SOFTRST_CON21::Register>,
    pub cru_softrst_con22: ReadWrite<u32, CRU_SOFTRST_CON22::Register>,
    pub cru_softrst_con23: ReadWrite<u32, CRU_SOFTRST_CON23::Register>,
    pub cru_softrst_con24: ReadWrite<u32, CRU_SOFTRST_CON24::Register>,
    pub cru_softrst_con25: ReadWrite<u32, CRU_SOFTRST_CON25::Register>,
    pub cru_softrst_con26: ReadWrite<u32, CRU_SOFTRST_CON26::Register>,
    pub cru_softrst_con27: ReadWrite<u32, CRU_SOFTRST_CON27::Register>,
    pub cru_softrst_con28: ReadWrite<u32, CRU_SOFTRST_CON28::Register>,
    pub cru_softrst_con29: ReadWrite<u32, CRU_SOFTRST_CON29::Register>,
    pub cru_softrst_con30: ReadWrite<u32, CRU_SOFTRST_CON30::Register>,
    pub cru_softrst_con31: ReadWrite<u32, CRU_SOFTRST_CON31::Register>,
    pub cru_softrst_con32: ReadWrite<u32, CRU_SOFTRST_CON32::Register>,
    pub cru_softrst_con33: ReadWrite<u32, CRU_SOFTRST_CON33::Register>,
    pub cru_softrst_con34: ReadWrite<u32, CRU_SOFTRST_CON34::Register>,
    pub cru_softrst_con35: ReadWrite<u32, CRU_SOFTRST_CON35::Register>,
    _reserved1: u32,
    // pub cru_softrst_con36: ReadWrite<u32, CRU_SOFTRST_CON36::Register>,
    pub cru_softrst_con37: ReadWrite<u32, CRU_SOFTRST_CON37::Register>,
    _reserved2: [u32; 2],
    // pub cru_softrst_con38: ReadWrite<u32, CRU_SOFTRST_CON38::Register>,
    // pub cru_softrst_con39: ReadWrite<u32, CRU_SOFTRST_CON39::Register>,
    pub cru_softrst_con40: ReadWrite<u32, CRU_SOFTRST_CON40::Register>,
    pub cru_softrst_con41: ReadWrite<u32, CRU_SOFTRST_CON41::Register>,
    pub cru_softrst_con42: ReadWrite<u32, CRU_SOFTRST_CON42::Register>,
    pub cru_softrst_con43: ReadWrite<u32, CRU_SOFTRST_CON43::Register>,
    pub cru_softrst_con44: ReadWrite<u32, CRU_SOFTRST_CON44::Register>,
    pub cru_softrst_con45: ReadWrite<u32, CRU_SOFTRST_CON45::Register>,
    _reserved3: u32,
    // pub cru_softrst_con46: ReadWrite<u32, CRU_SOFTRST_CON46::Register>,
    pub cru_softrst_con47: ReadWrite<u32, CRU_SOFTRST_CON47::Register>,
    pub cru_softrst_con48: ReadWrite<u32, CRU_SOFTRST_CON48::Register>,
    pub cru_softrst_con49: ReadWrite<u32, CRU_SOFTRST_CON49::Register>,
    pub cru_softrst_con50: ReadWrite<u32, CRU_SOFTRST_CON50::Register>,
    pub cru_softrst_con51: ReadWrite<u32, CRU_SOFTRST_CON51::Register>,
    pub cru_softrst_con52: ReadWrite<u32, CRU_SOFTRST_CON52::Register>,
    pub cru_softrst_con53: ReadWrite<u32, CRU_SOFTRST_CON53::Register>,
    _reserved4: u32,
    // pub cru_softrst_con54: ReadWrite<u32, CRU_SOFTRST_CON54::Register>,
    pub cru_softrst_con55: ReadWrite<u32, CRU_SOFTRST_CON55::Register>,
    pub cru_softrst_con56: ReadWrite<u32, CRU_SOFTRST_CON56::Register>,
    pub cru_softrst_con57: ReadWrite<u32, CRU_SOFTRST_CON57::Register>,
    _reserved5: u32,
    // pub cru_softrst_con58: ReadWrite<u32, CRU_SOFTRST_CON58::Register>,
    pub cru_softrst_con59: ReadWrite<u32, CRU_SOFTRST_CON59::Register>,
    pub cru_softrst_con60: ReadWrite<u32, CRU_SOFTRST_CON60::Register>,
    pub cru_softrst_con61: ReadWrite<u32, CRU_SOFTRST_CON61::Register>,
    pub cru_softrst_con62: ReadWrite<u32, CRU_SOFTRST_CON62::Register>,
    pub cru_softrst_con63: ReadWrite<u32, CRU_SOFTRST_CON63::Register>,
    pub cru_softrst_con64: ReadWrite<u32, CRU_SOFTRST_CON64::Register>,
    pub cru_softrst_con65: ReadWrite<u32, CRU_SOFTRST_CON65::Register>,
    pub cru_softrst_con66: ReadWrite<u32, CRU_SOFTRST_CON66::Register>,
    pub cru_softrst_con67: ReadWrite<u32, CRU_SOFTRST_CON67::Register>,
    pub cru_softrst_con68: ReadWrite<u32, CRU_SOFTRST_CON68::Register>,
    pub cru_softrst_con69: ReadWrite<u32, CRU_SOFTRST_CON69::Register>,
    pub cru_softrst_con70: ReadWrite<u32, CRU_SOFTRST_CON70::Register>,
    _reserved6: u32,
    // pub cru_softrst_con71: ReadWrite<u32, CRU_SOFTRST_CON71::Register>,
    pub cru_softrst_con72: ReadWrite<u32, CRU_SOFTRST_CON72::Register>,
    pub cru_softrst_con73: ReadWrite<u32, CRU_SOFTRST_CON73::Register>,
    pub cru_softrst_con74: ReadWrite<u32, CRU_SOFTRST_CON74::Register>,
    pub cru_softrst_con75: ReadWrite<u32, CRU_SOFTRST_CON75::Register>,
    pub cru_softrst_con76: ReadWrite<u32, CRU_SOFTRST_CON76::Register>,
    pub cru_softrst_con77: ReadWrite<u32, CRU_SOFTRST_CON77::Register>,
}

// CRU_SOFTRST_CON01  0x0A04;
register_bitfields![u32,
    CRU_SOFTRST_CON01 [
        _reserved0 OFFSET(0) NUMBITS(3) [],
        aresetn_top_biu OFFSET(3) NUMBITS(1) [],
        presetn_top_biu OFFSET(4) NUMBITS(1) [],
        _reserved1 OFFSET(5) NUMBITS(1) [],
        presetn_csiphy0 OFFSET(6) NUMBITS(1) [],
        _reserved2 OFFSET(7) NUMBITS(1) [],
        presetn_csiphy1 OFFSET(8) NUMBITS(1) [],
        _reserved3 OFFSET(9) NUMBITS(6) [],
        aresetn_top_m500_biu OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON02  0x0A08;
register_bitfields![u32,
    CRU_SOFTRST_CON02 [
        reseraresetn_top_m400_biuved OFFSET(0) NUMBITS(1) [],
        aresetn_top_s200_biu OFFSET(1) NUMBITS(1) [],
        aresetn_top_s400_biu OFFSET(2) NUMBITS(1) [],
        aresetn_top_m300_biu OFFSET(3) NUMBITS(1) [],
        _reserved0 OFFSET(4) NUMBITS(4) [],
        resetn_usbdp_combo_phy0_init OFFSET(8) NUMBITS(1) [],
        resetn_usbdp_combo_phy0_cmn OFFSET(9) NUMBITS(1) [],
        resetn_usbdp_combo_phy0_lane OFFSET(10) NUMBITS(1) [],
        resetn_usbdp_combo_phy0_pcs OFFSET(11) NUMBITS(1) [],
        _reserved1 OFFSET(12) NUMBITS(3) [],
        resetn_usbdp_combo_phy1_init OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON03  0x0A0C;
register_bitfields![u32,
    CRU_SOFTRST_CON03 [
        resetn_usbdp_combo_phy1_cmn OFFSET(0) NUMBITS(1) [],
        resetn_usbdp_combo_phy1_lane OFFSET(1) NUMBITS(1) [],
        resetn_usbdp_combo_phy1_pcs OFFSET(2) NUMBITS(1) [],
        _reserved OFFSET(3) NUMBITS(11) [],
        presetn_mipi_dcphy0 OFFSET(14) NUMBITS(1) [],
        presetn_mipi_dcphy0_grf OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON04  0x0A10;
register_bitfields![u32,
    CRU_SOFTRST_CON04 [
        _reserved0 OFFSET(0) NUMBITS(3) [],
        presetn_mipi_dcphy1 OFFSET(3) NUMBITS(1) [],
        presetn_mipi_dcphy1_grf OFFSET(4) NUMBITS(1) [],
        presetn_apb2asb_slv_cdphy OFFSET(5) NUMBITS(1) [],
        presetn_apb2asb_slv_csiphy OFFSET(6) NUMBITS(1) [],
        presetn_apb2asb_slv_vccio3_5 OFFSET(7) NUMBITS(1) [],
        presetn_apb2asb_slv_vccio6 OFFSET(8) NUMBITS(1) [],
        presetn_apb2asb_slv_emmcio OFFSET(9) NUMBITS(1) [],
        presetn_apb2asb_slv_ioc_top OFFSET(10) NUMBITS(1) [],
        presetn_apb2asb_slv_ioc_right OFFSET(11) NUMBITS(1) [],
        _reserved1 OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON05  0x0A14;
register_bitfields![u32,
    CRU_SOFTRST_CON05 [
        presetn_cru OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(6) [],
        aresetn_channel_secure2vo1usb OFFSET(7) NUMBITS(1) [],
        aresetn_channel_secure2center OFFSET(8) NUMBITS(1) [],
        _reserved1 OFFSET(9) NUMBITS(5) [],
        hresetn_channel_secure2vo1usb OFFSET(14) NUMBITS(1) [],
        hresetn_channel_secure2center OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON06  0x0A18;
register_bitfields![u32,
    CRU_SOFTRST_CON06 [
        presetn_channel_secure2vo1usb OFFSET(0) NUMBITS(1) [],
        presetn_channel_secure2center OFFSET(1) NUMBITS(1) [],
        _reserved OFFSET(2) NUMBITS(14) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON07  0x0A1C;
register_bitfields![u32,
    CRU_SOFTRST_CON07 [
        _reserved0 OFFSET(0) NUMBITS(2) [],
        hresetn_audio_biu OFFSET(2) NUMBITS(1) [],
        presetn_audio_biu OFFSET(3) NUMBITS(1) [],
        hresetn_i2s0_8ch OFFSET(4) NUMBITS(1) [],
        _reserved1 OFFSET(5) NUMBITS(2) [],
        mresetn_i2s0_8ch_tx OFFSET(7) NUMBITS(1) [],
        _reserved2 OFFSET(8) NUMBITS(2) [],
        mresetn_i2s0_8ch_rx OFFSET(10) NUMBITS(1) [],
        presetn_acdcdig OFFSET(11) NUMBITS(1) [],
        hresetn_i2s2_2ch OFFSET(12) NUMBITS(1) [],
        hresetn_i2s3_2ch OFFSET(13) NUMBITS(1) [],
        _reserved3 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON08  0x0A20;
register_bitfields![u32,
    CRU_SOFTRST_CON08 [
        mresetn_i2s2_2ch OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(2) [],
        mresetn_i2s3_2ch OFFSET(3) NUMBITS(1) [],
        resetn_dac_acdcdig OFFSET(4) NUMBITS(1) [],
        _reserved1 OFFSET(5) NUMBITS(9) [],
        hresetn_spdif0 OFFSET(14) NUMBITS(1) [],
        _reserved2 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON09  0x0A24;
register_bitfields![u32,
    CRU_SOFTRST_CON09 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        mresetn_spdif0 OFFSET(1) NUMBITS(1) [],
        hresetn_spdif1 OFFSET(2) NUMBITS(1) [],
        _reserved1 OFFSET(3) NUMBITS(2) [],
        mresetn_spdif1 OFFSET(5) NUMBITS(1) [],
        hresetn_pdm1 OFFSET(6) NUMBITS(1) [],
        resetn_pdm1 OFFSET(7) NUMBITS(1) [],
        _reserved2 OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON10  0x0A28;
register_bitfields![u32,
    CRU_SOFTRST_CON10 [
        _reserved OFFSET(0) NUMBITS(1) [],
        aresetn_bus_biu OFFSET(1) NUMBITS(1) [],
        presetn_bus_biu OFFSET(2) NUMBITS(1) [],
        aresetn_gic OFFSET(3) NUMBITS(1) [],
        aresetn_gic_dbg OFFSET(4) NUMBITS(1) [],
        aresetn_dmac0 OFFSET(5) NUMBITS(1) [],
        aresetn_dmac1 OFFSET(6) NUMBITS(1) [],
        aresetn_dmac2 OFFSET(7) NUMBITS(1) [],
        presetn_i2c1 OFFSET(8) NUMBITS(1) [],
        presetn_i2c2 OFFSET(9) NUMBITS(1) [],
        presetn_i2c3 OFFSET(10) NUMBITS(1) [],
        presetn_i2c4 OFFSET(11) NUMBITS(1) [],
        presetn_i2c5 OFFSET(12) NUMBITS(1) [],
        presetn_i2c6 OFFSET(13) NUMBITS(1) [],
        presetn_i2c7 OFFSET(14) NUMBITS(1) [],
        presetn_i2c8 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON11  0x0A2C;
register_bitfields![u32,
    CRU_SOFTRST_CON11 [
        resetn_i2c1 OFFSET(0) NUMBITS(1) [],
        resetn_i2c2 OFFSET(1) NUMBITS(1) [],
        resetn_i2c3 OFFSET(2) NUMBITS(1) [],
        resetn_i2c4 OFFSET(3) NUMBITS(1) [],
        resetn_i2c5 OFFSET(4) NUMBITS(1) [],
        resetn_i2c6 OFFSET(5) NUMBITS(1) [],
        resetn_i2c7 OFFSET(6) NUMBITS(1) [],
        resetn_i2c8 OFFSET(7) NUMBITS(1) [],
        presetn_can0 OFFSET(8) NUMBITS(1) [],
        resetn_can0 OFFSET(9) NUMBITS(1) [],
        presetn_can1 OFFSET(10) NUMBITS(1) [],
        resetn_can1 OFFSET(11) NUMBITS(1) [],
        presetn_can2 OFFSET(12) NUMBITS(1) [],
        resetn_can2 OFFSET(13) NUMBITS(1) [],
        presetn_saradc OFFSET(14) NUMBITS(1) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON12  0x0A30;
register_bitfields![u32,
    CRU_SOFTRST_CON12 [
        presetn_tsadc OFFSET(0) NUMBITS(1) [],
        resetn_tsadc OFFSET(1) NUMBITS(1) [],
        presetn_uart1 OFFSET(2) NUMBITS(1) [],
        presetn_uart2 OFFSET(3) NUMBITS(1) [],
        presetn_uart3 OFFSET(4) NUMBITS(1) [],
        presetn_uart4 OFFSET(5) NUMBITS(1) [],
        presetn_uart5 OFFSET(6) NUMBITS(1) [],
        presetn_uart6 OFFSET(7) NUMBITS(1) [],
        presetn_uart7 OFFSET(8) NUMBITS(1) [],
        presetn_uart8 OFFSET(9) NUMBITS(1) [],
        presetn_uart9 OFFSET(10) NUMBITS(1) [],
        _reserved0 OFFSET(11) NUMBITS(2) [],
        sresetn_uart1 OFFSET(13) NUMBITS(1) [],
        _reserved1 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON13  0x0A34;
register_bitfields![u32,
    CRU_SOFTRST_CON13 [
        sresetn_uart2 OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(2) [],
        sresetn_uart3 OFFSET(3) NUMBITS(1) [],
        _reserved1 OFFSET(4) NUMBITS(2) [],
        sresetn_uart4 OFFSET(6) NUMBITS(1) [],
        _reserved2 OFFSET(7) NUMBITS(2) [],
        sresetn_uart5 OFFSET(9) NUMBITS(1) [],
        _reserved3 OFFSET(10) NUMBITS(2) [],
        sresetn_uart6 OFFSET(12) NUMBITS(1) [],
        _reserved4 OFFSET(13) NUMBITS(2) [],
        sresetn_uart7 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON14  0x0A38;
register_bitfields![u32,
    CRU_SOFTRST_CON14 [
        _reserved0 OFFSET(0) NUMBITS(2) [],
        sresetn_uart8 OFFSET(2) NUMBITS(1) [],
        _reserved1 OFFSET(3) NUMBITS(2) [],
        sresetn_uart9 OFFSET(5) NUMBITS(1) [],
        presetn_spi0 OFFSET(6) NUMBITS(1) [],
        presetn_spi1 OFFSET(7) NUMBITS(1) [],
        presetn_spi2 OFFSET(8) NUMBITS(1) [],
        presetn_spi3 OFFSET(9) NUMBITS(1) [],
        presetn_spi4 OFFSET(10) NUMBITS(1) [],
        resetn_spi0 OFFSET(11) NUMBITS(1) [],
        resetn_spi1 OFFSET(12) NUMBITS(1) [],
        resetn_spi2 OFFSET(13) NUMBITS(1) [],
        resetn_spi3 OFFSET(14) NUMBITS(1) [],
        resetn_spi4 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON15  0x0A3C;
register_bitfields![u32,
    CRU_SOFTRST_CON15 [
        presetn_wdt0 OFFSET(0) NUMBITS(1) [],
        tresetn_wdt0 OFFSET(1) NUMBITS(1) [],
        presetn_sys_grf OFFSET(2) NUMBITS(1) [],
        presetn_pwm1 OFFSET(3) NUMBITS(1) [],
        resetn_pwm1 OFFSET(4) NUMBITS(1) [],
        _reserved0 OFFSET(5) NUMBITS(1) [],
        presetn_pwm2 OFFSET(6) NUMBITS(1) [],
        resetn_pwm2 OFFSET(7) NUMBITS(1) [],
        _reserved1 OFFSET(8) NUMBITS(1) [],
        presetn_pwm3 OFFSET(9) NUMBITS(1) [],
        resetn_pwm3 OFFSET(10) NUMBITS(1) [],
        _reserved2 OFFSET(11) NUMBITS(1) [],
        presetn_bustimer0 OFFSET(12) NUMBITS(1) [],
        presetn_bustimer1 OFFSET(13) NUMBITS(1) [],
        _reserved3 OFFSET(14) NUMBITS(1) [],
        resetn_bustimer0 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON16  0x0A40;
register_bitfields![u32,
    CRU_SOFTRST_CON16 [
        resetn_bustimer1 OFFSET(0) NUMBITS(1) [],
        resetn_bustimer2 OFFSET(1) NUMBITS(1) [],
        resetn_bustimer3 OFFSET(2) NUMBITS(1) [],
        resetn_bustimer4 OFFSET(3) NUMBITS(1) [],
        resetn_bustimer5 OFFSET(4) NUMBITS(1) [],
        resetn_bustimer6 OFFSET(5) NUMBITS(1) [],
        resetn_bustimer7 OFFSET(6) NUMBITS(1) [],
        resetn_bustimer8 OFFSET(7) NUMBITS(1) [],
        resetn_bustimer9 OFFSET(8) NUMBITS(1) [],
        resetn_bustimer10 OFFSET(9) NUMBITS(1) [],
        resetn_bustimer11 OFFSET(10) NUMBITS(1) [],
        presetn_mailbox0 OFFSET(11) NUMBITS(1) [],
        presetn_mailbox1 OFFSET(12) NUMBITS(1) [],
        presetn_mailbox2 OFFSET(13) NUMBITS(1) [],
        presetn_gpio1 OFFSET(14) NUMBITS(1) [],
        dbresetn_gpio1 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON17  0x0A44;
register_bitfields![u32,
    CRU_SOFTRST_CON17 [
        presetn_gpio2 OFFSET(0) NUMBITS(1) [],
        dbresetn_gpio2 OFFSET(1) NUMBITS(1) [],
        presetn_gpio3 OFFSET(2) NUMBITS(1) [],
        dbresetn_gpio3 OFFSET(3) NUMBITS(1) [],
        presetn_gpio4 OFFSET(4) NUMBITS(1) [],
        dbresetn_gpio4 OFFSET(5) NUMBITS(1) [],
        aresetn_decom OFFSET(6) NUMBITS(1) [],
        presetn_decom OFFSET(7) NUMBITS(1) [],
        dresetn_decom OFFSET(8) NUMBITS(1) [],
        presetn_top OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(1) [],
        aresetn_gicadb_gic2core_bus OFFSET(11) NUMBITS(1) [],
        presetn_dft2apb OFFSET(12) NUMBITS(1) [],
        presetn_apb2asb_mst_top OFFSET(13) NUMBITS(1) [],
        presetn_apb2asb_mst_cdphy OFFSET(14) NUMBITS(1) [],
        presetn_apb2asb_mst_bot_right OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON18  0x0A48;
register_bitfields![u32,
    CRU_SOFTRST_CON18 [
        presetn_apb2asb_mst_ioc_top OFFSET(0) NUMBITS(1) [],
        presetn_apb2asb_mst_ioc_right OFFSET(1) NUMBITS(1) [],
        presetn_apb2asb_mst_csiphy OFFSET(2) NUMBITS(1) [],
        presetn_apb2asb_mst_vccio3_5 OFFSET(3) NUMBITS(1) [],
        presetn_apb2asb_mst_vccio6 OFFSET(4) NUMBITS(1) [],
        presetn_apb2asb_mst_emmcio OFFSET(5) NUMBITS(1) [],
        aresetn_spinlock OFFSET(6) NUMBITS(1) [],
        _reserved0 OFFSET(7) NUMBITS(2) [],
        presetn_otpc_ns OFFSET(9) NUMBITS(1) [],
        resetn_otpc_ns OFFSET(10) NUMBITS(1) [],
        resetn_otpc_arb OFFSET(11) NUMBITS(1) [],
        _reserved1 OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON19  0x0A4C;
register_bitfields![u32,
    CRU_SOFTRST_CON19 [
        presetn_busioc OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(3) [],
        presetn_pmucm0_intmux OFFSET(4) NUMBITS(1) [],
        presetn_ddrcm0_intmux OFFSET(5) NUMBITS(1) [],
        _reserved1 OFFSET(6) NUMBITS(10) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON20  0x0A50;
register_bitfields![u32,
    CRU_SOFTRST_CON20 [
        presetn_ddr_dfictl_ch0 OFFSET(0) NUMBITS(1) [],
        presetn_ddr_mon_ch0 OFFSET(1) NUMBITS(1) [],
        presetn_ddr_standby_ch0 OFFSET(2) NUMBITS(1) [],
        presetn_ddr_upctl_ch0 OFFSET(3) NUMBITS(1) [],
        tmresetn_ddr_mon_ch0 OFFSET(4) NUMBITS(1) [],
        presetn_ddr_grf_ch01 OFFSET(5) NUMBITS(1) [],
        resetn_dfi_ch0 OFFSET(6) NUMBITS(1) [],
        resetn_sbr_ch0 OFFSET(7) NUMBITS(1) [],
        resetn_ddr_upctl_ch0 OFFSET(8) NUMBITS(1) [],
        resetn_ddr_dfictl_ch0 OFFSET(9) NUMBITS(1) [],
        resetn_ddr_mon_ch0 OFFSET(10) NUMBITS(1) [],
        resetn_ddr_standby_ch0 OFFSET(11) NUMBITS(1) [],
        aresetn_ddr_upctl_ch0 OFFSET(12) NUMBITS(1) [],
        presetn_ddr_dfictl_ch1 OFFSET(13) NUMBITS(1) [],
        presetn_ddr_mon_ch1 OFFSET(14) NUMBITS(1) [],
        presetn_ddr_standby_ch1 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON21  0x0A54;
register_bitfields![u32,
    CRU_SOFTRST_CON21 [
        presetn_ddr_upctl_ch1 OFFSET(0) NUMBITS(1) [],
        tmresetn_ddr_mon_ch1 OFFSET(1) NUMBITS(1) [],
        resetn_dfi_ch1 OFFSET(2) NUMBITS(1) [],
        resetn_sbr_ch1 OFFSET(3) NUMBITS(1) [],
        resetn_ddr_upctl_ch1 OFFSET(4) NUMBITS(1) [],
        resetn_ddr_dfictl_ch1 OFFSET(5) NUMBITS(1) [],
        resetn_ddr_mon_ch1 OFFSET(6) NUMBITS(1) [],
        resetn_ddr_standby_ch1 OFFSET(7) NUMBITS(1) [],
        aresetn_ddr_upctl_ch1 OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(4) [],
        aresetn_ddr_ddrsch0 OFFSET(13) NUMBITS(1) [],
        aresetn_ddr_rs_ddrsch0 OFFSET(14) NUMBITS(1) [],
        aresetn_ddr_frs_ddrsch0 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON22  0x0A58;
register_bitfields![u32,
    CRU_SOFTRST_CON22 [
        aresetn_ddr_scramble0 OFFSET(0) NUMBITS(1) [],
        aresetn_ddr_frs_scramble0 OFFSET(1) NUMBITS(1) [],
        aresetn_ddr_ddrsch1 OFFSET(2) NUMBITS(1) [],
        aresetn_ddr_rs_ddrsch1 OFFSET(3) NUMBITS(1) [],
        aresetn_ddr_frs_ddrsch1 OFFSET(4) NUMBITS(1) [],
        aresetn_ddr_scramble1 OFFSET(5) NUMBITS(1) [],
        aresetn_ddr_frs_scramble1 OFFSET(6) NUMBITS(1) [],
        presetn_ddr_ddrsch0 OFFSET(7) NUMBITS(1) [],
        presetn_ddr_ddrsch1 OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON23  0x0A5C;
register_bitfields![u32,
    CRU_SOFTRST_CON23 [
        presetn_ddr_dfictl_ch2 OFFSET(0) NUMBITS(1) [],
        presetn_ddr_mon_ch2 OFFSET(1) NUMBITS(1) [],
        presetn_ddr_standby_ch2 OFFSET(2) NUMBITS(1) [],
        presetn_ddr_upctl_ch2 OFFSET(3) NUMBITS(1) [],
        tmresetn_ddr_mon_ch2 OFFSET(4) NUMBITS(1) [],
        presetn_ddr_grf_ch23 OFFSET(5) NUMBITS(1) [],
        resetn_dfi_ch2 OFFSET(6) NUMBITS(1) [],
        resetn_sbr_ch2 OFFSET(7) NUMBITS(1) [],
        resetn_ddr_upctl_ch2 OFFSET(8) NUMBITS(1) [],
        resetn_ddr_dfictl_ch2 OFFSET(9) NUMBITS(1) [],
        resetn_ddr_mon_ch2 OFFSET(10) NUMBITS(1) [],
        resetn_ddr_standby_ch2 OFFSET(11) NUMBITS(1) [],
        aresetn_ddr_upctl_ch2 OFFSET(12) NUMBITS(1) [],
        presetn_ddr_dfictl_ch3 OFFSET(13) NUMBITS(1) [],
        presetn_ddr_mon_ch3 OFFSET(14) NUMBITS(1) [],
        presetn_ddr_standby_ch3 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON24  0x0A60;
register_bitfields![u32,
    CRU_SOFTRST_CON24 [
        presetn_ddr_upctl_ch3 OFFSET(0) NUMBITS(1) [],
        tmresetn_ddr_mon_ch3 OFFSET(1) NUMBITS(1) [],
        resetn_dfi_ch3 OFFSET(2) NUMBITS(1) [],
        resetn_sbr_ch3 OFFSET(3) NUMBITS(1) [],
        resetn_ddr_upctl_ch3 OFFSET(4) NUMBITS(1) [],
        resetn_ddr_dfictl_ch3 OFFSET(5) NUMBITS(1) [],
        resetn_ddr_mon_ch3 OFFSET(6) NUMBITS(1) [],
        resetn_ddr_standby_ch3 OFFSET(7) NUMBITS(1) [],
        aresetn_ddr_upctl_ch3 OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(4) [],
        aresetn_ddr_ddrsch2 OFFSET(13) NUMBITS(1) [],
        aresetn_ddr_rs_ddrsch2 OFFSET(14) NUMBITS(1) [],
        aresetn_ddr_frs_ddrsch2 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON25  0x0A64;
register_bitfields![u32,
    CRU_SOFTRST_CON25 [
        aresetn_ddr_scramble2 OFFSET(0) NUMBITS(1) [],
        aresetn_ddr_frs_scramble2 OFFSET(1) NUMBITS(1) [],
        aresetn_ddr_ddrsch3 OFFSET(2) NUMBITS(1) [],
        reseraresetn_ddr_rs_ddrsch3ved OFFSET(3) NUMBITS(1) [],
        aresetn_ddr_frs_ddrsch3 OFFSET(4) NUMBITS(1) [],
        aresetn_ddr_scramble3 OFFSET(5) NUMBITS(1) [],
        aresetn_ddr_frs_scramble3 OFFSET(6) NUMBITS(1) [],
        presetn_ddr_ddrsch2 OFFSET(7) NUMBITS(1) [],
        presetn_ddr_ddrsch3 OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON26  0x0A68;
register_bitfields![u32,
    CRU_SOFTRST_CON26 [
        _reserved0 OFFSET(0) NUMBITS(3) [],
        resetn_isp1 OFFSET(3) NUMBITS(1) [],
        resetn_isp1_vicap OFFSET(4) NUMBITS(1) [],
        _reserved1 OFFSET(5) NUMBITS(1) [],
        aresetn_isp1_biu OFFSET(6) NUMBITS(1) [],
        _reserved2 OFFSET(7) NUMBITS(1) [],
        hresetn_isp1_biu OFFSET(8) NUMBITS(1) [],
        _reserved3 OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON27  0x0A6C;
register_bitfields![u32,
    CRU_SOFTRST_CON27 [
        aresetn_rknn1 OFFSET(0) NUMBITS(1) [],
        aresetn_rknn1_biu OFFSET(1) NUMBITS(1) [],
        hresetn_rknn1 OFFSET(2) NUMBITS(1) [],
        hresetn_rknn1_biu OFFSET(3) NUMBITS(1) [],
        _reserved OFFSET(4) NUMBITS(12) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON28  0x0A70;
register_bitfields![u32,
    CRU_SOFTRST_CON28 [
        aresetn_rknn2 OFFSET(0) NUMBITS(1) [],
        aresetn_rknn2_biu OFFSET(1) NUMBITS(1) [],
        hresetn_rknn2 OFFSET(2) NUMBITS(1) [],
        hresetn_rknn2_biu OFFSET(3) NUMBITS(1) [],
        _reserved OFFSET(4) NUMBITS(12) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON29  0x0A74;
register_bitfields![u32,
    CRU_SOFTRST_CON29 [
        _reserved0 OFFSET(0) NUMBITS(3) [],
        aresetn_rknn_dsu0 OFFSET(3) NUMBITS(1) [],
        _reserved1 OFFSET(4) NUMBITS(1) [],
        presetn_nputop_biu OFFSET(5) NUMBITS(1) [],
        presetn_npu_timer OFFSET(6) NUMBITS(1) [],
        _reserved2 OFFSET(7) NUMBITS(1) [],
        resetn_nputimer0 OFFSET(8) NUMBITS(1) [],
        resetn_nputimer1 OFFSET(9) NUMBITS(1) [],
        presetn_npu_wdt OFFSET(10) NUMBITS(1) [],
        tresetn_npu_wdt OFFSET(11) NUMBITS(1) [],
        presetn_pvtm1 OFFSET(12) NUMBITS(1) [],
        presetn_npu_grf OFFSET(13) NUMBITS(1) [],
        resetn_pvtm1 OFFSET(14) NUMBITS(1) [],
        _reserved3 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON30  0x0A78;
register_bitfields![u32,
    CRU_SOFTRST_CON30 [
        resetn_npu_pvtpll OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(1) [],
        hresetn_npu_cm0_biu OFFSET(2) NUMBITS(1) [],
        fresetn_npu_cm0_core OFFSET(3) NUMBITS(1) [],
        tresetn_npu_cm0_jtag OFFSET(4) NUMBITS(1) [],
        _reserved1 OFFSET(5) NUMBITS(1) [],
        aresetn_rknn0 OFFSET(6) NUMBITS(1) [],
        aresetn_rknn0_biu OFFSET(7) NUMBITS(1) [],
        hresetn_rknn0 OFFSET(8) NUMBITS(1) [],
        hresetn_rknn0_biu OFFSET(9) NUMBITS(1) [],
        _reserved2 OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON31  0x0A7C;
register_bitfields![u32,
    CRU_SOFTRST_CON31 [
        _reserved0 OFFSET(0) NUMBITS(2) [],
        hresetn_nvm_biu OFFSET(2) NUMBITS(1) [],
        aresetn_nvm_biu OFFSET(3) NUMBITS(1) [],
        hresetn_emmc OFFSET(4) NUMBITS(1) [],
        aresetn_emmc OFFSET(5) NUMBITS(1) [],
        cresetn_emmc OFFSET(6) NUMBITS(1) [],
        bresetn_emmc OFFSET(7) NUMBITS(1) [],
        tresetn_emmc OFFSET(8) NUMBITS(1) [],
        sresetn_sfc OFFSET(9) NUMBITS(1) [],
        hresetn_sfc OFFSET(10) NUMBITS(1) [],
        hresetn_sfc_xip OFFSET(11) NUMBITS(1) [],
        _reserved1 OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON32  0x0A80;
register_bitfields![u32,
    CRU_SOFTRST_CON32 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        presetn_grf OFFSET(1) NUMBITS(1) [],
        presetn_dec_biu OFFSET(2) NUMBITS(1) [],
        _reserved1 OFFSET(3) NUMBITS(2) [],
        presetn_php_biu OFFSET(5) NUMBITS(1) [],
        _reserved2 OFFSET(6) NUMBITS(2) [],
        aresetn_pcie_bridge OFFSET(8) NUMBITS(1) [],
        aresetn_php_biu OFFSET(9) NUMBITS(1) [],
        aresetn_gmac0 OFFSET(10) NUMBITS(1) [],
        aresetn_gmac1 OFFSET(11) NUMBITS(1) [],
        aresetn_pcie_biu OFFSET(12) NUMBITS(1) [],
        resetn_pcie_4l_power_up OFFSET(13) NUMBITS(1) [],
        resetn_pcie_2l_power_up OFFSET(14) NUMBITS(1) [],
        resetn_pcie_1l0_power_up OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON33  0x0A84;
register_bitfields![u32,
    CRU_SOFTRST_CON33 [
        resetn_pcie_1l1_power_up OFFSET(0) NUMBITS(1) [],
        resetn_pcie_1l2_power_up OFFSET(1) NUMBITS(1) [],
        _reserved OFFSET(2) NUMBITS(10) [],
        presetn_pcie_4l OFFSET(12) NUMBITS(1) [],
        presetn_pcie_2l OFFSET(13) NUMBITS(1) [],
        presetn_pcie_1l0 OFFSET(14) NUMBITS(1) [],
        presetn_pcie_1l1 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON34  0x0A88;
register_bitfields![u32,
    CRU_SOFTRST_CON34 [
        presetn_pcie_1l2 OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(5) [],
        aresetn_php_gic_its OFFSET(6) NUMBITS(1) [],
        aresetn_mmu_pcie OFFSET(7) NUMBITS(1) [],
        aresetn_mmu_php OFFSET(8) NUMBITS(1) [],
        aresetn_mmu_biu OFFSET(9) NUMBITS(1) [],
        _reserved1 OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON35  0x0A8C;
register_bitfields![u32,
    CRU_SOFTRST_CON35 [
        _reserved0 OFFSET(0) NUMBITS(7) [],
        aresetn_usb3otg2 OFFSET(7) NUMBITS(1) [],
        _reserved1 OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON37  0x0A94;
register_bitfields![u32,
    CRU_SOFTRST_CON37 [
        _reserved OFFSET(0) NUMBITS(4) [],
        resetn_pmalive0 OFFSET(4) NUMBITS(1) [],
        resetn_pmalive1 OFFSET(5) NUMBITS(1) [],
        resetn_pmalive2 OFFSET(6) NUMBITS(1) [],
        aresetn_sata0 OFFSET(7) NUMBITS(1) [],
        aresetn_sata1 OFFSET(8) NUMBITS(1) [],
        aresetn_sata2 OFFSET(9) NUMBITS(1) [],
        resetn_rxoob0 OFFSET(10) NUMBITS(1) [],
        resetn_rxoob1 OFFSET(11) NUMBITS(1) [],
        resetn_rxoob2 OFFSET(12) NUMBITS(1) [],
        resetn_asic0 OFFSET(13) NUMBITS(1) [],
        resetn_asic1 OFFSET(14) NUMBITS(1) [],
        resetn_asic2 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON40  0x0AA0;
register_bitfields![u32,
    CRU_SOFTRST_CON40 [
        _reserved0 OFFSET(0) NUMBITS(2) [],
        aresetn_rkvdec_ccu OFFSET(2) NUMBITS(1) [],
        hresetn_rkvdec0 OFFSET(3) NUMBITS(1) [],
        aresetn_rkvdec0 OFFSET(4) NUMBITS(1) [],
        hresetn_rkvdec0_biu OFFSET(5) NUMBITS(1) [],
        aresetn_rkvdec0_biu OFFSET(6) NUMBITS(1) [],
        resetn_rkvdec0_ca OFFSET(7) NUMBITS(1) [],
        resetn_rkvdec0_hevc_c OFFSET(8) NUMBITS(1) [],
        resetn_rkvdec0_core OFFSET(9) NUMBITS(1) [],
        _reserved1 OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON41  0x0AA4;
register_bitfields![u32,
    CRU_SOFTRST_CON41 [
        _reserved0 OFFSET(0) NUMBITS(2) [],
        hresetn_rkvdec1 OFFSET(2) NUMBITS(1) [],
        rearesetn_rkvdec1served OFFSET(3) NUMBITS(1) [],
        hresetn_rkvdec1_biu OFFSET(4) NUMBITS(1) [],
        aresetn_rkvdec1_biu OFFSET(5) NUMBITS(1) [],
        resetn_rkvdec1_ca OFFSET(6) NUMBITS(1) [],
        resetn_rkvdec1_hevc_ca OFFSET(7) NUMBITS(1) [],
        resetn_rkvdec1_core OFFSET(8) NUMBITS(1) [],
        _reserved1 OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON42  0x0AA8;
register_bitfields![u32,
    CRU_SOFTRST_CON42 [
        _reserved0 OFFSET(0) NUMBITS(2) [],
        aresetn_usb_biu OFFSET(2) NUMBITS(1) [],
        hresetn_usb_biu OFFSET(3) NUMBITS(1) [],
        aresetn_usb3otg0 OFFSET(4) NUMBITS(1) [],
        _reserved1 OFFSET(5) NUMBITS(2) [],
        aresetn_usb3otg1 OFFSET(7) NUMBITS(1) [],
        _reserved2 OFFSET(8) NUMBITS(2) [],
        hresetn_host0 OFFSET(10) NUMBITS(1) [],
        hresetn_host_arb0 OFFSET(11) NUMBITS(1) [],
        hresetn_host1 OFFSET(12) NUMBITS(1) [],
        hresetn_host_arb1 OFFSET(13) NUMBITS(1) [],
        aresetn_usb_grf OFFSET(14) NUMBITS(1) [],
        cresetn_usb2p0_host0 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON43  0x0AAC;
register_bitfields![u32,
    CRU_SOFTRST_CON43 [
        cresetn_usb2p0_host1 OFFSET(0) NUMBITS(1) [],
        resetn_host_utmi0 OFFSET(1) NUMBITS(1) [],
        resetn_host_utmi1 OFFSET(2) NUMBITS(1) [],
        _reserved OFFSET(3) NUMBITS(13) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON44  0x0AB0;
register_bitfields![u32,
    CRU_SOFTRST_CON44 [
        _reserved OFFSET(0) NUMBITS(4) [],
        aresetn_vdpu_biu OFFSET(4) NUMBITS(1) [],
        aresetn_vdpu_low_biu OFFSET(5) NUMBITS(1) [],
        reserhresetn_vdpu_biuved OFFSET(6) NUMBITS(1) [],
        aresetn_jpeg_decoder_biu OFFSET(7) NUMBITS(1) [],
        aresetn_vpu OFFSET(8) NUMBITS(1) [],
        hresetn_vpu OFFSET(9) NUMBITS(1) [],
        aresetn_jpeg_encoder0 OFFSET(10) NUMBITS(1) [],
        hresetn_jpeg_encoder0 OFFSET(11) NUMBITS(1) [],
        aresetn_jpeg_encoder1 OFFSET(12) NUMBITS(1) [],
        hresetn_jpeg_encoder1 OFFSET(13) NUMBITS(1) [],
        aresetn_jpeg_encoder2 OFFSET(14) NUMBITS(1) [],
        hresetn_jpeg_encoder2 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON45  0x0AB4;
register_bitfields![u32,
    CRU_SOFTRST_CON45 [
        aresetn_jpeg_encoder3 OFFSET(0) NUMBITS(1) [],
        hresetn_jpeg_encoder3 OFFSET(1) NUMBITS(1) [],
        aresetn_jpeg_decoder OFFSET(2) NUMBITS(1) [],
        hresetn_jpeg_decoder OFFSET(3) NUMBITS(1) [],
        hresetn_iep2p0 OFFSET(4) NUMBITS(1) [],
        aresetn_iep2p0 OFFSET(5) NUMBITS(1) [],
        resetn_iep2p0_core OFFSET(6) NUMBITS(1) [],
        hresetn_rga2 OFFSET(7) NUMBITS(1) [],
        aresetn_rga2 OFFSET(8) NUMBITS(1) [],
        resetn_rga2_core OFFSET(9) NUMBITS(1) [],
        hresetn_rga3_0 OFFSET(10) NUMBITS(1) [],
        aresetn_rga3_0 OFFSET(11) NUMBITS(1) [],
        resetn_rga3_0_core OFFSET(12) NUMBITS(1) [],
        _reserved OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON47  0x0ABC;
register_bitfields![u32,
    CRU_SOFTRST_CON47 [
        _reserved0 OFFSET(0) NUMBITS(2) [],
        hresetn_rkvenc0_biu OFFSET(2) NUMBITS(1) [],
        aresetn_rkvenc0_biu OFFSET(3) NUMBITS(1) [],
        hresetn_rkvenc0 OFFSET(4) NUMBITS(1) [],
        aresetn_rkvenc0 OFFSET(5) NUMBITS(1) [],
        resetn_rkvenc0_core OFFSET(6) NUMBITS(1) [],
        _reserved1 OFFSET(7) NUMBITS(9) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON48  0x0AC0;
register_bitfields![u32,
    CRU_SOFTRST_CON48 [
        _reserved0 OFFSET(0) NUMBITS(2) [],
        hresetn_rkvenc1_biu OFFSET(2) NUMBITS(1) [],
        aresetn_rkvenc1_biu OFFSET(3) NUMBITS(1) [],
        hresetn_rkvenc1 OFFSET(4) NUMBITS(1) [],
        aresetn_rkvenc1 OFFSET(5) NUMBITS(1) [],
        resetn_rkvenc1_core OFFSET(6) NUMBITS(1) [],
        _reserved1 OFFSET(7) NUMBITS(9) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON49  0x0AC4;
register_bitfields![u32,
    CRU_SOFTRST_CON49 [
        _reserved0 OFFSET(0) NUMBITS(3) [],
        aresetn_vi_biu OFFSET(3) NUMBITS(1) [],
        hresetn_vi_biu OFFSET(4) NUMBITS(1) [],
        presetn_vi_biu OFFSET(5) NUMBITS(1) [],
        dresetn_vicap OFFSET(6) NUMBITS(1) [],
        aresetn_vicap OFFSET(7) NUMBITS(1) [],
        hresetn_vicap OFFSET(8) NUMBITS(1) [],
        _reserved1 OFFSET(9) NUMBITS(1) [],
        resetn_isp0 OFFSET(10) NUMBITS(1) [],
        resetn_isp0_vicap OFFSET(11) NUMBITS(1) [],
        _reserved2 OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON50  0x0AC8;
register_bitfields![u32,
    CRU_SOFTRST_CON50 [
        resetn_fisheye0 OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(2) [],
        resetn_fisheye1 OFFSET(3) NUMBITS(1) [],
        presetn_csi_host_0 OFFSET(4) NUMBITS(1) [],
        presetn_csi_host_1 OFFSET(5) NUMBITS(1) [],
        presetn_csi_host_2 OFFSET(6) NUMBITS(1) [],
        presetn_csi_host_3 OFFSET(7) NUMBITS(1) [],
        presetn_csi_host_4 OFFSET(8) NUMBITS(1) [],
        presetn_csi_host_5 OFFSET(9) NUMBITS(1) [],
        _reserved1 OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON51  0x0ACC;
register_bitfields![u32,
    CRU_SOFTRST_CON51 [
        _reserved0 OFFSET(0) NUMBITS(4) [],
        resetn_csihost0_vicap OFFSET(4) NUMBITS(1) [],
        resetn_csihost1_vicap OFFSET(5) NUMBITS(1) [],
        resetn_csihost2_vicap OFFSET(6) NUMBITS(1) [],
        resetn_csihost3_vicap OFFSET(7) NUMBITS(1) [],
        resetn_csihost4_vicap OFFSET(8) NUMBITS(1) [],
        resetn_csihost5_vicap OFFSET(9) NUMBITS(1) [],
        _reserved1 OFFSET(10) NUMBITS(3) [],
        resetn_cifin OFFSET(13) NUMBITS(1) [],
        _reserved2 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON52  0x0AD0;
register_bitfields![u32,
    CRU_SOFTRST_CON52 [
        _reserved0 OFFSET(0) NUMBITS(4) [],
        aresetn_vop_biu OFFSET(4) NUMBITS(1) [],
        aresetn_vop_low_biu OFFSET(5) NUMBITS(1) [],
        hresetn_vop_biu OFFSET(6) NUMBITS(1) [],
        presetn_vop_biu OFFSET(7) NUMBITS(1) [],
        hresetn_vop OFFSET(8) NUMBITS(1) [],
        aresetn_vop OFFSET(9) NUMBITS(1) [],
        _reserved1 OFFSET(10) NUMBITS(3) [],
        dresetn_vp0 OFFSET(13) NUMBITS(1) [],
        dresetn_vp2hdmi_bridge0 OFFSET(14) NUMBITS(1) [],
        dresetn_vp2hdmi_bridge1 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON53  0x0AD4;
register_bitfields![u32,
    CRU_SOFTRST_CON53 [
        dresetn_vp1 OFFSET(0) NUMBITS(1) [],
        dresetn_vp2 OFFSET(1) NUMBITS(1) [],
        dresetn_vp3 OFFSET(2) NUMBITS(1) [],
        presetn_vopgrf OFFSET(3) NUMBITS(1) [],
        presetn_dsihost0 OFFSET(4) NUMBITS(1) [],
        presetn_dsihost1 OFFSET(5) NUMBITS(1) [],
        resetn_dsihost0 OFFSET(6) NUMBITS(1) [],
        resetn_dsihost1 OFFSET(7) NUMBITS(1) [],
        resetn_vop_pmu OFFSET(8) NUMBITS(1) [],
        presetn_vop_channel_biu OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON55  0x0ADC;
register_bitfields![u32,
    CRU_SOFTRST_CON55 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        hresetn_vo0_biu OFFSET(5) NUMBITS(1) [],
        hresetn_vo0_s_biu OFFSET(6) NUMBITS(1) [],
        presetn_vo0_biu OFFSET(7) NUMBITS(1) [],
        presetn_vo0_s_biu OFFSET(8) NUMBITS(1) [],
        aresetn_hdcp0_biu OFFSET(9) NUMBITS(1) [],
        presetn_vo0grf OFFSET(10) NUMBITS(1) [],
        hresetn_hdcp_key0 OFFSET(11) NUMBITS(1) [],
        aresetn_hdcp0 OFFSET(12) NUMBITS(1) [],
        hresetn_hdcp0 OFFSET(13) NUMBITS(1) [],
        _reserved1 OFFSET(14) NUMBITS(1) [],
        resetn_hdcp0 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON56  0x0AE0;
register_bitfields![u32,
    CRU_SOFTRST_CON56 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        presetn_trng0 OFFSET(1) NUMBITS(1) [],
        _reserved1 OFFSET(2) NUMBITS(6) [],
        resetn_dp0 OFFSET(8) NUMBITS(1) [],
        resetn_dp1 OFFSET(9) NUMBITS(1) [],
        hresetn_i2s4_8ch OFFSET(10) NUMBITS(1) [],
        _reserved2 OFFSET(11) NUMBITS(2) [],
        mresetn_i2s4_8ch_tx OFFSET(13) NUMBITS(1) [],
        hresetn_i2s8_8ch OFFSET(14) NUMBITS(1) [],
        _reserved3 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON57  0x0AE4;
register_bitfields![u32,
    CRU_SOFTRST_CON57 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        mresetn_i2s8_8ch_tx OFFSET(1) NUMBITS(1) [],
        hresetn_spdif2_dp0 OFFSET(2) NUMBITS(1) [],
        _reserved1 OFFSET(3) NUMBITS(3) [],
        mresetn_spdif2_dp0 OFFSET(6) NUMBITS(1) [],
        hresetn_spdif5_dp1 OFFSET(7) NUMBITS(1) [],
        _reserved2 OFFSET(8) NUMBITS(3) [],
        mresetn_spdif5_dp1 OFFSET(11) NUMBITS(1) [],
        _reserved3 OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON59  0x0AEC;
register_bitfields![u32,
    CRU_SOFTRST_CON59 [
        _reserved0 OFFSET(0) NUMBITS(6) [],
        aresetn_hdcp1_biu OFFSET(6) NUMBITS(1) [],
        _reserved1 OFFSET(7) NUMBITS(1) [],
        aresetn_vo1_biu OFFSET(8) NUMBITS(1) [],
        hresetn_vo1_biu0 OFFSET(9) NUMBITS(1) [],
        hresetn_vo1_s_biu OFFSET(10) NUMBITS(1) [],
        hresetn_vo1_biu1 OFFSET(11) NUMBITS(1) [],
        presetn_vo1grf OFFSET(12) NUMBITS(1) [],
        presetn_vo1_s_biu OFFSET(13) NUMBITS(1) [],
        _reserved2 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON60  0x0AF0;
register_bitfields![u32,
    CRU_SOFTRST_CON60 [
        hresetn_i2s7_8ch OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(2) [],
        mresetn_i2s7_8ch_rx OFFSET(3) NUMBITS(1) [],
        hresetn_hdcp_key1 OFFSET(4) NUMBITS(1) [],
        aresetn_hdcp1 OFFSET(5) NUMBITS(1) [],
        hresetn_hdcp1 OFFSET(6) NUMBITS(1) [],
        _reserved1 OFFSET(7) NUMBITS(1) [],
        resetn_hdcp1 OFFSET(8) NUMBITS(1) [],
        _reserved2 OFFSET(9) NUMBITS(1) [],
        presetn_trng1 OFFSET(10) NUMBITS(1) [],
        presetn_hdmitx0 OFFSET(11) NUMBITS(1) [],
        _reserved3 OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON61  0x0AF4;
register_bitfields![u32,
    CRU_SOFTRST_CON61 [
        resetn_hdmitx0_ref OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(1) [],
        presetn_hdmitx1 OFFSET(2) NUMBITS(1) [],
        _reserved1 OFFSET(3) NUMBITS(4) [],
        resetn_hdmitx1_ref OFFSET(7) NUMBITS(1) [],
        _reserved2 OFFSET(8) NUMBITS(1) [],
        aresetn_hdmirx OFFSET(9) NUMBITS(1) [],
        presetn_hdmirx OFFSET(10) NUMBITS(1) [],
        resetn_hdmirx_ref OFFSET(11) NUMBITS(1) [],
        _reserved3 OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON62  0x0AF8;
register_bitfields![u32,
    CRU_SOFTRST_CON62 [
        presetn_edp0 OFFSET(0) NUMBITS(1) [],
        resetn_edp0_24m OFFSET(1) NUMBITS(1) [],
        _reserved0 OFFSET(2) NUMBITS(1) [],
        presetn_edp1 OFFSET(3) NUMBITS(1) [],
        resetn_edp1_24m OFFSET(4) NUMBITS(1) [],
        _reserved1 OFFSET(5) NUMBITS(3) [],
        mresetn_i2s5_8ch_tx OFFSET(8) NUMBITS(1) [],
        _reserved2 OFFSET(9) NUMBITS(3) [],
        hresetn_i2s5_8ch OFFSET(12) NUMBITS(1) [],
        _reserved3 OFFSET(13) NUMBITS(2) [],
        mresetn_i2s6_8ch_tx OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON63  0x0AFC;
register_bitfields![u32,
    CRU_SOFTRST_CON63 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        mresetn_i2s6_8ch_rx OFFSET(2) NUMBITS(1) [],
        hresetn_i2s6_8ch OFFSET(3) NUMBITS(1) [],
        hresetn_spdif3 OFFSET(4) NUMBITS(1) [],
        _reserved1 OFFSET(5) NUMBITS(2) [],
        mresetn_spdif3 OFFSET(7) NUMBITS(1) [],
        hresetn_spdif4 OFFSET(8) NUMBITS(1) [],
        _reserved2 OFFSET(9) NUMBITS(2) [],
        mresetn_spdif4 OFFSET(11) NUMBITS(1) [],
        hresetn_spdifrx0 OFFSET(12) NUMBITS(1) [],
        mresetn_spdifrx0 OFFSET(13) NUMBITS(1) [],
        hresetn_spdifrx1 OFFSET(14) NUMBITS(1) [],
        mresetn_spdifrx1 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON64  0x0B00;
register_bitfields![u32,
    CRU_SOFTRST_CON64 [
        hresetn_spdifrx2 OFFSET(0) NUMBITS(1) [],
        mresetn_spdifrx2 OFFSET(1) NUMBITS(1) [],
        _reserved OFFSET(2) NUMBITS(10) [],
        resetn_linksym_hdmitxphy0 OFFSET(12) NUMBITS(1) [],
        resetn_linksym_hdmitxphy1 OFFSET(13) NUMBITS(1) [],
        resetn_vo1_bridge0 OFFSET(14) NUMBITS(1) [],
        resetn_vo1_bridge1 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON65  0x0B04;
register_bitfields![u32,
    CRU_SOFTRST_CON65 [
        hresetn_i2s9_8ch OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(2) [],
        mresetn_i2s9_8ch_rx OFFSET(3) NUMBITS(1) [],
        hresetn_i2s10_8ch OFFSET(4) NUMBITS(1) [],
        _reserved1 OFFSET(5) NUMBITS(2) [],
        mresetn_i2s10_8ch_rx OFFSET(7) NUMBITS(1) [],
        presetn_s_hdmirx OFFSET(8) NUMBITS(1) [],
        _reserved2 OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON66  0x0B08;
register_bitfields![u32,
    CRU_SOFTRST_CON66 [
        _reserved0 OFFSET(0) NUMBITS(4) [],
        resetn_gpu OFFSET(4) NUMBITS(1) [],
        sysresetn_gpu OFFSET(5) NUMBITS(1) [],
        _reserved1 OFFSET(6) NUMBITS(2) [],
        aresetn_s_gpu_biu OFFSET(8) NUMBITS(1) [],
        aresetn_m0_gpu_biu OFFSET(9) NUMBITS(1) [],
        aresetn_m1_gpu_biu OFFSET(10) NUMBITS(1) [],
        aresetn_m2_gpu_biu OFFSET(11) NUMBITS(1) [],
        aresetn_m3_gpu_biu OFFSET(12) NUMBITS(1) [],
        _reserved2 OFFSET(13) NUMBITS(1) [],
        presetn_gpu_biu OFFSET(14) NUMBITS(1) [],
        presetn_pvtm2 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON67  0x0B0C;
register_bitfields![u32,
    CRU_SOFTRST_CON67 [
        resetn_pvtm2 OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(1) [],
        presetn_gpu_grf OFFSET(2) NUMBITS(1) [],
        resetn_gpu_pvtpll OFFSET(3) NUMBITS(1) [],
        poresetn_gpu_jtag OFFSET(4) NUMBITS(1) [],
        _reserved1 OFFSET(5) NUMBITS(11) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON68  0x0B10;
register_bitfields![u32,
    CRU_SOFTRST_CON68 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        aresetn_av1_biu OFFSET(1) NUMBITS(1) [],
        aresetn_av1 OFFSET(2) NUMBITS(1) [],
        _reserved1 OFFSET(3) NUMBITS(1) [],
        presetn_av1_biu OFFSET(4) NUMBITS(1) [],
        presetn_av1 OFFSET(5) NUMBITS(1) [],
        _reserved2 OFFSET(6) NUMBITS(10) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON69  0x0B14;
register_bitfields![u32,
    CRU_SOFTRST_CON69 [
        _reserved0 OFFSET(0) NUMBITS(4) [],
        aresetn_ddr_biu OFFSET(4) NUMBITS(1) [],
        aresetn_dma2ddr OFFSET(5) NUMBITS(1) [],
        aresetn_ddr_sharemem OFFSET(6) NUMBITS(1) [],
        aresetn_ddr_sharemem_biu OFFSET(7) NUMBITS(1) [],
        _reserved1 OFFSET(8) NUMBITS(2) [],
        aresetn_center_s200_biu OFFSET(10) NUMBITS(1) [],
        aresetn_center_s400_biu OFFSET(11) NUMBITS(1) [],
        hresetn_ahb2apb OFFSET(12) NUMBITS(1) [],
        hresetn_center_biu OFFSET(13) NUMBITS(1) [],
        fresetn_ddr_cm0_core OFFSET(14) NUMBITS(1) [],
        _reserved2 OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON70  0x0B18;
register_bitfields![u32,
    CRU_SOFTRST_CON70 [
        resetn_ddr_timer0 OFFSET(0) NUMBITS(1) [],
        resetn_ddr_timer1 OFFSET(1) NUMBITS(1) [],
        tresetn_wdt_ddr OFFSET(2) NUMBITS(1) [],
        tresetn_ddr_cm0_jtag OFFSET(3) NUMBITS(1) [],
        _reserved0 OFFSET(4) NUMBITS(1) [],
        presetn_center_grf OFFSET(5) NUMBITS(1) [],
        presetn_ahb2apb OFFSET(6) NUMBITS(1) [],
        presetn_wdt OFFSET(7) NUMBITS(1) [],
        presetn_timer OFFSET(8) NUMBITS(1) [],
        presetn_dma2ddr OFFSET(9) NUMBITS(1) [],
        presetn_sharemem OFFSET(10) NUMBITS(1) [],
        presetn_center_biu OFFSET(11) NUMBITS(1) [],
        presetn_center_channel_biu OFFSET(12) NUMBITS(1) [],
        _reserved1 OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON72  0x0B20;
register_bitfields![u32,
    CRU_SOFTRST_CON72 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        presetn_usbdpgrf0 OFFSET(1) NUMBITS(1) [],
        presetn_usbdpphy0 OFFSET(2) NUMBITS(1) [],
        presetn_usbdpgrf1 OFFSET(3) NUMBITS(1) [],
        presetn_usbdpphy1 OFFSET(4) NUMBITS(1) [],
        presetn_hdptx0 OFFSET(5) NUMBITS(1) [],
        presetn_hdptx1 OFFSET(6) NUMBITS(1) [],
        presetn_apb2asb_slv_bot_right OFFSET(7) NUMBITS(1) [],
        presetn_usb2phy_u3_0_grf0 OFFSET(8) NUMBITS(1) [],
        presetn_usb2phy_u3_1_grf0 OFFSET(9) NUMBITS(1) [],
        presetn_usb2phy_u2_0_grf0 OFFSET(10) NUMBITS(1) [],
        presetn_usb2phy_u2_1_grf0 OFFSET(11) NUMBITS(1) [],
        _reserved1 OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON73  0x0B24;
register_bitfields![u32,
    CRU_SOFTRST_CON73 [
        _reserved0 OFFSET(0) NUMBITS(12) [],
        resetn_hdmihdp0 OFFSET(12) NUMBITS(1) [],
        resetn_hdmihdp1 OFFSET(13) NUMBITS(1) [],
        _reserved1 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON74  0x0B28;
register_bitfields![u32,
    CRU_SOFTRST_CON74 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        aresetn_vo1usb_top_biu OFFSET(1) NUMBITS(1) [],
        _reserved1 OFFSET(2) NUMBITS(1) [],
        hresetn_vo1usb_top_biu OFFSET(3) NUMBITS(1) [],
        _reserved2 OFFSET(4) NUMBITS(12) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON75  0x0B2C;
register_bitfields![u32,
    CRU_SOFTRST_CON75 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        hresetn_sdio_biu OFFSET(1) NUMBITS(1) [],
        hresetn_sdio OFFSET(2) NUMBITS(1) [],
        resetn_sdio OFFSET(3) NUMBITS(1) [],
        _reserved1 OFFSET(4) NUMBITS(12) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON76  0x0B30;
register_bitfields![u32,
    CRU_SOFTRST_CON76 [
        _reserved0 OFFSET(0) NUMBITS(2) [],
        hresetn_rga3_biu OFFSET(2) NUMBITS(1) [],
        aresetn_rga3_biu OFFSET(3) NUMBITS(1) [],
        hresetn_rga3_1 OFFSET(4) NUMBITS(1) [],
        aresetn_rga3_1 OFFSET(5) NUMBITS(1) [],
        resetn_rga3_1_core OFFSET(6) NUMBITS(1) [],
        _reserved1 OFFSET(7) NUMBITS(9) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SOFTRST_CON77  0x0B34;
register_bitfields![u32,
    CRU_SOFTRST_CON77 [
        _reserved0 OFFSET(0) NUMBITS(6) [],
        resetn_ref_pipe_phy0 OFFSET(6) NUMBITS(1) [],
        resetn_ref_pipe_phy1 OFFSET(7) NUMBITS(1) [],
        resetn_ref_pipe_phy2 OFFSET(8) NUMBITS(1) [],
        _reserved1 OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];
