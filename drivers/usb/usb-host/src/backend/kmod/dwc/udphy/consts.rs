use tock_registers::register_bitfields;

pub const PHY_LANE_MUX_DP: u32 = 1;

/// PMA CMN 寄存器偏移
#[allow(unused)]
pub mod pma_offset {
    pub const CMN_LANE_MUX_AND_EN: usize = 0x0288;
    pub const CMN_DP_LINK: usize = 0x028c;
    pub const CMN_SSC_EN: usize = 0x02d0;
    pub const CMN_ANA_LCPLL_DONE: usize = 0x0350;
    pub const CMN_ANA_ROPLL_DONE: usize = 0x0354;
    pub const CMN_DP_RSTN: usize = 0x038c;
    pub const TRSV_LN0_MON_RX_CDR: usize = 0x0b84;
    pub const TRSV_LN2_MON_RX_CDR: usize = 0x1b84;
}

register_bitfields![u32,
    pub CMN_LANE_MUX_EN [
        /// Lane 3 multiplexer select
        LANE3_MUX OFFSET(7) NUMBITS(1) [
            USB = 0,
            DP = 1
        ],
        /// Lane 2 multiplexer select
        LANE2_MUX OFFSET(6) NUMBITS(1) [
            USB = 0,
            DP = 1
        ],
        /// Lane 1 multiplexer select
        LANE1_MUX OFFSET(5) NUMBITS(1) [
            USB = 0,
            DP = 1
        ],
        /// Lane 0 multiplexer select
        LANE0_MUX OFFSET(4) NUMBITS(1) [
            USB = 0,
            DP = 1
        ],
        /// Lane 3 enable
        LANE3_EN OFFSET(3) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],
        /// Lane 2 enable
        LANE2_EN OFFSET(2) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],
        /// Lane 1 enable
        LANE1_EN OFFSET(1) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],
        /// Lane 0 enable
        LANE0_EN OFFSET(0) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],
    ]
];

// CMN_DP_RSTN 寄存器位字段
register_bitfields![u32,
    pub CMN_DP_RSTN [
        // CDR watchdog enable
        CDR_WTCHGD_MSK_CDR_EN OFFSET(0) NUMBITS(1) [
            Mask = 0,
            Enable = 1
        ],
        // CDR watchdog enable
        CDR_WTCHDG_EN OFFSET(1) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],
        // DP common reset
        DP_CMN_RSTN OFFSET(2) NUMBITS(1) [
            Reset = 0,
            Enable = 1
        ],
        // DP init reset
        DP_INIT_RSTN OFFSET(3) NUMBITS(1) [
            Reset = 0,
            Enable = 1
        ],
    ]
];

// CMN_ANA_LCPLL_DONE 寄存器位字段
register_bitfields![u32,
    pub CMN_ANA_LCPLL [
        // LCPLL AFC done
        AFC_DONE OFFSET(6) NUMBITS(1) [
            NotDone = 0,
            Done = 1
        ],
        // LCPLL lock done
        LOCK_DONE OFFSET(7) NUMBITS(1) [
            NotLocked = 0,
            Locked = 1
        ],
    ]
];

register_bitfields![u32,
    pub TRSV_LN0_MON_RX_CDR [
        // RX CDR lock done (bit 0)
        // 从接收数据流中恢复时钟的锁定状态
        LOCK_DONE OFFSET(0) NUMBITS(1) [
            NotLocked = 0,
            Locked = 1
        ],
    ]
];

// TRSV_LN2_MON_RX_CDR 寄存器位字段 (Lane 2 RX CDR 监控)
// 用于 USB3 模式下的 Lane 2 检查
register_bitfields![u32,
    pub TRSV_LN2_MON_RX_CDR [
        // RX CDR lock done (bit 0)
        LOCK_DONE OFFSET(0) NUMBITS(1) [
            NotLocked = 0,
            Locked = 1
        ],
    ]
];
