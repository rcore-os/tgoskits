//! NS16550 MMIO 版本实现
//!
//! 适用于嵌入式平台的内存映射 IO 版本

use core::ptr::NonNull;

use rdif_serial::{BSerial, InterfaceRaw, SerialDyn};

use super::{Kind, Ns16550};
use crate::ns16550::{Ns16550IrqHandler, Ns16550Receiver, Ns16550Sender};

#[derive(Clone)]
pub struct Mmio {
    base: usize,
    width: usize,
}

impl Kind for Mmio {
    fn read_reg(&self, reg: u8) -> u8 {
        unsafe {
            let addr = self.base + (reg as usize) * self.width;
            (addr as *const u8).read_volatile()
        }
    }

    fn write_reg(&self, reg: u8, val: u8) {
        unsafe {
            let addr = self.base + (reg as usize) * self.width;
            (addr as *mut u8).write_volatile(val);
        }
    }

    fn get_base(&self) -> usize {
        self.base
    }
}

impl Ns16550<Mmio> {
    pub fn new_mmio(base: NonNull<u8>, clock_freq: u32, reg_width: usize) -> Ns16550<Mmio> {
        let base = Mmio {
            base: base.as_ptr() as usize,
            width: reg_width,
        };

        Ns16550 {
            base: base.clone(),
            clock_freq,
            irq: Some(Ns16550IrqHandler { base: base.clone() }),
            tx: Some(crate::Sender::Ns16550MmioSender(Ns16550Sender {
                base: base.clone(),
            })),
            rx: Some(crate::Receiver::Ns16550MmioReceiver(Ns16550Receiver {
                base,
            })),
        }
    }

    pub fn new_mmio_boxed(base: NonNull<u8>, clock_freq: u32, reg_width: usize) -> BSerial {
        let mut serial = Ns16550::new_mmio(base, clock_freq, reg_width);
        serial.open();
        SerialDyn::new_boxed(serial)
    }

    pub fn take_tx(&mut self) -> Option<crate::Sender> {
        self.tx.take()
    }

    pub fn take_rx(&mut self) -> Option<crate::Receiver> {
        self.rx.take()
    }
}
