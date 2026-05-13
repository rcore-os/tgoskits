//! DWC3 寄存器定义
//! 基于 Linux drivers/usb/dwc3/core.h

use core::time::Duration;

use tock_registers::interfaces::*;
use tock_registers::{register_bitfields, register_structs, registers::*};

use super::super::osal::Kernel;
use crate::osal::SpinWhile;

/// DWC3 全局寄存器基址偏移 (相对于 xHCI 寄存器区域)
const DWC3_GLOBALS_REGS_START: usize = 0xc100;

// DWC3 寄存器映射结构
// 使用 register_structs! 宏定义，自动计算 padding 和偏移
//
// 基于 U-Boot drivers/usb/dwc3/core.h 的寄存器定义
// 全局寄存器区域: 0xc100 - 0xc6ff
// 设备寄存器区域: 0xc700 - 0xcbff
register_structs! {
    pub Dwc3Registers {
        // === 全局寄存器区域 (0xc100 - 0xc6ff) ===

        // 0xc100 - 0xc10c: 总线配置寄存器 (未使用)
        (0x00 => _rsv1),

        /// 0xc110 - Global Control Register
        (0x10 => pub gctl: ReadWrite<u32, GCTL::Register>),

        /// 0xc114 - Global Event Enable Register
        (0x14 => gevten: ReadWrite<u32, GEVTEN::Register>),

        /// 0xc118 - Global Status Register
        (0x18 => gsts: ReadWrite<u32, GSTS::Register>),

        /// 0xc11c - Global User Control 1 Register
        (0x1C => pub guctl1: ReadWrite<u32, GUCTL1::Register>),

        /// 0xc120 - SNPSID Register (只读)
        (0x20 => gsnpsid: ReadOnly<u32, GSNPSID::Register>),

        /// 0xc124 - GPIO Register
        (0x24 => ggpio: ReadWrite<u32, GGPIO::Register>),

        /// 0xc128 - GUID Register
        (0x28 => guid: ReadWrite<u32, GUID::Register>),

        /// 0xc12c - User Control Register
        (0x2C => guctl: ReadWrite<u32, GUCTL::Register>),

        // 0xc130 - 0xc13c: 总线错误和端口映射寄存器
        (0x30 => _rsv_buserr),

        /// 0xc140 - Global Hardware Parameters 0
        (0x40 => pub ghwparams0: ReadOnly<u32, GHWPARAMS0::Register>),

        /// 0xc144 - Global Hardware Parameters 1
        (0x44 => pub ghwparams1: ReadOnly<u32, GHWPARAMS1::Register>),

        /// 0xc148 - Global Hardware Parameters 2
        (0x48 => pub ghwparams2: ReadOnly<u32, GHWPARAMS2::Register>),

        /// 0xc14c - Global Hardware Parameters 3
        (0x4C => pub ghwparams3: ReadOnly<u32, GHWPARAMS3::Register>),

        /// 0xc150 - Global Hardware Parameters 4
        (0x50 => pub ghwparams4: ReadOnly<u32, GHWPARAMS4::Register>),

        /// 0xc154 - Global Hardware Parameters 5
        (0x54 => pub ghwparams5: ReadOnly<u32, GHWPARAMS5::Register>),

        /// 0xc158 - Global Hardware Parameters 6
        (0x58 => pub ghwparams6: ReadOnly<u32, GHWPARAMS6::Register>),

        /// 0xc15c - Global Hardware Parameters 7
        (0x5C => pub ghwparams7: ReadOnly<u32, GHWPARAMS7::Register>),

        // 0xc160 - 0xc1fc: 调试和其他寄存器 (未使用)
        (0x60 => _rsv_debug),

        /// 0xc200 - USB2 PHY Configuration Register 0
        (0x100 => pub gusb2phycfg0: ReadWrite<u32, GUSB2PHYCFG::Register>),

        // 0xc204 - 0xc2bc: USB2 PHY 其他寄存器 (未使用)
        (0x104 => _reserved_usb2phy),

        /// 0xc2c0 - USB3 PIPE Control Register 0
        (0x1C0 => pub gusb3pipectl0: ReadWrite<u32, GUSB3PIPECTL::Register>),

        // 0xc2c4 - 0xc2fc: USB3 PHY 其他寄存器 (未使用)
        (0x1C4 => _reserved_usb3_extra),

        // Event Buffer 0 - DMA 地址低 32 位 (0xc400)
        (0x300 => pub gevnt: [Gevnt; 4]),

        // 0xc340 - 0xc5fc: 保留区域
        (0x340 => _reserved_gevnt_extra),

        // === 设备寄存器区域 (0xc700 - 0xcbff) ===

        /// 0xc704 - Device Control Register
        (0x604 => dctl: ReadWrite<u32, DCTL::Register>),

        // 标记结构体结束 (0xc708 = 偏移 0x608)
        (0x608 => @END),
    }
}

// =============================================================================
// 寄存器位字段定义
// =============================================================================

#[repr(C)]
pub struct Gevnt {
    pub adrlo: ReadWrite<u32, GEVNTADRLO::Register>,
    /// Event Buffer 0 - DMA 地址高 32 位 (0xc304)
    pub adrhi: ReadWrite<u32, GEVNTADRHI::Register>,
    /// Event Buffer 0 - 缓冲区大小 (0xc308)
    pub size: ReadWrite<u32, GEVNTSIZ::Register>,
    /// Event Buffer 0 - 事件计数器 (0xc30c)
    pub count: ReadWrite<u32, GEVNTCOUNT::Register>,
}

// Global Control Register (GCTL) - 0xc110
register_bitfields![u32,
    pub GCTL [
        /// 禁止时钟门控
        DSBLCLKGTNG OFFSET(0) NUMBITS(1) [
            Enable = 0,
            Disable = 1
        ],

        /// 全局休眠使能
        GBLHIBERNATIONEN OFFSET(1) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// U2 退出 LFPS
        U2EXIT_LFPS OFFSET(2) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 禁止 scrambler
        DISSCRAMBLE OFFSET(3) NUMBITS(1) [
            Enable = 0,
            Disable = 1
        ],

        /// 缩放因子
        SCALEDOWN OFFSET(4) NUMBITS(2) [
            None = 0,
            Minimum = 1,
            Low = 2,
            Maximum = 3
        ],

        /// 时钟选择
        RAMCLKSEL OFFSET(6) NUMBITS(2) [
            Bus = 0,
            Pipe = 1,
            PipeHalf = 2
        ],

        /// 帧内同步
        SOFITPSYNC OFFSET(10) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 核心软复位
        CORESOFTRESET OFFSET(11) NUMBITS(1) [
            Normal = 0,
            Reset = 1
        ],

        /// 端口能力方向
        PRTCAPDIR OFFSET(12) NUMBITS(2) [
            Host = 1,
            Device = 2,
            OTG = 3
        ],

        /// U2 复位使能控制
        U2RSTECN OFFSET(16) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 电源 down 缩放因子
        PWRDNSCALE OFFSET(19) NUMBITS(13) []
    ]
];

// Global Status Register (GSTS) - 0xc118
register_bitfields![u32,
    GSTS [
        /// 当前模式
        CURMOD OFFSET(0) NUMBITS(2) [
            Device = 0,
            Host = 1
        ],

        /// 总线错误地址有效
        BUS_ERR_ADDR_VLD OFFSET(4) NUMBITS(1) [],

        /// CSR 超时
        CSR_TIMEOUT OFFSET(5) NUMBITS(1) [],

        /// 设备 IP 处理中
        DEVICE_IP OFFSET(6) NUMBITS(1) [],

        /// 主机 IP 处理中
        HOST_IP OFFSET(7) NUMBITS(1) []
    ]
];

// SNPSID Register (GSNPSID) - 0xc120 (只读)
register_bitfields![u32,
    GSNPSID [
        /// 仿真 ID
        SIMULATION OFFSET(31) NUMBITS(1) [
            Production = 0,
            Simulation = 1
        ],

        /// 修订号
        REVISION OFFSET(16) NUMBITS(16) [],

        /// 产品 ID
        PRODUCT_ID OFFSET(0) NUMBITS(16) []
    ]
];

// Global USB2 PHY Configuration Register (GUSB2PHYCFG) - 0xc200
register_bitfields![u32,
    pub GUSB2PHYCFG [
        /// PHY 软复位
        PHYSOFTRST OFFSET(31) NUMBITS(1) [
            Normal = 0,
            Reset = 1
        ],

        /// 使能低功耗暂停
        ENBLSLPM OFFSET(29) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 自由时钟存在
        U2_FREECLK_EXISTS OFFSET(30) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// USB 转发时间
        USBTRDTIM OFFSET(10) NUMBITS(4) [],

        /// 使能低功耗暂停
        SUSPHY OFFSET(6) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// ULPI 或 UTMI+
        ULPI_UTMI OFFSET(4) NUMBITS(1) [
            UTMI = 0,
            ULPI = 1
        ],

        /// PHY 接口
        PHYIF OFFSET(3) NUMBITS(1) [
            EightBit = 0,
            SixteenBit = 1
        ]
    ]
];

// Global USB3 PIPE Control Register (GUSB3PIPECTL) - 0xc2c0
register_bitfields![u32,
    pub GUSB3PIPECTL [
        /// PIPE 物理复位
        PHYSOFTRST OFFSET(31) NUMBITS(1) [
            Normal = 0,
            Reset = 1
        ],

        /// 使能 LBP
        ENABLE_LBP OFFSET(30) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 发送延迟
        TX_DEEMPHASIS OFFSET(29) NUMBITS(1) [
            Minus6dB = 0,
            Minus3_5dB = 1
        ],

        /// 暂停时 PIPE 进入 P3
        PIPE_P3_P2_TO_P1 OFFSET(25) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// 在 U0 中请求暂停
        REQP0P1P2P3 OFFSET(24) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// 实现延迟
        U1U2_EXIT_LATENCY OFFSET(17) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// PHY 配置
        PHY_CONFIG OFFSET(16) NUMBITS(1) [
            Unchanged = 0,
            Force = 1
        ],

        /// U2 状态进入 P3
        U2SSINP3OK OFFSET(15) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 延迟 P1/P2/P3 (bit 19)
        /// 值为 1 表示使能
        DEP1P2P3 OFFSET(19) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 延迟 PHY 电源变化 (bit 18)
        DEPOCHANGE OFFSET(18) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// LFPS 滤波 (bit 9)
        LFPSFILT OFFSET(9) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// RX 检测轮询 (bit 8)
        RX_DETOPOLL OFFSET(8) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// TX 去加重值 (bits 0-1)
        TX_DEEPH OFFSET(0) NUMBITS(2) [],

        /// 禁止接收检测在 P3
        DISRXDETINP3 OFFSET(14) NUMBITS(1) [
            Enable = 0,
            Disable = 1
        ],

        /// TX 历史
        TX_TX_HISTORY_T OFFSET(6) NUMBITS(1) [
            FullSpeed = 0,
            HighSpeed = 1
        ],

        /// 实体延迟发送
        LATENCY_OFFSET_TX OFFSET(3) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// 实体延迟接收
        LATENCY_OFFSET_RX OFFSET(2) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// 使能暂停 PHY
        SUSPHY OFFSET(1) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 延迟 P1/P2 到 P0
        UX_EXIT_PX OFFSET(0) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ]
    ]
];

// Global Event Enable Register (GEVTEN) - 0xc114
register_bitfields![u32,
    GEVTEN [
        /// OTG 事件使能
        OTGEVTEN OFFSET(17) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 设备事件使能
        DEVEVTEN OFFSET(16) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// Carkit 事件使能
        CARKITEVTEN OFFSET(8) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// I2C 事件使能
        I2CEVTEN OFFSET(7) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ]
    ]
];

// Global User Control 1 Register (GUCTL1) - 0xc11c
register_bitfields![u32,
    pub GUCTL1 [
        /// 设备解耦 L1L2 事件
        DEV_DECOUPLE_L1L2_EVT OFFSET(31) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// TX IPGAP 线检查禁止
        TX_IPGAP_LINECHECK_DIS OFFSET(28) NUMBITS(1) [
            Enable = 0,
            Disable = 1
        ],

        /// 驱动强制 20_CLK 用于 30_CLK
        DEV_FORCE_20_CLK_FOR_30_CLK OFFSET(26) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 设备 L1 退出由硬件
        DEV_L1_EXIT_BY_HW OFFSET(24) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// 禁止 SS 停车模式
        PARKMODE_DISABLE_SS OFFSET(17) NUMBITS(1) [
            Enable = 0,
            Disable = 1
        ],

        /// 恢复操作模式 HS 主机
        RESUME_OPMODE_HS_HOST OFFSET(10) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ]
    ]
];

// User Control Register (GUCTL) - 0xc12c
register_bitfields![u32,
    GUCTL [
        /// 跳止发送
        GTSTOP_SEND OFFSET(31) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 禁止 HNP
        HST_DISCONNECT OFFSET(17) NUMBITS(1) [
            Enable = 0,
            Disable = 1
        ],

        /// 触发 USB 链接
        USBTRGTIM OFFSET(10) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ]
    ]
];

// GPIO Register (GGPIO) - 0xc124
register_bitfields![u32,
    GGPIO [
        /// GPIO 方向
        GPIO_DIR OFFSET(16) NUMBITS(16) [],

        /// GPIO 数据
        GPIO_DATA OFFSET(0) NUMBITS(16) []
    ]
];

register_bitfields![u32,
    GUID [
        /// GUID 值
        GUID_VALUE OFFSET(0) NUMBITS(32) []
    ]
];

// =============================================================================
// Global Hardware Parameters Registers (GHWPARAMS0-7)
// =============================================================================

register_bitfields![u32,
    pub GHWPARAMS0 [
        /// 操作模式 (bits 0-1)
        MODE OFFSET(0) NUMBITS(2) [
            Gadget = 0,
            Host = 1,
            DRD = 2
        ],

        /// 主总线类型 (bits 3-5)
        MBUS_TYPE OFFSET(3) NUMBITS(3) [],

        /// 从总线类型 (bits 6-7)
        SBUS_TYPE OFFSET(6) NUMBITS(2) [],

        /// 主数据总线宽度 (bits 8-15)
        /// 以 32-bit 字为单位
        MDWIDTH OFFSET(8) NUMBITS(8) [],

        /// 从数据总线宽度 (bits 16-23)
        /// 以 32-bit 字为单位
        SDWIDTH OFFSET(16) NUMBITS(8) [],

        /// 地址总线宽度 (bits 24-31)
        /// 以位为单位
        AWIDTH OFFSET(24) NUMBITS(8) []
    ]
];

// Global Hardware Parameters 1 Register (GHWPARAMS1) - 0xc144
// 描述电源管理选项和事件缓冲区数量
register_bitfields![u32,
    pub GHWPARAMS1 [
        /// 使能 Data Burst 能力 (bit 31)
        ENDBC OFFSET(31) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 电源管理选项 (bits 24-25)
        EN_PWROPT OFFSET(24) NUMBITS(2) [
            No = 0,
            Clock = 1,
            Hibernation = 2
        ],

        /// 事件缓冲区数量 (bits 15-20)
        NUM_EVENT_BUFFERS OFFSET(15) NUMBITS(6) [],

        /// 设备端点数量 (bits 0-4)
        NUM_DEVS OFFSET(0) NUMBITS(5) []
    ]
];

register_bitfields![u32,
    pub GHWPARAMS2 [
        /// 事务类型 (bits 0-1)
        TYPE_TRANSACTIONS OFFSET(0) NUMBITS(2) [],

        /// 低功耗连接地址数量 (bits 1-5)
        NUM_DEV_IN_EPS OFFSET(1) NUMBITS(5) [],

        /// 设备端点总数 (bits 16-20)
        NUM_DEV_EPS OFFSET(16) NUMBITS(5) []
    ]
];

register_bitfields![u32,
    pub GHWPARAMS3 [
        /// SuperSpeed PHY 接口类型 (bits 0-1)
        SSPHY_IFC OFFSET(0) NUMBITS(2) [
            Disabled = 0,
            Enabled = 1,
            Gen2 = 2
        ],

        /// HighSpeed PHY 接口类型 (bits 2-3)
        HSPHY_IFC OFFSET(2) NUMBITS(2) [
            Disabled = 0,
            UTMI = 1,
            ULPI = 2,
            UTMI_ULPI = 3
        ],

        /// FullSpeed PHY 接口类型 (bits 4-5)
        FSPHY_IFC OFFSET(4) NUMBITS(2) [
            Disabled = 0,
            Enabled = 1
        ],

        /// PHY 宽度 (bits 6-7)
        PHY_WIDTH OFFSET(6) NUMBITS(2) [],

        /// 控制 endpoint 数量 (bits 16-18)
        NUM_CTRL_EPS OFFSET(16) NUMBITS(3) []
    ]
];

register_bitfields![u32,
    pub GHWPARAMS4 [
        /// 休眠 Scratch Buffer 数量 (bits 13-16)
        HIBER_SCRATCHBUFS OFFSET(13) NUMBITS(4) [],

        /// 低功耗资源地址数量 (bits 4-8)
        NUM_DEV_MODE_EPS OFFSET(4) NUMBITS(5) [],

        /// Host 端点数量 (bits 0-3)
        NUM_HOST_EPS OFFSET(0) NUMBITS(4) []
    ]
];

register_bitfields![u32,
    pub GHWPARAMS5 [
        /// 超时值 (bits 0-15)
        TIMEOUT_VALUE OFFSET(0) NUMBITS(16) [],

        /// 设备模式请求队列深度 (bits 16-23)
        DEV_REQ_Q_DEPTH OFFSET(16) NUMBITS(8) []
    ]
];

register_bitfields![u32,
    pub GHWPARAMS6 [
        /// 充电支持 (bit 14)
        BCSUPPORT OFFSET(14) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// OTG 3.0 支持 (bit 13)
        OTG3SUPPORT OFFSET(13) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// ADP 支持 (bit 12)
        ADPSUPPORT OFFSET(12) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// HNP 支持 (bit 11)
        HNPSUPPORT OFFSET(11) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// SRP 支持 (bit 10)
        SRPSUPPORT OFFSET(10) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// FPGA 使能 (bit 7)
        EN_FPGA OFFSET(7) NUMBITS(1) [
            No = 0,
            Yes = 1
        ],

        /// 总线宽度和流水线选项 (bits 4-6)
        WIDTH_PIPED OFFSET(4) NUMBITS(3) [],

        /// RAM0 深度 (bits 16-31)
        /// 以 32-bit 字为单位
        RAM0_DEPTH OFFSET(16) NUMBITS(16) []
    ]
];

register_bitfields![u32,
    pub GHWPARAMS7 [
        /// RAM1 深度 (bits 0-15)
        /// 以 32-bit 字为单位
        RAM1_DEPTH OFFSET(0) NUMBITS(16) [],

        /// RAM2 深度 (bits 16-31)
        /// 以 32-bit 字为单位
        RAM2_DEPTH OFFSET(16) NUMBITS(16) []
    ]
];

register_bitfields![u32,
    DCTL [
        /// 运行/停止 (bit 31)
        /// 0 = 停止，1 = 运行
        RUN_STOP OFFSET(31) NUMBITS(1) [
            Stop = 0,
            Run = 1
        ],

        /// 核心软复位 (bit 30)
        CSFTRST OFFSET(30) NUMBITS(1) [
            Normal = 0,
            Reset = 1
        ],

        /// 链路层软复位 (bit 29)
        LSFTRST OFFSET(29) NUMBITS(1) [
            Normal = 0,
            Reset = 1
        ],

        /// HIRD 阈值 (bits 24-28)
        /// 主机发起的远程唤醒延迟阈值
        HIRD_THRES OFFSET(24) NUMBITS(5) [],

        /// 应用层特定复位 (bit 23)
        APPL1RES OFFSET(23) NUMBITS(1) [
            Normal = 0,
            Reset = 1
        ],

        /// LPM Errata (bits 20-23)
        /// 仅适用于版本 1.94a 及更新
        LPM_ERRATA OFFSET(20) NUMBITS(4) [],

        /// 保持连接 (bit 19)
        /// 仅适用于版本 1.94a 及更新
        KEEP_CONNECT OFFSET(19) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// L1 休眠使能 (bit 18)
        L1_HIBER_EN OFFSET(18) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 继续远程唤醒 (bit 17)
        CRS OFFSET(17) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 继续同步 (bit 16)
        CSS OFFSET(16) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 初始化 U2 使能 (bit 12)
        INITU2ENA OFFSET(12) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 接受 U2 使能 (bit 11)
        ACCEPTU2ENA OFFSET(11) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 初始化 U1 使能 (bit 10)
        INITU1ENA OFFSET(10) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 接受 U1 使能 (bit 9)
        ACCEPTU1ENA OFFSET(9) NUMBITS(1) [
            Disable = 0,
            Enable = 1
        ],

        /// 测试控制掩码 (bits 1-4)
        TSTCTRL_MASK OFFSET(1) NUMBITS(4) [],

        /// USB 链路状态改变请求 (bits 5-8)
        ULSTCHNGREQ OFFSET(5) NUMBITS(4) [
            NoAction = 0,
            SSDisabled = 4,
            RxDetect = 5,
            SSInactive = 6,
            Recovery = 8,
            Compliance = 10,
            Loopback = 11
        ]
    ]
];

// Global Event Buffer Registers
// 基地址: 0xc400，每个缓冲区占用 0x10 字节
// GEVNTADRLO(n): 0xc400 + (n * 0x10) - 事件缓冲区 DMA 地址低 32 位
// GEVNTADRHI(n): 0xc404 + (n * 0x10) - 事件缓冲区 DMA 地址高 32 位
// GEVNTSIZ(n): 0xc408 + (n * 0x10) - 事件缓冲区大小
// GEVNTCOUNT(n): 0xc40c + (n * 0x10) - 事件缓冲区计数器

register_bitfields![u32,
    /// GEVNTADRLO - Global Event Buffer Address Low Register
    /// 事件缓冲区 DMA 地址低 32 位
    pub GEVNTADRLO [
        /// DMA 地址低 32 位 (bits 0-31)
        /// 必须按照事件缓冲区大小对齐
        BUSADDR OFFSET(0) NUMBITS(32) []
    ],

    /// GEVNTADRHI - Global Event Buffer Address High Register
    /// 事件缓冲区 DMA 地址高 32 位
    pub GEVNTADRHI [
        /// DMA 地址高 32 位 (bits 0-31)
        /// 用于支持超过 4GB 的物理地址空间
        BUSADDR OFFSET(0) NUMBITS(32) []
    ],

    /// GEVNTSIZ - Global Event Size Register
    pub GEVNTSIZ [
        /// 中断掩码 (bit 31)
        INTMASK OFFSET(31) NUMBITS(1) [
            Unmasked = 0,
            Masked = 1
        ],

        /// 事件缓冲区大小 (bits 0-15)
        SIZE OFFSET(0) NUMBITS(16) []
    ],

    /// GEVNTCOUNT - Global Event Buffer Count Register
    pub GEVNTCOUNT [
        /// 事件缓冲区计数器 (bits 0-15)
        /// 硬件自动更新，指向下一个事件的位置
        COUNT OFFSET(0) NUMBITS(16) []
    ]
];

/// DWC3 寄存器访问器
#[derive(Clone)]
pub struct Dwc3Regs {
    base: usize,
}

impl Dwc3Regs {
    /// 创建新的 DWC3 寄存器访问器
    ///
    /// # Safety
    ///
    /// 调用者必须确保 `base` 地址有效且可以访问
    pub unsafe fn new(base: usize) -> Self {
        Self { base }
    }

    /// 获取全局寄存器
    pub fn globals(&self) -> &'static Dwc3Registers {
        let addr = self.base + DWC3_GLOBALS_REGS_START;
        unsafe { &*(addr as *const Dwc3Registers) }
    }

    // ==================== 寄存器操作封装 ====================

    // pub fn hwparams(&self) -> Dwc3Hwparams {
    //     Dwc3Hwparams {
    //         hwparams0: self.globals().gsnpsid.get(),
    //         hwparams1: 0, // TODO: 读取其他 HWPARAMS 寄存器
    //         hwparams2: 0,
    //         hwparams3: 0,
    //         hwparams4: 0,
    //         hwparams5: 0,
    //         hwparams6: 0,
    //         hwparams7: 0,
    //         hwparams8: 0,
    //     }
    // }

    /// 读取 SNPSID 的产品 ID
    pub fn read_product_id(&self) -> u32 {
        self.globals().gsnpsid.read(GSNPSID::PRODUCT_ID)
    }

    /// 读取 SNPSID 的版本号
    pub fn read_revision(&self) -> u32 {
        self.globals().gsnpsid.read(GSNPSID::REVISION) << 16
    }

    pub async fn device_soft_reset(&mut self) {
        self.globals().dctl.modify(DCTL::CSFTRST::Reset);
        trace!("DWC3: Device waiting for soft reset...");
        SpinWhile::new(|| self.globals().dctl.is_set(DCTL::CSFTRST)).await;
        trace!("DWC3: Device soft reset completed");
    }

    pub async fn core_soft_reset(&self, kernel: &Kernel) {
        // Before Resetting PHY, put Core in Reset
        self.globals().gctl.modify(GCTL::CORESOFTRESET::Reset);

        // Assert USB3 PHY reset
        self.globals()
            .gusb3pipectl0
            .modify(GUSB3PIPECTL::PHYSOFTRST::Reset);

        self.globals()
            .gusb2phycfg0
            .modify(GUSB2PHYCFG::PHYSOFTRST::Reset);

        kernel.delay(Duration::from_millis(100));

        // Clear USB3 PHY reset
        self.globals()
            .gusb3pipectl0
            .modify(GUSB3PIPECTL::PHYSOFTRST::Normal);

        // Clear USB2 PHY reset
        self.globals()
            .gusb2phycfg0
            .modify(GUSB2PHYCFG::PHYSOFTRST::Normal);

        kernel.delay(Duration::from_millis(100));

        // After PHYs are stable we can take Core out of reset state
        self.globals().gctl.modify(GCTL::CORESOFTRESET::Normal);

        debug!("DWC3: Core soft reset completed");
    }
}
