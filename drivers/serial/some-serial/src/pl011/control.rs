use super::*;

/// PL011 UART 驱动结构体
pub struct Pl011 {
    pub(super) base: Reg,
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

    pub(super) fn registers(&self) -> &Pl011Registers {
        unsafe { &*self.base.0.as_ptr() }
    }

    pub(super) fn current_baudrate(&self) -> u32 {
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
    pub(super) fn set_baudrate_internal(&self, baudrate: u32) -> Result<(), ConfigError> {
        // PL011 波特率计算公式：
        // BAUDDIV = (FUARTCLK / (16 * Baud rate))
        // IBRD = integer(BAUDDIV)
        // FBRD = integer((BAUDDIV - IBRD) * 64 + 0.5)

        let scaled_baudrate = baudrate
            .checked_mul(16)
            .filter(|scaled| *scaled != 0)
            .ok_or(ConfigError::InvalidBaudrate)?;
        let bauddiv = self.clock_freq / scaled_baudrate;
        let remainder = self.clock_freq % scaled_baudrate;
        let fbrd = (remainder * 64 + (scaled_baudrate / 2)) / scaled_baudrate;

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

    fn wait_until_not_busy(&self) -> Result<(), ConfigError> {
        for _ in 0..BUSY_POLL_BUDGET {
            if !self.registers().uartfr.is_set(UARTFR::BUSY) {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(ConfigError::Timeout)
    }

    pub(super) fn set_data_bits_internal(&self, bits: DataBits) -> Result<(), ConfigError> {
        let wlen = match bits {
            DataBits::Five => UARTLCR_H::WLEN::FiveBit,
            DataBits::Six => UARTLCR_H::WLEN::SixBit,
            DataBits::Seven => UARTLCR_H::WLEN::SevenBit,
            DataBits::Eight => UARTLCR_H::WLEN::EightBit,
        };

        self.registers().uartlcr_h.modify(wlen);
        Ok(())
    }

    pub(super) fn set_stop_bits_internal(&self, bits: StopBits) -> Result<(), ConfigError> {
        match bits {
            StopBits::One => self.registers().uartlcr_h.modify(UARTLCR_H::STP2::CLEAR),
            StopBits::Two => self.registers().uartlcr_h.modify(UARTLCR_H::STP2::SET),
        }

        Ok(())
    }

    pub(super) fn set_parity_internal(&self, parity: Parity) -> Result<(), ConfigError> {
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
    pub fn open(&mut self) -> Result<(), ConfigError> {
        let original_cr = self.registers().uartcr.get();

        // 禁用 UART
        self.registers().uartcr.modify(UARTCR::UARTEN::CLEAR);

        // 等待当前传输完成
        if let Err(error) = self.wait_until_not_busy() {
            self.registers().uartcr.set(original_cr);
            return Err(error);
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
        Ok(())
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
