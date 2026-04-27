use tock_registers::{register_bitfields, registers::ReadWrite};

#[repr(C)]
pub struct ClkSelRegisters {
    pub cru_clksel_con00: ReadWrite<u32, CRU_CLKSEL_CON00::Register>,
    pub cru_clksel_con01: ReadWrite<u32, CRU_CLKSEL_CON01::Register>,
    pub cru_clksel_con02: ReadWrite<u32, CRU_CLKSEL_CON02::Register>,
    pub cru_clksel_con03: ReadWrite<u32, CRU_CLKSEL_CON03::Register>,
    pub cru_clksel_con04: ReadWrite<u32, CRU_CLKSEL_CON04::Register>,
    pub cru_clksel_con05: ReadWrite<u32, CRU_CLKSEL_CON05::Register>,
    pub cru_clksel_con06: ReadWrite<u32, CRU_CLKSEL_CON06::Register>,
    pub cru_clksel_con07: ReadWrite<u32, CRU_CLKSEL_CON07::Register>,
    pub cru_clksel_con08: ReadWrite<u32, CRU_CLKSEL_CON08::Register>,
    pub cru_clksel_con09: ReadWrite<u32, CRU_CLKSEL_CON09::Register>,
    pub cru_clksel_con10: ReadWrite<u32, CRU_CLKSEL_CON10::Register>,
    _reserved0: [u32; 4],
    pub cru_clksel_con15: ReadWrite<u32, CRU_CLKSEL_CON15::Register>,
    pub cru_clksel_con16: ReadWrite<u32, CRU_CLKSEL_CON16::Register>,
    pub cru_clksel_con17: ReadWrite<u32, CRU_CLKSEL_CON17::Register>,
    pub cru_clksel_con18: ReadWrite<u32, CRU_CLKSEL_CON18::Register>,
    pub cru_clksel_con19: ReadWrite<u32, CRU_CLKSEL_CON19::Register>,
    pub cru_clksel_con20: ReadWrite<u32, CRU_CLKSEL_CON20::Register>,
    pub cru_clksel_con21: ReadWrite<u32, CRU_CLKSEL_CON21::Register>,
    pub cru_clksel_con22: ReadWrite<u32, CRU_CLKSEL_CON22::Register>,
    _reserved1: u32,
    pub cru_clksel_con24: ReadWrite<u32, CRU_CLKSEL_CON24::Register>,
    pub cru_clksel_con25: ReadWrite<u32, CRU_CLKSEL_CON25::Register>,
    pub cru_clksel_con26: ReadWrite<u32, CRU_CLKSEL_CON26::Register>,
    pub cru_clksel_con27: ReadWrite<u32, CRU_CLKSEL_CON27::Register>,
    pub cru_clksel_con28: ReadWrite<u32, CRU_CLKSEL_CON28::Register>,
    pub cru_clksel_con29: ReadWrite<u32, CRU_CLKSEL_CON29::Register>,
    pub cru_clksel_con30: ReadWrite<u32, CRU_CLKSEL_CON30::Register>,
    pub cru_clksel_con31: ReadWrite<u32, CRU_CLKSEL_CON31::Register>,
    pub cru_clksel_con32: ReadWrite<u32, CRU_CLKSEL_CON32::Register>,
    pub cru_clksel_con33: ReadWrite<u32, CRU_CLKSEL_CON33::Register>,
    pub cru_clksel_con34: ReadWrite<u32, CRU_CLKSEL_CON34::Register>,
    pub cru_clksel_con35: ReadWrite<u32, CRU_CLKSEL_CON35::Register>,
    pub cru_clksel_con36: ReadWrite<u32, CRU_CLKSEL_CON36::Register>,
    _reserved2: u32,
    pub cru_clksel_con38: ReadWrite<u32, CRU_CLKSEL_CON38::Register>,
    pub cru_clksel_con39: ReadWrite<u32, CRU_CLKSEL_CON39::Register>,
    pub cru_clksel_con40: ReadWrite<u32, CRU_CLKSEL_CON40::Register>,
    pub cru_clksel_con41: ReadWrite<u32, CRU_CLKSEL_CON41::Register>,
    pub cru_clksel_con42: ReadWrite<u32, CRU_CLKSEL_CON42::Register>,
    pub cru_clksel_con43: ReadWrite<u32, CRU_CLKSEL_CON43::Register>,
    pub cru_clksel_con44: ReadWrite<u32, CRU_CLKSEL_CON44::Register>,
    pub cru_clksel_con45: ReadWrite<u32, CRU_CLKSEL_CON45::Register>,
    pub cru_clksel_con46: ReadWrite<u32, CRU_CLKSEL_CON46::Register>,
    pub cru_clksel_con47: ReadWrite<u32, CRU_CLKSEL_CON47::Register>,
    pub cru_clksel_con48: ReadWrite<u32, CRU_CLKSEL_CON48::Register>,
    pub cru_clksel_con49: ReadWrite<u32, CRU_CLKSEL_CON49::Register>,
    pub cru_clksel_con50: ReadWrite<u32, CRU_CLKSEL_CON50::Register>,
    pub cru_clksel_con51: ReadWrite<u32, CRU_CLKSEL_CON51::Register>,
    pub cru_clksel_con52: ReadWrite<u32, CRU_CLKSEL_CON52::Register>,
    pub cru_clksel_con53: ReadWrite<u32, CRU_CLKSEL_CON53::Register>,
    pub cru_clksel_con54: ReadWrite<u32, CRU_CLKSEL_CON54::Register>,
    pub cru_clksel_con55: ReadWrite<u32, CRU_CLKSEL_CON55::Register>,
    pub cru_clksel_con56: ReadWrite<u32, CRU_CLKSEL_CON56::Register>,
    pub cru_clksel_con57: ReadWrite<u32, CRU_CLKSEL_CON57::Register>,
    pub cru_clksel_con58: ReadWrite<u32, CRU_CLKSEL_CON58::Register>,
    pub cru_clksel_con59: ReadWrite<u32, CRU_CLKSEL_CON59::Register>,
    pub cru_clksel_con60: ReadWrite<u32, CRU_CLKSEL_CON60::Register>,
    pub cru_clksel_con61: ReadWrite<u32, CRU_CLKSEL_CON61::Register>,
    pub cru_clksel_con62: ReadWrite<u32, CRU_CLKSEL_CON62::Register>,
    pub cru_clksel_con63: ReadWrite<u32, CRU_CLKSEL_CON63::Register>,
    // cru_clksel_con64: ReadWrite<u32, CRU_CLKSEL_CON64::Register>,
    _reserved3: u32,
    pub cru_clksel_con65: ReadWrite<u32, CRU_CLKSEL_CON65::Register>,
    // cru_clksel_con66: ReadWrite<u32, CRU_CLKSEL_CON66::Register>,
    _reserved4: u32,
    pub cru_clksel_con67: ReadWrite<u32, CRU_CLKSEL_CON67::Register>,
    _reserved5: [u32; 5],
    // cru_clksel_con68: ReadWrite<u32, CRU_CLKSEL_CON68::Register>,
    // cru_clksel_con69: ReadWrite<u32, CRU_CLKSEL_CON69::Register>,
    // cru_clksel_con70: ReadWrite<u32, CRU_CLKSEL_CON70::Register>,
    // cru_clksel_con71: ReadWrite<u32, CRU_CLKSEL_CON71::Register>,
    // cru_clksel_con72: ReadWrite<u32, CRU_CLKSEL_CON72::Register>,
    pub cru_clksel_con73: ReadWrite<u32, CRU_CLKSEL_CON73::Register>,
    pub cru_clksel_con74: ReadWrite<u32, CRU_CLKSEL_CON74::Register>,
    // cru_clksel_con75: ReadWrite<u32, CRU_CLKSEL_CON75::Register>,
    // cru_clksel_con76: ReadWrite<u32, CRU_CLKSEL_CON76::Register>,
    _reserved6: [u32; 2],
    pub cru_clksel_con77: ReadWrite<u32, CRU_CLKSEL_CON77::Register>,
    pub cru_clksel_con78: ReadWrite<u32, CRU_CLKSEL_CON78::Register>,
    _reserved7: u32,
    // cru_clksel_con79: ReadWrite<u32, CRU_CLKSEL_CON79::Register>,
    pub cru_clksel_con80: ReadWrite<u32, CRU_CLKSEL_CON80::Register>,
    pub cru_clksel_con81: ReadWrite<u32, CRU_CLKSEL_CON81::Register>,
    pub cru_clksel_con82: ReadWrite<u32, CRU_CLKSEL_CON82::Register>,
    pub cru_clksel_con83: ReadWrite<u32, CRU_CLKSEL_CON83::Register>,
    pub cru_clksel_con84: ReadWrite<u32, CRU_CLKSEL_CON84::Register>,
    pub cru_clksel_con85: ReadWrite<u32, CRU_CLKSEL_CON85::Register>,
    _reserved8: [u32; 3],
    // cru_clksel_con86: ReadWrite<u32, CRU_CLKSEL_CON86::Register>,
    // cru_clksel_con87: ReadWrite<u32, CRU_CLKSEL_CON87::Register>,
    // cru_clksel_con88: ReadWrite<u32, CRU_CLKSEL_CON88::Register>,
    pub cru_clksel_con89: ReadWrite<u32, CRU_CLKSEL_CON89::Register>,
    pub cru_clksel_con90: ReadWrite<u32, CRU_CLKSEL_CON90::Register>,
    pub cru_clksel_con91: ReadWrite<u32, CRU_CLKSEL_CON91::Register>,
    _reserved9: u32,
    // cru_clksel_con92: ReadWrite<u32, CRU_CLKSEL_CON92::Register>,
    pub cru_clksel_con93: ReadWrite<u32, CRU_CLKSEL_CON93::Register>,
    pub cru_clksel_con94: ReadWrite<u32, CRU_CLKSEL_CON94::Register>,
    _reserved10: u32,
    // cru_clksel_con95: ReadWrite<u32, CRU_CLKSEL_CON95::Register>,
    pub cru_clksel_con96: ReadWrite<u32, CRU_CLKSEL_CON96::Register>,
    _reserved11: u32,
    // cru_clksel_con97: ReadWrite<u32, CRU_CLKSEL_CON97::Register>,
    pub cru_clksel_con98: ReadWrite<u32, CRU_CLKSEL_CON98::Register>,
    pub cru_clksel_con99: ReadWrite<u32, CRU_CLKSEL_CON99::Register>,
    pub cru_clksel_con100: ReadWrite<u32, CRU_CLKSEL_CON100::Register>,
    _reserved12: u32,
    // cru_clksel_con101: ReadWrite<u32, CRU_CLKSEL_CON101::Register>,
    pub cru_clksel_con102: ReadWrite<u32, CRU_CLKSEL_CON102::Register>,
    _reserved13: u32,
    // cru_clksel_con103: ReadWrite<u32, CRU_CLKSEL_CON103::Register>,
    cru_clksel_con104: ReadWrite<u32, CRU_CLKSEL_CON104::Register>,
    _reserved14: u32,
    // cru_clksel_con105: ReadWrite<u32, CRU_CLKSEL_CON105::Register>,
    pub cru_clksel_con106: ReadWrite<u32, CRU_CLKSEL_CON106::Register>,
    pub cru_clksel_con107: ReadWrite<u32, CRU_CLKSEL_CON107::Register>,
    pub cru_clksel_con108: ReadWrite<u32, CRU_CLKSEL_CON108::Register>,
    _reserved15: u32,
    // cru_clksel_con109: ReadWrite<u32, CRU_CLKSEL_CON109::Register>,
    pub cru_clksel_con110: ReadWrite<u32, CRU_CLKSEL_CON110::Register>,
    pub cru_clksel_con111: ReadWrite<u32, CRU_CLKSEL_CON111::Register>,
    pub cru_clksel_con112: ReadWrite<u32, CRU_CLKSEL_CON112::Register>,
    pub cru_clksel_con113: ReadWrite<u32, CRU_CLKSEL_CON113::Register>,
    pub cru_clksel_con114: ReadWrite<u32, CRU_CLKSEL_CON114::Register>,
    pub cru_clksel_con115: ReadWrite<u32, CRU_CLKSEL_CON115::Register>,
    pub cru_clksel_con116: ReadWrite<u32, CRU_CLKSEL_CON116::Register>,
    pub cru_clksel_con117: ReadWrite<u32, CRU_CLKSEL_CON117::Register>,
    pub cru_clksel_con118: ReadWrite<u32, CRU_CLKSEL_CON118::Register>,
    pub cru_clksel_con119: ReadWrite<u32, CRU_CLKSEL_CON119::Register>,
    pub cru_clksel_con120: ReadWrite<u32, CRU_CLKSEL_CON120::Register>,
    pub cru_clksel_con121: ReadWrite<u32, CRU_CLKSEL_CON121::Register>,
    pub cru_clksel_con122: ReadWrite<u32, CRU_CLKSEL_CON122::Register>,
    pub cru_clksel_con123: ReadWrite<u32, CRU_CLKSEL_CON123::Register>,
    pub cru_clksel_con124: ReadWrite<u32, CRU_CLKSEL_CON124::Register>,
    pub cru_clksel_con125: ReadWrite<u32, CRU_CLKSEL_CON125::Register>,
    pub cru_clksel_con126: ReadWrite<u32, CRU_CLKSEL_CON126::Register>,
    _reserved16: u32,
    // cru_clksel_con127: ReadWrite<u32, CRU_CLKSEL_CON127::Register>,
    pub cru_clksel_con128: ReadWrite<u32, CRU_CLKSEL_CON128::Register>,
    pub cru_clksel_con129: ReadWrite<u32, CRU_CLKSEL_CON129::Register>,
    pub cru_clksel_con130: ReadWrite<u32, CRU_CLKSEL_CON130::Register>,
    pub cru_clksel_con131: ReadWrite<u32, CRU_CLKSEL_CON131::Register>,
    _reserved17: u32,
    // cru_clksel_con132: ReadWrite<u32, CRU_CLKSEL_CON132::Register>,
    pub cru_clksel_con133: ReadWrite<u32, CRU_CLKSEL_CON133::Register>,
    _reserved18: [u32; 2],
    // cru_clksel_con134: ReadWrite<u32, CRU_CLKSEL_CON134::Register>,
    // cru_clksel_con135: ReadWrite<u32, CRU_CLKSEL_CON135::Register>,
    pub cru_clksel_con136: ReadWrite<u32, CRU_CLKSEL_CON136::Register>,
    _reserved19: u32,
    // cru_clksel_con137: ReadWrite<u32, CRU_CLKSEL_CON137::Register>,
    pub cru_clksel_con138: ReadWrite<u32, CRU_CLKSEL_CON138::Register>,
    pub cru_clksel_con139: ReadWrite<u32, CRU_CLKSEL_CON139::Register>,
    pub cru_clksel_con140: ReadWrite<u32, CRU_CLKSEL_CON140::Register>,
    pub cru_clksel_con141: ReadWrite<u32, CRU_CLKSEL_CON141::Register>,
    pub cru_clksel_con142: ReadWrite<u32, CRU_CLKSEL_CON142::Register>,
    _reserved20: u32,
    // cru_clksel_con143: ReadWrite<u32, CRU_CLKSEL_CON143::Register>,
    pub cru_clksel_con144: ReadWrite<u32, CRU_CLKSEL_CON144::Register>,
    pub cru_clksel_con145: ReadWrite<u32, CRU_CLKSEL_CON145::Register>,
    pub cru_clksel_con146: ReadWrite<u32, CRU_CLKSEL_CON146::Register>,
    pub cru_clksel_con147: ReadWrite<u32, CRU_CLKSEL_CON147::Register>,
    pub cru_clksel_con148: ReadWrite<u32, CRU_CLKSEL_CON148::Register>,
    pub cru_clksel_con149: ReadWrite<u32, CRU_CLKSEL_CON149::Register>,
    pub cru_clksel_con150: ReadWrite<u32, CRU_CLKSEL_CON150::Register>,
    pub cru_clksel_con151: ReadWrite<u32, CRU_CLKSEL_CON151::Register>,
    pub cru_clksel_con152: ReadWrite<u32, CRU_CLKSEL_CON152::Register>,
    pub cru_clksel_con153: ReadWrite<u32, CRU_CLKSEL_CON153::Register>,
    pub cru_clksel_con154: ReadWrite<u32, CRU_CLKSEL_CON154::Register>,
    pub cru_clksel_con155: ReadWrite<u32, CRU_CLKSEL_CON155::Register>,
    pub cru_clksel_con156: ReadWrite<u32, CRU_CLKSEL_CON156::Register>,
    pub cru_clksel_con157: ReadWrite<u32, CRU_CLKSEL_CON157::Register>,
    pub cru_clksel_con158: ReadWrite<u32, CRU_CLKSEL_CON158::Register>,
    pub cru_clksel_con159: ReadWrite<u32, CRU_CLKSEL_CON159::Register>,
    pub cru_clksel_con160: ReadWrite<u32, CRU_CLKSEL_CON160::Register>,
    pub cru_clksel_con161: ReadWrite<u32, CRU_CLKSEL_CON161::Register>,
    _reserved21: u32,
    // cru_clksel_con162: ReadWrite<u32, CRU_CLKSEL_CON162::Register>,
    pub cru_clksel_con163: ReadWrite<u32, CRU_CLKSEL_CON163::Register>,
    _reserved22: u32,
    // cru_clksel_con164: ReadWrite<u32, CRU_CLKSEL_CON164::Register>,
    pub cru_clksel_con165: ReadWrite<u32, CRU_CLKSEL_CON165::Register>,
    pub cru_clksel_con166: ReadWrite<u32, CRU_CLKSEL_CON166::Register>,
    _reserved23: [u32; 3],
    // cru_clksel_con167: ReadWrite<u32, CRU_CLKSEL_CON167::Register>,
    // cru_clksel_con168: ReadWrite<u32, CRU_CLKSEL_CON168::Register>,
    // cru_clksel_con169: ReadWrite<u32, CRU_CLKSEL_CON169::Register>,
    pub cru_clksel_con170: ReadWrite<u32, CRU_CLKSEL_CON170::Register>,
    _reserved24: u32,
    // cru_clksel_con171: ReadWrite<u32, CRU_CLKSEL_CON171::Register>,
    pub cru_clksel_con172: ReadWrite<u32, CRU_CLKSEL_CON172::Register>,
    _reserved25: u32,
    // cru_clksel_con173: ReadWrite<u32, CRU_CLKSEL_CON173::Register>,
    pub cru_clksel_con174: ReadWrite<u32, CRU_CLKSEL_CON174::Register>,
    _reserved26: u32,
    // cru_clksel_con175: ReadWrite<u32, CRU_CLKSEL_CON175::Register>,
    pub cru_clksel_con176: ReadWrite<u32, CRU_CLKSEL_CON176::Register>,
    pub cru_clksel_con177: ReadWrite<u32, CRU_CLKSEL_CON177::Register>,
    _reserved27: [u32; 10],
}

// CRU_CLKSEL_CON00  0x0300;
register_bitfields![u32,
    CRU_CLKSEL_CON00 [
        clk_matrix_50m_src_div OFFSET(0) NUMBITS(5) [],
        clk_matrix_50m_src_sel OFFSET(5) NUMBITS(1) [],
        clk_matrix_100m_src_div OFFSET(6) NUMBITS(5) [],
        clk_matrix_100m_src_sel OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON01  0x0304;
register_bitfields![u32,
    CRU_CLKSEL_CON01 [
        clk_matrix_150m_src_div OFFSET(0) NUMBITS(5) [],
        clk_matrix_150m_src_sel OFFSET(5) NUMBITS(1) [],
        clk_matrix_200m_src_div OFFSET(6) NUMBITS(5) [],
        clk_matrix_200m_src_sel OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON02  0x0308;
register_bitfields![u32,
    CRU_CLKSEL_CON02 [
        clk_matrix_250m_src_div OFFSET(0) NUMBITS(5) [],
        clk_matrix_250m_src_sel OFFSET(5) NUMBITS(1) [],
        clk_matrix_300m_src_div OFFSET(6) NUMBITS(5) [],
        clk_matrix_300m_src_sel OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON03  0x030C;
register_bitfields![u32,
    CRU_CLKSEL_CON03 [
        clk_matrix_350m_src_div OFFSET(0) NUMBITS(5) [],
        clk_matrix_350m_src_sel OFFSET(5) NUMBITS(1) [],
        clk_matrix_400m_src_div OFFSET(6) NUMBITS(5) [],
        clk_matrix_400m_src_sel OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON04  0x0310;
register_bitfields![u32,
    CRU_CLKSEL_CON04 [
        clk_matrix_450m_src_div OFFSET(0) NUMBITS(5) [],
        clk_matrix_450m_src_sel OFFSET(5) NUMBITS(1) [],
        clk_matrix_500m_src_div OFFSET(6) NUMBITS(5) [],
        clk_matrix_500m_src_sel OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON05  0x0314;
register_bitfields![u32,
    CRU_CLKSEL_CON05 [
        clk_matrix_600m_src_div OFFSET(0) NUMBITS(5) [],
        clk_matrix_600m_src_sel OFFSET(5) NUMBITS(1) [],
        clk_matrix_650m_src_div OFFSET(6) NUMBITS(5) [],
        clk_matrix_650m_src_sel OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON06  0x0318;
register_bitfields![u32,
    CRU_CLKSEL_CON06 [
        clk_matrix_700m_src_div OFFSET(0) NUMBITS(5) [],
        clk_matrix_700m_src_sel OFFSET(5) NUMBITS(1) [],
        clk_matrix_800m_src_div OFFSET(6) NUMBITS(5) [],
        clk_matrix_800m_src_sel OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON07  0x031C;
register_bitfields![u32,
    CRU_CLKSEL_CON07 [
        clk_matrix_1000m_src_div OFFSET(0) NUMBITS(5) [],
        clk_matrix_1000m_src_sel OFFSET(5) NUMBITS(1) [],
        clk_matrix_1200m_src_div OFFSET(6) NUMBITS(5) [],
        clk_matrix_1200m_src_sel OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON08  0x0320;
register_bitfields![u32,
    CRU_CLKSEL_CON08 [
        aclk_top_root_div OFFSET(0) NUMBITS(5) [],
        aclk_top_root_sel OFFSET(5) NUMBITS(2) [],
        pclk_top_root_sel OFFSET(7) NUMBITS(2) [],
        aclk_low_top_root_div OFFSET(9) NUMBITS(5) [],
        aclk_low_top_root_sel OFFSET(14) NUMBITS(1) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON09  0x0324;
register_bitfields![u32,
    CRU_CLKSEL_CON09 [
        aclk_top_m300_root_sel OFFSET(0) NUMBITS(2) [],
        aclk_top_m500_root_sel OFFSET(2) NUMBITS(2) [],
        aclk_top_m400_root_sel OFFSET(4) NUMBITS(2) [],
        aclk_top_s200_root_sel OFFSET(6) NUMBITS(2) [],
        aclk_top_s400_root_sel OFFSET(8) NUMBITS(2) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON10  0x0328;
register_bitfields![u32,
    CRU_CLKSEL_CON10 [
        clk_testout_top_div OFFSET(0) NUMBITS(6) [],
        clk_testout_top_sel OFFSET(6) NUMBITS(3) [],
        clk_testout_sel OFFSET(9) NUMBITS(3) [],
        clk_testout_grp0_sel OFFSET(12) NUMBITS(3) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON15  0x033C;
register_bitfields![u32,
    CRU_CLKSEL_CON15 [
        mclk_gmac0_out_div OFFSET(0) NUMBITS(7) [],
        mclk_gmac0_out_sel OFFSET(7) NUMBITS(1) [],
        refclko25m_eth0_out_div OFFSET(8) NUMBITS(7) [],
        refclko25m_eth0_out_sel OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON16  0x0340;
register_bitfields![u32,
    CRU_CLKSEL_CON16 [
        refclko25m_eth1_out_div OFFSET(0) NUMBITS(7) [],
        refclko25m_eth1_out_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON17  0x0344;
register_bitfields![u32,
    CRU_CLKSEL_CON17 [
        clk_cifout_out_div OFFSET(0) NUMBITS(8) [],
        clk_cifout_out_sel OFFSET(8) NUMBITS(2) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON18  0x0348;
register_bitfields![u32,
    CRU_CLKSEL_CON18 [
        clk_mipi_camaraout_m0_div OFFSET(0) NUMBITS(8) [],
        clk_mipi_camaraout_m0_sel OFFSET(8) NUMBITS(2) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON19  0x034C;
register_bitfields![u32,
    CRU_CLKSEL_CON19 [
        clk_mipi_camaraout_m1_div OFFSET(0) NUMBITS(8) [],
        clk_mipi_camaraout_m1_sel OFFSET(8) NUMBITS(2) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON20  0x0350;
register_bitfields![u32,
    CRU_CLKSEL_CON20 [
        clk_mipi_camaraout_m2_div OFFSET(0) NUMBITS(8) [],
        clk_mipi_camaraout_m2_sel OFFSET(8) NUMBITS(2) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON21  0x0354;
register_bitfields![u32,
    CRU_CLKSEL_CON21 [
        clk_mipi_camaraout_m3_div OFFSET(0) NUMBITS(8) [],
        clk_mipi_camaraout_m3_sel OFFSET(8) NUMBITS(2) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON22  0x0358;
register_bitfields![u32,
    CRU_CLKSEL_CON22 [
        clk_mipi_camaraout_m4_div OFFSET(0) NUMBITS(8) [],
        clk_mipi_camaraout_m4_sel OFFSET(8) NUMBITS(2) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON24  0x0360;
register_bitfields![u32,
    CRU_CLKSEL_CON24 [
        hclk_audio_root_sel OFFSET(0) NUMBITS(2) [],
        pclk_audio_root_sel OFFSET(2) NUMBITS(2) [],
        clk_i2s0_8ch_tx_src_div OFFSET(4) NUMBITS(5) [],
        clk_i2s0_8ch_tx_src_sel OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON25  0x0364;
register_bitfields![u32,
    CRU_CLKSEL_CON25 [
        clk_i2s0_8ch_tx_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON26  0x0368;
register_bitfields![u32,
    CRU_CLKSEL_CON26 [
        mclk_i2s0_8ch_tx_sel OFFSET(0) NUMBITS(2) [],
        clk_i2s0_8ch_rx_src_div OFFSET(2) NUMBITS(5) [],
        clk_i2s0_8ch_rx_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON27  0x036C;
register_bitfields![u32,
    CRU_CLKSEL_CON27 [
        clk_i2s0_8ch_rx_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON28  0x0370;
register_bitfields![u32,
    CRU_CLKSEL_CON28 [
        mclk_i2s0_8ch_rx_sel OFFSET(0) NUMBITS(2) [],
        i2s0_8ch_mclkout_sel OFFSET(2) NUMBITS(2) [],
        clk_i2s2_2ch_src_div OFFSET(4) NUMBITS(5) [],
        clk_i2s2_2ch_src_sel OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON29  0x0374;
register_bitfields![u32,
    CRU_CLKSEL_CON29 [
        clk_i2s2_2ch_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON30  0x0378;
register_bitfields![u32,
    CRU_CLKSEL_CON30 [
        mclk_i2s2_2ch_sel OFFSET(0) NUMBITS(2) [],
        i2s2_2ch_mclkout_sel OFFSET(2) NUMBITS(1) [],
        clk_i2s3_2ch_src_div OFFSET(3) NUMBITS(5) [],
        clk_i2s3_2ch_src_sel OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON31  0x037C;
register_bitfields![u32,
    CRU_CLKSEL_CON31 [
        clk_i2s3_2ch_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON32  0x0380;
register_bitfields![u32,
    CRU_CLKSEL_CON32 [
        mclk_i2s3_2ch_sel OFFSET(0) NUMBITS(2) [],
        i2s3_2ch_mclkout_sel OFFSET(2) NUMBITS(1) [],
        clk_spdif0_src_div OFFSET(3) NUMBITS(5) [],
        clk_spdif0_src_sel OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON33  0x0384;
register_bitfields![u32,
    CRU_CLKSEL_CON33 [
        clk_spdif0_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON34  0x0388;
register_bitfields![u32,
    CRU_CLKSEL_CON34 [
        mclk_spdif0_sel OFFSET(0) NUMBITS(2) [],
        clk_spdif1_src_div OFFSET(2) NUMBITS(5) [],
        clk_spdif1_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON35  0x038C;
register_bitfields![u32,
    CRU_CLKSEL_CON35 [
        clk_spdif1_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON36  0x0390;
register_bitfields![u32,
    CRU_CLKSEL_CON36 [
        mclk_spdif1_sel OFFSET(0) NUMBITS(2) [],
        mclk_pdm1_div OFFSET(2) NUMBITS(5) [],
        mclk_pdm1_sel OFFSET(7) NUMBITS(2) [],
        _reserved OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON38  0x0398;
register_bitfields![u32,
    CRU_CLKSEL_CON38 [
        aclk_bus_root_div OFFSET(0) NUMBITS(5) [],
        aclk_bus_root_sel OFFSET(5) NUMBITS(1) [],
        clk_i2c1_sel OFFSET(6) NUMBITS(1) [],
        clk_i2c2_sel OFFSET(7) NUMBITS(1) [],
        clk_i2c3_sel OFFSET(8) NUMBITS(1) [],
        clk_i2c4_sel OFFSET(9) NUMBITS(1) [],
        clk_i2c5_sel OFFSET(10) NUMBITS(1) [],
        clk_i2c6_sel OFFSET(11) NUMBITS(1) [],
        clk_i2c7_sel OFFSET(12) NUMBITS(1) [],
        clk_i2c8_sel OFFSET(13) NUMBITS(1) [],
        _reserved OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON39  0x039C;
register_bitfields![u32,
    CRU_CLKSEL_CON39 [
        clk_can0_div OFFSET(0) NUMBITS(5) [],
        clk_can0_sel OFFSET(5) NUMBITS(1) [],
        clk_can1_div OFFSET(6) NUMBITS(5) [],
        clk_can1_sel OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON40  0x03A0;
register_bitfields![u32,
    CRU_CLKSEL_CON40 [
        clk_can2_div OFFSET(0) NUMBITS(5) [],
        clk_can2_sel OFFSET(5) NUMBITS(1) [],
        clk_saradc_div OFFSET(6) NUMBITS(8) [],
        clk_saradc_sel OFFSET(14) NUMBITS(1) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON41  0x03A4;
register_bitfields![u32,
    CRU_CLKSEL_CON41 [
        clk_tsadc_div OFFSET(0) NUMBITS(8) [],
        clk_tsadc_sel OFFSET(8) NUMBITS(1) [],
        clk_uart1_src_div OFFSET(9) NUMBITS(5) [],
        clk_uart1_src_sel OFFSET(14) NUMBITS(1) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON42  0x03A8;
register_bitfields![u32,
    CRU_CLKSEL_CON42 [
        clk_uart1_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON43  0x03AC;
register_bitfields![u32,
    CRU_CLKSEL_CON43 [
        sclk_uart1_sel OFFSET(0) NUMBITS(2) [],
        clk_uart2_src_div OFFSET(2) NUMBITS(5) [],
        clk_uart2_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON44  0x03B0;
register_bitfields![u32,
    CRU_CLKSEL_CON44 [
        clk_uart2_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON45  0x03B4;
register_bitfields![u32,
    CRU_CLKSEL_CON45 [
        sclk_uart2_sel OFFSET(0) NUMBITS(2) [],
        clk_uart3_src_div OFFSET(2) NUMBITS(5) [],
        clk_uart3_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON46  0x03B8;
register_bitfields![u32,
    CRU_CLKSEL_CON46 [
        clk_uart3_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON47  0x03BC;
register_bitfields![u32,
    CRU_CLKSEL_CON47 [
        sclk_uart3_sel OFFSET(0) NUMBITS(2) [],
        clk_uart4_src_div OFFSET(2) NUMBITS(5) [],
        clk_uart4_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON48  0x03C0;
register_bitfields![u32,
    CRU_CLKSEL_CON48 [
        clk_uart4_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON49  0x03C4;
register_bitfields![u32,
    CRU_CLKSEL_CON49 [
        sclk_uart4_sel OFFSET(0) NUMBITS(2) [],
        clk_uart5_src_div OFFSET(2) NUMBITS(5) [],
        clk_uart5_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON50  0x03C8;
register_bitfields![u32,
    CRU_CLKSEL_CON50 [
        clk_uart5_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON51  0x03CC;
register_bitfields![u32,
    CRU_CLKSEL_CON51 [
        sclk_uart5_sel OFFSET(0) NUMBITS(2) [],
        clk_uart6_src_div OFFSET(2) NUMBITS(5) [],
        clk_uart6_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON52  0x03D0;
register_bitfields![u32,
    CRU_CLKSEL_CON52 [
        clk_uart6_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON53  0x03D4;
register_bitfields![u32,
    CRU_CLKSEL_CON53 [
        sclk_uart6_sel OFFSET(0) NUMBITS(2) [],
        clk_uart7_src_div OFFSET(2) NUMBITS(5) [],
        clk_uart7_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON54  0x03D8;
register_bitfields![u32,
    CRU_CLKSEL_CON54 [
        clk_uart7_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON55  0x03DC;
register_bitfields![u32,
    CRU_CLKSEL_CON55 [
        sclk_uart7_sel OFFSET(0) NUMBITS(2) [],
        clk_uart8_src_div OFFSET(2) NUMBITS(5) [],
        clk_uart8_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON56  0x03E0;
register_bitfields![u32,
    CRU_CLKSEL_CON56 [
        clk_uart8_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON57  0x03E4;
register_bitfields![u32,
    CRU_CLKSEL_CON57 [
        sclk_uart8_sel OFFSET(0) NUMBITS(2) [],
        clk_uart9_src_div OFFSET(2) NUMBITS(5) [],
        clk_uart9_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON58  0x03E8;
register_bitfields![u32,
    CRU_CLKSEL_CON58 [
        clk_uart9_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON59  0x03EC;
register_bitfields![u32,
    CRU_CLKSEL_CON59 [
        sclk_uart9_sel OFFSET(0) NUMBITS(2) [],
        clk_spi0_sel OFFSET(2) NUMBITS(2) [],
        clk_spi1_sel OFFSET(4) NUMBITS(2) [],
        clk_spi2_sel OFFSET(6) NUMBITS(2) [],
        clk_spi3_sel OFFSET(8) NUMBITS(2) [],
        clk_spi4_sel OFFSET(10) NUMBITS(2) [],
        clk_pwm1_sel OFFSET(12) NUMBITS(2) [],
        clk_pwm2_sel OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON60  0x03F0;
register_bitfields![u32,
    CRU_CLKSEL_CON60 [
        clk_pwm3_sel OFFSET(0) NUMBITS(2) [],
        clk_bus_timer_root_sel OFFSET(2) NUMBITS(1) [],
        dbclk_gpio1_div OFFSET(3) NUMBITS(5) [],
        dbclk_gpio1_sel OFFSET(8) NUMBITS(1) [],
        dbclk_gpio2_div OFFSET(9) NUMBITS(5) [],
        dbclk_gpio2_sel OFFSET(14) NUMBITS(1) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON61  0x03F4;
register_bitfields![u32,
    CRU_CLKSEL_CON61 [
        dbclk_gpio3_div OFFSET(0) NUMBITS(5) [],
        dbclk_gpio3_sel OFFSET(5) NUMBITS(1) [],
        dbclk_gpio4_div OFFSET(6) NUMBITS(5) [],
        dbclk_gpio4_sel OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON62  0x03F8;
register_bitfields![u32,
    CRU_CLKSEL_CON62 [
        dclk_decom_div OFFSET(0) NUMBITS(5) [],
        dclk_decom_sel OFFSET(5) NUMBITS(1) [],
        clk_bisrintf_pllsrc_div OFFSET(6) NUMBITS(5) [],
        _reserved OFFSET(11) NUMBITS(5) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON63  0x03FC;
register_bitfields![u32,
    CRU_CLKSEL_CON63 [
        clk_testout_ddr01_div OFFSET(0) NUMBITS(6) [],
        clk_testout_ddr01_sel OFFSET(6) NUMBITS(1) [],
        _reserved OFFSET(7) NUMBITS(9) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON65  0x0404;
register_bitfields![u32,
    CRU_CLKSEL_CON65 [
        clk_testout_ddr23_div OFFSET(0) NUMBITS(6) [],
        clk_testout_ddr23_sel OFFSET(6) NUMBITS(1) [],
        _reserved OFFSET(7) NUMBITS(9) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON67  0x040C;
register_bitfields![u32,
    CRU_CLKSEL_CON67 [
        aclk_isp1_root_div OFFSET(0) NUMBITS(5) [],
        aclk_isp1_root_sel OFFSET(5) NUMBITS(2) [],
        hclk_isp1_root_sel OFFSET(7) NUMBITS(2) [],
        clk_isp1_core_div OFFSET(9) NUMBITS(5) [],
        clk_isp1_core_sel OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON73  0x0424;
register_bitfields![u32,
    CRU_CLKSEL_CON73 [
        hclk_rknn_root_sel OFFSET(0) NUMBITS(2) [],
        clk_rknn_dsu0_src_t_div OFFSET(2) NUMBITS(5) [],
        clk_rknn_dsu0_src_t_sel OFFSET(7) NUMBITS(3) [],
        clk_testout_npu_div OFFSET(10) NUMBITS(5) [],
        clk_testout_npu_sel OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON74  0x0428;
register_bitfields![u32,
    CRU_CLKSEL_CON74 [
        clk_rknn_dsu0_sel OFFSET(0) NUMBITS(1) [],
        pclk_nputop_root_sel OFFSET(1) NUMBITS(2) [],
        clk_nputimer_root_sel OFFSET(3) NUMBITS(1) [],
        clk_npu_pvtpll_sel OFFSET(4) NUMBITS(1) [],
        hclk_npu_cm0_root_sel OFFSET(5) NUMBITS(2) [],
        clk_npu_cm0_rtc_div OFFSET(7) NUMBITS(5) [],
        clk_npu_cm0_rtc_sel OFFSET(12) NUMBITS(1) [],
        _reserved OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON77  0x0434;
register_bitfields![u32,
    CRU_CLKSEL_CON77 [
        hclk_nvm_root_sel OFFSET(0) NUMBITS(2) [],
        aclk_nvm_root_div OFFSET(2) NUMBITS(5) [],
        aclk_nvm_root_sel OFFSET(7) NUMBITS(1) [],
        cclk_emmc_div OFFSET(8) NUMBITS(6) [],
        cclk_emmc_sel OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON78  0x0438;
register_bitfields![u32,
    CRU_CLKSEL_CON78 [
        bclk_emmc_div OFFSET(0) NUMBITS(5) [],
        bclk_emmc_sel OFFSET(5) NUMBITS(1) [],
        sclk_sfc_div OFFSET(6) NUMBITS(6) [],
        sclk_sfc_sel OFFSET(12) NUMBITS(2) [],
        _reserved OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON80  0x0440;
register_bitfields![u32,
    CRU_CLKSEL_CON80 [
        pclk_php_root_sel OFFSET(0) NUMBITS(2) [],
        aclk_pcie_root_div OFFSET(2) NUMBITS(5) [],
        aclk_pcie_root_sel OFFSET(7) NUMBITS(1) [],
        aclk_php_root_div OFFSET(8) NUMBITS(5) [],
        aclk_php_root_sel OFFSET(13) NUMBITS(1) [],
        _reserved OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON81  0x0444;
register_bitfields![u32,
    CRU_CLKSEL_CON81 [
        clk_gmac0_ptp_ref_div OFFSET(0) NUMBITS(6) [],
        clk_gmac0_ptp_ref_sel OFFSET(6) NUMBITS(1) [],
        clk_gmac1_ptp_ref_div OFFSET(7) NUMBITS(6) [],
        clk_gmac1_ptp_ref_sel OFFSET(13) NUMBITS(1) [],
        _reserved OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON82  0x0448;
register_bitfields![u32,
    CRU_CLKSEL_CON82 [
        clk_rxoob0_div OFFSET(0) NUMBITS(7) [],
        clk_rxoob0_sel OFFSET(7) NUMBITS(1) [],
        clk_rxoob1_div OFFSET(8) NUMBITS(7) [],
        clk_rxoob1_sel OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON83  0x044C;
register_bitfields![u32,
    CRU_CLKSEL_CON83 [
        clk_rxoob2_div OFFSET(0) NUMBITS(7) [],
        clk_rxoob2_sel OFFSET(7) NUMBITS(1) [],
        clk_gmac_125m_cru_i_div OFFSET(8) NUMBITS(7) [],
        clk_gmac_125m_cru_i_sel OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON84  0x0450;
register_bitfields![u32,
    CRU_CLKSEL_CON84 [
        clk_gmac_50m_cru_i_div OFFSET(0) NUMBITS(7) [],
        clk_gmac_50m_cru_i_sel OFFSET(7) NUMBITS(1) [],
        clk_utmi_otg2_div OFFSET(8) NUMBITS(4) [],
        clk_utmi_otg2_sel OFFSET(12) NUMBITS(2) [],
        _reserved OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON85  0x0454;
register_bitfields![u32,
    CRU_CLKSEL_CON85 [
        clk_gmac0_tx_125m_o_div OFFSET(0) NUMBITS(6) [],
        clk_gmac1_tx_125m_o_div OFFSET(6) NUMBITS(6) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON89  0x0464;
register_bitfields![u32,
    CRU_CLKSEL_CON89 [
        hclk_rkvdec0_root_sel OFFSET(0) NUMBITS(2) [],
        aclk_rkvdec0_root_div OFFSET(2) NUMBITS(5) [],
        aclk_rkvdec0_root_sel OFFSET(7) NUMBITS(2) [],
        aclk_rkvdec_ccu_div OFFSET(9) NUMBITS(5) [],
        aclk_rkvdec_ccu_sel OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON90  0x0468;
register_bitfields![u32,
    CRU_CLKSEL_CON90 [
        clk_rkvdec0_ca_div OFFSET(0) NUMBITS(5) [],
        clk_rkvdec0_ca_sel OFFSET(5) NUMBITS(1) [],
        clk_rkvdec0_hevc_ca_div OFFSET(6) NUMBITS(5) [],
        clk_rkvdec0_hevc_ca_sel OFFSET(11) NUMBITS(2) [],
        _reserved OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON91  0x046C;
register_bitfields![u32,
    CRU_CLKSEL_CON91 [
        clk_rkvdec0_core_div OFFSET(0) NUMBITS(5) [],
        clk_rkvdec0_core_sel OFFSET(5) NUMBITS(1) [],
        _reserved OFFSET(6) NUMBITS(10) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON93  0x0474;
register_bitfields![u32,
    CRU_CLKSEL_CON93 [
        hclk_rkvdec1_root_sel OFFSET(0) NUMBITS(2) [],
        aclk_rkvdec1_root_div OFFSET(2) NUMBITS(5) [],
        aclk_rkvdec1_root_sel OFFSET(7) NUMBITS(2) [],
        clk_rkvdec1_ca_div OFFSET(9) NUMBITS(5) [],
        clk_rkvdec1_ca_sel OFFSET(14) NUMBITS(1) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON94  0x0478;
register_bitfields![u32,
    CRU_CLKSEL_CON94 [
        clk_rkvdec1_hevc_ca_div OFFSET(0) NUMBITS(5) [],
        clk_rkvdec1_hevc_ca_sel OFFSET(5) NUMBITS(2) [],
        clk_rkvdec1_core_div OFFSET(7) NUMBITS(5) [],
        clk_rkvdec1_core_sel OFFSET(12) NUMBITS(1) [],
        _reserved OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON96  0x0480;
register_bitfields![u32,
    CRU_CLKSEL_CON96 [
        aclk_usb_root_div OFFSET(0) NUMBITS(5) [],
        aclk_usb_root_sel OFFSET(5) NUMBITS(1) [],
        hclk_usb_root_sel OFFSET(6) NUMBITS(2) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON98  0x0488;
register_bitfields![u32,
    CRU_CLKSEL_CON98 [
        aclk_vdpu_root_div OFFSET(0) NUMBITS(5) [],
        aclk_vdpu_root_sel OFFSET(5) NUMBITS(2) [],
        aclk_vdpu_low_root_sel OFFSET(7) NUMBITS(2) [],
        hclk_vdpu_root_sel OFFSET(9) NUMBITS(2) [],
        _reserved OFFSET(11) NUMBITS(5) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON99  0x048C;
register_bitfields![u32,
    CRU_CLKSEL_CON99 [
        aclk_jpeg_decoder_root_div OFFSET(0) NUMBITS(5) [],
        aclk_jpeg_decoder_root_sel OFFSET(5) NUMBITS(2) [],
        resclk_iep2p0_core_diverved OFFSET(7) NUMBITS(5) [],
        clk_iep2p0_core_sel OFFSET(12) NUMBITS(1) [],
        _reserved OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON100  0x0490;
register_bitfields![u32,
    CRU_CLKSEL_CON100 [
        clk_rga2_core_div OFFSET(0) NUMBITS(5) [],
        clk_rga2_core_sel OFFSET(5) NUMBITS(3) [],
        clk_rga3_0_core_div OFFSET(8) NUMBITS(5) [],
        clk_rga3_0_core_sel OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON102  0x0498;
register_bitfields![u32,
    CRU_CLKSEL_CON102 [
        hclk_rkvenc0_root_sel OFFSET(0) NUMBITS(2) [],
        aclk_rkvenc0_root_div OFFSET(2) NUMBITS(5) [],
        aclk_rkvenc0_root_sel OFFSET(7) NUMBITS(2) [],
        clk_rkvenc0_core_div OFFSET(9) NUMBITS(5) [],
        clk_rkvenc0_core_sel OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON104  0x04A0;
register_bitfields![u32,
    CRU_CLKSEL_CON104 [
        hclk_rkvenc1_root_sel OFFSET(0) NUMBITS(2) [],
        aclk_rkvenc1_root_div OFFSET(2) NUMBITS(5) [],
        aclk_rkvenc1_root_sel OFFSET(7) NUMBITS(2) [],
        clk_rkvenc1_core_div OFFSET(9) NUMBITS(5) [],
        clk_rkvenc1_core_sel OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON106  0x04A8;
register_bitfields![u32,
    CRU_CLKSEL_CON106 [
        aclk_vi_root_div OFFSET(0) NUMBITS(5) [],
        aclk_vi_root_sel OFFSET(5) NUMBITS(3) [],
        hclk_vi_root_sel OFFSET(8) NUMBITS(2) [],
        pclk_vi_root_sel OFFSET(10) NUMBITS(2) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON107  0x04AC;
register_bitfields![u32,
    CRU_CLKSEL_CON107 [
        dclk_vicap_div OFFSET(0) NUMBITS(5) [],
        dclk_vicap_sel OFFSET(5) NUMBITS(1) [],
        clk_isp0_core_div OFFSET(6) NUMBITS(5) [],
        clk_isp0_core_sel OFFSET(11) NUMBITS(2) [],
        _reserved OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON108  0x04B0;
register_bitfields![u32,
    CRU_CLKSEL_CON108 [
        clk_fisheye0_core_div OFFSET(0) NUMBITS(5) [],
        clk_fisheye0_core_sel OFFSET(5) NUMBITS(2) [],
        clk_fisheye1_core_div OFFSET(7) NUMBITS(5) [],
        clk_fisheye1_core_sel OFFSET(12) NUMBITS(2) [],
        iclk_csihost01_sel OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON110  0x04B8;
register_bitfields![u32,
    CRU_CLKSEL_CON110 [
        aclk_vop_root_div OFFSET(0) NUMBITS(5) [],
        aclk_vop_root_sel OFFSET(5) NUMBITS(3) [],
        aclk_vop_low_root_sel OFFSET(8) NUMBITS(2) [],
        hclk_vop_root_sel OFFSET(10) NUMBITS(2) [],
        pclk_vop_root_sel OFFSET(12) NUMBITS(2) [],
        _reserved OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON111  0x04BC;
register_bitfields![u32,
    CRU_CLKSEL_CON111 [
        dclk_vp0_src_div OFFSET(0) NUMBITS(7) [],
        dclk_vp0_src_sel OFFSET(7) NUMBITS(2) [],
        dclk_vp1_src_div OFFSET(9) NUMBITS(5) [],
        dclk_vp1_src_sel OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON112  0x04C0;
register_bitfields![u32,
    CRU_CLKSEL_CON112 [
        dclk_vp2_src_div OFFSET(0) NUMBITS(5) [],
        dclk_vp2_src_sel OFFSET(5) NUMBITS(2) [],
        dclk_vp0_sel OFFSET(7) NUMBITS(2) [],
        dclk_vp1_sel OFFSET(9) NUMBITS(2) [],
        dclk_vp2_sel OFFSET(11) NUMBITS(2) [],
        _reserved OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON113  0x04C4;
register_bitfields![u32,
    CRU_CLKSEL_CON113 [
        dclk_vp3_div OFFSET(0) NUMBITS(7) [],
        dclk_vp3_sel OFFSET(7) NUMBITS(2) [],
        _reserved OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON114  0x04C8;
register_bitfields![u32,
    CRU_CLKSEL_CON114 [
        clk_dsihost0_div OFFSET(0) NUMBITS(7) [],
        clk_dsihost0_sel OFFSET(7) NUMBITS(2) [],
        _reserved OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON115  0x04CC;
register_bitfields![u32,
    CRU_CLKSEL_CON115 [
        clk_dsihost1_div OFFSET(0) NUMBITS(7) [],
        clk_dsihost1_sel OFFSET(7) NUMBITS(2) [],
        aclk_vop_sub_src_sel OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON116  0x04D0;
register_bitfields![u32,
    CRU_CLKSEL_CON116 [
        aclk_vo0_root_div OFFSET(0) NUMBITS(5) [],
        aclk_vo0_root_sel OFFSET(5) NUMBITS(1) [],
        hclk_vo0_root_sel OFFSET(6) NUMBITS(2) [],
        hclk_vo0_s_root_sel OFFSET(8) NUMBITS(2) [],
        pclk_vo0_root_sel OFFSET(10) NUMBITS(2) [],
        pclk_vo0_s_root_sel OFFSET(12) NUMBITS(2) [],
        _reserved OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON117  0x04D4;
register_bitfields![u32,
    CRU_CLKSEL_CON117 [
        clk_aux16mhz_0_div OFFSET(0) NUMBITS(8) [],
        clk_aux16mhz_1_div OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON118  0x04D8;
register_bitfields![u32,
    CRU_CLKSEL_CON118 [
        clk_i2s4_8ch_tx_src_div OFFSET(0) NUMBITS(5) [],
        clk_i2s4_8ch_tx_src_sel OFFSET(5) NUMBITS(1) [],
        _reserved OFFSET(6) NUMBITS(10) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON119  0x04DC;
register_bitfields![u32,
    CRU_CLKSEL_CON119 [
        clk_i2s4_8ch_tx_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON120  0x04E0;
register_bitfields![u32,
    CRU_CLKSEL_CON120 [
        mclk_i2s4_8ch_tx_sel OFFSET(0) NUMBITS(2) [],
        _reserved0 OFFSET(2) NUMBITS(1) [],
        clk_i2s8_8ch_tx_src_div OFFSET(3) NUMBITS(5) [],
        clk_i2s8_8ch_tx_src_sel OFFSET(8) NUMBITS(1) [],
        _reserved1 OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON121  0x04E4;
register_bitfields![u32,
    CRU_CLKSEL_CON121 [
        clk_i2s8_8ch_tx_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON122  0x04E8;
register_bitfields![u32,
    CRU_CLKSEL_CON122 [
        mclk_i2s8_8ch_tx_sel OFFSET(0) NUMBITS(2) [],
        _reserved0 OFFSET(2) NUMBITS(1) [],
        clk_spdif2_dp0_src_div OFFSET(3) NUMBITS(5) [],
        clk_spdif2_dp0_src_sel OFFSET(8) NUMBITS(1) [],
        _reserved1 OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON123  0x04EC;
register_bitfields![u32,
    CRU_CLKSEL_CON123 [
        clk_spdif2_dp0_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON124  0x04F0;
register_bitfields![u32,
    CRU_CLKSEL_CON124 [
        resmclk_4x_spdif2_dp0_selerved OFFSET(0) NUMBITS(2) [],
        clk_spdif5_dp1_src_div OFFSET(2) NUMBITS(5) [],
        clk_spdif5_dp1_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON125  0x04F4;
register_bitfields![u32,
    CRU_CLKSEL_CON125 [
        clk_spdif5_dp1_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON126  0x04F8;
register_bitfields![u32,
    CRU_CLKSEL_CON126 [
        mclk_4x_spdif5_dp1_sel OFFSET(0) NUMBITS(2) [],
        _reserved OFFSET(2) NUMBITS(14) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON128  0x0500;
register_bitfields![u32,
    CRU_CLKSEL_CON128 [
        aclk_hdcp1_root_div OFFSET(0) NUMBITS(5) [],
        aclk_hdcp1_root_sel OFFSET(5) NUMBITS(2) [],
        aclk_hdmirx_root_div OFFSET(7) NUMBITS(5) [],
        aclk_hdmirx_root_sel OFFSET(12) NUMBITS(1) [],
        hclk_vo1_root_sel OFFSET(13) NUMBITS(2) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON129  0x0504;
register_bitfields![u32,
    CRU_CLKSEL_CON129 [
        hclk_vo1_s_root_sel OFFSET(0) NUMBITS(2) [],
        pclk_vo1_root_sel OFFSET(2) NUMBITS(2) [],
        pclk_vo1_s_root_sel OFFSET(4) NUMBITS(2) [],
        clk_i2s7_8ch_rx_src_div OFFSET(6) NUMBITS(5) [],
        clk_i2s7_8ch_rx_src_sel OFFSET(11) NUMBITS(1) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON130  0x0508;
register_bitfields![u32,
    CRU_CLKSEL_CON130 [
        clk_i2s7_8ch_rx_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON131  0x050C;
register_bitfields![u32,
    CRU_CLKSEL_CON131 [
        mclk_i2s7_8ch_rx_sel OFFSET(0) NUMBITS(2) [],
        _reserved OFFSET(2) NUMBITS(14) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON133  0x0514;
register_bitfields![u32,
    CRU_CLKSEL_CON133 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        clk_hdmitx0_earc_div OFFSET(1) NUMBITS(5) [],
        clk_hdmitx0_earc_sel OFFSET(6) NUMBITS(1) [],
        _reserved1 OFFSET(7) NUMBITS(9) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON136  0x0520;
register_bitfields![u32,
    CRU_CLKSEL_CON136 [
        _reserved0 OFFSET(0) NUMBITS(1) [],
        clk_hdmitx1_earc_div OFFSET(1) NUMBITS(5) [],
        clk_hdmitx1_earc_sel OFFSET(6) NUMBITS(1) [],
        _reserved1 OFFSET(7) NUMBITS(9) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON138  0x0528;
register_bitfields![u32,
    CRU_CLKSEL_CON138 [
        clk_hdmirx_aud_src_div OFFSET(0) NUMBITS(8) [],
        clk_hdmirx_aud_src_sel OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON139  0x052C;
register_bitfields![u32,
    CRU_CLKSEL_CON139 [
        clk_hdmirx_aud_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON140  0x0530;
register_bitfields![u32,
    CRU_CLKSEL_CON140 [
        clk_hdmirx_aud_sel OFFSET(0) NUMBITS(1) [],
        clk_edp0_200m_sel OFFSET(1) NUMBITS(2) [],
        clk_edp1_200m_sel OFFSET(3) NUMBITS(2) [],
        clk_i2s5_8ch_tx_src_div OFFSET(5) NUMBITS(5) [],
        clk_i2s5_8ch_tx_src_sel OFFSET(10) NUMBITS(1) [],
        _reserved OFFSET(11) NUMBITS(5) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON141  0x0534;
register_bitfields![u32,
    CRU_CLKSEL_CON141 [
        clk_i2s5_8ch_tx_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON142  0x0538;
register_bitfields![u32,
    CRU_CLKSEL_CON142 [
        mclk_i2s5_8ch_tx_sel OFFSET(0) NUMBITS(2) [],
        _reserved OFFSET(2) NUMBITS(14) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON144  0x0540;
register_bitfields![u32,
    CRU_CLKSEL_CON144 [
        _reserved0 OFFSET(0) NUMBITS(3) [],
        clk_i2s6_8ch_tx_src_div OFFSET(3) NUMBITS(5) [],
        clk_i2s6_8ch_tx_src_sel OFFSET(8) NUMBITS(1) [],
        _reserved1 OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON145  0x0544;
register_bitfields![u32,
    CRU_CLKSEL_CON145 [
        clk_i2s6_8ch_tx_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON146  0x0548;
register_bitfields![u32,
    CRU_CLKSEL_CON146 [
        mclk_i2s6_8ch_tx_sel OFFSET(0) NUMBITS(2) [],
        clk_i2s6_8ch_rx_src_div OFFSET(2) NUMBITS(5) [],
        clk_i2s6_8ch_rx_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON147  0x054C;
register_bitfields![u32,
    CRU_CLKSEL_CON147 [
        clk_i2s6_8ch_rx_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON148  0x0550;
register_bitfields![u32,
    CRU_CLKSEL_CON148 [
        mclk_i2s6_8ch_rx_sel OFFSET(0) NUMBITS(2) [],
        i2s6_8ch_mclkout_sel OFFSET(2) NUMBITS(2) [],
        clk_spdif3_src_div OFFSET(4) NUMBITS(5) [],
        clk_spdif3_src_sel OFFSET(9) NUMBITS(1) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON149  0x0554;
register_bitfields![u32,
    CRU_CLKSEL_CON149 [
        clk_spdif3_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON150  0x0558;
register_bitfields![u32,
    CRU_CLKSEL_CON150 [
        mclk_spdif3_sel OFFSET(0) NUMBITS(2) [],
        clk_spdif4_src_div OFFSET(2) NUMBITS(5) [],
        clk_spdif4_src_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON151  0x055C;
register_bitfields![u32,
    CRU_CLKSEL_CON151 [
        clk_spdif4_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON152  0x0560;
register_bitfields![u32,
    CRU_CLKSEL_CON152 [
        mclk_spdif4_sel OFFSET(0) NUMBITS(2) [],
        mclk_spdifrx0_div OFFSET(2) NUMBITS(5) [],
        mclk_spdifrx0_sel OFFSET(7) NUMBITS(2) [],
        mclk_spdifrx1_div OFFSET(9) NUMBITS(5) [],
        mclk_spdifrx1_sel OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON153  0x0564;
register_bitfields![u32,
    CRU_CLKSEL_CON153 [
        mclk_spdifrx2_div OFFSET(0) NUMBITS(5) [],
        mclk_spdifrx2_sel OFFSET(5) NUMBITS(2) [],
        clk_i2s9_8ch_rx_src_div OFFSET(7) NUMBITS(5) [],
        clk_i2s9_8ch_rx_src_sel OFFSET(12) NUMBITS(1) [],
        _reserved OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON154  0x0568;
register_bitfields![u32,
    CRU_CLKSEL_CON154 [
        clk_i2s9_8ch_rx_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON155  0x056C;
register_bitfields![u32,
    CRU_CLKSEL_CON155 [
        mclk_i2s9_8ch_rx_sel OFFSET(0) NUMBITS(2) [],
        _reserved0 OFFSET(2) NUMBITS(1) [],
        clk_i2s10_8ch_rx_src_div OFFSET(3) NUMBITS(5) [],
        clk_i2s10_8ch_rx_src_sel OFFSET(8) NUMBITS(1) [],
        _reserved1 OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON156  0x0570;
register_bitfields![u32,
    CRU_CLKSEL_CON156 [
        clk_i2s10_8ch_rx_frac_div OFFSET(0) NUMBITS(32) []
    ]
];

// CRU_CLKSEL_CON157  0x0574;
register_bitfields![u32,
    CRU_CLKSEL_CON157 [
        mclk_i2s10_8ch_rx_sel OFFSET(0) NUMBITS(2) [],
        clk_hdmitrx_refsrc_div OFFSET(2) NUMBITS(5) [],
        clk_hdmitrx_refsrc_sel OFFSET(7) NUMBITS(1) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON158  0x0578;
register_bitfields![u32,
    CRU_CLKSEL_CON158 [
        clk_gpu_src_t_div OFFSET(0) NUMBITS(5) [],
        clk_gpu_src_t_sel OFFSET(5) NUMBITS(3) [],
        clk_testout_gpu_div OFFSET(8) NUMBITS(5) [],
        clk_testout_gpu_sel OFFSET(13) NUMBITS(1) [],
        clk_gpu_src_sel OFFSET(14) NUMBITS(1) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON159  0x057C;
register_bitfields![u32,
    CRU_CLKSEL_CON159 [
        clk_gpu_stacks_div OFFSET(0) NUMBITS(5) [],
        aclk_s_gpu_biu_div OFFSET(5) NUMBITS(5) [],
        aclk_m0_gpu_biu_div OFFSET(10) NUMBITS(5) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON160  0x0580;
register_bitfields![u32,
    CRU_CLKSEL_CON160 [
        aclk_m1_gpu_biu_div OFFSET(0) NUMBITS(5) [],
        aclk_m2_gpu_biu_div OFFSET(5) NUMBITS(5) [],
        aclk_m3_gpu_biu_div OFFSET(10) NUMBITS(5) [],
        _reserved OFFSET(15) NUMBITS(1) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON161  0x0584;
register_bitfields![u32,
    CRU_CLKSEL_CON161 [
        pclk_gpu_root_sel OFFSET(0) NUMBITS(2) [],
        clk_gpu_pvtpll_sel OFFSET(2) NUMBITS(1) [],
        _reserved OFFSET(3) NUMBITS(13) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON163  0x058C;
register_bitfields![u32,
    CRU_CLKSEL_CON163 [
        aclk_av1_root_div OFFSET(0) NUMBITS(7) [],
        aclk_av1_root_sel OFFSET(7) NUMBITS(2) [],
        pclk_av1_root_sel OFFSET(9) NUMBITS(2) [],
        _reserved OFFSET(11) NUMBITS(5) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON165  0x0594;
register_bitfields![u32,
    CRU_CLKSEL_CON165 [
        aclk_center_root_sel OFFSET(0) NUMBITS(2) [],
        aclk_center_low_root_sel OFFSET(2) NUMBITS(2) [],
        hclk_center_root_sel OFFSET(4) NUMBITS(2) [],
        pclk_center_root_sel OFFSET(6) NUMBITS(2) [],
        aclk_center_s200_root_sel OFFSET(8) NUMBITS(2) [],
        aclk_center_s400_root_sel OFFSET(10) NUMBITS(2) [],
        clk_ddr_timer_root_sel OFFSET(12) NUMBITS(1) [],
        _reserved OFFSET(13) NUMBITS(3) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON166  0x0598;
register_bitfields![u32,
    CRU_CLKSEL_CON166 [
        clk_ddr_cm0_rtc_div OFFSET(0) NUMBITS(5) [],
        clk_ddr_cm0_rtc_sel OFFSET(5) NUMBITS(1) [],
        _reserved OFFSET(6) NUMBITS(10) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON170  0x05A8;
register_bitfields![u32,
    CRU_CLKSEL_CON170 [
        aclk_vo1usb_top_root_div OFFSET(0) NUMBITS(5) [],
        aclk_vo1usb_top_root_sel OFFSET(5) NUMBITS(1) [],
        hclk_vo1usb_top_root_sel OFFSET(6) NUMBITS(2) [],
        _reserved OFFSET(8) NUMBITS(8) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON172  0x05B0;
register_bitfields![u32,
    CRU_CLKSEL_CON172 [
        hclk_sdio_root_sel OFFSET(0) NUMBITS(2) [],
        cclk_src_sdio_div OFFSET(2) NUMBITS(6) [],
        cclk_src_sdio_sel OFFSET(8) NUMBITS(2) [],
        _reserved OFFSET(10) NUMBITS(6) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON174  0x05B8;
register_bitfields![u32,
    CRU_CLKSEL_CON174 [
        aclk_rga3_root_div OFFSET(0) NUMBITS(5) [],
        aclk_rga3_root_sel OFFSET(5) NUMBITS(2) [],
        hclk_rga3_root_sel OFFSET(7) NUMBITS(2) [],
        clk_rga3_1_core_div OFFSET(9) NUMBITS(5) [],
        clk_rga3_1_core_sel OFFSET(14) NUMBITS(2) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON176  0x05C0;
register_bitfields![u32,
    CRU_CLKSEL_CON176 [
        clk_ref_pipe_phy0_pll_src_div OFFSET(0) NUMBITS(6) [],
        clk_ref_pipe_phy1_pll_src_div OFFSET(6) NUMBITS(6) [],
        _reserved OFFSET(12) NUMBITS(4) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];

// CRU_CLKSEL_CON177  0x05C4;
register_bitfields![u32,
    CRU_CLKSEL_CON177 [
        clk_ref_pipe_phy2_pll_src_div OFFSET(0) NUMBITS(6) [],
        clk_ref_pipe_phy0_sel OFFSET(6) NUMBITS(1) [],
        clk_ref_pipe_phy1_sel OFFSET(7) NUMBITS(1) [],
        clk_ref_pipe_phy2_sel OFFSET(8) NUMBITS(1) [],
        _reserved OFFSET(9) NUMBITS(7) [],
        write_enable OFFSET(16) NUMBITS(16) []
    ]
];
