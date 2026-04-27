use tock_registers::{register_bitfields, registers::ReadWrite};

#[repr(C)]
pub struct V0pllRegisters {
    pub v0pll_con0: ReadWrite<u32, CRU_V0PLL_CON0::Register>,
    pub v0pll_con1: ReadWrite<u32, CRU_V0PLL_CON1::Register>,
    pub v0pll_con2: ReadWrite<u32, CRU_V0PLL_CON2::Register>,
    pub v0pll_con3: ReadWrite<u32, CRU_V0PLL_CON3::Register>,
    pub v0pll_con4: ReadWrite<u32, CRU_V0PLL_CON4::Register>,
    pub v0pll_con5: ReadWrite<u32, CRU_V0PLL_CON5::Register>,
    pub v0pll_con6: ReadWrite<u32, CRU_V0PLL_CON6::Register>,
    _reserved: u32,
}

// CRU_V0PLL_CON0  0x0160
register_bitfields![u32,
    CRU_V0PLL_CON0 [
        v0pll_m OFFSET(0) NUMBITS(10) [],
        _reserved OFFSET(10) NUMBITS(5) [],
        v0pll_bp OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_V0PLL_CON1  0x0164
register_bitfields![u32,
    CRU_V0PLL_CON1 [
        v0pll_p OFFSET(0) NUMBITS(6) [],
        v0pll_s OFFSET(6) NUMBITS(3) [],
        _reserved0 OFFSET(9) NUMBITS(4) [],
        v0pll_resetb OFFSET(13) NUMBITS(1) [],
        _reserved1 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_V0PLL_CON2  0x0168
register_bitfields![u32,
    CRU_V0PLL_CON2 [
        v0pll_k OFFSET(0) NUMBITS(16) [],
        _reserved OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_V0PLL_CON3  0x016C
register_bitfields![u32,
    CRU_V0PLL_CON3 [
        v0pll_mfr OFFSET(0) NUMBITS(8) [],
        v0pll_mrr OFFSET(8) NUMBITS(6) [],
        v0pll_sel_pf OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_V0PLL_CON4  0x0170
register_bitfields![u32,
    CRU_V0PLL_CON4 [
        v0pll_sscg_en OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(2) [],
        v0pll_afc_enb OFFSET(3) NUMBITS(1) [],
        v0pll_extafc OFFSET(4) NUMBITS(5) [],
        _reserved1 OFFSET(9) NUMBITS(5) [],
        v0pll_feed_en OFFSET(14) NUMBITS(1) [],
        v0pll_fsel OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_V0PLL_CON5  0x0174
register_bitfields![u32,
    CRU_V0PLL_CON5 [
        v0pll_fout_mask OFFSET(0) NUMBITS(1) [],
        _reserved OFFSET(1) NUMBITS(15) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_V0PLL_CON6  0x0178
register_bitfields![u32,
    CRU_V0PLL_CON6 [
        _reserved OFFSET(0) NUMBITS(10) [],
        v0pll_afc_code OFFSET(10) NUMBITS(4) [],
        v0pll_lock OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

#[repr(C)]
pub struct AupllRegisters {
    pub aupll_con0: ReadWrite<u32, CRU_AUPLL_CON0::Register>,
    pub aupll_con1: ReadWrite<u32, CRU_AUPLL_CON1::Register>,
    pub aupll_con2: ReadWrite<u32, CRU_AUPLL_CON2::Register>,
    pub aupll_con3: ReadWrite<u32, CRU_AUPLL_CON3::Register>,
    pub aupll_con4: ReadWrite<u32, CRU_AUPLL_CON4::Register>,
    pub aupll_con5: ReadWrite<u32, CRU_AUPLL_CON5::Register>,
    pub aupll_con6: ReadWrite<u32, CRU_AUPLL_CON6::Register>,
    _reserved: u32,
}

// CRU_AUPLL_CON0  0x0180
register_bitfields![u32,
    CRU_AUPLL_CON0 [
        aupll_m OFFSET(0) NUMBITS(10) [],
        _reserved OFFSET(10) NUMBITS(5) [],
        aupll_bp OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUPLL_CON1  0x0184
register_bitfields![u32,
    CRU_AUPLL_CON1 [
        aupll_p OFFSET(0) NUMBITS(6) [],
        aupll_s OFFSET(6) NUMBITS(3) [],
        _reserved0 OFFSET(9) NUMBITS(4) [],
        aupll_resetb OFFSET(13) NUMBITS(1) [],
        _reserved1 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUPLL_CON2  0x0188
register_bitfields![u32,
    CRU_AUPLL_CON2 [
        aupll_k OFFSET(0) NUMBITS(16) [],
        _reserved OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUPLL_CON3  0x018C
register_bitfields![u32,
    CRU_AUPLL_CON3 [
        aupll_mfr OFFSET(0) NUMBITS(8) [],
        aupll_mrr OFFSET(8) NUMBITS(6) [],
        aupll_sel_pf OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUPLL_CON4  0x0190
register_bitfields![u32,
    CRU_AUPLL_CON4 [
        aupll_sscg_en OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(2) [],
        aupll_afc_enb OFFSET(3) NUMBITS(1) [],
        aupll_extafc OFFSET(4) NUMBITS(5) [],
        _reserved1 OFFSET(9) NUMBITS(5) [],
        aupll_feed_en OFFSET(14) NUMBITS(1) [],
        aupll_fsel OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUPLL_CON5  0x0194
register_bitfields![u32,
    CRU_AUPLL_CON5 [
        aupll_fout_mask OFFSET(0) NUMBITS(1) [],
        _reserved OFFSET(1) NUMBITS(15) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_AUPLL_CON6  0x0198
register_bitfields![u32,
    CRU_AUPLL_CON6 [
        _reserved OFFSET(0) NUMBITS(10) [],
        aupll_afc_code OFFSET(10) NUMBITS(4) [],
        aupll_lock OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

#[repr(C)]
pub struct CpllRegisters {
    pub cpll_con0: ReadWrite<u32, CRU_CPLL_CON0::Register>,
    pub cpll_con1: ReadWrite<u32, CRU_CPLL_CON1::Register>,
    pub cpll_con2: ReadWrite<u32, CRU_CPLL_CON2::Register>,
    pub cpll_con3: ReadWrite<u32, CRU_CPLL_CON3::Register>,
    pub cpll_con4: ReadWrite<u32, CRU_CPLL_CON4::Register>,
    pub cpll_con5: ReadWrite<u32, CRU_CPLL_CON5::Register>,
    pub cpll_con6: ReadWrite<u32, CRU_CPLL_CON6::Register>,
    _reserved: u32,
}

// CRU_CPLL_CON0  0x01A0
register_bitfields![u32,
    CRU_CPLL_CON0 [
        cpll_m OFFSET(0) NUMBITS(10) [],
        _reserved OFFSET(10) NUMBITS(5) [],
        cpll_bp OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CPLL_CON1  0x01A4
register_bitfields![u32,
    CRU_CPLL_CON1 [
        cpll_p OFFSET(0) NUMBITS(6) [],
        cpll_s OFFSET(6) NUMBITS(3) [],
        _reserved0 OFFSET(9) NUMBITS(4) [],
        cpll_resetb OFFSET(13) NUMBITS(1) [],
        _reserved1 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CPLL_CON2  0x01A8
register_bitfields![u32,
    CRU_CPLL_CON2 [
        cpll_k OFFSET(0) NUMBITS(16) [],
        _reserved OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CPLL_CON3  0x01AC
register_bitfields![u32,
    CRU_CPLL_CON3 [
        cpll_mfr OFFSET(0) NUMBITS(8) [],
        cpll_mrr OFFSET(8) NUMBITS(6) [],
        cpll_sel_pf OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CPLL_CON4  0x01B0
register_bitfields![u32,
    CRU_CPLL_CON4 [
        cpll_sscg_en OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(2) [],
        cpll_afc_enb OFFSET(3) NUMBITS(1) [],
        cpll_extafc OFFSET(4) NUMBITS(5) [],
        _reserved1 OFFSET(9) NUMBITS(5) [],
        cpll_feed_en OFFSET(14) NUMBITS(1) [],
        cpll_fsel OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CPLL_CON5  0x01B4
register_bitfields![u32,
    CRU_CPLL_CON5 [
        cpll_fout_mask OFFSET(0) NUMBITS(1) [],
        _reserved OFFSET(1) NUMBITS(15) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CPLL_CON6  0x01B8
register_bitfields![u32,
    CRU_CPLL_CON6 [
        _reserved OFFSET(0) NUMBITS(10) [],
        cpll_afc_code OFFSET(10) NUMBITS(4) [],
        cpll_lock OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

#[repr(C)]
pub struct GpllRegisters {
    pub gpll_con0: ReadWrite<u32, CRU_GPLL_CON0::Register>,
    pub gpll_con1: ReadWrite<u32, CRU_GPLL_CON1::Register>,
    pub gpll_con2: ReadWrite<u32, CRU_GPLL_CON2::Register>,
    pub gpll_con3: ReadWrite<u32, CRU_GPLL_CON3::Register>,
    pub gpll_con4: ReadWrite<u32, CRU_GPLL_CON4::Register>,
    pub gpll_con5: ReadWrite<u32, CRU_GPLL_CON5::Register>,
    pub gpll_con6: ReadWrite<u32, CRU_GPLL_CON6::Register>,
    _reserved: u32,
}

// CRU_GPLL_CON0  0x01C0
register_bitfields![u32,
    CRU_GPLL_CON0 [
        gpll_m OFFSET(0) NUMBITS(10) [],
        _reserved OFFSET(10) NUMBITS(5) [],
        gpll_bp OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GPLL_CON1  0x01C4
register_bitfields![u32,
    CRU_GPLL_CON1 [
        gpll_p OFFSET(0) NUMBITS(6) [],
        gpll_s OFFSET(6) NUMBITS(3) [],
        _reserved0 OFFSET(9) NUMBITS(4) [],
        gpll_resetb OFFSET(13) NUMBITS(1) [],
        _reserved1 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GPLL_CON2  0x01C8
register_bitfields![u32,
    CRU_GPLL_CON2 [
        gpll_k OFFSET(0) NUMBITS(16) [],
        _reserved OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GPLL_CON3  0x01CC
register_bitfields![u32,
    CRU_GPLL_CON3 [
        gpll_mfr OFFSET(0) NUMBITS(8) [],
        gpll_mrr OFFSET(8) NUMBITS(6) [],
        gpll_sel_pf OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GPLL_CON4  0x01D0
register_bitfields![u32,
    CRU_GPLL_CON4 [
        gpll_sscg_en OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(2) [],
        gpll_afc_enb OFFSET(3) NUMBITS(1) [],
        gpll_extafc OFFSET(4) NUMBITS(5) [],
        _reserved1 OFFSET(9) NUMBITS(5) [],
        gpll_feed_en OFFSET(14) NUMBITS(1) [],
        gpll_fsel OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GPLL_CON5  0x01D4
register_bitfields![u32,
    CRU_GPLL_CON5 [
        gpll_fout_mask OFFSET(0) NUMBITS(1) [],
        _reserved OFFSET(1) NUMBITS(15) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_GPLL_CON6  0x01D8
register_bitfields![u32,
    CRU_GPLL_CON6 [
        _reserved OFFSET(0) NUMBITS(10) [],
        gpll_afc_code OFFSET(10) NUMBITS(4) [],
        gpll_lock OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

#[repr(C)]
pub struct NpllRegisters {
    pub npll_con0: ReadWrite<u32, CRU_NPLL_CON0::Register>,
    pub npll_con1: ReadWrite<u32, CRU_NPLL_CON1::Register>,
    pub npll_con2: ReadWrite<u32, CRU_NPLL_CON2::Register>,
    pub npll_con3: ReadWrite<u32, CRU_NPLL_CON3::Register>,
    pub npll_con4: ReadWrite<u32, CRU_NPLL_CON4::Register>,
    pub npll_con5: ReadWrite<u32, CRU_NPLL_CON5::Register>,
    pub npll_con6: ReadWrite<u32, CRU_NPLL_CON6::Register>,
    _reserved: u32,
}

// CRU_NPLL_CON0  0x01E0
register_bitfields![u32,
    CRU_NPLL_CON0 [
        npll_m OFFSET(0) NUMBITS(10) [],
        _reserved OFFSET(10) NUMBITS(5) [],
        npll_bp OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_NPLL_CON1  0x01E4
register_bitfields![u32,
    CRU_NPLL_CON1 [
        npll_p OFFSET(0) NUMBITS(6) [],
        npll_s OFFSET(6) NUMBITS(3) [],
        _reserved0 OFFSET(9) NUMBITS(4) [],
        npll_resetb OFFSET(13) NUMBITS(1) [],
        _reserved1 OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_NPLL_CON2  0x01E8
register_bitfields![u32,
    CRU_NPLL_CON2 [
        npll_k OFFSET(0) NUMBITS(16) [],
        _reserved OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_NPLL_CON3  0x01EC
register_bitfields![u32,
    CRU_NPLL_CON3 [
        npll_mfr OFFSET(0) NUMBITS(8) [],
        npll_mrr OFFSET(8) NUMBITS(6) [],
        npll_sel_pf OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_NPLL_CON4  0x01F0
register_bitfields![u32,
    CRU_NPLL_CON4 [
        npll_sscg_en OFFSET(0) NUMBITS(1) [],
        _reserved0 OFFSET(1) NUMBITS(2) [],
        npll_afc_enb OFFSET(3) NUMBITS(1) [],
        npll_extafc OFFSET(4) NUMBITS(5) [],
        _reserved1 OFFSET(9) NUMBITS(5) [],
        npll_feed_en OFFSET(14) NUMBITS(1) [],
        npll_fsel OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_NPLL_CON5  0x01F4
register_bitfields![u32,
    CRU_NPLL_CON5 [
        npll_fout_mask OFFSET(0) NUMBITS(1) [],
        _reserved OFFSET(1) NUMBITS(15) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_NPLL_CON6  0x01F8
register_bitfields![u32,
    CRU_NPLL_CON6 [
        _reserved OFFSET(0) NUMBITS(10) [],
        npll_afc_code OFFSET(10) NUMBITS(4) [],
        npll_lock OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];
