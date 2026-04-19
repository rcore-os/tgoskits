use tock_registers::{register_bitfields, registers::ReadWrite};

pub struct GateRegisters {
    pub gate_con0: ReadWrite<u32, CRU_GATE_CON00::Register>,
    pub gate_con1: ReadWrite<u32, CRU_GATE_CON01::Register>,
    pub gate_con2: ReadWrite<u32, CRU_GATE_CON02::Register>,
    pub gate_con3: ReadWrite<u32, CRU_GATE_CON03::Register>,
    pub gate_con4: ReadWrite<u32, CRU_GATE_CON04::Register>,
    pub gate_con5: ReadWrite<u32, CRU_GATE_CON05::Register>,
    pub gate_con6: ReadWrite<u32, CRU_GATE_CON06::Register>,
    pub gate_con7: ReadWrite<u32, CRU_GATE_CON07::Register>,
    pub gate_con8: ReadWrite<u32, CRU_GATE_CON08::Register>,
    pub gate_con9: ReadWrite<u32, CRU_GATE_CON09::Register>,
    pub gate_con10: ReadWrite<u32, CRU_GATE_CON10::Register>,
    pub gate_con11: ReadWrite<u32, CRU_GATE_CON11::Register>,
    pub gate_con12: ReadWrite<u32, CRU_GATE_CON12::Register>,
    pub gate_con13: ReadWrite<u32, CRU_GATE_CON13::Register>,
    pub gate_con14: ReadWrite<u32, CRU_GATE_CON14::Register>,
    pub gate_con15: ReadWrite<u32, CRU_GATE_CON15::Register>,
    pub gate_con16: ReadWrite<u32, CRU_GATE_CON16::Register>,
    pub gate_con17: ReadWrite<u32, CRU_GATE_CON17::Register>,
    pub gate_con18: ReadWrite<u32, CRU_GATE_CON18::Register>,
    pub gate_con19: ReadWrite<u32, CRU_GATE_CON19::Register>,
    pub gate_con20: ReadWrite<u32, CRU_GATE_CON20::Register>,
    pub gate_con21: ReadWrite<u32, CRU_GATE_CON21::Register>,
    pub gate_con22: ReadWrite<u32, CRU_GATE_CON22::Register>,
    pub gate_con23: ReadWrite<u32, CRU_GATE_CON23::Register>,
    pub gate_con24: ReadWrite<u32, CRU_GATE_CON24::Register>,
    pub gate_con25: ReadWrite<u32, CRU_GATE_CON25::Register>,
    pub gate_con26: ReadWrite<u32, CRU_GATE_CON26::Register>,
    pub gate_con27: ReadWrite<u32, CRU_GATE_CON27::Register>,
    pub gate_con28: ReadWrite<u32, CRU_GATE_CON28::Register>,
    pub gate_con29: ReadWrite<u32, CRU_GATE_CON29::Register>,
    pub gate_con30: ReadWrite<u32, CRU_GATE_CON30::Register>,
    pub gate_con31: ReadWrite<u32, CRU_GATE_CON31::Register>,
    pub gate_con32: ReadWrite<u32, CRU_GATE_CON32::Register>,
    pub gate_con33: ReadWrite<u32, CRU_GATE_CON33::Register>,
    pub gate_con34: ReadWrite<u32, CRU_GATE_CON34::Register>,
    pub gate_con35: ReadWrite<u32, CRU_GATE_CON35::Register>,
    _reserved0: u32,
    pub gate_con37: ReadWrite<u32, CRU_GATE_CON37::Register>,
    pub gate_con38: ReadWrite<u32, CRU_GATE_CON38::Register>,
    pub gate_con39: ReadWrite<u32, CRU_GATE_CON39::Register>,
    pub gate_con40: ReadWrite<u32, CRU_GATE_CON40::Register>,
    pub gate_con41: ReadWrite<u32, CRU_GATE_CON41::Register>,
    pub gate_con42: ReadWrite<u32, CRU_GATE_CON42::Register>,
    pub gate_con43: ReadWrite<u32, CRU_GATE_CON43::Register>,
    pub gate_con44: ReadWrite<u32, CRU_GATE_CON44::Register>,
    pub gate_con45: ReadWrite<u32, CRU_GATE_CON45::Register>,
    _reserved1: u32,
    pub gate_con47: ReadWrite<u32, CRU_GATE_CON47::Register>,
    pub gate_con48: ReadWrite<u32, CRU_GATE_CON48::Register>,
    pub gate_con49: ReadWrite<u32, CRU_GATE_CON49::Register>,
    pub gate_con50: ReadWrite<u32, CRU_GATE_CON50::Register>,
    pub gate_con51: ReadWrite<u32, CRU_GATE_CON51::Register>,
    pub gate_con52: ReadWrite<u32, CRU_GATE_CON52::Register>,
    pub gate_con53: ReadWrite<u32, CRU_GATE_CON53::Register>,
    _reserved2: u32,
    pub gate_con55: ReadWrite<u32, CRU_GATE_CON55::Register>,
    pub gate_con56: ReadWrite<u32, CRU_GATE_CON56::Register>,
    pub gate_con57: ReadWrite<u32, CRU_GATE_CON57::Register>,
    _reserved3: u32,
    pub gate_con59: ReadWrite<u32, CRU_GATE_CON59::Register>,
    pub gate_con60: ReadWrite<u32, CRU_GATE_CON60::Register>,
    pub gate_con61: ReadWrite<u32, CRU_GATE_CON61::Register>,
    pub gate_con62: ReadWrite<u32, CRU_GATE_CON62::Register>,
    pub gate_con63: ReadWrite<u32, CRU_GATE_CON63::Register>,
    pub gate_con64: ReadWrite<u32, CRU_GATE_CON64::Register>,
    pub gate_con65: ReadWrite<u32, CRU_GATE_CON65::Register>,
    pub gate_con66: ReadWrite<u32, CRU_GATE_CON66::Register>,
    pub gate_con67: ReadWrite<u32, CRU_GATE_CON67::Register>,
    pub gate_con68: ReadWrite<u32, CRU_GATE_CON68::Register>,
    pub gate_con69: ReadWrite<u32, CRU_GATE_CON69::Register>,
    pub gate_con70: ReadWrite<u32, CRU_GATE_CON70::Register>,
    _reserved4: u32,
    pub gate_con72: ReadWrite<u32, CRU_GATE_CON72::Register>,
    pub gate_con73: ReadWrite<u32, CRU_GATE_CON73::Register>,
    pub gate_con74: ReadWrite<u32, CRU_GATE_CON74::Register>,
    pub gate_con75: ReadWrite<u32, CRU_GATE_CON75::Register>,
    pub gate_con76: ReadWrite<u32, CRU_GATE_CON76::Register>,
}

// CRU_GATE_CON00  0x0800
register_bitfields![u32,
    CRU_GATE_CON00 [
        clk_matrix_50m_src_en OFFSET(0) NUMBITS(1) [],
        clk_matrix_100m_src_en OFFSET(1) NUMBITS(1) [],
        clk_matrix_150m_src_en OFFSET(2) NUMBITS(1) [],
        clk_matrix_200m_src_en OFFSET(3) NUMBITS(1) [],
        clk_matrix_250m_src_en OFFSET(4) NUMBITS(1) [],
        clk_matrix_300m_src_en OFFSET(5) NUMBITS(1) [],
        clk_matrix_350m_src_en OFFSET(6) NUMBITS(1) [],
        clk_matrix_400m_src_en OFFSET(7) NUMBITS(1) [],
        clk_matrix_450m_src_en OFFSET(8) NUMBITS(1) [],
        clk_matrix_500m_src_en OFFSET(9) NUMBITS(1) [],
        clk_matrix_600m_src_en OFFSET(10) NUMBITS(1) [],
        clk_matrix_650m_src_en OFFSET(11) NUMBITS(1) [],
        clk_matrix_700m_src_en OFFSET(12) NUMBITS(1) [],
        clk_matrix_800m_src_en OFFSET(13) NUMBITS(1) [],
        clk_matrix_1000m_src_en OFFSET(14) NUMBITS(1) [],
        clk_matrix_1200m_src_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON01  0x0804
register_bitfields![u32,
    CRU_GATE_CON01 [
        aclk_top_root_en OFFSET(0) NUMBITS(1) [],
        pclk_top_root_en OFFSET(1) NUMBITS(1) [],
        aclk_low_top_root_en OFFSET(2) NUMBITS(1) [],
        aclk_top_biu_en OFFSET(3) NUMBITS(1) [],
        pclk_top_biu_en OFFSET(4) NUMBITS(1) [],
        _reserved0 OFFSET(5) NUMBITS(1) [],
        pclk_csiphy0_en OFFSET(6) NUMBITS(1) [],
        _reserved1 OFFSET(7) NUMBITS(1) [],
        pclk_csiphy1_en OFFSET(8) NUMBITS(1) [],
        _reserved2 OFFSET(9) NUMBITS(1) [],
        aclk_top_m300_root_en OFFSET(10) NUMBITS(1) [],
        aclk_top_m500_root_en OFFSET(11) NUMBITS(1) [],
        aclk_top_m400_root_en OFFSET(12) NUMBITS(1) [],
        aclk_top_s200_root_en OFFSET(13) NUMBITS(1) [],
        aclk_top_s400_root_en OFFSET(14) NUMBITS(1) [],
        aclk_top_m500_biu_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON02  0x0808
register_bitfields![u32,
    CRU_GATE_CON02 [
        aclk_top_m400_biu_en OFFSET(0) NUMBITS(1) [],
        aclk_top_s200_biu_en OFFSET(1) NUMBITS(1) [],
        aclk_top_s400_biu_en OFFSET(2) NUMBITS(1) [],
        aclk_top_m300_biu_en OFFSET(3) NUMBITS(1) [],
        clk_testout_top_en OFFSET(4) NUMBITS(1) [],
        _reserved0 OFFSET(5) NUMBITS(1) [],
        clk_testout_grp0_en OFFSET(6) NUMBITS(1) [],
        _reserved1 OFFSET(7) NUMBITS(1) [],
        clk_usbdp_combo_phy0_immortal_en OFFSET(8) NUMBITS(1) [],
        _reserved2 OFFSET(9) NUMBITS(6) [],
        clk_usbdp_combo_phy1_immortal_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON03  0x080C
register_bitfields![u32,
    CRU_GATE_CON03 [
        _reserved OFFSET(0) NUMBITS(14) [],
        pclk_mipi_dcphy0_en OFFSET(14) NUMBITS(1) [],
        pclk_mipi_dcphy0_grf_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON04  0x0810
register_bitfields![u32,
    CRU_GATE_CON04 [
        _reserved0 OFFSET(0) NUMBITS(3) [],
        pclk_mipi_dcphy1_en OFFSET(3) NUMBITS(1) [],
        pclk_mipi_dcphy1_grf_en OFFSET(4) NUMBITS(1) [],
        pclk_apb2asb_slv_cdphy_en OFFSET(5) NUMBITS(1) [],
        pclk_apb2asb_slv_csiphy_en OFFSET(6) NUMBITS(1) [],
        pclk_apb2asb_slv_vccio3_5_en OFFSET(7) NUMBITS(1) [],
        pclk_apb2asb_slv_vccio6_en OFFSET(8) NUMBITS(1) [],
        pclk_apb2asb_slv_emmcio_en OFFSET(9) NUMBITS(1) [],
        pclk_apb2asb_slv_ioc_top_en OFFSET(10) NUMBITS(1) [],
        pclk_apb2asb_slv_ioc_right_en OFFSET(11) NUMBITS(1) [],
        _reserved1 OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON05  0x0814
register_bitfields![u32,
    CRU_GATE_CON05 [
        pclk_cru_en OFFSET(0) NUMBITS(1) [],
        _reserved OFFSET(1) NUMBITS(2) [],
        mclk_gmac0_out_en OFFSET(3) NUMBITS(1) [],
        refclko25m_eth0_out_en OFFSET(4) NUMBITS(1) [],
        refclko25m_eth1_out_en OFFSET(5) NUMBITS(1) [],
        clk_cifout_out_en OFFSET(6) NUMBITS(1) [],
        aclk_channel_secure2vo1usb_en OFFSET(7) NUMBITS(1) [],
        aclk_channel_secure2center_en OFFSET(8) NUMBITS(1) [],
        clk_mipi_cameraout_m0_en OFFSET(9) NUMBITS(1) [],
        clk_mipi_cameraout_m1_en OFFSET(10) NUMBITS(1) [],
        clk_mipi_cameraout_m2_en OFFSET(11) NUMBITS(1) [],
        clk_mipi_cameraout_m3_en OFFSET(12) NUMBITS(1) [],
        clk_mipi_cameraout_m4_en OFFSET(13) NUMBITS(1) [],
        hclk_channel_secure2vo1usb_en OFFSET(14) NUMBITS(1) [],
        hclk_channel_secure2center_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON06  0x0818
register_bitfields![u32,
    CRU_GATE_CON06 [
        pclk_channel_secure2vo1usb_en OFFSET(0) NUMBITS(1) [],
        pclk_channel_secure2center_en OFFSET(1) NUMBITS(1) [],
        _reserved OFFSET(2) NUMBITS(14) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON07  0x081C
register_bitfields![u32,
    CRU_GATE_CON07 [
        hclk_audio_root_en OFFSET(0) NUMBITS(1) [],
        pclk_audio_root_en OFFSET(1) NUMBITS(1) [],
        hclk_audio_biu_en OFFSET(2) NUMBITS(1) [],
        pclk_audio_biu_en OFFSET(3) NUMBITS(1) [],
        hclk_i2s0_8ch_en OFFSET(4) NUMBITS(1) [],
        clk_i2s0_8ch_tx_en OFFSET(5) NUMBITS(1) [],
        clk_i2s0_8ch_frac_tx_en OFFSET(6) NUMBITS(1) [],
        mclk_i2s0_8ch_tx_en OFFSET(7) NUMBITS(1) [],
        clk_i2s0_8ch_rx_en OFFSET(8) NUMBITS(1) [],
        clk_i2s0_8ch_frac_rx_en OFFSET(9) NUMBITS(1) [],
        mclk_i2s0_8ch_rx_en OFFSET(10) NUMBITS(1) [],
        pclk_acdcdig_en OFFSET(11) NUMBITS(1) [],
        hclk_i2s2_2ch_en OFFSET(12) NUMBITS(1) [],
        hclk_i2s3_2ch_en OFFSET(13) NUMBITS(1) [],
        clk_i2s2_2ch_en OFFSET(14) NUMBITS(1) [],
        clk_i2s2_2ch_frac_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON08  0x0820
register_bitfields![u32,
    CRU_GATE_CON08 [
        mclk_i2s2_2ch_en OFFSET(0) NUMBITS(1) [],
        clk_i2s3_2ch_en OFFSET(1) NUMBITS(1) [],
        clk_i2s3_2ch_frac_en OFFSET(2) NUMBITS(1) [],
        mclk_i2s3_2ch_en OFFSET(3) NUMBITS(1) [],
        clk_dac_acdcdig_en OFFSET(4) NUMBITS(1) [],
        _reserved OFFSET(5) NUMBITS(9) [],
        hclk_spdif0_en OFFSET(14) NUMBITS(1) [],
        clk_spdif0_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON09  0x0824
register_bitfields![u32,
    CRU_GATE_CON09 [
        clk_spdif0_frac_en OFFSET(0) NUMBITS(1) [],
        mclk_spdif0_en OFFSET(1) NUMBITS(1) [],
        hclk_spdif1_en OFFSET(2) NUMBITS(1) [],
        clk_spdif1_en OFFSET(3) NUMBITS(1) [],
        clk_spdif1_frac_en OFFSET(4) NUMBITS(1) [],
        mclk_spdif1_en OFFSET(5) NUMBITS(1) [],
        hclk_pdm1_en OFFSET(6) NUMBITS(1) [],
        mclk_pdm1_en OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON10  0x0828
register_bitfields![u32,
    CRU_GATE_CON10 [
        aclk_bus_root_en OFFSET(0) NUMBITS(1) [],
        aclk_bus_biu_en OFFSET(1) NUMBITS(1) [],
        pclk_bus_biu_en OFFSET(2) NUMBITS(1) [],
        aclk_gic_en OFFSET(3) NUMBITS(1) [],
        _reserved OFFSET(4) NUMBITS(1) [],
        aclk_dmac0_en OFFSET(5) NUMBITS(1) [],
        aclk_dmac1_en OFFSET(6) NUMBITS(1) [],
        aclk_dmac2_en OFFSET(7) NUMBITS(1) [],
        pclk_i2c1_en OFFSET(8) NUMBITS(1) [],
        pclk_i2c2_en OFFSET(9) NUMBITS(1) [],
        pclk_i2c3_en OFFSET(10) NUMBITS(1) [],
        pclk_i2c4_en OFFSET(11) NUMBITS(1) [],
        pclk_i2c5_en OFFSET(12) NUMBITS(1) [],
        pclk_i2c6_en OFFSET(13) NUMBITS(1) [],
        pclk_i2c7_en OFFSET(14) NUMBITS(1) [],
        pclk_i2c8_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON11  0x082C
register_bitfields![u32,
    CRU_GATE_CON11 [
        clk_i2c1_en OFFSET(0) NUMBITS(1) [],
        clk_i2c2_en OFFSET(1) NUMBITS(1) [],
        clk_i2c3_en OFFSET(2) NUMBITS(1) [],
        clk_i2c4_en OFFSET(3) NUMBITS(1) [],
        clk_i2c5_en OFFSET(4) NUMBITS(1) [],
        clk_i2c6_en OFFSET(5) NUMBITS(1) [],
        clk_i2c7_en OFFSET(6) NUMBITS(1) [],
        clk_i2c8_en OFFSET(7) NUMBITS(1) [],
        pclk_can0_en OFFSET(8) NUMBITS(1) [],
        clk_can0_en OFFSET(9) NUMBITS(1) [],
        pclk_can1_en OFFSET(10) NUMBITS(1) [],
        clk_can1_en OFFSET(11) NUMBITS(1) [],
        pclk_can2_en OFFSET(12) NUMBITS(1) [],
        clk_can2_en OFFSET(13) NUMBITS(1) [],
        pclk_saradc_en OFFSET(14) NUMBITS(1) [],
        clk_saradc_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON12  0x0830
register_bitfields![u32,
    CRU_GATE_CON12 [
        pclk_tsadc_en OFFSET(0) NUMBITS(1) [],
        clk_tsadc_en OFFSET(1) NUMBITS(1) [],
        pclk_uart1_en OFFSET(2) NUMBITS(1) [],
        pclk_uart2_en OFFSET(3) NUMBITS(1) [],
        pclk_uart3_en OFFSET(4) NUMBITS(1) [],
        pclk_uart4_en OFFSET(5) NUMBITS(1) [],
        pclk_uart5_en OFFSET(6) NUMBITS(1) [],
        pclk_uart6_en OFFSET(7) NUMBITS(1) [],
        pclk_uart7_en OFFSET(8) NUMBITS(1) [],
        pclk_uart8_en OFFSET(9) NUMBITS(1) [],
        pclk_uart9_en OFFSET(10) NUMBITS(1) [],
        clk_uart1_en OFFSET(11) NUMBITS(1) [],
        clk_uart1_frac_en OFFSET(12) NUMBITS(1) [],
        sclk_uart1_en OFFSET(13) NUMBITS(1) [],
        clk_uart2_en OFFSET(14) NUMBITS(1) [],
        clk_uart2_frac_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON13  0x0834
register_bitfields![u32,
    CRU_GATE_CON13 [
        sclk_uart2_en OFFSET(0) NUMBITS(1) [],
        clk_uart3_en OFFSET(1) NUMBITS(1) [],
        clk_uart3_frac_en OFFSET(2) NUMBITS(1) [],
        sclk_uart3_en OFFSET(3) NUMBITS(1) [],
        clk_uart4_en OFFSET(4) NUMBITS(1) [],
        clk_uart4_frac_en OFFSET(5) NUMBITS(1) [],
        sclk_uart4_en OFFSET(6) NUMBITS(1) [],
        clk_uart5_en OFFSET(7) NUMBITS(1) [],
        clk_uart5_frac_en OFFSET(8) NUMBITS(1) [],
        sclk_uart5_en OFFSET(9) NUMBITS(1) [],
        clk_uart6_en OFFSET(10) NUMBITS(1) [],
        clk_uart6_frac_en OFFSET(11) NUMBITS(1) [],
        sclk_uart6_en OFFSET(12) NUMBITS(1) [],
        clk_uart7_en OFFSET(13) NUMBITS(1) [],
        clk_uart7_frac_en OFFSET(14) NUMBITS(1) [],
        sclk_uart7_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON14  0x0838
register_bitfields![u32,
    CRU_GATE_CON14 [
        clk_uart8_en OFFSET(0) NUMBITS(1) [],
        clk_uart8_frac_en OFFSET(1) NUMBITS(1) [],
        sclk_uart8_en OFFSET(2) NUMBITS(1) [],
        clk_uart9_en OFFSET(3) NUMBITS(1) [],
        clk_uart9_frac_en OFFSET(4) NUMBITS(1) [],
        sclk_uart9_en OFFSET(5) NUMBITS(1) [],
        pclk_spi0_en OFFSET(6) NUMBITS(1) [],
        pclk_spi1_en OFFSET(7) NUMBITS(1) [],
        pclk_spi2_en OFFSET(8) NUMBITS(1) [],
        pclk_spi3_en OFFSET(9) NUMBITS(1) [],
        pclk_spi4_en OFFSET(10) NUMBITS(1) [],
        clk_spi0_en OFFSET(11) NUMBITS(1) [],
        clk_spi1_en OFFSET(12) NUMBITS(1) [],
        clk_spi2_en OFFSET(13) NUMBITS(1) [],
        clk_spi3_en OFFSET(14) NUMBITS(1) [],
        clk_spi4_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON15  0x083C
register_bitfields![u32,
    CRU_GATE_CON15 [
        pclk_wdt0_en OFFSET(0) NUMBITS(1) [],
        tclk_wdt0_en OFFSET(1) NUMBITS(1) [],
        pclk_sys_grf_en OFFSET(2) NUMBITS(1) [],
        pclk_pwm1_en OFFSET(3) NUMBITS(1) [],
        clk_pwm1_en OFFSET(4) NUMBITS(1) [],
        clk_pwm1_capture_en OFFSET(5) NUMBITS(1) [],
        pclk_pwm2_en OFFSET(6) NUMBITS(1) [],
        clk_pwm2_en OFFSET(7) NUMBITS(1) [],
        clk_pwm2_capture_en OFFSET(8) NUMBITS(1) [],
        pclk_pwm3_en OFFSET(9) NUMBITS(1) [],
        clk_pwm3_en OFFSET(10) NUMBITS(1) [],
        clk_pwm3_capture_en OFFSET(11) NUMBITS(1) [],
        pclk_bustimer0_en OFFSET(12) NUMBITS(1) [],
        pclk_bustimer1_en OFFSET(13) NUMBITS(1) [],
        clk_bustimer_root_en OFFSET(14) NUMBITS(1) [],
        clk_bustimer0_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON16  0x0840
register_bitfields![u32,
    CRU_GATE_CON16 [
        clk_bustimer1_en OFFSET(0) NUMBITS(1) [],
        clk_bustimer2_en OFFSET(1) NUMBITS(1) [],
        clk_bustimer3_en OFFSET(2) NUMBITS(1) [],
        clk_bustimer4_en OFFSET(3) NUMBITS(1) [],
        clk_bustimer5_en OFFSET(4) NUMBITS(1) [],
        clk_bustimer6_en OFFSET(5) NUMBITS(1) [],
        clk_bustimer7_en OFFSET(6) NUMBITS(1) [],
        clk_bustimer8_en OFFSET(7) NUMBITS(1) [],
        clk_bustimer9_en OFFSET(8) NUMBITS(1) [],
        clk_bustimer10_en OFFSET(9) NUMBITS(1) [],
        clk_bustimer11_en OFFSET(10) NUMBITS(1) [],
        pclk_mailbox0_en OFFSET(11) NUMBITS(1) [],
        pclk_mailbox1_en OFFSET(12) NUMBITS(1) [],
        pclk_mailbox2_en OFFSET(13) NUMBITS(1) [],
        pclk_gpio1_en OFFSET(14) NUMBITS(1) [],
        dbclk_gpio1_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON17  0x0844
register_bitfields![u32,
    CRU_GATE_CON17 [
        pclk_gpio2_en OFFSET(0) NUMBITS(1) [],
        dbclk_gpio2_en OFFSET(1) NUMBITS(1) [],
        pclk_gpio3_en OFFSET(2) NUMBITS(1) [],
        dbclk_gpio3_en OFFSET(3) NUMBITS(1) [],
        pclk_gpio4_en OFFSET(4) NUMBITS(1) [],
        dbclk_gpio4_en OFFSET(5) NUMBITS(1) [],
        aclk_decom_en OFFSET(6) NUMBITS(1) [],
        pclk_decom_en OFFSET(7) NUMBITS(1) [],
        dclk_decom_en OFFSET(8) NUMBITS(1) [],
        pclk_top_en OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(1) [],
        aclk_gicadb_gic2core_bus_en OFFSET(11) NUMBITS(1) [],
        pclk_dft2apb_en OFFSET(12) NUMBITS(1) [],
        pclk_apb2asb_mst_top_en OFFSET(13) NUMBITS(1) [],
        pclk_apb2asb_mst_cdphy_en OFFSET(14) NUMBITS(1) [],
        pclk_apb2asb_mst_bot_right_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON18  0x0848
register_bitfields![u32,
    CRU_GATE_CON18 [
        pclk_apb2asb_mst_ioc_top_en OFFSET(0) NUMBITS(1) [],
        pclk_apb2asb_mst_ioc_right_en OFFSET(1) NUMBITS(1) [],
        pclk_apb2asb_mst_csiphy_en OFFSET(2) NUMBITS(1) [],
        pclk_apb2asb_mst_vccio3_5_en OFFSET(3) NUMBITS(1) [],
        pclk_apb2asb_mst_vccio6_en OFFSET(4) NUMBITS(1) [],
        pclk_apb2asb_mst_emmcio_en OFFSET(5) NUMBITS(1) [],
        aclk_spinlock_en OFFSET(6) NUMBITS(1) [],
        _reserved0 OFFSET(7) NUMBITS(2) [],
        pclk_otpc_ns_en OFFSET(9) NUMBITS(1) [],
        clk_otpc_ns_en OFFSET(10) NUMBITS(1) [],
        clk_otpc_arb_en OFFSET(11) NUMBITS(1) [],
        clk_otpc_auto_rd_en OFFSET(12) NUMBITS(1) [],
        clk_otp_phy_en OFFSET(13) NUMBITS(1) [],
        _reserved1 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON19  0x084C
register_bitfields![u32,
    CRU_GATE_CON19 [
        pclk_busioc_en OFFSET(0) NUMBITS(1) [],
        clk_bisrintf_pllsrc_en OFFSET(1) NUMBITS(1) [],
        clk_bisrintf_en OFFSET(2) NUMBITS(1) [],
        pclk_pmu2_en OFFSET(3) NUMBITS(1) [],
        pclk_pmucm0_intmux_en OFFSET(4) NUMBITS(1) [],
        pclk_ddrcm0_intmux_en OFFSET(5) NUMBITS(1) [],
        _reserved OFFSET(6) NUMBITS(10) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON20  0x0850
register_bitfields![u32,
    CRU_GATE_CON20 [
        pclk_ddr_dfictl_ch0_en OFFSET(0) NUMBITS(1) [],
        pclk_ddr_mon_ch0_en OFFSET(1) NUMBITS(1) [],
        pclk_ddr_standby_ch0_en OFFSET(2) NUMBITS(1) [],
        pclk_ddr_upctl_ch0_en OFFSET(3) NUMBITS(1) [],
        tmclk_ddr_mon_ch0_en OFFSET(4) NUMBITS(1) [],
        pclk_ddr_grf_ch01_en OFFSET(5) NUMBITS(1) [],
        clk_dfi_ch0_en OFFSET(6) NUMBITS(1) [],
        clk_sbr_ch0_en OFFSET(7) NUMBITS(1) [],
        clk_ddr_upctl_ch0_en OFFSET(8) NUMBITS(1) [],
        clk_ddr_dfictl_ch0_en OFFSET(9) NUMBITS(1) [],
        clk_ddr_mon_ch0_en OFFSET(10) NUMBITS(1) [],
        clk_ddr_standby_ch0_en OFFSET(11) NUMBITS(1) [],
        aclk_ddr_upctl_ch0_en OFFSET(12) NUMBITS(1) [],
        pclk_ddr_dfictl_ch1_en OFFSET(13) NUMBITS(1) [],
        pclk_ddr_mon_ch1_en OFFSET(14) NUMBITS(1) [],
        pclk_ddr_standby_ch1_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON21  0x0854
register_bitfields![u32,
    CRU_GATE_CON21 [
        pclk_ddr_upctl_ch1_en OFFSET(0) NUMBITS(1) [],
        tmclk_ddr_mon_ch1_en OFFSET(1) NUMBITS(1) [],
        clk_dfi_ch1_en OFFSET(2) NUMBITS(1) [],
        clk_sbr_ch1_en OFFSET(3) NUMBITS(1) [],
        clk_ddr_upctl_ch1_en OFFSET(4) NUMBITS(1) [],
        clk_ddr_dfictl_ch1_en OFFSET(5) NUMBITS(1) [],
        clk_ddr_mon_ch1_en OFFSET(6) NUMBITS(1) [],
        clk_ddr_standby_ch1_en OFFSET(7) NUMBITS(1) [],
        aclk_ddr_upctl_ch1_en OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(4) [],
        aclk_ddr_ddrsch0_en OFFSET(13) NUMBITS(1) [],
        aclk_ddr_rs_ddrsch0_en OFFSET(14) NUMBITS(1) [],
        aclk_ddr_frs_ddrsch0_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON22  0x0858
register_bitfields![u32,
    CRU_GATE_CON22 [
        aclk_ddr_scramble0_en OFFSET(0) NUMBITS(1) [],
        aclk_ddr_frs_scramble0_en OFFSET(1) NUMBITS(1) [],
        aclk_ddr_ddrsch1_en OFFSET(2) NUMBITS(1) [],
        aclk_ddr_rs_ddrsch1_en OFFSET(3) NUMBITS(1) [],
        aclk_ddr_frs_ddrsch1_en OFFSET(4) NUMBITS(1) [],
        aclk_ddr_scramble1_en OFFSET(5) NUMBITS(1) [],
        aclk_ddr_frs_scramble1_en OFFSET(6) NUMBITS(1) [],
        pclk_ddr_ddrsch0_en OFFSET(7) NUMBITS(1) [],
        pclk_ddr_ddrsch1_en OFFSET(8) NUMBITS(1) [],
        clk_testout_ddr01_en OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON23  0x085C
register_bitfields![u32,
    CRU_GATE_CON23 [
        pclk_ddr_dfictl_ch2_en OFFSET(0) NUMBITS(1) [],
        pclk_ddr_mon_ch2_en OFFSET(1) NUMBITS(1) [],
        pclk_ddr_standby_ch2_en OFFSET(2) NUMBITS(1) [],
        pclk_ddr_upctl_ch2_en OFFSET(3) NUMBITS(1) [],
        tmclk_ddr_mon_ch2_en OFFSET(4) NUMBITS(1) [],
        pclk_ddr_grf_ch23_en OFFSET(5) NUMBITS(1) [],
        clk_dfi_ch2_en OFFSET(6) NUMBITS(1) [],
        clk_sbr_ch2_en OFFSET(7) NUMBITS(1) [],
        clk_ddr_upctl_ch2_en OFFSET(8) NUMBITS(1) [],
        clk_ddr_dfictl_ch2_en OFFSET(9) NUMBITS(1) [],
        clk_ddr_mon_ch2_en OFFSET(10) NUMBITS(1) [],
        clk_ddr_standby_ch2_en OFFSET(11) NUMBITS(1) [],
        aclk_ddr_upctl_ch2_en OFFSET(12) NUMBITS(1) [],
        pclk_ddr_dfictl_ch3_en OFFSET(13) NUMBITS(1) [],
        pclk_ddr_mon_ch3_en OFFSET(14) NUMBITS(1) [],
        pclk_ddr_standby_ch3_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON24  0x0860
register_bitfields![u32,
    CRU_GATE_CON24 [
        pclk_ddr_upctl_ch3_en OFFSET(0) NUMBITS(1) [],
        tmclk_ddr_mon_ch3_en OFFSET(1) NUMBITS(1) [],
        clk_dfi_ch3_en OFFSET(2) NUMBITS(1) [],
        clk_sbr_ch3_en OFFSET(3) NUMBITS(1) [],
        clk_ddr_upctl_ch3_en OFFSET(4) NUMBITS(1) [],
        clk_ddr_dfictl_ch3_en OFFSET(5) NUMBITS(1) [],
        clk_ddr_mon_ch3_en OFFSET(6) NUMBITS(1) [],
        clk_ddr_standby_ch3_en OFFSET(7) NUMBITS(1) [],
        aclk_ddr_upctl_ch3_en OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(4) [],
        aclk_ddr_ddrsch2_en OFFSET(13) NUMBITS(1) [],
        aclk_ddr_rs_ddrsch2_en OFFSET(14) NUMBITS(1) [],
        aclk_ddr_frs_ddrsch2_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON25  0x0864
register_bitfields![u32,
    CRU_GATE_CON25 [
        aclk_ddr_scramble2_en OFFSET(0) NUMBITS(1) [],
        aclk_ddr_frs_scramble2_en OFFSET(1) NUMBITS(1) [],
        aclk_ddr_ddrsch3_en OFFSET(2) NUMBITS(1) [],
        aclk_ddr_rs_ddrsch3_en OFFSET(3) NUMBITS(1) [],
        aclk_ddr_frs_ddrsch3_en OFFSET(4) NUMBITS(1) [],
        aclk_ddr_scramble3_en OFFSET(5) NUMBITS(1) [],
        aclk_ddr_frs_scramble3_en OFFSET(6) NUMBITS(1) [],
        pclk_ddr_ddrsch2_en OFFSET(7) NUMBITS(1) [],
        pclk_ddr_ddrsch3_en OFFSET(8) NUMBITS(1) [],
        clk_testout_ddr23_en OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON26  0x0868
register_bitfields![u32,
    CRU_GATE_CON26 [
        aclk_isp1_root_en OFFSET(0) NUMBITS(1) [],
        hclk_isp1_root_en OFFSET(1) NUMBITS(1) [],
        clk_isp1_core_en OFFSET(2) NUMBITS(1) [],
        clk_isp1_core_marvin_en OFFSET(3) NUMBITS(1) [],
        clk_isp1_core_vicap_en OFFSET(4) NUMBITS(1) [],
        aclk_isp1_en OFFSET(5) NUMBITS(1) [],
        aclk_isp1_biu_en OFFSET(6) NUMBITS(1) [],
        hclk_isp1_en OFFSET(7) NUMBITS(1) [],
        hclk_isp1_biu_en OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON27  0x086C
register_bitfields![u32,
    CRU_GATE_CON27 [
        aclk_rknn1_en OFFSET(0) NUMBITS(1) [],
        aclk_rknn1_biu_en OFFSET(1) NUMBITS(1) [],
        hclk_rknn1_en OFFSET(2) NUMBITS(1) [],
        hclk_rknn1_biu_en OFFSET(3) NUMBITS(1) [],
        _reserved OFFSET(4) NUMBITS(12) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON28  0x0870
register_bitfields![u32,
    CRU_GATE_CON28 [
        aclk_rknn2_en OFFSET(0) NUMBITS(1) [],
        aclk_rknn2_biu_en OFFSET(1) NUMBITS(1) [],
        hclk_rknn2_en OFFSET(2) NUMBITS(1) [],
        hclk_rknn2_biu_en OFFSET(3) NUMBITS(1) [],
        _reserved OFFSET(4) NUMBITS(12) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON29  0x0874
register_bitfields![u32,
    CRU_GATE_CON29 [
        hclk_rknn_root_en OFFSET(0) NUMBITS(1) [],
        clk_rknn_dsu0_df_en OFFSET(1) NUMBITS(1) [],
        clk_testout_npu_en OFFSET(2) NUMBITS(1) [],
        clk_rknn_dsu0_en OFFSET(3) NUMBITS(1) [],
        pclk_nputop_root_en OFFSET(4) NUMBITS(1) [],
        pclk_nputop_biu_en OFFSET(5) NUMBITS(1) [],
        pclk_npu_timer_en OFFSET(6) NUMBITS(1) [],
        clk_nputimer_root_en OFFSET(7) NUMBITS(1) [],
        clk_nputimer0_en OFFSET(8) NUMBITS(1) [],
        clk_nputimer1_en OFFSET(9) NUMBITS(1) [],
        pclk_npu_wdt_en OFFSET(10) NUMBITS(1) [],
        tclk_npu_wdt_en OFFSET(11) NUMBITS(1) [],
        pclk_pvtm1_en OFFSET(12) NUMBITS(1) [],
        pclk_npu_grf_en OFFSET(13) NUMBITS(1) [],
        clk_pvtm1_en OFFSET(14) NUMBITS(1) [],
        clk_npu_pvtm_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON30  0x0878
register_bitfields![u32,
    CRU_GATE_CON30 [
        clk_npu_pvtpll_en OFFSET(0) NUMBITS(1) [],
        hclk_npu_cm0_root_en OFFSET(1) NUMBITS(1) [],
        hclk_npu_cm0_biu_en OFFSET(2) NUMBITS(1) [],
        fclk_npu_cm0_core_en OFFSET(3) NUMBITS(1) [],
        _reserved0 OFFSET(4) NUMBITS(1) [],
        clk_npu_cm0_rtc_en OFFSET(5) NUMBITS(1) [],
        aclk_rknn0_en OFFSET(6) NUMBITS(1) [],
        aclk_rknn0_biu_en OFFSET(7) NUMBITS(1) [],
        hclk_rknn0_en OFFSET(8) NUMBITS(1) [],
        hclk_rknn0_biu_en OFFSET(9) NUMBITS(1) [],
        _reserved1 OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON31  0x087C
register_bitfields![u32,
    CRU_GATE_CON31 [
        hclk_nvm_root_en OFFSET(0) NUMBITS(1) [],
        aclk_nvm_root_en OFFSET(1) NUMBITS(1) [],
        hclk_nvm_biu_en OFFSET(2) NUMBITS(1) [],
        aclk_nvm_biu_en OFFSET(3) NUMBITS(1) [],
        hclk_emmc_en OFFSET(4) NUMBITS(1) [],
        aclk_emmc_en OFFSET(5) NUMBITS(1) [],
        cclk_emmc_en OFFSET(6) NUMBITS(1) [],
        bclk_emmc_en OFFSET(7) NUMBITS(1) [],
        tmclk_emmc_en OFFSET(8) NUMBITS(1) [],
        sclk_sfc_en OFFSET(9) NUMBITS(1) [],
        hclk_sfc_en OFFSET(10) NUMBITS(1) [],
        hclk_sfc_xip_en OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON32  0x0880
register_bitfields![u32,
    CRU_GATE_CON32 [
        pclk_php_root_en OFFSET(0) NUMBITS(1) [],
        pclk_grf_en OFFSET(1) NUMBITS(1) [],
        pclk_dec_biu_en OFFSET(2) NUMBITS(1) [],
        pclk_gmac0_en OFFSET(3) NUMBITS(1) [],
        pclk_gmac1_en OFFSET(4) NUMBITS(1) [],
        pclk_php_biu_en OFFSET(5) NUMBITS(1) [],
        aclk_pcie_root_en OFFSET(6) NUMBITS(1) [],
        aclk_php_root_en OFFSET(7) NUMBITS(1) [],
        aclk_pcie_bridge_en OFFSET(8) NUMBITS(1) [],
        aclk_php_biu_en OFFSET(9) NUMBITS(1) [],
        aclk_gmac0_en OFFSET(10) NUMBITS(1) [],
        aclk_gmac1_en OFFSET(11) NUMBITS(1) [],
        aclk_pcie_biu_en OFFSET(12) NUMBITS(1) [],
        aclk_pcie_4l_dbi_en OFFSET(13) NUMBITS(1) [],
        aclk_pcie_2l_dbi_en OFFSET(14) NUMBITS(1) [],
        aclk_pcie_1l0_dbi_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON33  0x0884
register_bitfields![u32,
    CRU_GATE_CON33 [
        aclk_pcie_1l1_dbi_en OFFSET(0) NUMBITS(1) [],
        aclk_pcie_1l2_dbi_en OFFSET(1) NUMBITS(1) [],
        aclk_pcie_4l_mstr_en OFFSET(2) NUMBITS(1) [],
        aclk_pcie_2l_mstr_en OFFSET(3) NUMBITS(1) [],
        aclk_pcie_1l0_mstr_en OFFSET(4) NUMBITS(1) [],
        aclk_pcie_1l1_mstr_en OFFSET(5) NUMBITS(1) [],
        aclk_pcie_1l2_mstr_en OFFSET(6) NUMBITS(1) [],
        aclk_pcie_4l_slv_en OFFSET(7) NUMBITS(1) [],
        aclk_pcie_2l_slv_en OFFSET(8) NUMBITS(1) [],
        aclk_pcie_1l0_slv_en OFFSET(9) NUMBITS(1) [],
        aclk_pcie_1l1_slv_en OFFSET(10) NUMBITS(1) [],
        aclk_pcie_1l2_slv_en OFFSET(11) NUMBITS(1) [],
        pclk_pcie_4l_en OFFSET(12) NUMBITS(1) [],
        pclk_pcie_2l_en OFFSET(13) NUMBITS(1) [],
        pclk_pcie_1l0_en OFFSET(14) NUMBITS(1) [],
        pclk_pcie_1l1_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON34  0x0888
register_bitfields![u32,
    CRU_GATE_CON34 [
        pclk_pcie_1l2_en OFFSET(0) NUMBITS(1) [],
        clk_pcie_4l_aux_en OFFSET(1) NUMBITS(1) [],
        clk_pcie_2l_aux_en OFFSET(2) NUMBITS(1) [],
        clk_pcie_1l0_aux_en OFFSET(3) NUMBITS(1) [],
        clk_pcie_1l1_aux_en OFFSET(4) NUMBITS(1) [],
        clk_pcie_1l2_aux_en OFFSET(5) NUMBITS(1) [],
        aclk_php_gic_its_en OFFSET(6) NUMBITS(1) [],
        aclk_mmu_pcie_en OFFSET(7) NUMBITS(1) [],
        aclk_mmu_php_en OFFSET(8) NUMBITS(1) [],
        aclk_mmu_biu_en OFFSET(9) NUMBITS(1) [],
        clk_gmac0_ptp_ref_en OFFSET(10) NUMBITS(1) [],
        clk_gmac1_ptp_ref_en OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON35  0x088C
register_bitfields![u32,
    CRU_GATE_CON35 [
        _reserved0 OFFSET(0) NUMBITS(5) [],
        clk_gmac_125m_cru_en OFFSET(5) NUMBITS(1) [],
        clk_gmac_50m_cru_en OFFSET(6) NUMBITS(1) [],
        aclk_usb3otg2_en OFFSET(7) NUMBITS(1) [],
        suspend_clk_usb3otg2_en OFFSET(8) NUMBITS(1) [],
        ref_clk_usb3otg2_en OFFSET(9) NUMBITS(1) [],
        clk_utmi_otg2_en OFFSET(10) NUMBITS(1) [],
        _reserved1 OFFSET(11) NUMBITS(5) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON37  0x0894
register_bitfields![u32,
    CRU_GATE_CON37 [
        clk_pipephy0_ref_en OFFSET(0) NUMBITS(1) [],
        clk_pipephy1_ref_en OFFSET(1) NUMBITS(1) [],
        clk_pipephy2_ref_en OFFSET(2) NUMBITS(1) [],
        _reserved0 OFFSET(3) NUMBITS(1) [],
        clk_pmalive0_en OFFSET(4) NUMBITS(1) [],
        clk_pmalive1_en OFFSET(5) NUMBITS(1) [],
        clk_pmalive2_en OFFSET(6) NUMBITS(1) [],
        aclk_sata0_en OFFSET(7) NUMBITS(1) [],
        aclk_sata1_en OFFSET(8) NUMBITS(1) [],
        aclk_sata2_en OFFSET(9) NUMBITS(1) [],
        clk_rxoob0_en OFFSET(10) NUMBITS(1) [],
        clk_rxoob1_en OFFSET(11) NUMBITS(1) [],
        clk_rxoob2_en OFFSET(12) NUMBITS(1) [],
        _reserved1 OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON38  0x0898
register_bitfields![u32,
    CRU_GATE_CON38 [
        _reserved0 OFFSET(0) NUMBITS(3) [],
        clk_pipephy0_pipe_g_en OFFSET(3) NUMBITS(1) [],
        clk_pipephy1_pipe_g_en OFFSET(4) NUMBITS(1) [],
        clk_pipephy2_pipe_g_en OFFSET(5) NUMBITS(1) [],
        clk_pipephy0_pipe_asic_g_en OFFSET(6) NUMBITS(1) [],
        clk_pipephy1_pipe_asic_g_en OFFSET(7) NUMBITS(1) [],
        clk_pipephy2_pipe_asic_g_en OFFSET(8) NUMBITS(1) [],
        clk_pipephy2_pipe_u3_g_en OFFSET(9) NUMBITS(1) [],
        _reserved1 OFFSET(10) NUMBITS(3) [],
        clk_pcie_1l2_pipe_en OFFSET(13) NUMBITS(1) [],
        clk_pcie_1l0_pipe_en OFFSET(14) NUMBITS(1) [],
        clk_pcie_1l1_pipe_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON39  0x089C
register_bitfields![u32,
    CRU_GATE_CON39 [
        clk_pcie_4l_pipe_en OFFSET(0) NUMBITS(1) [],
        clk_pcie_2l_pipe_en OFFSET(1) NUMBITS(1) [],
        _reserved OFFSET(2) NUMBITS(14) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON40  0x08A0
register_bitfields![u32,
    CRU_GATE_CON40 [
        hclk_rkvdec0_root_en OFFSET(0) NUMBITS(1) [],
        aclk_rkvdec0_root_en OFFSET(1) NUMBITS(1) [],
        aclk_rkvdec_ccu_en OFFSET(2) NUMBITS(1) [],
        hclk_rkvdec0_en OFFSET(3) NUMBITS(1) [],
        aclk_rkvdec0_en OFFSET(4) NUMBITS(1) [],
        hclk_rkvdec0_biu_en OFFSET(5) NUMBITS(1) [],
        aclk_rkvdec0_biu_en OFFSET(6) NUMBITS(1) [],
        clk_rkvdec0_ca_en OFFSET(7) NUMBITS(1) [],
        clk_rkvdec0_hevc_ca_en OFFSET(8) NUMBITS(1) [],
        clk_rkvdec0_core_en OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON41  0x08A4
register_bitfields![u32,
    CRU_GATE_CON41 [
        hclk_rkvdec1_root_en OFFSET(0) NUMBITS(1) [],
        aclk_rkvdec1_root_en OFFSET(1) NUMBITS(1) [],
        hclk_rkvdec1_en OFFSET(2) NUMBITS(1) [],
        aclk_rkvdec1_en OFFSET(3) NUMBITS(1) [],
        hclk_rkvdec1_biu_en OFFSET(4) NUMBITS(1) [],
        aclk_rkvdec1_biu_en OFFSET(5) NUMBITS(1) [],
        clk_rkvdec1_ca_en OFFSET(6) NUMBITS(1) [],
        clk_rkvdec1_hevc_ca_en OFFSET(7) NUMBITS(1) [],
        clk_rkvdec1_core_en OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON42  0x08A8
register_bitfields![u32,
    CRU_GATE_CON42 [
        aclk_usb_root_en OFFSET(0) NUMBITS(1) [],
        hclk_usb_root_en OFFSET(1) NUMBITS(1) [],
        aclk_usb_biu_en OFFSET(2) NUMBITS(1) [],
        hclk_usb_biu_en OFFSET(3) NUMBITS(1) [],
        aclk_usb3otg0_en OFFSET(4) NUMBITS(1) [],
        suspend_clk_usb3otg0_en OFFSET(5) NUMBITS(1) [],
        ref_clk_usb3otg0_en OFFSET(6) NUMBITS(1) [],
        aclk_usb3otg1_en OFFSET(7) NUMBITS(1) [],
        suspend_clk_usb3otg1_en OFFSET(8) NUMBITS(1) [],
        ref_clk_usb3otg1_en OFFSET(9) NUMBITS(1) [],
        hclk_host0_en OFFSET(10) NUMBITS(1) [],
        hclk_host_arb0_en OFFSET(11) NUMBITS(1) [],
        hclk_host1_en OFFSET(12) NUMBITS(1) [],
        hclk_host_arb1_en OFFSET(13) NUMBITS(1) [],
        aclk_usb_grf_en OFFSET(14) NUMBITS(1) [],
        utmi_ohci_clk48_host0_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON43  0x08AC
register_bitfields![u32,
    CRU_GATE_CON43 [
        utmi_ohci_clk48_host1_en OFFSET(0) NUMBITS(1) [],
        _reserved OFFSET(1) NUMBITS(15) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON44  0x08B0
register_bitfields![u32,
    CRU_GATE_CON44 [
        aclk_vdpu_root_en OFFSET(0) NUMBITS(1) [],
        aclk_vdpu_low_root_en OFFSET(1) NUMBITS(1) [],
        hclk_vdpu_root_en OFFSET(2) NUMBITS(1) [],
        aclk_jpeg_decoder_root_en OFFSET(3) NUMBITS(1) [],
        aclk_vdpu_biu_en OFFSET(4) NUMBITS(1) [],
        aclk_vdpu_low_biu_en OFFSET(5) NUMBITS(1) [],
        hclk_vdpu_biu_en OFFSET(6) NUMBITS(1) [],
        aclk_jpeg_decoder_biu_en OFFSET(7) NUMBITS(1) [],
        aclk_vpu_en OFFSET(8) NUMBITS(1) [],
        hclk_vpu_en OFFSET(9) NUMBITS(1) [],
        aclk_jpeg_encoder0_en OFFSET(10) NUMBITS(1) [],
        hclk_jpeg_encoder0_en OFFSET(11) NUMBITS(1) [],
        aclk_jpeg_encoder1_en OFFSET(12) NUMBITS(1) [],
        hclk_jpeg_encoder1_en OFFSET(13) NUMBITS(1) [],
        aclk_jpeg_encoder2_en OFFSET(14) NUMBITS(1) [],
        hclk_jpeg_encoder2_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON45  0x08B4
register_bitfields![u32,
    CRU_GATE_CON45 [
        aclk_jpeg_encoder3_en OFFSET(0) NUMBITS(1) [],
        hclk_jpeg_encoder3_en OFFSET(1) NUMBITS(1) [],
        aclk_jpeg_decoder_en OFFSET(2) NUMBITS(1) [],
        hclk_jpeg_decoder_en OFFSET(3) NUMBITS(1) [],
        hclk_iep2p0_en OFFSET(4) NUMBITS(1) [],
        aclk_iep2p0_en OFFSET(5) NUMBITS(1) [],
        clk_iep2p0_core_en OFFSET(6) NUMBITS(1) [],
        hclk_rga2_en OFFSET(7) NUMBITS(1) [],
        aclk_rga2_en OFFSET(8) NUMBITS(1) [],
        clk_rga2_core_en OFFSET(9) NUMBITS(1) [],
        hclk_rga3_0_en OFFSET(10) NUMBITS(1) [],
        aclk_rga3_0_en OFFSET(11) NUMBITS(1) [],
        clk_rga3_0_core_en OFFSET(12) NUMBITS(1) [],
        _reserved OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON47  0x08BC
register_bitfields![u32,
    CRU_GATE_CON47 [
        hclk_rkvenc0_root_en OFFSET(0) NUMBITS(1) [],
        aclk_rkvenc0_root_en OFFSET(1) NUMBITS(1) [],
        hclk_rkvenc0_biu_en OFFSET(2) NUMBITS(1) [],
        aclk_rkvenc0_biu_en OFFSET(3) NUMBITS(1) [],
        hclk_rkvenc0_en OFFSET(4) NUMBITS(1) [],
        aclk_rkvenc0_en OFFSET(5) NUMBITS(1) [],
        clk_rkvenc0_core_en OFFSET(6) NUMBITS(1) [],
        _reserved OFFSET(7) NUMBITS(9) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON48  0x08C0
register_bitfields![u32,
    CRU_GATE_CON48 [
        hclk_rkvenc1_root_en OFFSET(0) NUMBITS(1) [],
        aclk_rkvenc1_root_en OFFSET(1) NUMBITS(1) [],
        hclk_rkvenc1_biu_en OFFSET(2) NUMBITS(1) [],
        aclk_rkvenc1_biu_en OFFSET(3) NUMBITS(1) [],
        hclk_rkvenc1_en OFFSET(4) NUMBITS(1) [],
        aclk_rkvenc1_en OFFSET(5) NUMBITS(1) [],
        clk_rkvenc1_core_en OFFSET(6) NUMBITS(1) [],
        _reserved OFFSET(7) NUMBITS(9) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON49  0x08C4
register_bitfields![u32,
    CRU_GATE_CON49 [
        aclk_vi_root_en OFFSET(0) NUMBITS(1) [],
        hclk_vi_root_en OFFSET(1) NUMBITS(1) [],
        pclk_vi_root_en OFFSET(2) NUMBITS(1) [],
        aclk_vi_biu_en OFFSET(3) NUMBITS(1) [],
        hclk_vi_biu_en OFFSET(4) NUMBITS(1) [],
        pclk_vi_biu_en OFFSET(5) NUMBITS(1) [],
        dclk_vicap_en OFFSET(6) NUMBITS(1) [],
        aclk_vicap_en OFFSET(7) NUMBITS(1) [],
        hclk_vicap_en OFFSET(8) NUMBITS(1) [],
        clk_isp0_core_en OFFSET(9) NUMBITS(1) [],
        clk_isp0_core_marvin_en OFFSET(10) NUMBITS(1) [],
        clk_isp0_core_vicap_en OFFSET(11) NUMBITS(1) [],
        aclk_isp0_en OFFSET(12) NUMBITS(1) [],
        hclk_isp0_en OFFSET(13) NUMBITS(1) [],
        aclk_fisheye0_en OFFSET(14) NUMBITS(1) [],
        hclk_fisheye0_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON50  0x08C8
register_bitfields![u32,
    CRU_GATE_CON50 [
        clk_fisheye0_core_en OFFSET(0) NUMBITS(1) [],
        aclk_fisheye1_en OFFSET(1) NUMBITS(1) [],
        hclk_fisheye1_en OFFSET(2) NUMBITS(1) [],
        clk_fisheye1_core_en OFFSET(3) NUMBITS(1) [],
        pclk_csi_host_0_en OFFSET(4) NUMBITS(1) [],
        pclk_csi_host_1_en OFFSET(5) NUMBITS(1) [],
        pclk_csi_host_2_en OFFSET(6) NUMBITS(1) [],
        pclk_csi_host_3_en OFFSET(7) NUMBITS(1) [],
        pclk_csi_host_4_en OFFSET(8) NUMBITS(1) [],
        pclk_csi_host_5_en OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON51  0x08CC
register_bitfields![u32,
    CRU_GATE_CON51 [
        _reserved0 OFFSET(0) NUMBITS(4) [],
        clk_csihost0_vicap_en OFFSET(4) NUMBITS(1) [],
        clk_csihost1_vicap_en OFFSET(5) NUMBITS(1) [],
        clk_csihost2_vicap_en OFFSET(6) NUMBITS(1) [],
        clk_csihost3_vicap_en OFFSET(7) NUMBITS(1) [],
        clk_csihost4_vicap_en OFFSET(8) NUMBITS(1) [],
        clk_csihost5_vicap_en OFFSET(9) NUMBITS(1) [],
        iclk_csihost01_en OFFSET(10) NUMBITS(1) [],
        iclk_csihost0_en OFFSET(11) NUMBITS(1) [],
        iclk_csihost1_en OFFSET(12) NUMBITS(1) [],
        _reserved1 OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON52  0x08D0
register_bitfields![u32,
    CRU_GATE_CON52 [
        aclk_vop_root_en OFFSET(0) NUMBITS(1) [],
        aclk_vop_low_root_en OFFSET(1) NUMBITS(1) [],
        hclk_vop_root_en OFFSET(2) NUMBITS(1) [],
        pclk_vop_root_en OFFSET(3) NUMBITS(1) [],
        aclk_vop_biu_en OFFSET(4) NUMBITS(1) [],
        aclk_vop_low_biu_en OFFSET(5) NUMBITS(1) [],
        hclk_vop_biu_en OFFSET(6) NUMBITS(1) [],
        pclk_vop_biu_en OFFSET(7) NUMBITS(1) [],
        hclk_vop_en OFFSET(8) NUMBITS(1) [],
        aclk_vop_en OFFSET(9) NUMBITS(1) [],
        dclk_vp0_src_en OFFSET(10) NUMBITS(1) [],
        dclk_vp1_src_en OFFSET(11) NUMBITS(1) [],
        dclk_vp2_src_en OFFSET(12) NUMBITS(1) [],
        dclk_vp0_en OFFSET(13) NUMBITS(1) [],
        _reserved OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON53  0x08D4
register_bitfields![u32,
    CRU_GATE_CON53 [
        dclk_vp1_en OFFSET(0) NUMBITS(1) [],
        dclk_vp2_en OFFSET(1) NUMBITS(1) [],
        dclk_vp3_en OFFSET(2) NUMBITS(1) [],
        pclk_vopgrf_en OFFSET(3) NUMBITS(1) [],
        pclk_dsihost0_en OFFSET(4) NUMBITS(1) [],
        pclk_dsihost1_en OFFSET(5) NUMBITS(1) [],
        clk_dsihost0_en OFFSET(6) NUMBITS(1) [],
        clk_dsihost1_en OFFSET(7) NUMBITS(1) [],
        clk_vop_pmu_en OFFSET(8) NUMBITS(1) [],
        pclk_vop_channel_biu_en OFFSET(9) NUMBITS(1) [],
        aclk_vop_doby_en OFFSET(10) NUMBITS(1) [],
        _reserved OFFSET(11) NUMBITS(5) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON55  0x08DC
register_bitfields![u32,
    CRU_GATE_CON55 [
        aclk_vo0_root_en OFFSET(0) NUMBITS(1) [],
        hclk_vo0_root_en OFFSET(1) NUMBITS(1) [],
        hclk_vo0_s_root_en OFFSET(2) NUMBITS(1) [],
        pclk_vo0_root_en OFFSET(3) NUMBITS(1) [],
        pclk_vo0_s_root_en OFFSET(4) NUMBITS(1) [],
        hclk_vo0_biu_en OFFSET(5) NUMBITS(1) [],
        hclk_vo0_s_biu_en OFFSET(6) NUMBITS(1) [],
        pclk_vo0_biu_en OFFSET(7) NUMBITS(1) [],
        pclk_vo0_s_biu_en OFFSET(8) NUMBITS(1) [],
        aclk_hdcp0_biu_en OFFSET(9) NUMBITS(1) [],
        pclk_vo0grf_en OFFSET(10) NUMBITS(1) [],
        hclk_hdcp_key0_en OFFSET(11) NUMBITS(1) [],
        aclk_hdcp0_en OFFSET(12) NUMBITS(1) [],
        hclk_hdcp0_en OFFSET(13) NUMBITS(1) [],
        pclk_hdcp0_en OFFSET(14) NUMBITS(1) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON56  0x08E0
register_bitfields![u32,
    CRU_GATE_CON56 [
        aclk_trng0_en OFFSET(0) NUMBITS(1) [],
        pclk_trng0_en OFFSET(1) NUMBITS(1) [],
        clk_aux16mhz_0_en OFFSET(2) NUMBITS(1) [],
        clk_aux16mhz_1_en OFFSET(3) NUMBITS(1) [],
        pclk_dp0_en OFFSET(4) NUMBITS(1) [],
        pclk_dp1_en OFFSET(5) NUMBITS(1) [],
        pclk_s_dp0_en OFFSET(6) NUMBITS(1) [],
        pclk_s_dp1_en OFFSET(7) NUMBITS(1) [],
        clk_dp0_en OFFSET(8) NUMBITS(1) [],
        clk_dp1_en OFFSET(9) NUMBITS(1) [],
        hclk_i2s4_8ch_en OFFSET(10) NUMBITS(1) [],
        clk_i2s4_8ch_tx_en OFFSET(11) NUMBITS(1) [],
        clk_i2s4_8ch_frac_tx_en OFFSET(12) NUMBITS(1) [],
        mclk_i2s4_8ch_tx_en OFFSET(13) NUMBITS(1) [],
        hclk_i2s8_8ch_en OFFSET(14) NUMBITS(1) [],
        clk_i2s8_8ch_tx_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON57  0x08E4
register_bitfields![u32,
    CRU_GATE_CON57 [
        clk_i2s8_8ch_frac_tx_en OFFSET(0) NUMBITS(1) [],
        mclk_i2s8_8ch_tx_en OFFSET(1) NUMBITS(1) [],
        hclk_spdif2_dp0_en OFFSET(2) NUMBITS(1) [],
        clk_spdif2_dp0_en OFFSET(3) NUMBITS(1) [],
        clk_spdif2_dp0_frac_en OFFSET(4) NUMBITS(1) [],
        mclk_spdif2_dp0_en OFFSET(5) NUMBITS(1) [],
        mclk_spdif2_en OFFSET(6) NUMBITS(1) [],
        hclk_spdif5_dp1_en OFFSET(7) NUMBITS(1) [],
        clk_spdif5_dp1_en OFFSET(8) NUMBITS(1) [],
        clk_spdif5_dp1_frac_en OFFSET(9) NUMBITS(1) [],
        mclk_spdif5_dp1_en OFFSET(10) NUMBITS(1) [],
        mclk_spdif5_en OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON59  0x08EC
register_bitfields![u32,
    CRU_GATE_CON59 [
        aclk_hdcp1_root_en OFFSET(0) NUMBITS(1) [],
        aclk_hdmirx_root_en OFFSET(1) NUMBITS(1) [],
        hclk_vo1_root_en OFFSET(2) NUMBITS(1) [],
        hclk_vo1_s_root_en OFFSET(3) NUMBITS(1) [],
        pclk_vo1_root_en OFFSET(4) NUMBITS(1) [],
        pclk_vo1_s_root_en OFFSET(5) NUMBITS(1) [],
        aclk_hdcp1_biu_en OFFSET(6) NUMBITS(1) [],
        _reserved OFFSET(7) NUMBITS(1) [],
        aclk_vo1_biu_en OFFSET(8) NUMBITS(1) [],
        hclk_vo1_biu_en OFFSET(9) NUMBITS(1) [],
        hclk_vo1_s_biu_en OFFSET(10) NUMBITS(1) [],
        pclk_vo1_biu_en OFFSET(11) NUMBITS(1) [],
        pclk_vo1grf_en OFFSET(12) NUMBITS(1) [],
        pclk_vo1_s_biu_en OFFSET(13) NUMBITS(1) [],
        pclk_s_edp0_en OFFSET(14) NUMBITS(1) [],
        pclk_s_edp1_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON60  0x08F0
register_bitfields![u32,
    CRU_GATE_CON60 [
        hclk_i2s7_8ch_en OFFSET(0) NUMBITS(1) [],
        clk_i2s7_8ch_rx_en OFFSET(1) NUMBITS(1) [],
        clk_i2s7_8ch_frac_rx_en OFFSET(2) NUMBITS(1) [],
        mclk_i2s7_8ch_rx_en OFFSET(3) NUMBITS(1) [],
        hclk_hdcp_key1_en OFFSET(4) NUMBITS(1) [],
        aclk_hdcp1_en OFFSET(5) NUMBITS(1) [],
        hclk_hdcp1_en OFFSET(6) NUMBITS(1) [],
        pclk_hdcp1_en OFFSET(7) NUMBITS(1) [],
        _reserved0 OFFSET(8) NUMBITS(1) [],
        aclk_trng1_en OFFSET(9) NUMBITS(1) [],
        pclk_trng1_en OFFSET(10) NUMBITS(1) [],
        pclk_hdmitx0_en OFFSET(11) NUMBITS(1) [],
        _reserved1 OFFSET(12) NUMBITS(3) [],
        clk_hdmitx0_earc_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON61  0x08F4
register_bitfields![u32,
    CRU_GATE_CON61 [
        clk_hdmitx0_ref_en OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(1) [],
        pclk_hdmitx1_en OFFSET(2) NUMBITS(1) [],
        _reserved1 OFFSET(3) NUMBITS(3) [],
        clk_hdmitx1_earc_en OFFSET(6) NUMBITS(1) [],
        clk_hdmitx1_ref_en OFFSET(6) NUMBITS(1) [],
        _reserved2 OFFSET(8) NUMBITS(1) [],
        aclk_hdmirx_en OFFSET(9) NUMBITS(1) [],
        pclk_hdmirx_en OFFSET(10) NUMBITS(1) [],
        clk_hdmirx_ref_en OFFSET(11) NUMBITS(1) [],
        clk_hdmirx_aud_src_en OFFSET(12) NUMBITS(1) [],
        clk_hdmirx_aud_frac_en OFFSET(13) NUMBITS(1) [],
        clk_hdmirx_aud_en OFFSET(14) NUMBITS(1) [],
        clk_hdmirx_tmdsqp_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON62  0x08F8
register_bitfields![u32,
    CRU_GATE_CON62 [
        pclk_edp0_en OFFSET(0) NUMBITS(1) [],
        clk_edp0_24m_en OFFSET(1) NUMBITS(1) [],
        clk_edp0_200m_en OFFSET(2) NUMBITS(1) [],
        pclk_edp1_en OFFSET(3) NUMBITS(1) [],
        clk_edp1_24m_en OFFSET(4) NUMBITS(1) [],
        clk_edp1_200m_en OFFSET(5) NUMBITS(1) [],
        clk_i2s5_8ch_tx_en OFFSET(6) NUMBITS(1) [],
        clk_i2s5_8ch_frac_tx_en OFFSET(7) NUMBITS(1) [],
        mclk_i2s5_8ch_tx_en OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(3) [],
        hclk_i2s5_8ch_en OFFSET(12) NUMBITS(1) [],
        clk_i2s6_8ch_tx_en OFFSET(13) NUMBITS(1) [],
        clk_i2s6_8ch_frac_tx_en OFFSET(14) NUMBITS(1) [],
        mclk_i2s6_8ch_tx_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON63  0x08FC
register_bitfields![u32,
    CRU_GATE_CON63 [
        clk_i2s6_8ch_rx_en OFFSET(0) NUMBITS(1) [],
        clk_i2s6_8ch_frac_rx_en OFFSET(1) NUMBITS(1) [],
        mclk_i2s6_8ch_rx_en OFFSET(2) NUMBITS(1) [],
        hclk_i2s6_8ch_en OFFSET(3) NUMBITS(1) [],
        hclk_spdif3_en OFFSET(4) NUMBITS(1) [],
        clk_spdif3_en OFFSET(5) NUMBITS(1) [],
        clk_spdif3_frac_en OFFSET(6) NUMBITS(1) [],
        mclk_spdif3_en OFFSET(7) NUMBITS(1) [],
        hclk_spdif4_en OFFSET(8) NUMBITS(1) [],
        clk_spdif4_en OFFSET(9) NUMBITS(1) [],
        clk_spdif4_frac_en OFFSET(10) NUMBITS(1) [],
        mclk_spdif4_en OFFSET(11) NUMBITS(1) [],
        hclk_spdifrx0_en OFFSET(12) NUMBITS(1) [],
        mclk_spdifrx0_en OFFSET(13) NUMBITS(1) [],
        hclk_spdifrx1_en OFFSET(14) NUMBITS(1) [],
        mclk_spdifrx1_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON64  0x0900
register_bitfields![u32,
    CRU_GATE_CON64 [
        hclk_spdifrx2_en OFFSET(0) NUMBITS(1) [],
        mclk_spdifrx2_en OFFSET(1) NUMBITS(1) [],
        _reserved OFFSET(2) NUMBITS(12) [],
        dclk_vp2hdmi_bridge0_vo1_en OFFSET(14) NUMBITS(1) [],
        dclk_vp2hdmi_bridge1_vo1_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON65  0x0904
register_bitfields![u32,
    CRU_GATE_CON65 [
        hclk_i2s9_8ch_en OFFSET(0) NUMBITS(1) [],
        clk_i2s9_8ch_rx_en OFFSET(1) NUMBITS(1) [],
        clk_i2s9_8ch_frac_rx_en OFFSET(2) NUMBITS(1) [],
        mclk_i2s9_8ch_rx_en OFFSET(3) NUMBITS(1) [],
        hclk_i2s10_8ch_en OFFSET(4) NUMBITS(1) [],
        clk_i2s10_8ch_rx_en OFFSET(5) NUMBITS(1) [],
        clk_i2s10_8ch_frac_rx_en OFFSET(6) NUMBITS(1) [],
        mclk_i2s10_8ch_rx_en OFFSET(7) NUMBITS(1) [],
        pclk_s_hdmirx_en OFFSET(8) NUMBITS(1) [],
        clk_hdmitrx_refsrc_en OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON66  0x0908
register_bitfields![u32,
    CRU_GATE_CON66 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        clk_gpu_src_df_en OFFSET(1) NUMBITS(1) [],
        clk_testout_gpu_en OFFSET(2) NUMBITS(1) [],
        clk_gpu_src_en OFFSET(3) NUMBITS(1) [],
        clk_gpu_en OFFSET(4) NUMBITS(1) [],
        _reserved1 OFFSET(5) NUMBITS(1) [],
        clk_gpu_coregroup_en OFFSET(6) NUMBITS(1) [],
        clk_gpu_stacks_en OFFSET(7) NUMBITS(1) [],
        aclk_s_gpu_biu_en OFFSET(8) NUMBITS(1) [],
        aclk_m0_gpu_biu_en OFFSET(9) NUMBITS(1) [],
        aclk_m1_gpu_biu_en OFFSET(10) NUMBITS(1) [],
        aclk_m2_gpu_biu_en OFFSET(11) NUMBITS(1) [],
        aclk_m3_gpu_biu_en OFFSET(12) NUMBITS(1) [],
        pclk_gpu_root_en OFFSET(13) NUMBITS(1) [],
        pclk_gpu_biu_en OFFSET(14) NUMBITS(1) [],
        pclk_pvtm2_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON67  0x090C
register_bitfields![u32,
    CRU_GATE_CON67 [
        clk_pvtm2_en OFFSET(0) NUMBITS(1) [],
        clk_gpu_pvtm_en OFFSET(1) NUMBITS(1) [],
        pclk_gpu_grf_en OFFSET(2) NUMBITS(1) [],
        clk_gpu_pvtpll_en OFFSET(3) NUMBITS(1) [],
        _reserved OFFSET(4) NUMBITS(12) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON68  0x0910
register_bitfields![u32,
    CRU_GATE_CON68 [
        aclk_av1_root_en OFFSET(0) NUMBITS(1) [],
        aclk_av1_biu_en OFFSET(1) NUMBITS(1) [],
        aclk_av1_en OFFSET(2) NUMBITS(1) [],
        pclk_av1_root_en OFFSET(3) NUMBITS(1) [],
        pclk_av1_biu_en OFFSET(4) NUMBITS(1) [],
        pclk_av1_en OFFSET(5) NUMBITS(1) [],
        _reserved OFFSET(6) NUMBITS(10) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON69  0x0914
register_bitfields![u32,
    CRU_GATE_CON69 [
        aclk_center_root_en OFFSET(0) NUMBITS(1) [],
        aclk_center_low_root_en OFFSET(1) NUMBITS(1) [],
        hclk_center_root_en OFFSET(2) NUMBITS(1) [],
        pclk_center_root_en OFFSET(3) NUMBITS(1) [],
        aclk_ddr_biu_en OFFSET(4) NUMBITS(1) [],
        aclk_dma2ddr_en OFFSET(5) NUMBITS(1) [],
        aclk_ddr_sharemem_en OFFSET(6) NUMBITS(1) [],
        aclk_ddr_sharemem_biu_en OFFSET(7) NUMBITS(1) [],
        aclk_center_s200_root_en OFFSET(8) NUMBITS(1) [],
        aclk_center_s400_root_en OFFSET(9) NUMBITS(1) [],
        aclk_center_s200_biu_en OFFSET(10) NUMBITS(1) [],
        aclk_center_s400_biu_en OFFSET(11) NUMBITS(1) [],
        hclk_ahb2apb_en OFFSET(12) NUMBITS(1) [],
        hclk_center_biu_en OFFSET(13) NUMBITS(1) [],
        fclk_ddr_cm0_core_en OFFSET(14) NUMBITS(1) [],
        clk_ddr_timer_root_en OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON70  0x0918
register_bitfields![u32,
    CRU_GATE_CON70 [
        clk_ddr_timer0_en OFFSET(0) NUMBITS(1) [],
        clk_ddr_timer1_en OFFSET(1) NUMBITS(1) [],
        tclk_wdt_ddr_en OFFSET(2) NUMBITS(1) [],
        _reserved0 OFFSET(3) NUMBITS(1) [],
        clk_ddr_cm0_rtc_en OFFSET(4) NUMBITS(1) [],
        pclk_center_grf_en OFFSET(5) NUMBITS(1) [],
        pclk_ahb2apb_en OFFSET(6) NUMBITS(1) [],
        pclk_wdt_en OFFSET(7) NUMBITS(1) [],
        pclk_timer_en OFFSET(8) NUMBITS(1) [],
        pclk_dma2ddr_en OFFSET(9) NUMBITS(1) [],
        pclk_sharemem_en OFFSET(10) NUMBITS(1) [],
        pclk_center_biu_en OFFSET(11) NUMBITS(1) [],
        pclk_center_channel_biu_en OFFSET(12) NUMBITS(1) [],
        _reserved1 OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON72  0x0920
register_bitfields![u32,
    CRU_GATE_CON72 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        pclk_usbdpgrf0_en OFFSET(1) NUMBITS(1) [],
        pclk_usbdpphy0_en OFFSET(2) NUMBITS(1) [],
        pclk_usbdpgrf1_en OFFSET(3) NUMBITS(1) [],
        pclk_usbdpphy1_en OFFSET(4) NUMBITS(1) [],
        pclk_hdptx0_en OFFSET(5) NUMBITS(1) [],
        pclk_hdptx1_en OFFSET(6) NUMBITS(1) [],
        pclk_apb2asb_slv_bot_right_en OFFSET(7) NUMBITS(1) [],
        pclk_usb2phy_u3_0_grf0_en OFFSET(8) NUMBITS(1) [],
        pclk_usb2phy_u3_1_grf0_en OFFSET(9) NUMBITS(1) [],
        pclk_usb2phy_u2_0_grf0_en OFFSET(10) NUMBITS(1) [],
        pclk_usb2phy_u2_1_grf0_en OFFSET(11) NUMBITS(1) [],
        _reserved1 OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON73  0x0924
register_bitfields![u32,
    CRU_GATE_CON73 [
        _reserved0 OFFSET(0) NUMBITS(12) [],
        clk_hdmihdp0_en OFFSET(12) NUMBITS(1) [],
        clk_hdmihdp1_en OFFSET(13) NUMBITS(1) [],
        _reserved1 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON74  0x0928
register_bitfields![u32,
    CRU_GATE_CON74 [
        aclk_vo1usb_top_root_en OFFSET(0) NUMBITS(1) [],
        aclk_vo1usb_top_biu_en OFFSET(1) NUMBITS(1) [],
        hclk_vo1usb_top_root_en OFFSET(2) NUMBITS(1) [],
        hclk_vo1usb_top_biu_en OFFSET(3) NUMBITS(1) [],
        _reserved OFFSET(4) NUMBITS(12) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON75  0x092C
register_bitfields![u32,
    CRU_GATE_CON75 [
        hclk_sdio_root_en OFFSET(0) NUMBITS(1) [],
        hclk_sdio_biu_en OFFSET(1) NUMBITS(1) [],
        hclk_sdio_en OFFSET(2) NUMBITS(1) [],
        cclk_src_sdio_en OFFSET(3) NUMBITS(1) [],
        _reserved OFFSET(4) NUMBITS(12) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON76  0x0930
register_bitfields![u32,
    CRU_GATE_CON76 [
        aclk_rga3_root_en OFFSET(0) NUMBITS(1) [],
        hclk_rga3_root_en OFFSET(1) NUMBITS(1) [],
        hclk_rga3_biu_en OFFSET(2) NUMBITS(1) [],
        aclk_rga3_biu_en OFFSET(3) NUMBITS(1) [],
        hclk_rga3_1_en OFFSET(4) NUMBITS(1) [],
        aclk_rga3_1_en OFFSET(5) NUMBITS(1) [],
        clk_rga3_1_core_en OFFSET(6) NUMBITS(1) [],
        _reserved OFFSET(7) NUMBITS(9) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GATE_CON77  0x0934
register_bitfields![u32,
    CRU_GATE_CON77 [
        clk_ref_pipe_phy0_osc_src_en OFFSET(0) NUMBITS(1) [],
        clk_ref_pipe_phy1_osc_src_en OFFSET(1) NUMBITS(1) [],
        clk_ref_pipe_phy2_osc_src_en OFFSET(2) NUMBITS(1) [],
        clk_ref_pipe_phy0_pll_src_en OFFSET(3) NUMBITS(1) [],
        clk_ref_pipe_phy1_pll_src_en OFFSET(4) NUMBITS(1) [],
        clk_ref_pipe_phy2_pll_src_en OFFSET(5) NUMBITS(1) [],
        _reserved OFFSET(6) NUMBITS(10) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];
