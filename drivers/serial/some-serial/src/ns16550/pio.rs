//! NS16550 IO Port 版本实现
//!
//! 仅在 x86_64 架构下编译，使用 x86_64 crate 进行端口 I/O

use rdif_serial::InterfaceRaw;

use super::{Kind, Ns16550, Ns16550IrqHandler, Ns16550Receiver, Ns16550Sender};

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
            base: base.clone(),
            clock_freq,
            irq: Some(Ns16550IrqHandler { base: base.clone() }),
            tx: Some(crate::Sender::Ns16550Sender(Ns16550Sender {
                base: base.clone(),
            })),
            rx: Some(crate::Receiver::Ns16550Receiver(Ns16550Receiver { base })),
        }
    }

    pub fn new_port_boxed(port: u16, clock_freq: u32) -> rdif_serial::BSerial {
        let mut serial = Ns16550::new_port(port, clock_freq);
        serial.open();
        rdif_serial::SerialDyn::new_boxed(serial)
    }

    pub fn take_tx(&mut self) -> Option<crate::Sender> {
        self.tx.take()
    }

    pub fn take_rx(&mut self) -> Option<crate::Receiver> {
        self.rx.take()
    }
}
