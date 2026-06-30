//! NS16550 IO Port 版本实现
//!
//! 仅在 x86_64 架构下编译，使用 x86_64 crate 进行端口 I/O

use super::{Kind, Ns16550};

/// NS16550 IO Port 版本驱动
#[derive(Clone, Debug)]
pub struct Port {
    port: u16,
}

impl Kind for Port {
    fn read_reg(&self, reg: u8) -> u8 {
        unsafe { x86::io::inb(self.port + reg as u16) }
    }

    fn write_reg(&self, reg: u8, val: u8) {
        unsafe { x86::io::outb(self.port + reg as u16, val) }
    }

    fn get_base(&self) -> usize {
        self.port as _
    }
}

impl Ns16550<Port> {
    /// 创建一个新的 NS16550 IO Port 版本驱动实例
    ///
    /// # 参数
    ///
    /// * `port` - 串口基地址 (如 COM1 为 0x3F8)
    /// * `clock_freq` - UART 时钟频率，通常为 1.8432 MHz
    pub fn new_port(port: u16, clock_freq: u32) -> Ns16550<Port> {
        let base = Port { port };

        Ns16550 {
            base,
            clock_freq,
            saved_lsr: super::registers::LineStatusFlags::empty(),
        }
    }
}
