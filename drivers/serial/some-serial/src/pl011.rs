use core::ptr::NonNull;

use rdif_serial::{
    Config, ConfigError, DataBits, IrqRxSink, Parity, RxErrorFlags, RxFlag, RxSample,
    SerialEventSet, SerialIrqEvent, SplitUart, StopBits, UartInfo, UartIrq, UartParts, UartPort,
};
use tock_registers::{
    LocalRegisterCopy, interfaces::*, register_bitfields, register_structs, registers::*,
};

use crate::{PollingUart, SerialDirection, SerialEvent, TransBytesError, TransferError};

register_bitfields! [
    u32,

    /// Data Register
    UARTDR [
        DATA OFFSET(0) NUMBITS(8) [],
        FE OFFSET(8) NUMBITS(1) [],
        PE OFFSET(9) NUMBITS(1) [],
        BE OFFSET(10) NUMBITS(1) [],
        OE OFFSET(11) NUMBITS(1) []
    ],

    /// Receive Status Register / Error Clear Register
    UARTRSR_ECR [
        FE OFFSET(0) NUMBITS(1) [],
        PE OFFSET(1) NUMBITS(1) [],
        BE OFFSET(2) NUMBITS(1) [],
        OE OFFSET(3) NUMBITS(1) []
    ],

    /// Flag Register
    UARTFR [
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
    UARTIBRD [
        BAUD_DIVINT OFFSET(0) NUMBITS(16) []
    ],

    /// Fractional Baud Rate Register
    UARTFBRD [
        BAUD_DIVFRAC OFFSET(0) NUMBITS(6) []
    ],

    /// Line Control Register
    UARTLCR_H [
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
    UARTCR [
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
    UARTIFLS [
        TXIFLSEL OFFSET(0) NUMBITS(3) [],
        RXIFLSEL OFFSET(3) NUMBITS(3) []
    ],

    /// Interrupt Mask Set/Clear Register
    UARTIS [
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
    UARTDMACR [
        RXDMAE OFFSET(0) NUMBITS(1) [],
        TXDMAE OFFSET(1) NUMBITS(1) [],
        DMAONERR OFFSET(2) NUMBITS(1) []
    ]
];

register_structs! {
    pub Pl011Registers {
        (0x000 => uartdr: ReadWrite<u32, UARTDR::Register>),        // 数据寄存器（收发数据/错误标志）
        (0x004 => uartrsr_ecr: ReadWrite<u32, UARTRSR_ECR::Register>), // 接收状态/错误清除寄存器
        (0x008 => _reserved1),                                      // 保留
        (0x018 => uartfr: ReadOnly<u32, UARTFR::Register>),         // 标志寄存器（状态标志，如忙/空/满等）
        (0x01c => _reserved2),                                      // 保留
        (0x020 => uartilpr: ReadWrite<u32>),                        // 红外低功耗波特率寄存器（很少用）
        (0x024 => uartibrd: ReadWrite<u32, UARTIBRD::Register>),    // 整数波特率分频寄存器
        (0x028 => uartfbrd: ReadWrite<u32, UARTFBRD::Register>),    // 小数波特率分频寄存器
        (0x02c => uartlcr_h: ReadWrite<u32, UARTLCR_H::Register>),  // 线路控制寄存器（数据位、停止位、校验等）
        (0x030 => uartcr: ReadWrite<u32, UARTCR::Register>),        // 控制寄存器（UART使能、收发使能等）
        (0x034 => uartifls: ReadWrite<u32, UARTIFLS::Register>),    // FIFO中断触发级别选择寄存器
        (0x038 => uartimsc: ReadWrite<u32, UARTIS::Register>),      // 中断屏蔽设置/清除寄存器
        (0x03c => uartris: ReadOnly<u32, UARTIS::Register>),        // 原始中断状态寄存器
        (0x040 => uartmis: ReadOnly<u32, UARTIS::Register>),        // 屏蔽后的中断状态寄存器
        (0x044 => uarticr: WriteOnly<u32, UARTIS::Register>),       // 中断清除寄存器
        (0x048 => uartdmacr: ReadWrite<u32, UARTDMACR::Register>),  // DMA控制寄存器
        (0x04c => _reserved3),                                      // 保留
        (0x1000 => @END),
    }
}

// SAFETY: PL011 寄存器访问是原子的，硬件保证了内存映射寄存器的线程安全
unsafe impl Sync for Pl011Registers {}

/// PL011 UART 驱动结构体
pub struct Pl011 {
    base: Reg,
    clock_freq: u32,
    saved_rx_status: Pl011RxStatus,
}

impl Pl011 {
    /// 创建新的 PL011 实例（仅基地址，使用默认配置）
    ///
    /// # Arguments
    /// * `base` - UART 寄存器基地址
    pub fn new_no_clock(base: NonNull<u8>) -> Self {
        // 自动检测时钟频率或使用合理的默认值
        let clock_freq = Self::detect_clock_frequency(base.as_ptr() as usize);
        Self::new(base, clock_freq)
    }

    pub fn new(base: NonNull<u8>, clock_freq: u32) -> Self {
        let base = Reg(base.cast());

        Self {
            base,
            clock_freq,
            saved_rx_status: Pl011RxStatus::empty(),
        }
    }

    fn registers(&self) -> &Pl011Registers {
        unsafe { &*self.base.0.as_ptr() }
    }

    fn current_baudrate(&self) -> u32 {
        let ibrd = self.registers().uartibrd.read(UARTIBRD::BAUD_DIVINT);
        let fbrd = self.registers().uartfbrd.read(UARTFBRD::BAUD_DIVFRAC);
        let divisor = ibrd * 64 + fbrd;
        if divisor == 0 {
            0
        } else {
            self.clock_freq * 64 / (16 * divisor)
        }
    }

    /// 自动检测或确定合理的时钟频率
    fn detect_clock_frequency(base: usize) -> u32 {
        // 尝试读取当前波特率设置来反向推算时钟频率
        let registers = unsafe { &*(base as *const Pl011Registers) };

        use tock_registers::interfaces::Readable;
        let ibrd = registers.uartibrd.read(UARTIBRD::BAUD_DIVINT);

        // 如果有设置值，假设波特率为 115200 来估算时钟频率
        if ibrd > 0 && ibrd <= 0xFFFF {
            // 假设波特率为 115200，计算时钟频率
            // FUARTCLK = 16 * BAUDDIV * Baud rate
            let estimated_clock = 16 * ibrd * 115200;

            // 合理的时钟频率范围：1MHz - 100MHz
            if (1_000_000..=100_000_000).contains(&estimated_clock) {
                return estimated_clock;
            }
        }

        // 默认使用 24MHz（最常见）
        24_000_000
    }

    // 内部私有方法，用于配置
    fn set_baudrate_internal(&self, baudrate: u32) -> Result<(), ConfigError> {
        // PL011 波特率计算公式：
        // BAUDDIV = (FUARTCLK / (16 * Baud rate))
        // IBRD = integer(BAUDDIV)
        // FBRD = integer((BAUDDIV - IBRD) * 64 + 0.5)

        let bauddiv = self.clock_freq / (16 * baudrate);
        let remainder = self.clock_freq % (16 * baudrate);
        let fbrd = (remainder * 64 + (16 * baudrate / 2)) / (16 * baudrate);

        if bauddiv == 0 || bauddiv > 0xFFFF {
            return Err(ConfigError::InvalidBaudrate);
        }

        self.registers()
            .uartibrd
            .write(UARTIBRD::BAUD_DIVINT.val(bauddiv));
        self.registers()
            .uartfbrd
            .write(UARTFBRD::BAUD_DIVFRAC.val(fbrd));

        Ok(())
    }

    fn set_data_bits_internal(&self, bits: DataBits) -> Result<(), ConfigError> {
        let wlen = match bits {
            DataBits::Five => UARTLCR_H::WLEN::FiveBit,
            DataBits::Six => UARTLCR_H::WLEN::SixBit,
            DataBits::Seven => UARTLCR_H::WLEN::SevenBit,
            DataBits::Eight => UARTLCR_H::WLEN::EightBit,
        };

        self.registers().uartlcr_h.modify(wlen);
        Ok(())
    }

    fn set_stop_bits_internal(&self, bits: StopBits) -> Result<(), ConfigError> {
        match bits {
            StopBits::One => self.registers().uartlcr_h.modify(UARTLCR_H::STP2::CLEAR),
            StopBits::Two => self.registers().uartlcr_h.modify(UARTLCR_H::STP2::SET),
        }

        Ok(())
    }

    fn set_parity_internal(&self, parity: Parity) -> Result<(), ConfigError> {
        match parity {
            Parity::None => {
                // PEN = 0, 无奇偶校验
                self.registers().uartlcr_h.modify(UARTLCR_H::PEN::CLEAR);
            }
            Parity::Odd => {
                // PEN = 1, EPS = 0 (奇校验), SPS = 0
                self.registers()
                    .uartlcr_h
                    .modify(UARTLCR_H::PEN::SET + UARTLCR_H::EPS::CLEAR + UARTLCR_H::SPS::CLEAR);
            }
            Parity::Even => {
                // PEN = 1, EPS = 1 (偶校验), SPS = 0
                self.registers()
                    .uartlcr_h
                    .modify(UARTLCR_H::PEN::SET + UARTLCR_H::EPS::SET + UARTLCR_H::SPS::CLEAR);
            }
            Parity::Mark => {
                // PEN = 1, SPS = 1, EPS = 0 (奇校验)
                self.registers()
                    .uartlcr_h
                    .modify(UARTLCR_H::PEN::SET + UARTLCR_H::EPS::CLEAR + UARTLCR_H::SPS::SET);
            }
            Parity::Space => {
                // PEN = 1, EPS = 1 (偶校验), SPS = 1
                self.registers()
                    .uartlcr_h
                    .modify(UARTLCR_H::PEN::SET + UARTLCR_H::EPS::SET + UARTLCR_H::SPS::SET);
            }
        }

        Ok(())
    }

    /// 初始化 PL011 UART
    pub fn open(&mut self) {
        // 禁用 UART
        self.registers().uartcr.modify(UARTCR::UARTEN::CLEAR);

        // 等待当前传输完成
        while self.registers().uartfr.is_set(UARTFR::BUSY) {
            core::hint::spin_loop();
        }

        // 清除发送 FIFO
        self.registers().uartlcr_h.modify(UARTLCR_H::FEN::CLEAR);

        // 启用 FIFO
        self.registers().uartlcr_h.modify(UARTLCR_H::FEN::SET);

        // 调试信息：输出 FIFO 配置
        #[cfg(debug_assertions)]
        {
            let ifls = self.registers().uartifls.get();
            let lcr_h = self.registers().uartlcr_h.get();
            log::debug!("UART IFLS: 0x{:02x}, LCR_H: 0x{:02x}", ifls, lcr_h);
            log::debug!("  FIFO enabled: {}", lcr_h & (1 << 4) != 0);
            log::debug!("  RX trigger level: 1/8");
            log::debug!("  TX trigger level: 1/2");
        }
        self.registers().uartimsc.set(0); // 禁用所有中断
        // 启用 UART
        self.registers()
            .uartcr
            .modify(UARTCR::UARTEN::SET + UARTCR::TXE::SET + UARTCR::RXE::SET);
    }

    pub fn set_irq_mask(&mut self, events: SerialEventSet) {
        self.registers().uartimsc.set(imsc_for_events(events));
    }

    pub fn get_irq_mask(&self) -> SerialEventSet {
        let imsc = self.registers().uartimsc.extract();
        let mut events = SerialEventSet::empty();

        if imsc.is_set(UARTIS::RX)
            || imsc.is_set(UARTIS::RT)
            || imsc.is_set(UARTIS::FE)
            || imsc.is_set(UARTIS::PE)
            || imsc.is_set(UARTIS::BE)
            || imsc.is_set(UARTIS::OE)
        {
            events |= SerialEventSet::RX;
        }
        if imsc.is_set(UARTIS::TX) {
            events |= SerialEventSet::TX_SPACE;
        }

        events
    }

    pub fn pending(&mut self, direction: SerialDirection) -> bool {
        match direction {
            SerialDirection::Input => !self.registers().uartfr.is_set(UARTFR::RXFE),
            SerialDirection::Output => !self.registers().uartfr.is_set(UARTFR::TXFF),
        }
    }

    pub fn poll_status(&mut self) -> SerialEvent {
        let mut event = SerialEvent::empty();
        let fr = self.registers().uartfr.extract();
        if !fr.is_set(UARTFR::RXFE) {
            event |= SerialEvent::RX_READY;
        }
        if !fr.is_set(UARTFR::TXFF) {
            event |= SerialEvent::TX_READY;
        }

        let status =
            self.saved_rx_status | Pl011RxStatus::from_rsr(self.registers().uartrsr_ecr.extract());
        if status.intersects(Pl011RxStatus::FRAMING | Pl011RxStatus::PARITY | Pl011RxStatus::BREAK)
        {
            event |= SerialEvent::RX_ERROR;
        }
        if status.contains(Pl011RxStatus::OVERRUN) {
            event |= SerialEvent::RX_ERROR | SerialEvent::OVERRUN;
        }

        event
    }

    pub fn try_write(&mut self, bytes: &[u8]) -> usize {
        let mut written = 0;
        for &byte in bytes {
            let status = self.poll_status();
            if !status.tx_ready() {
                break;
            }
            self.write_byte(byte);
            written += 1;
        }
        written
    }

    pub fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        let mut count = 0;
        for byte in bytes.iter_mut() {
            let status = self.poll_status();
            if !status.rx_ready() && !status.rx_error() {
                break;
            }
            match self.read_byte(status) {
                Some(Ok(b)) => {
                    *byte = b;
                }
                Some(Err(TransferError::Overrun(b))) => {
                    *byte = b;
                    count += 1;
                    return Err(TransBytesError {
                        bytes_transferred: count,
                        kind: TransferError::Overrun(b),
                    });
                }
                Some(Err(e)) => {
                    return Err(TransBytesError {
                        bytes_transferred: count,
                        kind: e,
                    });
                }
                None => break,
            }
            count += 1;
        }
        Ok(count)
    }

    pub fn write_byte(&mut self, byte: u8) {
        self.registers().uartdr.set(byte as _);
    }

    pub fn read_byte(&mut self, status: SerialEvent) -> Option<Result<u8, TransferError>> {
        if !status.rx_ready() && !status.rx_error() {
            return None;
        }

        let sample = self.read_rx()?;
        if sample.overrun {
            return Some(Err(TransferError::Overrun(sample.byte.unwrap_or(0))));
        }
        match sample.flag {
            RxFlag::Normal => sample.byte.map(Ok),
            RxFlag::Break => Some(Err(TransferError::Break)),
            RxFlag::Parity => Some(Err(TransferError::Parity)),
            RxFlag::Framing => Some(Err(TransferError::Framing)),
        }
    }

    pub fn read_rx(&mut self) -> Option<RxSample> {
        let base = self.base;
        // SAFETY: `base` is the mapped PL011 register block owned by this
        // endpoint and remains valid for the endpoint lifetime.
        let registers = unsafe { &*base.0.as_ptr() };
        read_rx_sample(registers, &mut self.saved_rx_status)
    }
}

fn read_rx_sample(
    registers: &Pl011Registers,
    saved_status: &mut Pl011RxStatus,
) -> Option<RxSample> {
    if registers.uartfr.is_set(UARTFR::RXFE) {
        *saved_status |= Pl011RxStatus::from_rsr(registers.uartrsr_ecr.extract());
        return saved_status.take_status_sample();
    }

    let dr = registers.uartdr.extract();
    let data = dr.read(UARTDR::DATA) as u8;
    let status = Pl011RxStatus::from_data(dr);
    if !status.is_empty() {
        saved_status.remove(status);
    }

    Some(RxSample {
        byte: Some(data),
        flag: status.flag(),
        overrun: status.contains(Pl011RxStatus::OVERRUN),
    })
}

fn rx_errors_from_sample(sample: RxSample) -> RxErrorFlags {
    let mut errors = match sample.flag {
        RxFlag::Normal => RxErrorFlags::empty(),
        RxFlag::Break => RxErrorFlags::BREAK,
        RxFlag::Parity => RxErrorFlags::PARITY,
        RxFlag::Framing => RxErrorFlags::FRAMING,
    };
    if sample.overrun {
        errors |= RxErrorFlags::OVERRUN;
    }
    errors
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct Pl011RxStatus: u32 {
        const FRAMING = 1 << 0;
        const PARITY  = 1 << 1;
        const BREAK   = 1 << 2;
        const OVERRUN = 1 << 3;
    }
}

impl Pl011RxStatus {
    fn to_irq_errors(self) -> RxErrorFlags {
        let mut errors = RxErrorFlags::empty();
        if self.contains(Self::BREAK) {
            errors |= RxErrorFlags::BREAK;
        }
        if self.contains(Self::PARITY) {
            errors |= RxErrorFlags::PARITY;
        }
        if self.contains(Self::FRAMING) {
            errors |= RxErrorFlags::FRAMING;
        }
        if self.contains(Self::OVERRUN) {
            errors |= RxErrorFlags::OVERRUN;
        }
        errors
    }

    fn from_data(dr: LocalRegisterCopy<u32, UARTDR::Register>) -> Self {
        let mut status = Self::empty();
        if dr.is_set(UARTDR::FE) {
            status |= Self::FRAMING;
        }
        if dr.is_set(UARTDR::PE) {
            status |= Self::PARITY;
        }
        if dr.is_set(UARTDR::BE) {
            status |= Self::BREAK;
        }
        if dr.is_set(UARTDR::OE) {
            status |= Self::OVERRUN;
        }
        status
    }

    fn from_irq_status(mis: LocalRegisterCopy<u32, UARTIS::Register>) -> Self {
        let mut status = Self::empty();
        if mis.is_set(UARTIS::FE) {
            status |= Self::FRAMING;
        }
        if mis.is_set(UARTIS::PE) {
            status |= Self::PARITY;
        }
        if mis.is_set(UARTIS::BE) {
            status |= Self::BREAK;
        }
        if mis.is_set(UARTIS::OE) {
            status |= Self::OVERRUN;
        }
        status
    }

    fn from_rsr(rsr: LocalRegisterCopy<u32, UARTRSR_ECR::Register>) -> Self {
        let mut status = Self::empty();
        if rsr.is_set(UARTRSR_ECR::FE) {
            status |= Self::FRAMING;
        }
        if rsr.is_set(UARTRSR_ECR::PE) {
            status |= Self::PARITY;
        }
        if rsr.is_set(UARTRSR_ECR::BE) {
            status |= Self::BREAK;
        }
        if rsr.is_set(UARTRSR_ECR::OE) {
            status |= Self::OVERRUN;
        }
        status
    }

    fn flag(self) -> RxFlag {
        if self.contains(Self::BREAK) {
            RxFlag::Break
        } else if self.contains(Self::PARITY) {
            RxFlag::Parity
        } else if self.contains(Self::FRAMING) {
            RxFlag::Framing
        } else {
            RxFlag::Normal
        }
    }

    fn take_status_sample(&mut self) -> Option<RxSample> {
        if self.is_empty() {
            return None;
        }

        let status = *self;
        *self = Self::empty();
        Some(RxSample {
            byte: None,
            flag: status.flag(),
            overrun: status.contains(Self::OVERRUN),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Reg(NonNull<Pl011Registers>);

unsafe impl Send for Reg {}
unsafe impl Sync for Reg {}

/// IRQ-only endpoint for a PL011 UART.
pub struct Pl011Irq {
    base: Reg,
    saved_rx_status: Pl011RxStatus,
}

impl Pl011Irq {
    fn registers(&self) -> &Pl011Registers {
        // SAFETY: `base` points at the mapped PL011 register block. The IRQ
        // endpoint intentionally exposes no FIFO data methods.
        unsafe { &*self.base.0.as_ptr() }
    }
}

impl UartIrq for Pl011Irq {
    fn handle(&mut self, rx: &mut dyn IrqRxSink) -> Option<SerialIrqEvent> {
        const RX_SAMPLE_BUDGET: usize = 256;

        let mis = self.registers().uartmis.extract();
        let active = mis.get();
        if active == 0 {
            return None;
        }

        let mut events = events_from_mis(mis);
        if active & !0x7ff != 0 {
            events |= SerialEventSet::FAULT;
        }
        let mut rx_errors = rx_errors_from_mis(mis);
        if events.intersects(SerialEventSet::RX) {
            let base = self.base;
            // SAFETY: `base` is the mapped PL011 register block shared with
            // the task endpoint under the runtime's same-CPU exclusion rule.
            let registers = unsafe { &*base.0.as_ptr() };
            for _ in 0..RX_SAMPLE_BUDGET {
                let Some(sample) = read_rx_sample(registers, &mut self.saved_rx_status) else {
                    break;
                };
                rx_errors |= rx_errors_from_sample(sample);
                rx.push(sample);
            }
        }

        let rearm = events & SerialEventSet::TX_SPACE;
        if events.contains(SerialEventSet::FAULT) {
            self.registers().uartimsc.set(0);
        } else if !rearm.is_empty() {
            let enabled = self.registers().uartimsc.get();
            self.registers()
                .uartimsc
                .set(enabled & !imsc_for_events(rearm));
        }
        self.registers().uarticr.set(active);

        Some(SerialIrqEvent {
            events,
            rx_errors,
            rearm,
        })
    }
}

impl UartPort for Pl011 {
    fn startup(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.open();
        self.set_config(config)?;
        self.mask_all();
        Ok(())
    }

    fn shutdown(&mut self) {
        self.registers().uartimsc.set(0);
        self.registers().uartcr.modify(UARTCR::UARTEN::CLEAR);
    }

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        use tock_registers::interfaces::Readable;

        // 根据ARM文档的建议配置流程：
        // 1. 禁用UART
        let original_cr = self.registers().uartcr.extract(); // 保存原始使能状态
        self.registers().uartcr.modify(UARTCR::UARTEN::CLEAR); // 禁用UART

        // 2. 等待当前字符传输完成
        while self.registers().uartfr.is_set(UARTFR::BUSY) {
            core::hint::spin_loop();
        }

        // 3. 刷新发送FIFO（通过设置FEN=0）
        self.registers().uartlcr_h.modify(UARTLCR_H::FEN::CLEAR);

        // 4. 配置各项参数
        if let Some(baudrate) = config.baudrate {
            self.set_baudrate_internal(baudrate)?;
        }
        if let Some(data_bits) = config.data_bits {
            self.set_data_bits_internal(data_bits)?;
        }
        if let Some(stop_bits) = config.stop_bits {
            self.set_stop_bits_internal(stop_bits)?;
        }
        if let Some(parity) = config.parity {
            self.set_parity_internal(parity)?;
        }

        // 5. 重新启用FIFO
        self.registers().uartlcr_h.modify(UARTLCR_H::FEN::SET);

        // 6. 恢复UART使能状态
        if original_cr.is_set(UARTCR::UARTEN) {
            self.registers().uartcr.modify(
                UARTCR::UARTEN.val(original_cr.read(UARTCR::UARTEN))
                    + UARTCR::TXE.val(original_cr.read(UARTCR::TXE))
                    + UARTCR::RXE.val(original_cr.read(UARTCR::RXE)),
            );
        }

        Ok(())
    }

    fn read_rx(&mut self) -> Option<RxSample> {
        Pl011::read_rx(self)
    }

    fn write_tx(&mut self, bytes: &[u8]) -> usize {
        let mut written = 0;
        for &byte in bytes {
            if self.registers().uartfr.is_set(UARTFR::TXFF) {
                break;
            }
            self.registers().uartdr.set(byte as u32);
            written += 1;
        }
        written
    }

    fn tx_idle(&mut self) -> bool {
        let fr = self.registers().uartfr.extract();
        !fr.is_set(UARTFR::BUSY) && !fr.is_set(UARTFR::TXFF)
    }

    fn mask_all(&mut self) {
        self.registers().uartimsc.set(0);
    }

    fn rearm(&mut self, sources: SerialEventSet) -> SerialEventSet {
        let enabled = self.registers().uartimsc.get() | imsc_for_events(sources);
        self.registers().uartimsc.set(enabled);

        let fr = self.registers().uartfr.extract();
        let rsr = self.registers().uartrsr_ecr.extract();
        let mut ready = SerialEventSet::empty();
        if sources.intersects(SerialEventSet::RX) && !fr.is_set(UARTFR::RXFE) {
            ready |= SerialEventSet::RX_DATA;
        }
        if sources.contains(SerialEventSet::RX_STATUS) && !Pl011RxStatus::from_rsr(rsr).is_empty() {
            ready |= SerialEventSet::RX_STATUS;
        }
        if sources.contains(SerialEventSet::TX_SPACE) && !fr.is_set(UARTFR::TXFF) {
            ready |= SerialEventSet::TX_SPACE;
        }
        if !ready.is_empty() {
            self.registers()
                .uartimsc
                .set(enabled & !imsc_for_events(ready));
        }
        ready
    }
}

impl SplitUart for Pl011 {
    type Port = Self;
    type Irq = Pl011Irq;

    fn runtime_info(&self) -> UartInfo {
        UartInfo {
            name: "PL011 UART",
            register_base: self.base.0.as_ptr() as usize,
            initial_baudrate: self.current_baudrate(),
        }
    }

    fn split(self) -> UartParts<Self::Port, Self::Irq> {
        let irq = Pl011Irq {
            base: self.base,
            saved_rx_status: Pl011RxStatus::empty(),
        };
        UartParts::new(self, irq)
    }
}

impl PollingUart for Pl011 {
    fn poll_status(&mut self) -> SerialEvent {
        Pl011::poll_status(self)
    }

    fn write_byte(&mut self, byte: u8) {
        Pl011::write_byte(self, byte);
    }

    fn read_byte(&mut self, status: SerialEvent) -> Option<Result<u8, TransferError>> {
        Pl011::read_byte(self, status)
    }
}

fn events_from_mis(mis: LocalRegisterCopy<u32, UARTIS::Register>) -> SerialEventSet {
    let mut events = SerialEventSet::empty();
    if mis.is_set(UARTIS::RX) {
        events |= SerialEventSet::RX_DATA;
    }
    if mis.is_set(UARTIS::RT) {
        events |= SerialEventSet::RX_TIMEOUT;
    }
    if mis.is_set(UARTIS::FE)
        || mis.is_set(UARTIS::PE)
        || mis.is_set(UARTIS::BE)
        || mis.is_set(UARTIS::OE)
    {
        events |= SerialEventSet::RX_STATUS;
    }
    if mis.is_set(UARTIS::TX) {
        events |= SerialEventSet::TX_SPACE;
    }
    if mis.is_set(UARTIS::CTSM)
        || mis.is_set(UARTIS::DSRM)
        || mis.is_set(UARTIS::DCDM)
        || mis.is_set(UARTIS::RIM)
    {
        events |= SerialEventSet::MODEM_STATUS;
    }
    events
}

fn rx_errors_from_mis(mis: LocalRegisterCopy<u32, UARTIS::Register>) -> RxErrorFlags {
    Pl011RxStatus::from_irq_status(mis).to_irq_errors()
}

fn imsc_for_events(events: SerialEventSet) -> u32 {
    let mut imsc = 0;
    if events.intersects(SerialEventSet::RX) {
        imsc |= UARTIS::RX::SET.value
            | UARTIS::RT::SET.value
            | UARTIS::FE::SET.value
            | UARTIS::PE::SET.value
            | UARTIS::BE::SET.value
            | UARTIS::OE::SET.value;
    }
    if events.contains(SerialEventSet::TX_SPACE) {
        imsc |= UARTIS::TX::SET.value;
    }
    if events.contains(SerialEventSet::MODEM_STATUS) {
        imsc |= UARTIS::RIM::SET.value
            | UARTIS::CTSM::SET.value
            | UARTIS::DCDM::SET.value
            | UARTIS::DSRM::SET.value;
    }
    imsc
}

// 额外的便利方法，用于 FIFO 和流控制
impl Pl011 {
    /// 启用或禁用 FIFO
    pub fn enable_fifo(&self, enable: bool) {
        if enable {
            self.registers().uartlcr_h.modify(UARTLCR_H::FEN::SET);
        } else {
            self.registers().uartlcr_h.modify(UARTLCR_H::FEN::CLEAR);
        }
    }

    /// 设置 FIFO 触发级别
    pub fn set_fifo_trigger_level(&self, rx_level: u8, tx_level: u8) {
        // PL011 FIFO 触发级别：
        // 0b000: 1/8 full
        // 0b001: 1/4 full
        // 0b010: 1/2 full
        // 0b011: 3/4 full
        // 0b100: 7/8 full

        let rx_iflsel = match rx_level {
            0..=2 => 0b000,  // 1/8
            3..=4 => 0b001,  // 1/4
            5..=8 => 0b010,  // 1/2
            9..=12 => 0b011, // 3/4
            _ => 0b100,      // 7/8
        };

        let tx_iflsel = match tx_level {
            0..=2 => 0b000,  // 1/8
            3..=4 => 0b001,  // 1/4
            5..=8 => 0b010,  // 1/2
            9..=12 => 0b011, // 3/4
            _ => 0b100,      // 7/8
        };

        self.registers()
            .uartifls
            .write(UARTIFLS::RXIFLSEL.val(rx_iflsel) + UARTIFLS::TXIFLSEL.val(tx_iflsel));
    }
}

// ModemStatus 现在在 lib.rs 中定义，这里只是导出

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;
    use std::{boxed::Box, vec::Vec};

    use super::*;

    #[derive(Default)]
    struct CollectRx(Vec<RxSample>);

    impl IrqRxSink for CollectRx {
        fn push(&mut self, sample: RxSample) {
            self.0.push(sample);
        }
    }

    fn handle_irq(irq: &mut impl UartIrq) -> (Option<SerialIrqEvent>, Vec<RxSample>) {
        let mut rx = CollectRx::default();
        let event = irq.handle(&mut rx);
        (event, rx.0)
    }

    fn pl011_with_registers() -> (Box<Pl011Registers>, Pl011) {
        let mut regs = Box::new(unsafe { core::mem::zeroed::<Pl011Registers>() });
        let ptr = NonNull::from(regs.as_mut()).cast::<u8>();
        let uart = Pl011::new(ptr, 24_000_000);
        (regs, uart)
    }

    fn pl011_with_overrun_data() -> (Box<Pl011Registers>, Pl011) {
        let (regs, uart) = pl011_with_registers();
        regs.uartdr
            .set((UARTDR::DATA.val(0xab) + UARTDR::OE::SET).into());
        (regs, uart)
    }

    fn write_test_reg(regs: &mut Pl011Registers, offset: usize, value: u32) {
        unsafe {
            (regs as *mut Pl011Registers)
                .cast::<u32>()
                .add(offset / core::mem::size_of::<u32>())
                .write_volatile(value);
        }
    }

    fn read_test_reg(regs: &Pl011Registers, offset: usize) -> u32 {
        unsafe {
            (regs as *const Pl011Registers)
                .cast::<u32>()
                .add(offset / core::mem::size_of::<u32>())
                .read_volatile()
        }
    }

    fn started_parts(uart: Pl011) -> UartParts<Pl011, Pl011Irq> {
        let mut parts = uart.split();
        parts.port.startup(&Config::new()).unwrap();
        parts
    }

    #[test]
    fn raw_rx_reports_overrun_instead_of_swallowing_it() {
        let (_regs, mut uart) = pl011_with_overrun_data();

        let mut buf = [0];
        let err = uart
            .try_read(&mut buf)
            .expect_err("overrun must be reported to the caller");

        assert_eq!(buf[0], 0xab);
        assert_eq!(err.bytes_transferred, 1);
        assert_eq!(err.kind, TransferError::Overrun(0xab));
    }

    #[test]
    fn raw_rx_sample_reports_overrun_instead_of_swallowing_it() {
        let (mut regs, uart) = pl011_with_overrun_data();
        let mut parts = uart.split();

        write_test_reg(&mut regs, 0x040, UARTIS::OE::SET.value);
        let (event, samples) = handle_irq(&mut parts.irq);
        let event = event.unwrap();
        assert!(event.events.contains(SerialEventSet::RX_STATUS));
        assert!(event.rx_errors.contains(RxErrorFlags::OVERRUN));
        assert_eq!(
            samples.len(),
            256,
            "the hard IRQ must enforce its RX budget"
        );
        let sample = samples[0];
        assert_eq!(sample.byte, Some(0xab));
        assert_eq!(sample.flag, RxFlag::Normal);
        assert!(sample.overrun);
    }

    #[test]
    fn rx_irq_keeps_source_enabled_after_bounded_fifo_drain() {
        let (mut regs, uart) = pl011_with_registers();
        let mut irq = uart.split().irq;
        let rx_mask = imsc_for_events(SerialEventSet::RX);
        write_test_reg(&mut regs, 0x038, rx_mask);
        write_test_reg(&mut regs, 0x040, UARTIS::RX::SET.value);
        write_test_reg(&mut regs, 0x018, 0);
        regs.uartdr.set(UARTDR::DATA.val(b'r' as u32).into());

        let (event, samples) = handle_irq(&mut irq);
        let event = event.unwrap();

        assert!(event.events.contains(SerialEventSet::RX_DATA));
        assert!(!event.rearm.intersects(SerialEventSet::RX));
        assert_eq!(samples.len(), 256);
        assert_eq!(read_test_reg(&regs, 0x038) & rx_mask, rx_mask);
    }

    #[test]
    fn irq_status_without_rx_byte_is_preserved_after_irq_ack() {
        let (mut regs, uart) = pl011_with_registers();
        let mut parts = uart.split();

        write_test_reg(
            &mut regs,
            0x040,
            UARTIS::OE::SET.value | UARTIS::PE::SET.value,
        );
        write_test_reg(&mut regs, 0x018, UARTFR::RXFE::SET.value);

        let event = handle_irq(&mut parts.irq).0.unwrap();
        assert!(event.events.contains(SerialEventSet::RX_STATUS));
        assert!(event.rx_errors.contains(RxErrorFlags::PARITY));
        assert!(event.rx_errors.contains(RxErrorFlags::OVERRUN));
        assert!(parts.port.read_rx().is_none());
    }

    #[test]
    fn tx_irq_exposes_space_without_owning_a_software_fifo() {
        let (mut regs, uart) = pl011_with_registers();
        let mut parts = started_parts(uart);

        write_test_reg(&mut regs, 0x018, 0);
        write_test_reg(&mut regs, 0x040, UARTIS::TX::SET.value);
        let event = handle_irq(&mut parts.irq).0.unwrap();
        assert!(event.events.contains(SerialEventSet::TX_SPACE));
        assert_eq!(parts.port.write_tx(b"x"), 1);
        assert_eq!(regs.uartdr.get() as u8, b'x');
    }

    #[test]
    fn tx_irq_endpoint_acknowledges_tx_interrupt() {
        let (mut regs, uart) = pl011_with_registers();
        let mut irq = uart.split().irq;

        write_test_reg(&mut regs, 0x000, 0x5a);
        write_test_reg(&mut regs, 0x038, UARTIS::TX::SET.value);
        write_test_reg(&mut regs, 0x040, UARTIS::TX::SET.value);
        let event = handle_irq(&mut irq).0.unwrap();

        assert!(event.events.contains(SerialEventSet::TX_SPACE));
        assert_eq!(event.rearm, SerialEventSet::TX_SPACE);
        assert_eq!(
            read_test_reg(&regs, 0x044) & UARTIS::TX::SET.value,
            UARTIS::TX::SET.value
        );
        assert_eq!(read_test_reg(&regs, 0x038) & UARTIS::TX::SET.value, 0);
        assert_eq!(read_test_reg(&regs, 0x000), 0x5a);
    }

    #[test]
    fn set_config_preserves_enabled_tx_and_rx_paths() {
        let (regs, mut uart) = pl011_with_registers();
        regs.uartcr
            .write(UARTCR::UARTEN::SET + UARTCR::TXE::SET + UARTCR::RXE::SET);

        uart.set_config(&Config::new()).unwrap();

        let cr = regs.uartcr.extract();
        assert!(cr.is_set(UARTCR::UARTEN));
        assert!(cr.is_set(UARTCR::TXE));
        assert!(cr.is_set(UARTCR::RXE));
    }

    #[test]
    fn rx_available_mask_enables_timeout_and_error_interrupts() {
        let (regs, mut uart) = pl011_with_registers();

        uart.set_irq_mask(SerialEventSet::RX);

        let imsc = regs.uartimsc.extract();
        assert!(imsc.is_set(UARTIS::RX));
        assert!(imsc.is_set(UARTIS::RT));
        assert!(imsc.is_set(UARTIS::FE));
        assert!(imsc.is_set(UARTIS::PE));
        assert!(imsc.is_set(UARTIS::BE));
        assert!(imsc.is_set(UARTIS::OE));
        assert_eq!(uart.get_irq_mask(), SerialEventSet::RX);
    }

    #[test]
    fn hard_irq_does_not_claim_rx_ready_without_mis() {
        let (mut regs, uart) = pl011_with_registers();
        let mut parts = uart.split();

        parts.port.set_irq_mask(SerialEventSet::RX);
        write_test_reg(&mut regs, 0x040, 0);
        write_test_reg(&mut regs, 0x018, 0);

        assert!(handle_irq(&mut parts.irq).0.is_none());
    }

    #[test]
    fn port_rx_ready_is_visible_without_irq_event() {
        let (mut regs, mut uart) = pl011_with_registers();

        uart.set_irq_mask(SerialEventSet::RX);
        write_test_reg(&mut regs, 0x040, 0);
        write_test_reg(&mut regs, 0x018, 0);
        regs.uartdr.set(UARTDR::DATA.val(b'r' as u32).into());

        let status = uart.poll_status();
        assert!(status.rx_ready());
        let sample = uart.read_rx().expect("RX sample should be available");
        assert_eq!(sample.byte, Some(b'r'));
        assert_eq!(sample.flag, RxFlag::Normal);
    }

    #[test]
    fn rearm_remasks_rx_when_fifo_is_already_ready() {
        let (mut regs, mut uart) = pl011_with_registers();
        write_test_reg(&mut regs, 0x018, 0);

        let ready = uart.rearm(SerialEventSet::RX);

        assert_eq!(ready, SerialEventSet::RX_DATA);
        assert_eq!(
            read_test_reg(&regs, 0x038) & imsc_for_events(SerialEventSet::RX),
            0
        );
    }

    #[test]
    fn unknown_irq_source_masks_all_without_fifo_access() {
        let (mut regs, uart) = pl011_with_registers();
        let mut irq = uart.split().irq;
        write_test_reg(&mut regs, 0x000, 0x5a);
        write_test_reg(&mut regs, 0x038, u32::MAX);
        write_test_reg(&mut regs, 0x040, 1 << 31);

        let event = handle_irq(&mut irq).0.unwrap();

        assert!(event.events.contains(SerialEventSet::FAULT));
        assert_eq!(read_test_reg(&regs, 0x038), 0);
        assert_eq!(read_test_reg(&regs, 0x000), 0x5a);
    }
}
