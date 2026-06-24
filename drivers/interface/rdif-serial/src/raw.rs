use core::{any::Any, num::NonZeroU32};

use crate::{
    Config, ConfigError, InterruptMask, IrqSnapshot, RxFlag, RxSample, SerialEvent, TransferError,
};

/// 无锁 UART 寄存器接口。
///
/// # 并发契约
///
/// 所有方法都必须由外层端口锁串行化。实现不得自行引入 Mutex、
/// SpinNoIrq、Arc、WaitQueue 或任务唤醒逻辑。
pub trait RawUart: Send + Any + 'static {
    fn name(&self) -> &'static str;
    fn base_addr(&self) -> usize;
    fn clock_freq(&self) -> Option<NonZeroU32>;

    /// 初始化 FIFO、控制寄存器和线路参数。
    ///
    /// 返回时所有设备 IRQ 应保持关闭。
    fn startup(&mut self, config: &Config) -> Result<(), ConfigError>;

    /// 关闭所有设备 IRQ 并停止端口。
    fn shutdown(&mut self);

    /// 调整 baud/data bits/parity/stop bits。
    ///
    /// 调用方已经持有端口锁，并已临时屏蔽设备 IRQ。
    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError>;

    fn baudrate(&self) -> u32;
    fn data_bits(&self) -> crate::DataBits;
    fn stop_bits(&self) -> crate::StopBits;
    fn parity(&self) -> crate::Parity;

    fn enable_loopback(&mut self);
    fn disable_loopback(&mut self);
    fn is_loopback_enabled(&self) -> bool;

    /// 写设备侧 IRQ mask。它只管理设备中断，不是同步原语。
    fn set_irq_mask(&mut self, mask: InterruptMask);

    /// 读取并按硬件要求确认当前 IRQ source。
    fn take_irq_snapshot(&mut self) -> IrqSnapshot;

    /// 从 RX FIFO 读取一个 sample；FIFO 空时返回 None。
    fn read_rx(&mut self) -> Option<RxSample>;

    /// 硬件 TX FIFO 是否仍可接收一个字符。
    fn tx_ready(&mut self) -> bool;

    /// 将一个字节写入硬件 TX FIFO。
    ///
    /// 调用前必须确认 tx_ready()。
    fn write_tx(&mut self, byte: u8);

    /// Read a raw hardware status snapshot.
    ///
    /// This is for polling users that directly own the raw UART, such as
    /// someboot early console. Runtime TX/RX queues must not call this method.
    fn poll_status(&mut self) -> SerialEvent;

    /// Direct polling helper for early console users.
    fn write_byte(&mut self, byte: u8) {
        self.write_tx(byte);
    }

    /// Consume one byte/error according to caller-owned polling state.
    fn read_byte(&mut self, status: SerialEvent) -> Option<Result<u8, TransferError>> {
        if !status.rx_ready() && !status.rx_error() {
            return None;
        }
        let sample = self.read_rx()?;
        if sample.overrun {
            return Some(Err(TransferError::Overrun(sample.byte.unwrap_or(0))));
        }
        let byte = sample.byte?;
        match sample.flag {
            RxFlag::Normal => Some(Ok(byte)),
            RxFlag::Break => Some(Err(TransferError::Break)),
            RxFlag::Parity => Some(Err(TransferError::Parity)),
            RxFlag::Framing => Some(Err(TransferError::Framing)),
        }
    }

    /// 一轮 IRQ 建议写入的最大字节数。
    fn tx_load_size(&self) -> usize {
        1
    }

    /// FIFO 和 shift register 是否都已空。
    fn tx_idle(&mut self) -> bool;

    fn ack_modem_status(&mut self) {}
    fn ack_busy_detect(&mut self) {}
}
