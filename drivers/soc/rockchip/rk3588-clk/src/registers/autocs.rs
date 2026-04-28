use tock_registers::{register_bitfields, registers::ReadWrite};

#[repr(C)]
pub struct ModeRegisters {
    pub mode_con00: ReadWrite<u32, CRU_MODE_CON00::Register>,
    _reserved: [u32; 31],
}

// CRU_MODE_CON00  0x0280
register_bitfields![u32,
    CRU_MODE_CON00 [
        clk_npll_mode OFFSET(0) NUMBITS(2) [],
        clk_gpll_mode OFFSET(2) NUMBITS(2) [],
        clk_v0pll_mode OFFSET(4) NUMBITS(2) [],
        clk_aupll_mode OFFSET(6) NUMBITS(2) [],
        clk_cpll_mode OFFSET(8) NUMBITS(2) [],
        _reserved OFFSET(10) NUMBITS(6) [],   // 保留位
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// const CRU_GLB_CNT_TH: u32 = 0x0C00;
register_bitfields![u32,
    CRU_GLB_CNT_TH [
        global_reset_counter_threshold OFFSET(0) NUMBITS(10) [],
        _reserved OFFSET(10) NUMBITS(22) []
    ]
];

// const CRU_GLBRST_ST: u32 = 0x0C04;
register_bitfields![u32,
    CRU_GLBRST_ST [
        first_glbrst_register_rst OFFSET(0) NUMBITS(1) [],
        second_glbrst_register_rst OFFSET(1) NUMBITS(1) [],
        first_glbrst_tsadc_rst OFFSET(2) NUMBITS(1) [],
        second_glbrst_tsadc_rst OFFSET(3) NUMBITS(1) [],
        first_glbrst_wdt_rst OFFSET(4) NUMBITS(1) [],
        second_glbrst_wdt_rst OFFSET(5) NUMBITS(1) [],
        glbrst_wdt_rst OFFSET(6) NUMBITS(1) [],
        glbrst_osc_chk_rst OFFSET(7) NUMBITS(1) [],
        glbrst_pmusgrf_crc_chk_rst OFFSET(8) NUMBITS(1) [],
        glbrst_dsusgrf_crc_chk_rst OFFSET(9) NUMBITS(1) [],
        glbrst_sgrf_crc_chk_rst OFFSET(10) NUMBITS(1) [],
        glbrst_wdt0_rst OFFSET(11) NUMBITS(1) [],
        glbrst_wdt1_rst OFFSET(12) NUMBITS(1) [],
        glbrst_wdt2_rst OFFSET(13) NUMBITS(1) [],
        glbrst_wdt3_rst OFFSET(14) NUMBITS(1) [],
        glbrst_wdt4_rst OFFSET(15) NUMBITS(1) [],
        _reserved OFFSET(16) NUMBITS(16) []
    ]
];

// const CRU_GLB_SRST_FST_VALUE: u32 = 0x0C08;
register_bitfields![u32,
    CRU_GLB_SRST_FST_VALUE [
        glb_srsc_first_value OFFSET(0) NUMBITS(16) [],
        _reserved OFFSET(16) NUMBITS(16) []
    ]
];

// const CRU_GLB_SRST_SND_VALUE: u32 = 0x0C0C;
register_bitfields![u32,
    CRU_GLB_SRST_SND_VALUE [
        glb_srsc_second_value OFFSET(0) NUMBITS(16) [],
        _reserved OFFSET(16) NUMBITS(16) []
    ]
];

// const CRU_GLB_RST_CON: u32 = 0x0C10;
register_bitfields![u32,
    CRU_GLB_RST_CON [
        tsadc_trig_glbrst_sel OFFSET(0) NUMBITS(1) [],
        tsadc_trig_glbrst_en OFFSET(1) NUMBITS(1) [],
        glbrst_trig_pmu_sel OFFSET(2) NUMBITS(1) [],
        glbrst_trig_pmu_en OFFSET(3) NUMBITS(1) [],
        wdt_trig_pmu_en OFFSET(4) NUMBITS(1) [],
        _reserved0 OFFSET(5) NUMBITS(1) [],
        wdt_trig_glbrst_en OFFSET(6) NUMBITS(1) [],
        osc_chk_trig_glbrst_en OFFSET(7) NUMBITS(1) [],
        crc_pmusgrf_chk_trig_glbrst_en OFFSET(8) NUMBITS(1) [],
        crc_dsusgrf_chk_trig_glbrst_en OFFSET(9) NUMBITS(1) [],
        crc_sgrf_chk_trig_glbrst_en OFFSET(10) NUMBITS(1) [],
        wdt_trig_glbrst_sel OFFSET(11) NUMBITS(1) [],
        osc_chk_trig_glbrst_sel OFFSET(12) NUMBITS(1) [],
        crc_pmusgrf_chk_trig_glbrst_sel OFFSET(13) NUMBITS(1) [],
        crc_dsusgrf_chk_trig_glbrst_sel OFFSET(14) NUMBITS(1) [],
        crc_sgrf_chk_trig_glbrst_sel OFFSET(15) NUMBITS(1) [],
        _reserved1 OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SDIO_CON0  0x0C24
register_bitfields![u32,
    CRU_SDIO_CON0 [
        sdio_con0 OFFSET(0) NUMBITS(16) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SDIO_CON1  0x0C28
register_bitfields![u32,
    CRU_SDIO_CON1 [
        sdio_con1 OFFSET(0) NUMBITS(16) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SDMMC_CON0  0x0C30
register_bitfields![u32,
    CRU_SDMMC_CON0 [
        sdmmc_con0 OFFSET(0) NUMBITS(16) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SDMMC_CON1  0x0C34
register_bitfields![u32,
    CRU_SDMMC_CON1 [
        sdmmc_con1 OFFSET(0) NUMBITS(16) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_PHYREF_ALT_GATE_CON  0x0C38
register_bitfields![u32,
    CRU_PHYREF_ALT_GATE_CON [
        phy0_ref_alt_clk_p_en OFFSET(0) NUMBITS(1) [],
        phy0_ref_alt_clk_m_en OFFSET(1) NUMBITS(1) [],
        phy1_ref_alt_clk_p_en OFFSET(2) NUMBITS(1) [],
        phy1_ref_alt_clk_m_en OFFSET(3) NUMBITS(1) [],
        _reserved OFFSET(4) NUMBITS(12) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CM0_GATEMASK_CON  0x0C3C
register_bitfields![u32,
    CRU_CM0_GATEMASK_CON [
        npucm0_dclk_cm0s_en OFFSET(0) NUMBITS(1) [],
        npucm0_hclk_cm0s_en OFFSET(1) NUMBITS(1) [],
        npucm0_sclk_cm0s_en OFFSET(2) NUMBITS(1) [],
        ddrcm0_dclk_cm0s_en OFFSET(3) NUMBITS(1) [],
        ddrcm0_hclk_cm0s_en OFFSET(4) NUMBITS(1) [],
        ddrcm0_sclk_cm0s_en OFFSET(5) NUMBITS(1) [],
        _reserved OFFSET(6) NUMBITS(10) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_QCHANNEL_CON01  0x0CA4
register_bitfields![u32,
    CRU_QCHANNEL_CON01 [
        aclk_gic_qc_en OFFSET(0) NUMBITS(1) [],
        aclk_gic_qc_gate_en OFFSET(1) NUMBITS(1) [],
        aclk_gicadb_gic2core_bus_qc_en OFFSET(2) NUMBITS(1) [],
        aclk_gicadb_gic2core_bus_qc_gate_en OFFSET(3) NUMBITS(1) [],
        aclk_php_gic_its_qc_en OFFSET(4) NUMBITS(1) [],
        aclk_php_gic_its_qc_gate_en OFFSET(5) NUMBITS(12) [],
        clk_gpu_qc_en OFFSET(6) NUMBITS(1) [],
        clk_gpu_qc_gate_en OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SMOTH_DIVFREE_CON08  0x0CC0
register_bitfields![u32,
    CRU_SMOTH_DIVFREE_CON08 [
        aclk_m0_gpu_step OFFSET(0) NUMBITS(5) [],
        _reserved OFFSET(5) NUMBITS(8) [],
        aclk_m0_gpu_smdiv_clk_off OFFSET(13) NUMBITS(1) [],
        aclk_m0_gpu_gate_smth_en OFFSET(14) NUMBITS(1) [],
        aclk_m0_gpu_bypass OFFSET(15) NUMBITS(1) [],
        aclk_m0_gpu_freq_keep OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SMOTH_DIVFREE_CON09  0x0CC4
register_bitfields![u32,
    CRU_SMOTH_DIVFREE_CON09 [
        aclk_m1_gpu_step OFFSET(0) NUMBITS(5) [],
        _reserved OFFSET(5) NUMBITS(8) [],
        aclk_m1_gpu_smdiv_clk_off OFFSET(13) NUMBITS(1) [],
        aclk_m1_gpu_gate_smth_en OFFSET(14) NUMBITS(1) [],
        aclk_m1_gpu_bypass OFFSET(15) NUMBITS(1) [],
        aclk_m1_gpu_freq_keep OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SMOTH_DIVFREE_CON10  0x0CC8
register_bitfields![u32,
    CRU_SMOTH_DIVFREE_CON10 [
        aclk_m2_gpu_step OFFSET(0) NUMBITS(5) [],
        _reserved OFFSET(5) NUMBITS(8) [],
        aclk_m2_gpu_smdiv_clk_off OFFSET(13) NUMBITS(1) [],
        aclk_m2_gpu_gate_smth_en OFFSET(14) NUMBITS(1) [],
        aclk_m2_gpu_bypass OFFSET(15) NUMBITS(1) [],
        aclk_m2_gpu_freq_keep OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SMOTH_DIVFREE_CON11  0x0CCC
register_bitfields![u32,
    CRU_SMOTH_DIVFREE_CON11 [
        aclk_m3_gpu_step OFFSET(0) NUMBITS(5) [],
        _reserved OFFSET(5) NUMBITS(8) [],
        aclk_m3_gpu_smdiv_clk_off OFFSET(13) NUMBITS(1) [],
        aclk_m3_gpu_gate_smth_en OFFSET(14) NUMBITS(1) [],
        aclk_m3_gpu_bypass OFFSET(15) NUMBITS(1) [],
        aclk_m3_gpu_freq_keep OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_SMOTH_DIVFREE_CON12  0x0CD0
register_bitfields![u32,
    CRU_SMOTH_DIVFREE_CON12 [
        clk_rknn_dsu0_src_step OFFSET(0) NUMBITS(5) [],
        _reserved OFFSET(5) NUMBITS(8) [],
        clk_rknn_dsu0_src_smdiv_clk_off OFFSET(13) NUMBITS(1) [],
        clk_rknn_dsu0_src_gate_smth_en OFFSET(14) NUMBITS(1) [],
        clk_rknn_dsu0_src_bypass OFFSET(15) NUMBITS(1) [],
        clk_rknn_dsu0_src_freq_keep OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_ROOT_CON0  0x0D00
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_ROOT_CON0 [
        aclk_top_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_top_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_ROOT_CON1  0x0D04
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_ROOT_CON1 [
        aclk_top_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_top_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_top_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_top_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_LOW_TOP_ROOT_CON0  0x0D08
register_bitfields![u32,
    CRU_AUTOCS_ACLK_LOW_TOP_ROOT_CON0 [
        aclk_low_top_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_low_top_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_LOW_TOP_ROOT_CON1  0x0D0C
register_bitfields![u32,
    CRU_AUTOCS_ACLK_LOW_TOP_ROOT_CON1 [
        aclk_low_top_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_low_top_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_low_top_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_low_top_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_M400_ROOT_CON0  0x0D10
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_M400_ROOT_CON0 [
        aclk_top_m400_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_top_m400_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_M400_ROOT_CON1  0x0D14
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_M400_ROOT_CON1 [
        aclk_top_m400_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_top_m400_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_top_m400_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_top_m400_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_S400_ROOT_CON0  0x0D18
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_S400_ROOT_CON0 [
        aclk_top_s400_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_top_s400_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_S400_ROOT_CON1  0x0D1C
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_S400_ROOT_CON1 [
        aclk_top_s400_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_top_s400_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_top_s400_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_top_s400_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_BUS_ROOT_CON0  0x0D20
register_bitfields![u32,
    CRU_AUTOCS_ACLK_BUS_ROOT_CON0 [
        aclk_bus_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_bus_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_BUS_ROOT_CON1  0x0D24
register_bitfields![u32,
    CRU_AUTOCS_ACLK_BUS_ROOT_CON1 [
        aclk_bus_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_bus_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_bus_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_bus_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_ISP1_ROOT_CON0  0x0D28
register_bitfields![u32,
    CRU_AUTOCS_ACLK_ISP1_ROOT_CON0 [
        aclk_isp1_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_isp1_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_ISP1_ROOT_CON1  0x0D2C
register_bitfields![u32,
    CRU_AUTOCS_ACLK_ISP1_ROOT_CON1 [
        aclk_isp1_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_isp1_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_isp1_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_isp1_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_CLK_RKNN_DSU0_CON0  0x0D30
register_bitfields![u32,
    CRU_AUTOCS_CLK_RKNN_DSU0_CON0 [
        clk_rknn_dsu0_idle_th OFFSET(0) NUMBITS(16) [],
        clk_rknn_dsu0_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_CLK_RKNN_DSU0_CON1  0x0D34
register_bitfields![u32,
    CRU_AUTOCS_CLK_RKNN_DSU0_CON1 [
        clk_rknn_dsu0_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        clk_rknn_dsu0_autocs_en OFFSET(12) NUMBITS(1) [],
        clk_rknn_dsu0_switch_en OFFSET(13) NUMBITS(1) [],
        clk_rknn_dsu0_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RKNN_ROOT_CON0  0x0D38
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RKNN_ROOT_CON0 [
        hclk_rknn_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_rknn_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RKNN_ROOT_CON1  0x0D3C
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RKNN_ROOT_CON1 [
        hclk_rknn_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_rknn_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_rknn_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_rknn_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_NVM_ROOT_CON0  0x0D40
register_bitfields![u32,
    CRU_AUTOCS_ACLK_NVM_ROOT_CON0 [
        aclk_nvm_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_nvm_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_NVM_ROOT_CON1  0x0D44
register_bitfields![u32,
    CRU_AUTOCS_ACLK_NVM_ROOT_CON1 [
        aclk_nvm_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_nvm_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_nvm_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_nvm_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_PHP_ROOT_CON0  0x0D48
register_bitfields![u32,
    CRU_AUTOCS_ACLK_PHP_ROOT_CON0 [
        aclk_php_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_php_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_PHP_ROOT_CON1  0x0D4C
register_bitfields![u32,
    CRU_AUTOCS_ACLK_PHP_ROOT_CON1 [
        aclk_php_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_php_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_php_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_php_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RKVDEC0_ROOT_CON0  0x0D50
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RKVDEC0_ROOT_CON0 [
        aclk_rkvdec0_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_rkvdec0_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RKVDEC0_ROOT_CON1  0x0D54
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RKVDEC0_ROOT_CON1 [
        aclk_rkvdec0_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_rkvdec0_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_rkvdec0_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_rkvdec0_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RKVDEC_CCU_CON0  0x0D58
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RKVDEC_CCU_CON0 [
        aclk_rkvdec_ccu_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_rkvdec_ccu_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RKVDEC_CCU_CON1  0x0D5C
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RKVDEC_CCU_CON1 [
        aclk_rkvdec_ccu_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_rkvdec_ccu_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_rkvdec_ccu_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_rkvdec_ccu_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RKVDEC1_ROOT_CON0  0x0D60
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RKVDEC1_ROOT_CON0 [
        aclk_rkvdec1_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_rkvdec1_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RKVDEC1_ROOT_CON1  0x0D64
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RKVDEC1_ROOT_CON1 [
        aclk_rkvdec1_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_rkvdec1_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_rkvdec1_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_rkvdec1_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_USB_ROOT_CON0  0x0D68
register_bitfields![u32,
    CRU_AUTOCS_ACLK_USB_ROOT_CON0 [
        aclk_usb_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_usb_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_USB_ROOT_CON1  0x0D6C
register_bitfields![u32,
    CRU_AUTOCS_ACLK_USB_ROOT_CON1 [
        aclk_usb_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_usb_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_usb_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_usb_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VDPU_ROOT_CON0  0x0D70
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VDPU_ROOT_CON0 [
        aclk_vdpu_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_vdpu_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VDPU_ROOT_CON1  0x0D74
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VDPU_ROOT_CON1 [
        aclk_vdpu_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_vdpu_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_vdpu_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_vdpu_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VDPU_LOW_ROOT_CON0  0x0D78
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VDPU_LOW_ROOT_CON0 [
        aclk_vdpu_low_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_vdpu_low_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VDPU_LOW_ROOT_CON1  0x0D7C
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VDPU_LOW_ROOT_CON1 [
        aclk_vdpu_low_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_vdpu_low_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_vdpu_low_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_vdpu_low_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_JPEG_DECODER_ROOT_CON0  0x0D80
register_bitfields![u32,
    CRU_AUTOCS_ACLK_JPEG_DECODER_ROOT_CON0 [
        aclk_jpeg_decoder_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_jpeg_decoder_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_JPEG_DECODER_ROOT_CON1  0x0D84
register_bitfields![u32,
    CRU_AUTOCS_ACLK_JPEG_DECODER_ROOT_CON1 [
        aclk_jpeg_decoder_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_jpeg_decoder_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_jpeg_decoder_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_jpeg_decoder_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RKVENC0_ROOT_CON0  0x0D88
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RKVENC0_ROOT_CON0 [
        aclk_rkvenc0_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_rkvenc0_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RKVENC0_ROOT_CON1  0x0D8C
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RKVENC0_ROOT_CON1 [
        aclk_rkvenc0_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_rkvenc0_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_rkvenc0_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_rkvenc0_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RKVENC1_ROOT_CON0  0x0D90
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RKVENC1_ROOT_CON0 [
        aclk_rkvenc1_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_rkvenc1_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RKVENC1_ROOT_CON1  0x0D94
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RKVENC1_ROOT_CON1 [
        aclk_rkvenc1_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_rkvenc1_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_rkvenc1_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_rkvenc1_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VI_ROOT_CON0  0x0D98
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VI_ROOT_CON0 [
        aclk_vi_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_vi_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VI_ROOT_CON1  0x0D9C
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VI_ROOT_CON1 [
        aclk_vi_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_vi_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_vi_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_vi_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VOP_ROOT_CON0  0x0DA0
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VOP_ROOT_CON0 [
        aclk_vop_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_vop_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VOP_ROOT_CON1  0x0DA4
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VOP_ROOT_CON1 [
        aclk_vop_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_vop_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_vop_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_vop_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VO0_ROOT_CON0  0x0DA8
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VO0_ROOT_CON0 [
        aclk_vo0_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_vo0_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VO0_ROOT_CON1  0x0DAC
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VO0_ROOT_CON1 [
        aclk_vo0_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_vo0_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_vo0_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_vo0_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_HDCP1_ROOT_CON0  0x0DB0
register_bitfields![u32,
    CRU_AUTOCS_ACLK_HDCP1_ROOT_CON0 [
        aclk_hdcp1_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_hdcp1_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_HDCP1_ROOT_CON1  0x0DB4
register_bitfields![u32,
    CRU_AUTOCS_ACLK_HDCP1_ROOT_CON1 [
        aclk_hdcp1_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_hdcp1_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_hdcp1_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_hdcp1_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_HDMIRX_ROOT_CON0  0x0DB8
register_bitfields![u32,
    CRU_AUTOCS_ACLK_HDMIRX_ROOT_CON0 [
        aclk_hdmirx_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_hdmirx_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_HDMIRX_ROOT_CON1  0x0DBC
register_bitfields![u32,
    CRU_AUTOCS_ACLK_HDMIRX_ROOT_CON1 [
        aclk_hdmirx_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_hdmirx_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_hdmirx_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_hdmirx_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_CLK_GPU_COREGROUP_CON0  0x0DC0
register_bitfields![u32,
    CRU_AUTOCS_CLK_GPU_COREGROUP_CON0 [
        clk_gpu_coregroup_idle_th OFFSET(0) NUMBITS(16) [],
        clk_gpu_coregroup_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_CLK_GPU_COREGROUP_CON1  0x0DC4
register_bitfields![u32,
    CRU_AUTOCS_CLK_GPU_COREGROUP_CON1 [
        clk_gpu_coregroup_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        clk_gpu_coregroup_autocs_en OFFSET(12) NUMBITS(1) [],
        clk_gpu_coregroup_switch_en OFFSET(13) NUMBITS(1) [],
        clk_gpu_coregroup_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_AV1_ROOT_CON0  0x0DE0
register_bitfields![u32,
    CRU_AUTOCS_ACLK_AV1_ROOT_CON0 [
        aclk_av1_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_av1_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_AV1_ROOT_CON1  0x0DE4
register_bitfields![u32,
    CRU_AUTOCS_ACLK_AV1_ROOT_CON1 [
        aclk_av1_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_av1_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_av1_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_av1_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_CENTER_ROOT_CON0  0x0DE8
register_bitfields![u32,
    CRU_AUTOCS_ACLK_CENTER_ROOT_CON0 [
        aclk_center_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_center_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_CENTER_ROOT_CON1  0x0DEC
register_bitfields![u32,
    CRU_AUTOCS_ACLK_CENTER_ROOT_CON1 [
        aclk_center_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_center_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_center_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_center_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_CENTER_LOW_ROOT_CON0  0x0DF0
register_bitfields![u32,
    CRU_AUTOCS_ACLK_CENTER_LOW_ROOT_CON0 [
        aclk_center_low_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_center_low_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_CENTER_LOW_ROOT_CON1  0x0DF4
register_bitfields![u32,
    CRU_AUTOCS_ACLK_CENTER_LOW_ROOT_CON1 [
        aclk_center_low_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_center_low_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_center_low_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_center_low_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_CENTER_S400_ROOT_CON0  0x0DF8
register_bitfields![u32,
    CRU_AUTOCS_ACLK_CENTER_S400_ROOT_CON0 [
        aclk_center_s400_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_center_s400_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_CENTER_S400_ROOT_CON1  0x0DFC
register_bitfields![u32,
    CRU_AUTOCS_ACLK_CENTER_S400_ROOT_CON1 [
        aclk_center_s400_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_center_s400_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_center_s400_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_center_s400_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VO1USB_TOP_ROOT_CON0  0x0E00
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VO1USB_TOP_ROOT_CON0 [
        aclk_vo1usb_top_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_vo1usb_top_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VO1USB_TOP_ROOT_CON1  0x0E04
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VO1USB_TOP_ROOT_CON1 [
        aclk_vo1usb_top_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_vo1usb_top_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_vo1usb_top_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_vo1usb_top_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RGA3_ROOT_CON0  0x0E08
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RGA3_ROOT_CON0 [
        aclk_rga3_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_rga3_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_RGA3_ROOT_CON1  0x0E0C
register_bitfields![u32,
    CRU_AUTOCS_ACLK_RGA3_ROOT_CON1 [
        aclk_rga3_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_rga3_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_rga3_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_rga3_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_AV1_ROOT_CON0  0x0E10
register_bitfields![u32,
    CRU_AUTOCS_PCLK_AV1_ROOT_CON0 [
        pclk_av1_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_av1_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_AV1_ROOT_CON1  0x0E14
register_bitfields![u32,
    CRU_AUTOCS_PCLK_AV1_ROOT_CON1 [
        pclk_av1_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_av1_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_av1_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_av1_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_ISP1_ROOT_CON0  0x0E18
register_bitfields![u32,
    CRU_AUTOCS_HCLK_ISP1_ROOT_CON0 [
        hclk_isp1_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_isp1_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_ISP1_ROOT_CON1  0x0E1C
register_bitfields![u32,
    CRU_AUTOCS_HCLK_ISP1_ROOT_CON1 [
        hclk_isp1_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_isp1_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_isp1_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_isp1_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_NPUTOP_ROOT_CON0  0x0E20
register_bitfields![u32,
    CRU_AUTOCS_PCLK_NPUTOP_ROOT_CON0 [
        pclk_nputop_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_nputop_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_NPUTOP_ROOT_CON1  0x0E24
register_bitfields![u32,
    CRU_AUTOCS_PCLK_NPUTOP_ROOT_CON1 [
        pclk_nputop_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_nputop_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_nputop_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_nputop_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_NPU_CM0_ROOT_CON0  0x0E28
register_bitfields![u32,
    CRU_AUTOCS_HCLK_NPU_CM0_ROOT_CON0 [
        hclk_npu_cm0_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_npu_cm0_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_NPU_CM0_ROOT_CON1  0x0E2C
register_bitfields![u32,
    CRU_AUTOCS_HCLK_NPU_CM0_ROOT_CON1 [
        hclk_npu_cm0_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_npu_cm0_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_npu_cm0_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_npu_cm0_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_NVM_ROOT_CON0  0x0E30
register_bitfields![u32,
    CRU_AUTOCS_HCLK_NVM_ROOT_CON0 [
        hclk_nvm_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_nvm_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_NVM_ROOT_CON1  0x0E34
register_bitfields![u32,
    CRU_AUTOCS_HCLK_NVM_ROOT_CON1 [
        hclk_nvm_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_nvm_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_nvm_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_nvm_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_PHP_ROOT_CON0  0x0E38
register_bitfields![u32,
    CRU_AUTOCS_PCLK_PHP_ROOT_CON0 [
        pclk_php_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_php_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_PHP_ROOT_CON1  0x0E3C
register_bitfields![u32,
    CRU_AUTOCS_PCLK_PHP_ROOT_CON1 [
        pclk_php_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_php_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_php_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_php_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_PCIE_ROOT_CON0  0x0E40
register_bitfields![u32,
    CRU_AUTOCS_ACLK_PCIE_ROOT_CON0 [
        aclk_pcie_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_pcie_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_PCIE_ROOT_CON1  0x0E44
register_bitfields![u32,
    CRU_AUTOCS_ACLK_PCIE_ROOT_CON1 [
        aclk_pcie_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_pcie_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_pcie_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_pcie_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RKVDEC0_ROOT_CON0  0x0E48
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RKVDEC0_ROOT_CON0 [
        hclk_rkvdec0_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_rkvdec0_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RKVDEC0_ROOT_CON1  0x0E4C
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RKVDEC0_ROOT_CON1 [
        hclk_rkvdec0_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_rkvdec0_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_rkvdec0_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_rkvdec0_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RKVDEC1_ROOT_CON0  0x0E50
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RKVDEC1_ROOT_CON0 [
        hclk_rkvdec1_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_rkvdec1_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RKVDEC1_ROOT_CON1  0x0E54
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RKVDEC1_ROOT_CON1 [
        hclk_rkvdec1_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_rkvdec1_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_rkvdec1_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_rkvdec1_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_TOP_ROOT_CON0  0x0E58
register_bitfields![u32,
    CRU_AUTOCS_PCLK_TOP_ROOT_CON0 [
        pclk_top_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_top_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_TOP_ROOT_CON1  0x0E5C
register_bitfields![u32,
    CRU_AUTOCS_PCLK_TOP_ROOT_CON1 [
        pclk_top_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_top_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_top_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_top_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_M500_ROOT_CON0  0x0E60
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_M500_ROOT_CON0 [
        aclk_top_m500_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_top_m500_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_M500_ROOT_CON1  0x0E64
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_M500_ROOT_CON1 [
        aclk_top_m500_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_top_m500_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_top_m500_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_top_m500_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_S200_ROOT_CON0  0x0E68
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_S200_ROOT_CON0 [
        aclk_top_s200_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_top_s200_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_S200_ROOT_CON1  0x0E6C
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_S200_ROOT_CON1 [
        aclk_top_s200_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_top_s200_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_top_s200_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_top_s200_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_USB_ROOT_CON0  0x0E70
register_bitfields![u32,
    CRU_AUTOCS_HCLK_USB_ROOT_CON0 [
        hclk_usb_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_usb_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_USB_ROOT_CON1  0x0E74
register_bitfields![u32,
    CRU_AUTOCS_HCLK_USB_ROOT_CON1 [
        hclk_usb_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_usb_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_usb_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_usb_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VDPU_ROOT_CON0  0x0E78
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VDPU_ROOT_CON0 [
        hclk_vdpu_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_vdpu_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VDPU_ROOT_CON1  0x0E7C
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VDPU_ROOT_CON1 [
        hclk_vdpu_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_vdpu_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_vdpu_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_vdpu_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RKVENC0_ROOT_CON0  0x0E80
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RKVENC0_ROOT_CON0 [
        hclk_rkvenc0_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_rkvenc0_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RKVENC0_ROOT_CON1  0x0E84
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RKVENC0_ROOT_CON1 [
        hclk_rkvenc0_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_rkvenc0_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_rkvenc0_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_rkvenc0_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RKVENC1_ROOT_CON0  0x0E88
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RKVENC1_ROOT_CON0 [
        hclk_rkvenc1_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_rkvenc1_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RKVENC1_ROOT_CON1  0x0E8C
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RKVENC1_ROOT_CON1 [
        hclk_rkvenc1_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_rkvenc1_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_rkvenc1_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_rkvenc1_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VI_ROOT_CON0  0x0E90
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VI_ROOT_CON0 [
        hclk_vi_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_vi_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VI_ROOT_CON1  0x0E94
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VI_ROOT_CON1 [
        hclk_vi_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_vi_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_vi_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_vi_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VI_ROOT_CON0  0x0E98
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VI_ROOT_CON0 [
        pclk_vi_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_vi_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VI_ROOT_CON1  0x0E9C
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VI_ROOT_CON1 [
        pclk_vi_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_vi_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_vi_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_vi_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VOP_LOW_ROOT_CON0  0x0EA0
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VOP_LOW_ROOT_CON0 [
        aclk_vop_low_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_vop_low_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_VOP_LOW_ROOT_CON1  0x0EA4
register_bitfields![u32,
    CRU_AUTOCS_ACLK_VOP_LOW_ROOT_CON1 [
        aclk_vop_low_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_vop_low_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_vop_low_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_vop_low_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VOP_ROOT_CON0  0x0EA8
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VOP_ROOT_CON0 [
        hclk_vop_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_vop_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VOP_ROOT_CON1  0x0EAC
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VOP_ROOT_CON1 [
        hclk_vop_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_vop_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_vop_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_vop_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VOP_ROOT_CON0  0x0EB0
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VOP_ROOT_CON0 [
        pclk_vop_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_vop_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VOP_ROOT_CON1  0x0EB4
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VOP_ROOT_CON1 [
        pclk_vop_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_vop_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_vop_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_vop_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VO0_ROOT_CON0  0x0EB8
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VO0_ROOT_CON0 [
        hclk_vo0_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_vo0_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VO0_ROOT_CON1  0x0EBC
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VO0_ROOT_CON1 [
        hclk_vo0_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_vo0_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_vo0_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_vo0_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VO0_S_ROOT_CON0  0x0EC0
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VO0_S_ROOT_CON0 [
        hclk_vo0_s_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_vo0_s_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VO0_S_ROOT_CON1  0x0EC4
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VO0_S_ROOT_CON1 [
        hclk_vo0_s_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_vo0_s_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_vo0_s_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_vo0_s_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VO0_ROOT_CON0  0x0EC8
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VO0_ROOT_CON0 [
        pclk_vo0_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_vo0_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VO0_ROOT_CON1  0x0ECC
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VO0_ROOT_CON1 [
        pclk_vo0_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_vo0_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_vo0_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_vo0_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VO0_S_ROOT_CON0  0x0ED0
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VO0_S_ROOT_CON0 [
        pclk_vo0_s_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_vo0_s_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VO0_S_ROOT_CON1  0x0ED4
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VO0_S_ROOT_CON1 [
        pclk_vo0_s_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_vo0_s_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_vo0_s_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_vo0_s_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VO1_ROOT_CON0  0x0ED8
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VO1_ROOT_CON0 [
        hclk_vo1_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_vo1_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VO1_ROOT_CON1  0x0EDC
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VO1_ROOT_CON1 [
        hclk_vo1_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_vo1_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_vo1_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_vo1_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VO1_S_ROOT_CON0  0x0EE0
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VO1_S_ROOT_CON0 [
        hclk_vo1_s_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_vo1_s_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VO1_S_ROOT_CON1  0x0EE4
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VO1_S_ROOT_CON1 [
        hclk_vo1_s_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_vo1_s_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_vo1_s_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_vo1_s_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VO1_ROOT_CON0  0x0EE8
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VO1_ROOT_CON0 [
        pclk_vo1_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_vo1_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VO1_ROOT_CON1  0x0EEC
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VO1_ROOT_CON1 [
        pclk_vo1_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_vo1_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_vo1_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_vo1_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VO1_S_ROOT_CON0  0x0EF0
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VO1_S_ROOT_CON0 [
        pclk_vo1_s_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_vo1_s_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_VO1_S_ROOT_CON1  0x0EF4
register_bitfields![u32,
    CRU_AUTOCS_PCLK_VO1_S_ROOT_CON1 [
        pclk_vo1_s_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_vo1_s_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_vo1_s_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_vo1_s_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_GPU_ROOT_CON0  0x0EF8
register_bitfields![u32,
    CRU_AUTOCS_PCLK_GPU_ROOT_CON0 [
        pclk_gpu_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_gpu_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_GPU_ROOT_CON1  0x0EFC
register_bitfields![u32,
    CRU_AUTOCS_PCLK_GPU_ROOT_CON1 [
        pclk_gpu_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_gpu_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_gpu_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_gpu_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_CENTER_ROOT_CON0  0x0F00
register_bitfields![u32,
    CRU_AUTOCS_HCLK_CENTER_ROOT_CON0 [
        hclk_center_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_center_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_CENTER_ROOT_CON1  0x0F04
register_bitfields![u32,
    CRU_AUTOCS_HCLK_CENTER_ROOT_CON1 [
        hclk_center_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_center_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_center_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_center_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_CENTER_ROOT_CON0  0x0F08
register_bitfields![u32,
    CRU_AUTOCS_PCLK_CENTER_ROOT_CON0 [
        pclk_center_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_center_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_CENTER_ROOT_CON1  0x0F0C
register_bitfields![u32,
    CRU_AUTOCS_PCLK_CENTER_ROOT_CON1 [
        pclk_center_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_center_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_center_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_center_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_CENTER_S200_ROOT_CON0  0x0F10
register_bitfields![u32,
    CRU_AUTOCS_ACLK_CENTER_S200_ROOT_CON0 [
        aclk_center_s200_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_center_s200_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_CENTER_S200_ROOT_CON1  0x0F14
register_bitfields![u32,
    CRU_AUTOCS_ACLK_CENTER_S200_ROOT_CON1 [
        aclk_center_s200_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_center_s200_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_center_s200_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_center_s200_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_SDIO_ROOT_CON0  0x0F18
register_bitfields![u32,
    CRU_AUTOCS_HCLK_SDIO_ROOT_CON0 [
        hclk_sdio_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_sdio_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_SDIO_ROOT_CON1  0x0F1C
register_bitfields![u32,
    CRU_AUTOCS_HCLK_SDIO_ROOT_CON1 [
        hclk_sdio_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_sdio_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_sdio_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_sdio_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RGA3_ROOT_CON0  0x0F20
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RGA3_ROOT_CON0 [
        hclk_rga3_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_rga3_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_RGA3_ROOT_CON1  0x0F24
register_bitfields![u32,
    CRU_AUTOCS_HCLK_RGA3_ROOT_CON1 [
        hclk_rga3_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_rga3_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_rga3_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_rga3_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VO1USB_TOP_ROOT_CON0  0x0F28
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VO1USB_TOP_ROOT_CON0 [
        hclk_vo1usb_top_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_vo1usb_top_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_VO1USB_TOP_ROOT_CON1  0x0F2C
register_bitfields![u32,
    CRU_AUTOCS_HCLK_VO1USB_TOP_ROOT_CON1 [
        hclk_vo1usb_top_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_vo1usb_top_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_vo1usb_top_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_vo1usb_top_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_M300_ROOT_CON0  0x0F30
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_M300_ROOT_CON0 [
        aclk_top_m300_root_idle_th OFFSET(0) NUMBITS(16) [],
        aclk_top_m300_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_ACLK_TOP_M300_ROOT_CON1  0x0F34
register_bitfields![u32,
    CRU_AUTOCS_ACLK_TOP_M300_ROOT_CON1 [
        aclk_top_m300_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        aclk_top_m300_root_autocs_en OFFSET(12) NUMBITS(1) [],
        aclk_top_m300_root_switch_en OFFSET(13) NUMBITS(1) [],
        aclk_top_m300_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_CLK_RKNN_DSU0_SRC_T_CON0  0x0F38
register_bitfields![u32,
    CRU_AUTOCS_CLK_RKNN_DSU0_SRC_T_CON0 [
        clk_rknn_dsu0_src_t_idle_th OFFSET(0) NUMBITS(16) [],
        clk_rknn_dsu0_src_t_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_CLK_RKNN_DSU0_SRC_T_CON1  0x0F3C
register_bitfields![u32,
    CRU_AUTOCS_CLK_RKNN_DSU0_SRC_T_CON1 [
        clk_rknn_dsu0_src_t_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        clk_rknn_dsu0_src_t_autocs_en OFFSET(12) NUMBITS(1) [],
        clk_rknn_dsu0_src_t_switch_en OFFSET(13) NUMBITS(1) [],
        clk_rknn_dsu0_src_t_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_AUDIO_ROOT_CON0  0x0F40
register_bitfields![u32,
    CRU_AUTOCS_HCLK_AUDIO_ROOT_CON0 [
        hclk_audio_root_idle_th OFFSET(0) NUMBITS(16) [],
        hclk_audio_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_HCLK_AUDIO_ROOT_CON1  0x0F44
register_bitfields![u32,
    CRU_AUTOCS_HCLK_AUDIO_ROOT_CON1 [
        hclk_audio_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        hclk_audio_root_autocs_en OFFSET(12) NUMBITS(1) [],
        hclk_audio_root_switch_en OFFSET(13) NUMBITS(1) [],
        hclk_audio_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_AUDIO_ROOT_CON0  0x0F48
register_bitfields![u32,
    CRU_AUTOCS_PCLK_AUDIO_ROOT_CON0 [
        pclk_audio_root_idle_th OFFSET(0) NUMBITS(16) [],
        pclk_audio_root_wait_th OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUTOCS_PCLK_AUDIO_ROOT_CON1  0x0F4C
register_bitfields![u32,
    CRU_AUTOCS_PCLK_AUDIO_ROOT_CON1 [
        pclk_audio_root_autocs_ctrl OFFSET(0) NUMBITS(12) [],
        pclk_audio_root_autocs_en OFFSET(12) NUMBITS(1) [],
        pclk_audio_root_switch_en OFFSET(13) NUMBITS(1) [],
        pclk_audio_root_clksel_cfg OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];
