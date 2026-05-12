//! NS16550/16450 UART 寄存器定义
//!
//! 参考Linux内核 drivers/tty/serial/8250/8250.h
//! 使用 const 定义寄存器偏移和位标志，同时提供类型安全的 bitflags 定义

#![allow(dead_code)]

use bitflags::bitflags;

// ===== 核心 bitflags 类型定义 =====

bitflags! {
    /// IER (0x01) - 中断使能寄存器
    /// 控制各类UART中断的使能状态，写入时启用相应中断源
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct InterruptEnableFlags: u8 {
        /// 接收数据可用中断使能 (RDI)
        /// 当接收缓冲区有数据时触发中断，用于接收数据检测
        const RECEIVED_DATA_AVAILABLE = 0x01;

        /// 发送保持寄存器空中断使能 (THRI)
        /// 当发送保持寄存器为空时可写入时触发中断，用于发送数据流控
        const TRANSMITTER_HOLDING_EMPTY = 0x02;

        /// 接收线路状态中断使能 (RLSI)
        /// 当接收线路状态改变（奇偶校验错误、帧错误等）时触发中断
        const RECEIVER_LINE_STATUS = 0x04;

        /// 调制解调器状态中断使能 (MSI)
        /// 当调制解调器信号状态改变（CTS、DSR等）时触发中断
        const MODEM_STATUS = 0x08;
    }
}

bitflags! {
    /// IIR (0x02) - 中断标识寄存器
    /// 查询当前挂起的中断类型，读取后可确定中断源
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct InterruptIdentificationFlags: u8 {
        /// 无中断挂起标志位
        /// 为1时表示无中断，为0时表示有中断待处理
        const NO_INTERRUPT_PENDING = 0x01;

        /// 中断ID掩码
        /// 用于提取中断类型位（bit 1-3）
        const INTERRUPT_ID_MASK = 0x0E;

        /// 接收线路状态中断 (RLSI)
        /// 接收线路状态错误中断（溢出、奇偶错误、帧错误、中止）
        const RECEIVER_LINE_STATUS = 0x06;

        /// 接收数据可用中断 (RDI)
        /// 接收缓冲区有数据中断
        const RECEIVED_DATA_AVAILABLE = 0x04;

        /// 字符超时指示 (CTI)
        /// FIFO模式下字符接收超时中断
        const CHARACTER_TIMEOUT = 0x0C;

        /// 发送保持寄存器空中断 (THRI)
        /// 发送保持寄存器空中断
        const TRANSMITTER_HOLDING_EMPTY = 0x02;

        /// 调制解调器状态中断 (MSI)
        /// 调制解调器信号状态改变中断
        const MODEM_STATUS = 0x00;

        /// FIFO使能位掩码
        /// bit 6-7，表示FIFO功能状态
        const FIFO_ENABLE_MASK = 0xC0;
    }
}

bitflags! {
    /// FCR (0x02) - FIFO控制寄存器
    /// 控制FIFO模式的使能、触发级别和清空操作
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct FifoControlFlags: u8 {
        /// FIFO使能
        /// 置1时启用发送和接收FIFO功能
        const ENABLE_FIFO = 0x01;

        /// 清空接收FIFO
        /// 置1时清空接收FIFO中的所有数据
        const CLEAR_RECEIVER_FIFO = 0x02;

        /// 清空发送FIFO
        /// 置1时清空发送FIFO中的所有数据
        const CLEAR_TRANSMITTER_FIFO = 0x04;

        /// DMA模式选择
        /// 置1时选择DMA模式0，清0时选择模式1
        const DMA_MODE_SELECT = 0x08;

        /// FIFO触发级别掩码
        /// bit 6-7，设置FIFO触发中断的阈值
        const TRIGGER_LEVEL_MASK = 0xC0;

        /// 1字节触发级别
        /// FIFO中1字节数据时触发接收中断
        const TRIGGER_1_BYTE = 0x00;

        /// 4字节触发级别
        /// FIFO中4字节数据时触发接收中断
        const TRIGGER_4_BYTES = 0x40;

        /// 8字节触发级别
        /// FIFO中8字节数据时触发接收中断
        const TRIGGER_8_BYTES = 0x80;

        /// 14字节触发级别
        /// FIFO中14字节数据时触发接收中断
        const TRIGGER_14_BYTES = 0xC0;
    }
}

bitflags! {
    /// LCR (0x03) - 线路控制寄存器
    /// 配置串口通信参数：数据位、停止位、校验位等
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct LineControlFlags: u8 {
        /// 数据位长度掩码
        /// bit 0-1，设置每帧的数据位数
        const WORD_LENGTH_MASK = 0x03;

        /// 5位数据长度
        const WORD_LENGTH_5 = 0x00;

        /// 6位数据长度
        const WORD_LENGTH_6 = 0x01;

        /// 7位数据长度
        const WORD_LENGTH_7 = 0x02;

        /// 8位数据长度
        const WORD_LENGTH_8 = 0x03;

        /// 停止位数
        /// 0: 1位停止位；1: 1.5或2位停止位（取决于数据位长度）
        const STOP_BITS = 0x04;

        /// 校验使能
        /// 置1时启用奇偶校验功能
        const PARITY_ENABLE = 0x08;

        /// 偶校验选择
        /// 与PARITY_ENABLE配合使用，1=偶校验，0=奇校验
        const EVEN_PARITY = 0x10;

        /// 固定校验位
        /// 置1时校验位固定为0或1（取决于EVEN_PARITY）
        const STICK_PARITY = 0x20;

        /// 发送中止
        /// 置1时发送端输出强制为逻辑0（中止信号）
        const SET_BREAK = 0x40;

        /// 除数锁存器访问位
        /// 置1时允许访问波特率除数锁存器（DLL/DLH）
        const DIVISOR_LATCH_ACCESS = 0x80;
    }
}

bitflags! {
    /// MCR (0x04) - 调制解调器控制寄存器
    /// 控制调制解调器接口信号和环回测试模式
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ModemControlFlags: u8 {
        /// 数据终端就绪 (DTR)
        /// 控制DTR输出信号，1表示DTR有效
        const DATA_TERMINAL_READY = 0x01;

        /// 请求发送 (RTS)
        /// 控制RTS输出信号，1表示请求发送数据
        const REQUEST_TO_SEND = 0x02;

        /// 输出1 (OUT1)
        /// 用户自定义输出信号1
        const OUT_1 = 0x04;

        /// 输出2 (OUT2)
        /// 用户自定义输出信号2，常用于中断使能控制
        const OUT_2 = 0x08;

        /// 环回测试模式使能
        /// 置1时启用内部环回，用于自测试
        const LOOPBACK_ENABLE = 0x10;

        /// 调制解调器控制掩码
        /// bit 0-3，调制解调器控制信号掩码
        const MODEM_CONTROL_MASK = 0x0F;
    }
}

bitflags! {
    /// LSR (0x05) - 线路状态寄存器
    /// 反映发送和接收状态，以及错误检测信息
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct LineStatusFlags: u8 {
        /// 数据就绪 (DR)
        /// 接收缓冲区中有数据可读取
        const DATA_READY = 0x01;

        /// 溢出错误 (OE)
        /// 接收缓冲区数据未被读取前又有新数据到达
        const OVERRUN_ERROR = 0x02;

        /// 奇偶校验错误 (PE)
        /// 接收数据的奇偶校验位错误
        const PARITY_ERROR = 0x04;

        /// 帧错误 (FE)
        /// 接收帧的停止位错误或格式错误
        const FRAMING_ERROR = 0x08;

        /// 中止中断 (BI)
        /// 接收到中止信号（发送端强制为0的时间超过一个完整帧）
        const BREAK_INTERRUPT = 0x10;

        /// 发送保持寄存器空 (THRE)
        /// 发送保持寄存器为空，可以写入新数据
        const TRANSMITTER_HOLDING_EMPTY = 0x20;

        /// 发送器空 (TEMT)
        /// 发送器和移位寄存器都为空，所有数据已发送完成
        const TRANSMITTER_EMPTY = 0x40;

        /// FIFO错误指示
        /// FIFO模式下存在错误，需要读取RBR来清除此标志
        const FIFO_ERROR = 0x80;

        /// 错误状态掩码
        /// 包含所有可能的接收错误类型
        const ERROR_MASK = 0x1E;
    }
}

bitflags! {
    /// MSR (0x06) - 调制解调器状态寄存器
    /// 反映调制解调器输入信号状态及其变化
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ModemStatusFlags: u8 {
        /// CTS变化指示 (DCTS)
        /// 自上次读取后CTS信号状态发生了变化
        const DELTA_CLEAR_TO_SEND = 0x01;

        /// DSR变化指示 (DDSR)
        /// 自上次读取后DSR信号状态发生了变化
        const DELTA_DATA_SET_READY = 0x02;

        /// 振铃指示下降沿 (TERI)
        /// 检测到振铃信号的下降沿（振铃结束）
        const TRAILING_EDGE_RING = 0x04;

        /// DCD变化指示 (DDCD)
        /// 自上次读取后DCD信号状态发生了变化
        const DELTA_DATA_CARRIER_DETECT = 0x08;

        /// 清除发送状态 (CTS)
        /// 当前CTS输入信号状态，1表示允许发送
        const CLEAR_TO_SEND = 0x10;

        /// 数据设备就绪状态 (DSR)
        /// 当前DSR输入信号状态，1表示数据设备就绪
        const DATA_SET_READY = 0x20;

        /// 振铃指示状态 (RI)
        /// 当前RI输入信号状态，1表示正在振铃
        const RING_INDICATOR = 0x40;

        /// 数据载波检测状态 (DCD)
        /// 当前DCD输入信号状态，1表示检测到载波信号
        const DATA_CARRIER_DETECT = 0x80;

        /// 信号变化掩码
        /// bit 0-3，反映自上次读取后的信号变化
        const DELTA_MASK = 0x0F;

        /// 信号状态掩码
        /// bit 4-7，反映当前信号状态
        const STATUS_MASK = 0xF0;
    }
}

// ===== 寄存器偏移 (相对于基地址) =====

/// UART_RBR: 接收缓冲寄存器 (Receiver Buffer Register)
/// 只读，读取时会清除接收中断。
pub const UART_RBR: u8 = 0x00;

/// UART_THR: 发送保持寄存器 (Transmitter Holding Register)
/// 只写，写入数据会触发发送。
pub const UART_THR: u8 = 0x00;

/// UART_DLL: 除数锁存低字节 (Divisor Latch LSB)
/// 可读可写，设置波特率除数低8位，需先设置LCR.DLAB=1。
pub const UART_DLL: u8 = 0x00;

/// UART_IER: 中断使能寄存器 (Interrupt Enable Register)
/// 可读可写，控制各类中断使能。
pub const UART_IER: u8 = 0x01;

/// UART_DLH: 除数锁存高字节 (Divisor Latch MSB)
/// 可读可写，设置波特率除数高8位，需先设置LCR.DLAB=1。
pub const UART_DLH: u8 = 0x01;

/// UART_IIR: 中断标识寄存器 (Interrupt Identification Register)
/// 只读，查询当前挂起的中断类型，读取不会清除中断。
pub const UART_IIR: u8 = 0x02;

/// UART_FCR: FIFO控制寄存器 (FIFO Control Register)
/// 只写，控制FIFO使能、清空等。
pub const UART_FCR: u8 = 0x02;

/// UART_LCR: 线路控制寄存器 (Line Control Register)
/// 可读可写，配置数据位、停止位、校验、DLAB等。
pub const UART_LCR: u8 = 0x03;

/// UART_MCR: 调制解调器控制寄存器 (Modem Control Register)
/// 可读可写，控制RTS/DTR/环回等。
pub const UART_MCR: u8 = 0x04;

/// UART_LSR: 线路状态寄存器 (Line Status Register)
pub const UART_LSR: u8 = 0x05;

/// UART_MSR: 调制解调器状态寄存器 (Modem Status Register)
/// 只读，反映调制解调器信号状态，读取可清除部分调制解调器中断。
pub const UART_MSR: u8 = 0x06;

/// UART_SCR: 临时寄存器 (Scratch Register)
/// 可读可写，用户自定义用途，无实际硬件功能。
pub const UART_SCR: u8 = 0x07;

// ===== 传统位标志常量 (向后兼容) =====

// IER (Interrupt Enable Register) 位定义
pub const UART_IER_RDI: u8 = 0x01; // Enable Received Data Available Interrupt
pub const UART_IER_THRI: u8 = 0x02; // Enable Transmitter Holding Register Empty Interrupt
pub const UART_IER_RLSI: u8 = 0x04; // Enable Receiver Line Status Interrupt
pub const UART_IER_MSI: u8 = 0x08; // Enable Modem Status Interrupt

// IIR (Interrupt Identification Register) 位定义
pub const UART_IIR_NO_INT: u8 = 0x01; // No interrupts pending
pub const UART_IIR_ID: u8 = 0x0E; // Interrupt ID mask
pub const UART_IIR_RLSI: u8 = 0x06; // Receiver Line Status Interrupt
pub const UART_IIR_RDI: u8 = 0x04; // Received Data Available Interrupt
pub const UART_IIR_CTI: u8 = 0x0C; // Character Timeout Indicator
pub const UART_IIR_THRI: u8 = 0x02; // Transmitter Holding Register Empty Interrupt
pub const UART_IIR_MSI: u8 = 0x00; // Modem Status Interrupt
pub const UART_IIR_FIFO_ENABLE: u8 = 0xC0; // FIFO Enable bits
pub const UART_IIR_FIFO_MASK: u8 = 0xC0; // FIFO bits mask

// FCR (FIFO Control Register) 位定义
pub const UART_FCR_ENABLE_FIFO: u8 = 0x01; // Enable FIFO
pub const UART_FCR_CLEAR_RCVR: u8 = 0x02; // Clear receiver FIFO
pub const UART_FCR_CLEAR_XMIT: u8 = 0x04; // Clear transmitter FIFO
pub const UART_FCR_DMA_SELECT: u8 = 0x08; // DMA mode select
pub const UART_FCR_TRIGGER_MASK: u8 = 0xC0; // Trigger level mask
pub const UART_FCR_TRIGGER_1: u8 = 0x00; // 1 byte trigger
pub const UART_FCR_TRIGGER_4: u8 = 0x40; // 4 byte trigger
pub const UART_FCR_TRIGGER_8: u8 = 0x80; // 8 byte trigger
pub const UART_FCR_TRIGGER_14: u8 = 0xC0; // 14 byte trigger

// LCR (Line Control Register) 位定义
pub const UART_LCR_WLEN5: u8 = 0x00; // 5 bits
pub const UART_LCR_WLEN6: u8 = 0x01; // 6 bits
pub const UART_LCR_WLEN7: u8 = 0x02; // 7 bits
pub const UART_LCR_WLEN8: u8 = 0x03; // 8 bits
pub const UART_LCR_STOP: u8 = 0x04; // Stop bits: 0=1 bit, 1=2 bits
pub const UART_LCR_PARITY: u8 = 0x08; // Parity enable
pub const UART_LCR_EPAR: u8 = 0x10; // Even parity
pub const UART_LCR_SPAR: u8 = 0x20; // Stick parity
pub const UART_LCR_SBRK: u8 = 0x40; // Set Break
pub const UART_LCR_DLAB: u8 = 0x80; // Divisor latch access bit

// MCR (Modem Control Register) 位定义
pub const UART_MCR_DTR: u8 = 0x01; // Data Terminal Ready
pub const UART_MCR_RTS: u8 = 0x02; // Request to Send
pub const UART_MCR_OUT1: u8 = 0x04; // Out 1
pub const UART_MCR_OUT2: u8 = 0x08; // Out 2
pub const UART_MCR_LOOP: u8 = 0x10; // Enable loopback test mode

// LSR (Line Status Register) 位定义
pub const UART_LSR_DR: u8 = 0x01; // Data ready
pub const UART_LSR_OE: u8 = 0x02; // Overrun error
pub const UART_LSR_PE: u8 = 0x04; // Parity error
pub const UART_LSR_FE: u8 = 0x08; // Framing error
pub const UART_LSR_BI: u8 = 0x10; // Break interrupt
pub const UART_LSR_THRE: u8 = 0x20; // Transmitter holding register empty
pub const UART_LSR_TEMT: u8 = 0x40; // Transmitter empty
pub const UART_LSR_FIFOE: u8 = 0x80; // Fifo error indication

// MSR (Modem Status Register) 位定义
pub const UART_MSR_DCTS: u8 = 0x01; // Delta CTS
pub const UART_MSR_DDSR: u8 = 0x02; // Delta DSR
pub const UART_MSR_TERI: u8 = 0x04; // Trail edge ring indicator
pub const UART_MSR_DDCD: u8 = 0x08; // Delta DCD
pub const UART_MSR_CTS: u8 = 0x10; // Clear to Send
pub const UART_MSR_DSR: u8 = 0x20; // Data Set Ready
pub const UART_MSR_RI: u8 = 0x40; // Ring Indicator
pub const UART_MSR_DCD: u8 = 0x80; // Data Carrier Detect

// ===== 默认配置和常量 =====

// 默认波特率除数（假设输入时钟 1.8432MHz）
pub const UART_DEFAULT_BAUD_RATE: u32 = 9600;
pub const UART_INPUT_CLOCK: u32 = 1_843_200;
pub const UART_DEFAULT_DIVISOR: u16 = (UART_INPUT_CLOCK / (16 * UART_DEFAULT_BAUD_RATE)) as u16;

// FIFO 深度
pub const UART_FIFO_SIZE: u8 = 16;

// 通用寄存器访问掩码
pub const UART_LCR_WLEN_MASK: u8 = 0x03;
pub const UART_IIR_INTERRUPT_MASK: u8 = 0x0E;
pub const UART_MCR_MODEM_MASK: u8 = 0x0F;
pub const UART_LSR_ERROR_MASK: u8 = 0x1E;
pub const UART_MSR_DELTA_MASK: u8 = 0x0F;
pub const UART_MSR_STATUS_MASK: u8 = 0xF0;
