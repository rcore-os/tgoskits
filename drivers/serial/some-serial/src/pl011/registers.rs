use super::*;

register_bitfields! [
    u32,

    /// Data Register
    pub UARTDR [
        DATA OFFSET(0) NUMBITS(8) [],
        FE OFFSET(8) NUMBITS(1) [],
        PE OFFSET(9) NUMBITS(1) [],
        BE OFFSET(10) NUMBITS(1) [],
        OE OFFSET(11) NUMBITS(1) []
    ],

    /// Receive Status Register / Error Clear Register
    pub UARTRSR_ECR [
        FE OFFSET(0) NUMBITS(1) [],
        PE OFFSET(1) NUMBITS(1) [],
        BE OFFSET(2) NUMBITS(1) [],
        OE OFFSET(3) NUMBITS(1) []
    ],

    /// Flag Register
    pub UARTFR [
        CTS OFFSET(0) NUMBITS(1) [],
        DSR OFFSET(1) NUMBITS(1) [],
        DCD OFFSET(2) NUMBITS(1) [],
        BUSY OFFSET(3) NUMBITS(1) [],
        RXFE OFFSET(4) NUMBITS(1) [],
        TXFF OFFSET(5) NUMBITS(1) [],
        RXFF OFFSET(6) NUMBITS(1) [],
        TXFE OFFSET(7) NUMBITS(1) [],
        RI OFFSET(8) NUMBITS(1) []
    ],

    /// Integer Baud Rate Register
    pub UARTIBRD [
        BAUD_DIVINT OFFSET(0) NUMBITS(16) []
    ],

    /// Fractional Baud Rate Register
    pub UARTFBRD [
        BAUD_DIVFRAC OFFSET(0) NUMBITS(6) []
    ],

    /// Line Control Register
    pub UARTLCR_H [
        BRK OFFSET(0) NUMBITS(1) [],
        PEN OFFSET(1) NUMBITS(1) [],
        EPS OFFSET(2) NUMBITS(1) [],
        STP2 OFFSET(3) NUMBITS(1) [],
        FEN OFFSET(4) NUMBITS(1) [],
        WLEN OFFSET(5) NUMBITS(2) [
            FiveBit = 0,
            SixBit = 1,
            SevenBit = 2,
            EightBit = 3
        ],
        SPS OFFSET(7) NUMBITS(1) []
    ],

    /// Control Register
    pub UARTCR [
        UARTEN OFFSET(0) NUMBITS(1) [],
        SIREN OFFSET(1) NUMBITS(1) [],
        SIRLP OFFSET(2) NUMBITS(1) [],
        LBE OFFSET(7) NUMBITS(1) [],
        TXE OFFSET(8) NUMBITS(1) [],
        RXE OFFSET(9) NUMBITS(1) [],
        DTR OFFSET(10) NUMBITS(1) [],
        RTS OFFSET(11) NUMBITS(1) [],
        OUT1 OFFSET(12) NUMBITS(1) [],
        OUT2 OFFSET(13) NUMBITS(1) [],
        RTSEN OFFSET(14) NUMBITS(1) [],
        CTSEN OFFSET(15) NUMBITS(1) []
    ],

    /// Interrupt FIFO Level Select Register
    pub UARTIFLS [
        TXIFLSEL OFFSET(0) NUMBITS(3) [],
        RXIFLSEL OFFSET(3) NUMBITS(3) []
    ],

    /// Interrupt Mask Set/Clear Register
    pub UARTIS [
        RIM OFFSET(0) NUMBITS(1) [],
        CTSM OFFSET(1) NUMBITS(1) [],
        DCDM OFFSET(2) NUMBITS(1) [],
        DSRM OFFSET(3) NUMBITS(1) [],
        RX OFFSET(4) NUMBITS(1) [],
        TX OFFSET(5) NUMBITS(1) [],
        RT OFFSET(6) NUMBITS(1) [],
        FE OFFSET(7) NUMBITS(1) [],
        PE OFFSET(8) NUMBITS(1) [],
        BE OFFSET(9) NUMBITS(1) [],
        OE OFFSET(10) NUMBITS(1) []
    ],

    /// DMA Control Register
    pub UARTDMACR [
        RXDMAE OFFSET(0) NUMBITS(1) [],
        TXDMAE OFFSET(1) NUMBITS(1) [],
        DMAONERR OFFSET(2) NUMBITS(1) []
    ]
];

register_structs! {
    pub Pl011Registers {
        (0x000 => pub(super) uartdr: ReadWrite<u32, UARTDR::Register>),        // 数据寄存器（收发数据/错误标志）
        (0x004 => pub(super) uartrsr_ecr: ReadWrite<u32, UARTRSR_ECR::Register>), // 接收状态/错误清除寄存器
        (0x008 => _reserved1),                                      // 保留
        (0x018 => pub(super) uartfr: ReadOnly<u32, UARTFR::Register>),         // 标志寄存器（状态标志，如忙/空/满等）
        (0x01c => _reserved2),                                      // 保留
        (0x020 => pub(super) uartilpr: ReadWrite<u32>),                        // 红外低功耗波特率寄存器（很少用）
        (0x024 => pub(super) uartibrd: ReadWrite<u32, UARTIBRD::Register>),    // 整数波特率分频寄存器
        (0x028 => pub(super) uartfbrd: ReadWrite<u32, UARTFBRD::Register>),    // 小数波特率分频寄存器
        (0x02c => pub(super) uartlcr_h: ReadWrite<u32, UARTLCR_H::Register>),  // 线路控制寄存器（数据位、停止位、校验等）
        (0x030 => pub(super) uartcr: ReadWrite<u32, UARTCR::Register>),        // 控制寄存器（UART使能、收发使能等）
        (0x034 => pub(super) uartifls: ReadWrite<u32, UARTIFLS::Register>),    // FIFO中断触发级别选择寄存器
        (0x038 => pub(super) uartimsc: ReadWrite<u32, UARTIS::Register>),      // 中断屏蔽设置/清除寄存器
        (0x03c => pub(super) uartris: ReadOnly<u32, UARTIS::Register>),        // 原始中断状态寄存器
        (0x040 => pub(super) uartmis: ReadOnly<u32, UARTIS::Register>),        // 屏蔽后的中断状态寄存器
        (0x044 => pub(super) uarticr: WriteOnly<u32, UARTIS::Register>),       // 中断清除寄存器
        (0x048 => pub(super) uartdmacr: ReadWrite<u32, UARTDMACR::Register>),  // DMA控制寄存器
        (0x04c => _reserved3),                                      // 保留
        (0x1000 => @END),
    }
}

// SAFETY: PL011 寄存器访问是原子的，硬件保证了内存映射寄存器的线程安全
unsafe impl Sync for Pl011Registers {}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Reg(pub(super) NonNull<Pl011Registers>);

unsafe impl Send for Reg {}
unsafe impl Sync for Reg {}
