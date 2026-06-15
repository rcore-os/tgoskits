extern crate alloc;

use alloc::sync::Arc;
use core::{
    num::NonZeroU32,
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
};

use rdif_serial::{
    BSerial, InterfaceRaw, SerialDirection, SerialDyn, SerialEvent, TIrqHandler, TRxQueue,
    TTxQueue, TransBytesError, TransferError,
};
use tock_registers::{interfaces::*, register_bitfields, register_structs, registers::*};

use crate::{Config, ConfigError, DataBits, InterruptMask, Parity, StopBits};

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

        Self { base, clock_freq }
    }

    pub fn new_boxed(base: NonNull<u8>, clock_freq: u32) -> BSerial {
        let mut serial = Self::new(base, clock_freq);
        serial.open();
        SerialDyn::new_boxed(serial)
    }

    fn registers(&self) -> &Pl011Registers {
        unsafe { &*self.base.0.as_ptr() }
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

    pub fn set_irq_mask(&mut self, mask: InterruptMask) {
        let mut imsc = 0;
        if mask.contains(InterruptMask::RX_AVAILABLE) {
            imsc += UARTIS::RX::SET.value;
        }
        if mask.contains(InterruptMask::TX_EMPTY) {
            imsc += UARTIS::TX::SET.value;
        }

        self.registers().uartimsc.set(imsc);
    }

    pub fn get_irq_mask(&self) -> InterruptMask {
        let imsc = self.registers().uartimsc.extract();
        let mut mask = InterruptMask::empty();

        if imsc.is_set(UARTIS::RX) {
            mask |= InterruptMask::RX_AVAILABLE;
        }
        if imsc.is_set(UARTIS::TX) {
            mask |= InterruptMask::TX_EMPTY;
        }

        mask
    }

    pub fn pending(&mut self, direction: SerialDirection) -> bool {
        match direction {
            SerialDirection::Input => !self.registers().uartfr.is_set(UARTFR::RXFE),
            SerialDirection::Output => !self.registers().uartfr.is_set(UARTFR::TXFF),
        }
    }

    pub fn poll(&mut self) -> SerialEvent {
        let mut event = SerialEvent::empty();
        let fr = self.registers().uartfr.extract();
        if !fr.is_set(UARTFR::RXFE) {
            event |= SerialEvent::RX_READY;
        }
        if !fr.is_set(UARTFR::TXFF) {
            event |= SerialEvent::TX_READY;
        }

        let rsr = self.registers().uartrsr_ecr.extract();
        if rsr.is_set(UARTRSR_ECR::FE) || rsr.is_set(UARTRSR_ECR::PE) || rsr.is_set(UARTRSR_ECR::BE)
        {
            event |= SerialEvent::RX_ERROR;
        }
        if rsr.is_set(UARTRSR_ECR::OE) {
            event |= SerialEvent::RX_ERROR | SerialEvent::OVERRUN;
        }

        event
    }

    pub fn try_write(&mut self, bytes: &[u8]) -> usize {
        let mut written = 0;
        for &byte in bytes {
            if !self.pending(SerialDirection::Output) {
                break;
            }
            self.registers().uartdr.set(byte as _);
            written += 1;
        }
        written
    }

    pub fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        let mut count = 0;
        for byte in bytes.iter_mut() {
            if !self.pending(SerialDirection::Input) {
                break;
            }
            match self.read_byte() {
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

    pub fn handle_irq(&mut self) -> SerialEvent {
        let mis = self.registers().uartmis.extract();
        let mut event = SerialEvent::empty();

        if mis.is_set(UARTIS::RX) || mis.is_set(UARTIS::RT) {
            event |= SerialEvent::RX_READY;
        }
        if mis.is_set(UARTIS::TX) {
            event |= SerialEvent::TX_READY;
        }
        if mis.is_set(UARTIS::FE) || mis.is_set(UARTIS::PE) || mis.is_set(UARTIS::BE) {
            event |= SerialEvent::RX_ERROR;
        }
        if mis.is_set(UARTIS::OE) {
            event |= SerialEvent::RX_ERROR | SerialEvent::OVERRUN;
        }

        self.registers().uarticr.set(mis.get());
        event
    }

    fn read_byte(&mut self) -> Option<Result<u8, TransferError>> {
        if self.registers().uartfr.is_set(UARTFR::RXFE) {
            return None;
        }

        let dr = self.registers().uartdr.extract();
        let data = dr.read(UARTDR::DATA) as u8;

        if dr.is_set(UARTDR::FE) {
            return Some(Err(TransferError::Framing));
        }
        if dr.is_set(UARTDR::PE) {
            return Some(Err(TransferError::Parity));
        }
        if dr.is_set(UARTDR::OE) {
            return Some(Err(TransferError::Overrun(data)));
        }
        if dr.is_set(UARTDR::BE) {
            return Some(Err(TransferError::Break));
        }

        Some(Ok(data))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Reg(NonNull<Pl011Registers>);

unsafe impl Send for Reg {}
unsafe impl Sync for Reg {}

impl InterfaceRaw for Pl011 {
    type SharedState = Pl011SharedState;
    type TxQueue = Pl011TxQueue;
    type RxQueue = Pl011RxQueue;
    type IrqHandler = Pl011IrqHandler;

    fn name(&self) -> &str {
        "PL011 UART"
    }

    fn base_addr(&self) -> usize {
        self.base.0.as_ptr() as usize
    }

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        use tock_registers::interfaces::Readable;

        // 根据ARM文档的建议配置流程：
        // 1. 禁用UART
        let original_enable = self.registers().uartcr.is_set(UARTCR::UARTEN); // 保存原始使能状态
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
        if original_enable {
            self.registers().uartcr.modify(UARTCR::UARTEN::SET); // 重新启用UART
        }

        Ok(())
    }

    fn baudrate(&self) -> u32 {
        let ibrd = self.registers().uartibrd.read(UARTIBRD::BAUD_DIVINT);
        let fbrd = self.registers().uartfbrd.read(UARTFBRD::BAUD_DIVFRAC);

        // 反向计算波特率
        // Baud rate = FUARTCLK / (16 * (IBRD + FBRD/64))
        let divisor = ibrd * 64 + fbrd;
        if divisor == 0 {
            return 0;
        }

        self.clock_freq * 64 / (16 * divisor)
    }

    fn data_bits(&self) -> DataBits {
        let wlen = self.registers().uartlcr_h.read(UARTLCR_H::WLEN);

        match wlen {
            0 => DataBits::Five,
            1 => DataBits::Six,
            2 => DataBits::Seven,
            3 => DataBits::Eight,
            _ => DataBits::Eight, // 默认值
        }
    }

    fn stop_bits(&self) -> StopBits {
        if self.registers().uartlcr_h.is_set(UARTLCR_H::STP2) {
            StopBits::Two
        } else {
            StopBits::One
        }
    }

    fn parity(&self) -> Parity {
        if !self.registers().uartlcr_h.is_set(UARTLCR_H::PEN) {
            Parity::None
        } else if self.registers().uartlcr_h.is_set(UARTLCR_H::SPS) {
            // Stick parity
            if self.registers().uartlcr_h.is_set(UARTLCR_H::EPS) {
                Parity::Space
            } else {
                Parity::Mark
            }
        } else {
            // Normal parity
            if self.registers().uartlcr_h.is_set(UARTLCR_H::EPS) {
                Parity::Even
            } else {
                Parity::Odd
            }
        }
    }

    fn open(&mut self) {
        Pl011::open(self)
    }

    fn close(&mut self) {
        // 禁用 UART
        self.registers().uartcr.modify(UARTCR::UARTEN::CLEAR);
    }

    fn clock_freq(&self) -> Option<NonZeroU32> {
        self.clock_freq.try_into().ok()
    }

    fn enable_loopback(&mut self) {
        self.registers().uartcr.modify(UARTCR::LBE::SET);
    }

    fn disable_loopback(&mut self) {
        self.registers().uartcr.modify(UARTCR::LBE::CLEAR);
    }

    fn is_loopback_enabled(&self) -> bool {
        self.registers().uartcr.is_set(UARTCR::LBE)
    }

    fn set_irq_mask(&mut self, mask: InterruptMask) {
        Pl011::set_irq_mask(self, mask);
    }

    fn get_irq_mask(&self) -> InterruptMask {
        Pl011::get_irq_mask(self)
    }

    fn new_shared_state(&self) -> Self::SharedState {
        Pl011SharedState::new()
    }

    fn tx_queue(&self, shared: &Self::SharedState) -> Self::TxQueue {
        Pl011TxQueue {
            base: self.base,
            shared: shared.clone(),
        }
    }

    fn rx_queue(&self, shared: &Self::SharedState) -> Self::RxQueue {
        Pl011RxQueue {
            base: self.base,
            shared: shared.clone(),
        }
    }

    fn irq_handler(&self, shared: &Self::SharedState) -> Self::IrqHandler {
        Pl011IrqHandler {
            base: self.base,
            shared: shared.clone(),
        }
    }
}

#[derive(Clone)]
pub struct Pl011SharedState {
    event_bits: Arc<AtomicU32>,
}

impl Pl011SharedState {
    fn new() -> Self {
        Self {
            event_bits: Arc::new(AtomicU32::new(0)),
        }
    }

    fn push_event(&self, event: SerialEvent) {
        if !event.is_empty() {
            self.event_bits.fetch_or(event.bits(), Ordering::AcqRel);
        }
    }

    fn take_event(&self, mask: SerialEvent) -> SerialEvent {
        let old = self.event_bits.fetch_and(!mask.bits(), Ordering::AcqRel);
        SerialEvent::from_bits_retain(old) & mask
    }

    fn peek_event(&self) -> SerialEvent {
        SerialEvent::from_bits_retain(self.event_bits.load(Ordering::Acquire))
    }
}

pub struct Pl011TxQueue {
    base: Reg,
    shared: Pl011SharedState,
}

impl Pl011TxQueue {
    fn registers(&self) -> &Pl011Registers {
        unsafe { &*self.base.0.as_ptr() }
    }
}

impl TTxQueue for Pl011TxQueue {
    fn base_addr(&self) -> usize {
        self.base.0.as_ptr() as usize
    }

    fn poll(&mut self) -> SerialEvent {
        if !self.registers().uartfr.is_set(UARTFR::TXFF) {
            let event = SerialEvent::TX_READY;
            self.shared.push_event(event);
            event
        } else {
            self.shared.peek_event() & SerialEvent::TX_READY
        }
    }

    fn try_write(&mut self, bytes: &[u8]) -> usize {
        let _ = self.shared.take_event(SerialEvent::TX_READY);
        let mut written = 0;
        for &byte in bytes {
            if self.registers().uartfr.is_set(UARTFR::TXFF) {
                break;
            }
            self.registers().uartdr.set(byte as _);
            written += 1;
        }
        written
    }
}

pub struct Pl011RxQueue {
    base: Reg,
    shared: Pl011SharedState,
}

impl Pl011RxQueue {
    fn registers(&self) -> &Pl011Registers {
        unsafe { &*self.base.0.as_ptr() }
    }

    fn read_byte(&self) -> Option<Result<u8, TransferError>> {
        if self.registers().uartfr.is_set(UARTFR::RXFE) {
            return None;
        }

        let dr = self.registers().uartdr.extract();
        let data = dr.read(UARTDR::DATA) as u8;

        if dr.is_set(UARTDR::FE) {
            return Some(Err(TransferError::Framing));
        }
        if dr.is_set(UARTDR::PE) {
            return Some(Err(TransferError::Parity));
        }
        if dr.is_set(UARTDR::OE) {
            return Some(Err(TransferError::Overrun(data)));
        }
        if dr.is_set(UARTDR::BE) {
            return Some(Err(TransferError::Break));
        }

        Some(Ok(data))
    }
}

impl TRxQueue for Pl011RxQueue {
    fn base_addr(&self) -> usize {
        self.base.0.as_ptr() as usize
    }

    fn poll(&mut self) -> SerialEvent {
        let mut event = SerialEvent::empty();
        let fr = self.registers().uartfr.extract();
        if !fr.is_set(UARTFR::RXFE) {
            event |= SerialEvent::RX_READY;
        }

        let rsr = self.registers().uartrsr_ecr.extract();
        if rsr.is_set(UARTRSR_ECR::FE) || rsr.is_set(UARTRSR_ECR::PE) || rsr.is_set(UARTRSR_ECR::BE)
        {
            event |= SerialEvent::RX_ERROR;
        }
        if rsr.is_set(UARTRSR_ECR::OE) {
            event |= SerialEvent::RX_ERROR | SerialEvent::OVERRUN;
        }

        self.shared.push_event(event);
        event
            | (self.shared.peek_event()
                & (SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN))
    }

    fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        let _ = self
            .shared
            .take_event(SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN);
        let mut count = 0;
        for byte in bytes.iter_mut() {
            if self.registers().uartfr.is_set(UARTFR::RXFE) {
                break;
            }
            match self.read_byte() {
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
}

pub struct Pl011IrqHandler {
    base: Reg,
    shared: Pl011SharedState,
}

impl Pl011IrqHandler {
    fn registers(&self) -> &Pl011Registers {
        unsafe { &*self.base.0.as_ptr() }
    }
}

impl TIrqHandler for Pl011IrqHandler {
    fn base_addr(&self) -> usize {
        self.base.0.as_ptr() as usize
    }

    fn handle_irq(&self) -> SerialEvent {
        let mis = self.registers().uartmis.extract();
        let mut event = SerialEvent::empty();

        if mis.is_set(UARTIS::RX) || mis.is_set(UARTIS::RT) {
            event |= SerialEvent::RX_READY;
        }
        if mis.is_set(UARTIS::TX) {
            event |= SerialEvent::TX_READY;
        }
        if mis.is_set(UARTIS::FE) || mis.is_set(UARTIS::PE) || mis.is_set(UARTIS::BE) {
            event |= SerialEvent::RX_ERROR;
        }
        if mis.is_set(UARTIS::OE) {
            event |= SerialEvent::RX_ERROR | SerialEvent::OVERRUN;
        }

        self.registers().uarticr.set(mis.get());
        self.shared.push_event(event);
        event
    }
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

    use super::*;

    fn pl011_with_overrun_data() -> (alloc::boxed::Box<Pl011Registers>, Pl011) {
        let mut regs = alloc::boxed::Box::new(unsafe { core::mem::zeroed::<Pl011Registers>() });
        regs.uartdr
            .set((UARTDR::DATA.val(0xab) + UARTDR::OE::SET).into());
        let ptr = NonNull::from(regs.as_mut()).cast::<u8>();
        let uart = Pl011::new(ptr, 24_000_000);
        (regs, uart)
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
    fn split_rx_reports_overrun_instead_of_swallowing_it() {
        let (_regs, uart) = pl011_with_overrun_data();
        let shared = uart.new_shared_state();
        let mut rx = uart.rx_queue(&shared);

        let mut buf = [0];
        let err = rx
            .try_read(&mut buf)
            .expect_err("overrun must be reported to the caller");

        assert_eq!(buf[0], 0xab);
        assert_eq!(err.bytes_transferred, 1);
        assert_eq!(err.kind, TransferError::Overrun(0xab));
    }
}
