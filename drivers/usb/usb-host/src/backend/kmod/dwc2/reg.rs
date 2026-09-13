use core::ptr::NonNull;

use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
    registers::{ReadOnly, ReadWrite},
};
use usb_if::host::hub::Speed;

use crate::Mmio;
pub(crate) const DWC2_MAX_CHANNELS: u8 = 16;

pub(crate) const DWC2_DMA_MASK_32: u64 = u32::MAX as u64;
// Linux DWC2 bounds GRSTCTL register polls to 10,000 reads. Keep the same
// budget: MMIO reads can be slow on embedded interconnects, and a missing
// hardware transition must fail initialization instead of monopolizing a CPU.
pub(crate) const DWC2_WAIT_ITERS: usize = 10_000;
pub(crate) const DWC2_DMA_ALIGN: usize = 64;
pub(crate) const DWC2_STATUS_BUF_SIZE: usize = 64;
pub(crate) const DWC2_MAX_DMA_DESCS: usize = 256; // NTD 8 位 → 单链 ≤ 256 desc

pub(crate) const GUSBCFG_TOUTCAL_MASK: u32 = 0x7;
pub(crate) const GUSBCFG_PHYIF16: u32 = 1 << 3;
pub(crate) const GUSBCFG_ULPI_UTMI_SEL: u32 = 1 << 4;
pub(crate) const GUSBCFG_FORCEHOSTMODE: u32 = 1 << 29;
pub(crate) const GUSBCFG_FORCEDEVMODE: u32 = 1 << 30;

pub(crate) const GRSTCTL_CSFTRST: u32 = 1 << 0;
pub(crate) const GRSTCTL_RXFFLSH: u32 = 1 << 4;
pub(crate) const GRSTCTL_TXFFLSH: u32 = 1 << 5;
pub(crate) const GRSTCTL_TXFNUM_ALL: u32 = 0x10 << 6;
pub(crate) const GRSTCTL_CSFTRST_DONE: u32 = 1 << 29;
pub(crate) const GRSTCTL_AHBIDLE: u32 = 1 << 31;

pub(crate) const GINTSTS_CURMODE_HOST: u32 = 1 << 0;
pub(crate) const GINTSTS_PRTINT: u32 = 1 << 24;
pub(crate) const GINTSTS_HCHINT: u32 = 1 << 25;
pub(crate) const GINTSTS_DISCONNINT: u32 = 1 << 29;
pub(crate) const DWC2_RUNTIME_GINTMSK: u32 = GINTSTS_PRTINT | GINTSTS_HCHINT | GINTSTS_DISCONNINT;
pub(crate) const DWC2_COMPLETION_DISCONNECTED: u32 = 1 << 31;

pub(crate) const GOTGCTL_VBVALOEN: u32 = 1 << 2;
pub(crate) const GOTGCTL_VBVALOVAL: u32 = 1 << 3;
pub(crate) const GOTGCTL_AVALOEN: u32 = 1 << 4;
pub(crate) const GOTGCTL_AVALOVAL: u32 = 1 << 5;
pub(crate) const GOTGCTL_DBNCE_FLTR_BYPASS: u32 = 1 << 15;

pub(crate) const HPRT_CONN_DET: u32 = 1 << 1;
pub(crate) const HPRT_ENA: u32 = 1 << 2;
pub(crate) const HPRT_ENA_CHG: u32 = 1 << 3;
pub(crate) const HPRT_OVRCUR_CHG: u32 = 1 << 5;
pub(crate) const HPRT_RST: u32 = 1 << 8;
pub(crate) const HPRT_PWR: u32 = 1 << 12;
pub(crate) const HPRT_W1C_MASK: u32 = HPRT_CONN_DET | HPRT_ENA | HPRT_ENA_CHG | HPRT_OVRCUR_CHG;

pub(crate) const HCCHAR_ODDFRM: u32 = 1 << 29;
pub(crate) const HCCHAR_EPDIR: u32 = 1 << 15;

// HCFG：周期调度使能与帧列表尺寸（HCFG.PERSCHEDENA / FRLISTEN）。
pub(crate) const HCFG_PERSCHEDENA: u32 = 1 << 26;
pub(crate) const HCFG_FRLISTEN_MASK: u32 = 0b11 << 24;
pub(crate) const HCFG_FRLISTEN_64: u32 = 0b11 << 24;

pub(crate) const HCINT_XFERCOMPL: u32 = 1 << 0;
pub(crate) const HCINT_CHHLTD: u32 = 1 << 1;
pub(crate) const HCINT_AHBERR: u32 = 1 << 2;
pub(crate) const HCINT_STALL: u32 = 1 << 3;
pub(crate) const HCINT_NAK: u32 = 1 << 4;
pub(crate) const HCINT_XACTERR: u32 = 1 << 7;
pub(crate) const HCINT_BBLERR: u32 = 1 << 8;
pub(crate) const HCINT_FRMOVRN: u32 = 1 << 9;
pub(crate) const HCINT_DATATGLERR: u32 = 1 << 10;
pub(crate) const HCINT_ALL_W1C: u32 = 0x7ff;
pub(crate) const HCINT_TRANSFER_MASK: u32 = HCINT_XFERCOMPL
    | HCINT_CHHLTD
    | HCINT_AHBERR
    | HCINT_STALL
    | HCINT_NAK
    | HCINT_XACTERR
    | HCINT_BBLERR
    | HCINT_FRMOVRN
    | HCINT_DATATGLERR;

// ═══════════════════════════════════════════
// 寄存器位字段定义
// ═══════════════════════════════════════════

register_bitfields![u32,
    /// GOTGCTL — OTG 控制
    pub GOTGCTL [
        /// OTG 协议版本（host 模式不使用，Linux 在 core_init 清除）
        OTGVER OFFSET(20) NUMBITS(1) [],
        /// Connector ID 状态（0 = A 设备）
        CONID_B OFFSET(16) NUMBITS(1) [],
        /// 旁路去抖滤波器
        DBNCE_FLTR_BYPASS OFFSET(15) NUMBITS(1) [],
        /// A 会话有效覆盖值
        AVALOVAL OFFSET(5) NUMBITS(1) [],
        /// A 会话有效覆盖使能
        AVALOEN OFFSET(4) NUMBITS(1) [],
        /// B 会话有效覆盖值
        VBVALOVAL OFFSET(3) NUMBITS(1) [],
        /// B 会话有效覆盖使能
        VBVALOEN OFFSET(2) NUMBITS(1) [],
    ]
];

register_bitfields![u32,
    /// GAHBCFG — AHB 配置
    pub GAHBCFG [
        /// 周期 TX FIFO 空阈值
        PTXFEHLVL OFFSET(8) NUMBITS(1) [],
        /// 非周期 TX FIFO 空阈值
        NPTXFEHLVL OFFSET(7) NUMBITS(1) [],
        /// DMA 模式使能
        DMAEN OFFSET(5) NUMBITS(1) [],
        /// 突发长度/类型（Linux hw.h GAHBCFG_HBSTLEN_*）
        HBSTLEN OFFSET(1) NUMBITS(3) [
            SINGLE = 0,
            INCR = 1,
            INCR4 = 3,
            INCR8 = 5,
            INCR16 = 7,
        ],
        /// 全局中断屏蔽
        GLBLINTRMSK OFFSET(0) NUMBITS(1) [],
    ]
];

register_bitfields![u32,
    /// GUSBCFG — USB 配置
    pub GUSBCFG [
        /// 强制设备模式
        FORCEDEVMODE OFFSET(30) NUMBITS(1) [],
        /// 强制主机模式
        FORCEHOSTMODE OFFSET(29) NUMBITS(1) [],
        /// ULPI/UTMI+ 选择
        ULPI_UTMI_SEL OFFSET(4) NUMBITS(1) [],
        /// PHY 接口 16 位
        PHYIF16 OFFSET(3) NUMBITS(1) [],
        /// HS/FS 超时校准（0x0-0x7）
        TOUTCAL OFFSET(0) NUMBITS(3) [],
    ]
];

register_bitfields![u32,
    /// GRSTCTL — 复位控制
    pub GRSTCTL [
        /// AHB 主控空闲
        AHBIDLE OFFSET(31) NUMBITS(1) [],
        /// 核软复位完成
        CSFTRST_DONE OFFSET(29) NUMBITS(1) [],
        /// 时钟切换定时器（v5.00a+，Linux dwc2_set_clock_switch_timer）
        CLOCK_SWITH_TIMER OFFSET(11) NUMBITS(3) [],
        /// TX FIFO 编号（0x10 = 全部）
        TXFNUM OFFSET(6) NUMBITS(5) [],
        /// TX FIFO 冲刷
        TXFFLSH OFFSET(5) NUMBITS(1) [],
        /// RX FIFO 冲刷
        RXFFLSH OFFSET(4) NUMBITS(1) [],
        /// 核软复位
        CSFTRST OFFSET(0) NUMBITS(1) [],
    ]
];

register_bitfields![u32,
    /// GINTSTS — 中断状态
    pub GINTSTS [
        /// 断开检测中断
        DISCONNINT OFFSET(29) NUMBITS(1) [],
        /// 主机通道中断
        HCHINT OFFSET(25) NUMBITS(1) [],
        /// 端口变化中断
        PRTINT OFFSET(24) NUMBITS(1) [],
        /// 当前模式：主机
        CURMODE_HOST OFFSET(0) NUMBITS(1) [],
    ]
];

register_bitfields![u32,
    /// GINTMSK — 中断屏蔽（与 GINTSTS 同位）
    pub GINTMSK [
        DISCONNINT OFFSET(29) NUMBITS(1) [],
        HCHINT OFFSET(25) NUMBITS(1) [],
        PRTINT OFFSET(24) NUMBITS(1) [],
        CURMODE_HOST OFFSET(0) NUMBITS(1) [],
    ]
];

register_bitfields![u32,
    /// GHWCFG2 — 硬件参数 2
    pub GHWCFG2 [
        /// OTG IC USB 使能
        OTG_ENABLE_IC_USB OFFSET(31) NUMBITS(1) [],
        /// 主机周期 TX 队列深度
        HOST_PERIO_TX_Q_DEPTH OFFSET(24) NUMBITS(2) [],
        /// 非周期 TX 队列深度
        NONPERIO_TX_Q_DEPTH OFFSET(22) NUMBITS(2) [],
        /// 动态 FIFO 支持
        DYNAMIC_FIFO OFFSET(19) NUMBITS(1) [],
        /// 支持周期端点
        PERIO_EP_SUPPORTED OFFSET(18) NUMBITS(1) [],
        /// 主机通道数 − 1
        NUM_HOST_CHAN OFFSET(14) NUMBITS(4) [],
        /// FS PHY 类型
        FS_PHY_TYPE OFFSET(8) NUMBITS(2) [],
        /// HS PHY 类型
        HS_PHY_TYPE OFFSET(6) NUMBITS(2) [],
        /// 点对点
        POINT2POINT OFFSET(5) NUMBITS(1) [],
        /// 架构
        ARCHITECTURE OFFSET(3) NUMBITS(2) [
            SlaveOnly = 0,
            ExternalDma = 1,
            InternalDma = 2,
        ],
        /// 操作模式
        OP_MODE OFFSET(0) NUMBITS(3) [],
    ]
];

register_bitfields![u32,
    /// GHWCFG3 — 硬件参数 3
    pub GHWCFG3 [
        /// FIFO RAM 总深度（字）
        DFIFO_DEPTH OFFSET(16) NUMBITS(16) [],
    ]
];

register_bitfields![u32,
    /// GHWCFG4 — 硬件参数 4
    pub GHWCFG4 [
        /// 支持描述符 DMA 模式
        DESC_DMA OFFSET(30) NUMBITS(1) [],
        /// 存在 IDDIG 去抖滤波器
        IDDIG_FILT_EN OFFSET(20) NUMBITS(1) [],
        /// UTMI PHY 数据宽度（0=8 位，1=16 位，2=8/16 可选）
        UTMI_PHY_DATA_WIDTH OFFSET(14) NUMBITS(2) [
            Width8 = 0,
            Width16 = 1,
        ],
    ]
];

register_bitfields![u32,
    /// HCFG — 主机配置
    pub HCFG [
        /// 模式变化时序使能
        MODECHTIMEN OFFSET(31) NUMBITS(1) [],
        /// 周期调度使能
        PERSCHEDENA OFFSET(26) NUMBITS(1) [],
        /// 帧列表条目（8/16/32/64）
        FRLISTEN OFFSET(24) NUMBITS(2) [
            Size8 = 0,
            Size16 = 1,
            Size32 = 2,
            Size64 = 3,
        ],
        /// 描述符 DMA 模式
        DESCDMA OFFSET(23) NUMBITS(1) [],
        /// FS/LS 时钟选择
        FSLSPCLKSEL OFFSET(0) NUMBITS(2) [],
    ]
];

register_bitfields![u32,
    /// HPRT — 主机端口控制与状态
    pub HPRT [
        /// 端口速度
        SPD OFFSET(17) NUMBITS(2) [
            High = 0,
            Full = 1,
            Low = 2,
        ],
        /// 测试控制
        TSTCTL OFFSET(13) NUMBITS(4) [],
        /// 端口电源（W1C，写 0 保持）
        PWR OFFSET(12) NUMBITS(1) [],
        /// 线状态
        LNSTS OFFSET(10) NUMBITS(2) [],
        /// 端口复位（W1C）
        RST OFFSET(8) NUMBITS(1) [],
        /// 挂起
        SUSP OFFSET(7) NUMBITS(1) [],
        /// 恢复
        RES OFFSET(6) NUMBITS(1) [],
        /// 过流变化（W1C）
        OVRCURRCHG OFFSET(5) NUMBITS(1) [],
        /// 过流激活
        OVRCURRACT OFFSET(4) NUMBITS(1) [],
        /// 端口使能变化（W1C）
        ENACHG OFFSET(3) NUMBITS(1) [],
        /// 端口使能（W1C）
        ENA OFFSET(2) NUMBITS(1) [],
        /// 连接检测（W1C）
        CONNDET OFFSET(1) NUMBITS(1) [],
        /// 当前连接状态
        CONNSTS OFFSET(0) NUMBITS(1) [],
    ]
];

register_bitfields![u32,
    /// HCCHAR — 主机通道特性
    pub HCCHAR [
        /// 通道使能
        CHENA OFFSET(31) NUMBITS(1) [],
        /// 通道禁用
        CHDIS OFFSET(30) NUMBITS(1) [],
        /// 奇帧
        ODDFRM OFFSET(29) NUMBITS(1) [],
        /// 设备地址
        DEVADDR OFFSET(22) NUMBITS(7) [],
        /// 多事务计数（ISO/INT，mult − 1）
        MULTICNT OFFSET(20) NUMBITS(2) [],
        /// 端点类型
        EPTYPE OFFSET(18) NUMBITS(2) [
            Control = 0,
            Isochronous = 1,
            Bulk = 2,
            Interrupt = 3,
        ],
        /// 低速设备
        LSPDDEV OFFSET(17) NUMBITS(1) [],
        /// 端点方向（1=IN）
        EPDIR OFFSET(15) NUMBITS(1) [],
        /// 端点号
        EPNUM OFFSET(11) NUMBITS(4) [],
        /// 最大包大小
        MPS OFFSET(0) NUMBITS(11) [],
    ]
];

register_bitfields![u32,
    /// HCSPLT — 主机通道拆分控制
    pub HCSPLT [
        SPLITEN OFFSET(31) NUMBITS(1) [],
        COMDONE OFFSET(30) NUMBITS(1) [],
    ]
];

register_bitfields![u32,
    /// HCINT / HCINTMSK — 主机通道中断
    pub HCINT [
        /// 帧列表回绕
        FRM_LIST_ROLL OFFSET(13) NUMBITS(1) [],
        /// 缓冲区不可用
        BNA OFFSET(11) NUMBITS(1) [],
        /// 数据翻转错误
        DATATGLERR OFFSET(10) NUMBITS(1) [],
        /// 帧过载
        FRMOVRUN OFFSET(9) NUMBITS(1) [],
        /// 串扰错误
        BBLERR OFFSET(8) NUMBITS(1) [],
        /// 事务错误
        XACTERR OFFSET(7) NUMBITS(1) [],
        /// NYET（未就绪）
        NYET OFFSET(6) NUMBITS(1) [],
        /// ACK（应答）
        ACK OFFSET(5) NUMBITS(1) [],
        /// NAK（否认）
        NAK OFFSET(4) NUMBITS(1) [],
        /// STALL（停滞）
        STALL OFFSET(3) NUMBITS(1) [],
        /// AHB 错误
        AHBERR OFFSET(2) NUMBITS(1) [],
        /// 通道暂停
        CHHLTD OFFSET(1) NUMBITS(1) [],
        /// 传输完成
        XFERCOMPL OFFSET(0) NUMBITS(1) [],
    ]
];

register_bitfields![u32,
    /// HCINTMSK — 与 HCINT 同位
    pub HCINTMSK [
        FRM_LIST_ROLL OFFSET(13) NUMBITS(1) [],
        BNA OFFSET(11) NUMBITS(1) [],
        DATATGLERR OFFSET(10) NUMBITS(1) [],
        FRMOVRUN OFFSET(9) NUMBITS(1) [],
        BBLERR OFFSET(8) NUMBITS(1) [],
        XACTERR OFFSET(7) NUMBITS(1) [],
        NYET OFFSET(6) NUMBITS(1) [],
        ACK OFFSET(5) NUMBITS(1) [],
        NAK OFFSET(4) NUMBITS(1) [],
        STALL OFFSET(3) NUMBITS(1) [],
        AHBERR OFFSET(2) NUMBITS(1) [],
        CHHLTD OFFSET(1) NUMBITS(1) [],
        XFERCOMPL OFFSET(0) NUMBITS(1) [],
    ]
];

register_bitfields![u32,
    /// HCTSIZ — 主机通道传输大小
    pub HCTSIZ [
        /// 执行 ping
        DOPNG OFFSET(31) NUMBITS(1) [],
        /// SC/MC/PID（非 DDMA 为 PID；ISO 为 MC）
        PID OFFSET(29) NUMBITS(2) [
            Data0 = 0,
            Data2 = 1,
            Data1 = 2,
            Setup = 3,
        ],
        /// 包计数
        PKTCNT OFFSET(19) NUMBITS(10) [],
        /// 传输描述符数 − 1（DDMA）
        NTD OFFSET(8) NUMBITS(8) [],
        /// 调度信息（DDMA ISO 微帧位图）
        SCHINFO OFFSET(0) NUMBITS(8) [],
        /// 传输大小（非 DDMA）
        XFERSIZE OFFSET(0) NUMBITS(19) [],
    ]
];

register_bitfields![u32,
    /// HFIR — 主机帧间隔
    pub HFIR [
        /// 运行期重载控制（Linux reload_ctl；初始配置后不可再改）
        RLDCTRL OFFSET(16) NUMBITS(1) [],
        /// 帧间隔（PHY 时钟数 − 1）
        FRINT OFFSET(0) NUMBITS(16) [],
    ]
];

register_bitfields![u32,
    /// HFNUM — 主机帧号
    pub HFNUM [
        /// 帧剩余
        FRREM OFFSET(16) NUMBITS(16) [],
        /// 帧号
        FRNUM OFFSET(0) NUMBITS(16) [],
    ]
];

register_bitfields![u32,
    /// HAINT / HAINTMSK — 主机全通道中断
    pub HAINT [
        /// 各通道中断位（bit n = 通道 n）
        CHANNELS OFFSET(0) NUMBITS(16) [],
    ]
];

register_bitfields![u32,
    /// FIFO 深度寄存器（GRXFSIZ/GNPTXFSIZ/HPTXFSIZ）
    pub FIFO [
        /// FIFO 深度（字）
        DEPTH OFFSET(0) NUMBITS(16) [],
        /// RAM 起始地址（字）
        STARTADDR OFFSET(16) NUMBITS(16) [],
    ]
];

// ═══════════════════════════════════════════
// 寄存器布局（register_structs!）
// ═══════════════════════════════════════════

register_structs! {
    /// 单通道寄存器组（每通道 0x20，HC_BASE + ch × 0x20）
    pub HcChannelRegs {
        (0x00 => pub hcchar: ReadWrite<u32, HCCHAR::Register>),
        (0x04 => pub hcsplt: ReadWrite<u32, HCSPLT::Register>),
        (0x08 => pub hcint: ReadWrite<u32, HCINT::Register>),
        (0x0c => pub hcintmsk: ReadWrite<u32, HCINTMSK::Register>),
        (0x10 => pub hctsiz: ReadWrite<u32, HCTSIZ::Register>),
        (0x14 => pub hcdma: ReadWrite<u32>),
        (0x18 => _rsv1),
        (0x1c => _rsv2),
        (0x20 => @END),
    }
}

register_structs! {
    /// DWC2 寄存器窗口（全局 0x000 + FIFO 0x100 + 主机 0x400 + 通道 0x500）
    pub Dwc2Regs {
        (0x000 => pub gotgctl: ReadWrite<u32, GOTGCTL::Register>),
        (0x004 => _rsv1),
        (0x008 => pub gahbcfg: ReadWrite<u32, GAHBCFG::Register>),
        (0x00c => pub gusbcfg: ReadWrite<u32, GUSBCFG::Register>),
        (0x010 => pub grstctl: ReadWrite<u32, GRSTCTL::Register>),
        (0x014 => pub gintsts: ReadWrite<u32, GINTSTS::Register>),
        (0x018 => pub gintmsk: ReadWrite<u32, GINTMSK::Register>),
        (0x01c => _rsv2),
        (0x020 => _rsv3),
        (0x024 => pub grxfsiz: ReadWrite<u32, FIFO::Register>),
        (0x028 => pub gnptxfsiz: ReadWrite<u32, FIFO::Register>),
        (0x02c => _rsv_fifo: [u32; 5]),
        (0x040 => pub gsnpsid: ReadOnly<u32>),
        (0x044 => _rsv_g1),
        (0x048 => pub ghwcfg2: ReadOnly<u32, GHWCFG2::Register>),
        (0x04c => pub ghwcfg3: ReadOnly<u32, GHWCFG3::Register>),
        (0x050 => pub ghwcfg4: ReadOnly<u32, GHWCFG4::Register>),
        (0x054 => _rsv_globals: [u32; 43]),
        (0x100 => pub hptxfsiz: ReadWrite<u32, FIFO::Register>),
        (0x104 => _rsv_host0: [u32; 191]),
        (0x400 => pub hcfg: ReadWrite<u32, HCFG::Register>),
        (0x404 => pub hfir: ReadWrite<u32, HFIR::Register>),
        (0x408 => pub hfnum: ReadOnly<u32, HFNUM::Register>),
        (0x40c => _rsv6),
        (0x410 => _rsv7),
        (0x414 => pub haint: ReadWrite<u32, HAINT::Register>),
        (0x418 => pub haintmsk: ReadWrite<u32, HAINT::Register>),
        (0x41c => pub hflbaddr: ReadWrite<u32>),
        (0x420 => _rsv_host1: [u32; 8]),
        (0x440 => pub hprt: ReadWrite<u32, HPRT::Register>),
        (0x444 => _rsv_host2: [u32; 47]),
        (0x500 => pub hc: [HcChannelRegs; DWC2_MAX_CHANNELS as usize]),
        (0x700 => _rsv_pcg: [u32; 448]),
        (0xe00 => pub pcgctl: ReadWrite<u32>),
        (0xe04 => @END),
    }
}

/// DWC2 寄存器的能力边界
#[derive(Clone, Copy)]
pub(crate) struct Dwc2Registers {
    pub(crate) base: NonNull<u8>,
}

unsafe impl Send for Dwc2Registers {}
unsafe impl Sync for Dwc2Registers {}

impl Dwc2Registers {
    pub(crate) fn new(base: Mmio) -> Self {
        Self { base }
    }

    pub(crate) fn regs(&self) -> &'static Dwc2Regs {
        unsafe { &*(self.base.as_ptr() as *const Dwc2Regs) }
    }

    /// 读取主机通道数（NUM_HOST_CHAN + 1，最小 2，最大 DWC2_MAX_CHANNELS）
    pub(crate) fn host_channel_count(&self) -> u8 {
        let raw = self.regs().ghwcfg2.read(GHWCFG2::NUM_HOST_CHAN);
        ((raw + 1) as u8).clamp(2, DWC2_MAX_CHANNELS)
    }

    /// 控制器是否支持描述符 DMA（GHWCFG4.DESC_DMA，只读硬件参数）。
    pub(crate) fn is_support_ddma(&self) -> bool {
        self.regs().ghwcfg4.is_set(GHWCFG4::DESC_DMA)
    }

    /// HPRT 端口
    pub(crate) fn hprt(self) -> Hprt<'static> {
        Hprt::new(&self.regs().hprt)
    }

    /// host channel 寄存器组
    pub(crate) fn channel(self, channel: u8) -> HcChannel<'static> {
        HcChannel::new(&self.regs().hc[usize::from(channel)])
    }

    /// 返回当前帧号（HFNUM.FRNUM，低 3 位为 HS 微帧位）。
    pub(crate) fn frame_number(&self) -> u32 {
        self.regs().hfnum.read(HFNUM::FRNUM)
    }

    /// 返回当前帧号的奇偶位（HFNUM.FRNUM & 1），用于设置 HCCHAR.ODDFRM
    pub(crate) fn periodic_odd_frame_bit(&self) -> u32 {
        if self.regs().hfnum.read(HFNUM::FRNUM) & 1 == 0 {
            HCCHAR_ODDFRM
        } else {
            0
        }
    }
}

/// HPRT 端口的能力边界
pub(crate) struct Hprt<'a> {
    reg: &'a ReadWrite<u32, HPRT::Register>,
}

impl<'a> Hprt<'a> {
    pub(crate) fn new(reg: &'a ReadWrite<u32, HPRT::Register>) -> Self {
        Self { reg }
    }

    /// HPRT 原始值
    pub(crate) fn raw(&self) -> u32 {
        self.reg.get()
    }

    /// 是否连接（CONNSTS；复位完成后硬件置位）
    pub(crate) fn is_connected(&self) -> bool {
        self.reg.is_set(HPRT::CONNSTS)
    }

    /// 端口是否已使能（ENA；复位完成后硬件置位）
    pub(crate) fn is_enabled(&self) -> bool {
        self.reg.is_set(HPRT::ENA)
    }

    pub(crate) fn speed(&self) -> Speed {
        match self.reg.read(HPRT::SPD) {
            0 => Speed::High,
            1 => Speed::Full,
            2 => Speed::Low,
            _ => Speed::Full, // 默认值
        }
    }

    /// 清除连接检测中断（CONNDET；W1C）
    pub(crate) fn clear_connect_detect(&self) {
        let current = self.reg.get();
        if current & HPRT_CONN_DET != 0 {
            self.reg.set((current & !HPRT_W1C_MASK) | HPRT_CONN_DET);
        }
    }

    pub(crate) fn write_safe(&self, value: u32) {
        self.reg.set(value & !HPRT_W1C_MASK);
    }

    pub(crate) fn update_safe(&self, f: impl FnOnce(u32) -> u32) {
        let value = self.reg.get() & !HPRT_W1C_MASK;
        self.reg.set(f(value) & !HPRT_W1C_MASK);
    }

    pub(crate) fn write(&self, value: u32) {
        self.reg.set(value);
    }
}

/// host channel 寄存器组能力边界
pub(crate) struct HcChannel<'a> {
    reg: &'a HcChannelRegs,
}

impl<'a> HcChannel<'a> {
    pub(crate) fn new(reg: &'a HcChannelRegs) -> Self {
        Self { reg }
    }

    /// 通道是否已使能（HCCHAR.CHENA）
    pub(crate) fn is_enabled(&self) -> bool {
        self.reg.hcchar.is_set(HCCHAR::CHENA)
    }

    /// 通道使能：写 HCCHAR 并置 CHENA
    pub(crate) fn enable(&self, hcchar: u32) {
        self.reg.hcchar.set(hcchar | u32::from(HCCHAR::CHENA::SET));
    }

    /// 停止通道：CHENA 时写 CHENA|CHDIS
    pub(crate) fn disable(&self) {
        if self.is_enabled() {
            self.reg
                .hcchar
                .modify(HCCHAR::CHENA::CLEAR + HCCHAR::CHDIS::SET);
        }
    }

    /// 使能传输终止中断（CHHLTD；DDMA 下故障由硬件 halt 通道后随 CHHLTD 读出）
    pub(crate) fn enable_irqs(&self) {
        self.reg.hcintmsk.set(HCINT_CHHLTD);
    }

    /// 直接编程 HCINTMSK（ISO 路径按传输类型选择 XFERCOMPL + 故障位）。
    pub(crate) fn set_hcintmsk(&self, value: u32) {
        self.reg.hcintmsk.set(value);
    }

    /// 清除该通道所有已置位的中断（HCINT W1C 全清）
    pub(crate) fn clear_all_irqs(&self) {
        self.reg.hcint.set(HCINT_ALL_W1C);
    }

    /// 取走并确认一次通道中断，返回有效中断位（CHHLTD 时返回原始位）
    pub(crate) fn take_irqs(&self) -> Option<u32> {
        let raw = self.reg.hcint.get() & HCINT_TRANSFER_MASK;
        let masked = raw & self.reg.hcintmsk.get();
        if masked == 0 {
            return None;
        }
        self.reg.hcint.set(raw);
        Some(if masked & HCINT_CHHLTD != 0 {
            raw
        } else {
            masked
        })
    }

    pub(crate) fn set_hctsiz(&self, value: u32) {
        self.reg.hctsiz.set(value);
    }

    pub(crate) fn set_hcsplt(&self, value: u32) {
        self.reg.hcsplt.set(value);
    }

    /// BDMA: 设置缓冲区地址
    /// DDMA: 设置描述符表地址
    pub(crate) fn set_hcdma(&self, value: u32) {
        self.reg.hcdma.set(value);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn host_channel_count_reads_ghwcfg2_and_clamps() {
        // 未预置（全零）→ NUM_HOST_CHAN = 0 → 钳制到 2。
        let (mut backing, regs) = crate::backend::kmod::dwc2::testutil::test_regs();
        assert_eq!(regs.host_channel_count(), 2);
        assert!(!regs.is_support_ddma());

        // 预置 8 通道 + DDMA。
        crate::backend::kmod::dwc2::testutil::preset_hw_caps(&mut backing);
        assert_eq!(regs.host_channel_count(), 8);
        assert!(regs.is_support_ddma());
    }

    #[test]
    fn hcchannel_take_irqs_returns_raw_reason_on_channel_halt() {
        let (_backing, regs) = crate::backend::kmod::dwc2::testutil::test_regs();
        let channel = regs.channel(5);

        // 屏蔽位只有 CHHLTD：halt 时返回原始完整位（故障位随行）。
        channel.set_hcintmsk(HCINT_CHHLTD);
        regs.regs().hc[5].hcint.set(HCINT_CHHLTD | HCINT_STALL);
        assert_eq!(channel.take_irqs(), Some(HCINT_CHHLTD | HCINT_STALL));

        // CHHLTD 已置位时，未屏蔽的 NAK 等状态位也随原始值保留。
        regs.regs().hc[5]
            .hcint
            .set(HCINT_CHHLTD | HCINT_STALL | HCINT_NAK);
        assert_eq!(
            channel.take_irqs(),
            Some(HCINT_CHHLTD | HCINT_STALL | HCINT_NAK)
        );

        // 中断位已消费且无新事件 → None（内存兜底寄存器需显式清 W1C 状态）。
        regs.regs().hc[5].hcint.set(0);
        assert_eq!(channel.take_irqs(), None);
    }
}
