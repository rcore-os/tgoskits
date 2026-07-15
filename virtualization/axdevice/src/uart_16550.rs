#[cfg(not(test))]
use ax_kspin::SpinNoIrq as StateLock;
#[cfg(test)]
use ax_kspin::SpinRaw as StateLock;
use axdevice_base::{
    AccessWidth, BaseDeviceOps, DeviceError, DeviceResult, EmuDeviceType, GuestPhysAddr,
    GuestPhysAddrRange,
};

use crate::{DeviceManagerError, DeviceManagerResult};

const MIN_MMIO_SIZE: usize = 8;
const FIFO_CAPACITY: usize = 4096;

const REG_RBR_THR_DLL: usize = 0;
const REG_IER_DLM: usize = 1;
const REG_IIR_FCR: usize = 2;
const REG_LCR: usize = 3;
const REG_MCR: usize = 4;
const REG_LSR: usize = 5;
const REG_MSR: usize = 6;
const REG_SCR: usize = 7;

const IER_RDI: u8 = 1 << 0;
const IER_THRI: u8 = 1 << 1;
const IIR_NO_INT: u8 = 0x01;
const IIR_THRI: u8 = 0x02;
const IIR_RDI: u8 = 0x04;
const LCR_DLAB: u8 = 1 << 7;
const LSR_DR: u8 = 1 << 0;
const LSR_THRE: u8 = 1 << 5;
const LSR_TEMT: u8 = 1 << 6;

#[derive(Debug)]
struct ByteFifo {
    bytes: [u8; FIFO_CAPACITY],
    head: usize,
    len: usize,
}

impl ByteFifo {
    const fn new() -> Self {
        Self {
            bytes: [0; FIFO_CAPACITY],
            head: 0,
            len: 0,
        }
    }

    fn push_drop_oldest(&mut self, byte: u8) {
        if self.len == FIFO_CAPACITY {
            self.head = (self.head + 1) % FIFO_CAPACITY;
            self.len -= 1;
        }
        self.bytes[(self.head + self.len) % FIFO_CAPACITY] = byte;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.bytes[self.head];
        self.head = (self.head + 1) % FIFO_CAPACITY;
        self.len -= 1;
        Some(byte)
    }

    fn drain_into(&mut self, output: &mut [u8]) -> usize {
        let mut count = 0;
        while count < output.len() {
            let Some(byte) = self.pop() else {
                break;
            };
            output[count] = byte;
            count += 1;
        }
        count
    }
}

#[derive(Debug)]
struct Uart16550State {
    rx: ByteFifo,
    tx: ByteFifo,
    ier: u8,
    lcr: u8,
    mcr: u8,
    msr: u8,
    scr: u8,
    dll: u8,
    dlm: u8,
}

impl Uart16550State {
    const fn new() -> Self {
        Self {
            rx: ByteFifo::new(),
            tx: ByteFifo::new(),
            ier: 0,
            lcr: 0x03,
            mcr: 0,
            msr: 0,
            scr: 0,
            dll: 0,
            dlm: 0,
        }
    }

    fn line_status(&self) -> u8 {
        let mut status = LSR_THRE | LSR_TEMT;
        if self.rx.len != 0 {
            status |= LSR_DR;
        }
        status
    }

    fn interrupt_identification(&self) -> u8 {
        if self.ier & IER_RDI != 0 && self.rx.len != 0 {
            IIR_RDI
        } else if self.ier & IER_THRI != 0 {
            IIR_THRI
        } else {
            IIR_NO_INT
        }
    }
}

/// Minimal ns16550a-compatible MMIO UART for AArch64 guests.
pub struct EmulatedUart16550 {
    base: GuestPhysAddr,
    size: usize,
    irq_id: usize,
    state: StateLock<Uart16550State>,
}

impl EmulatedUart16550 {
    /// Creates an emulated 16550 UART after validating its MMIO aperture.
    pub fn try_new(base: GuestPhysAddr, size: usize, irq_id: usize) -> DeviceManagerResult<Self> {
        if size < MIN_MMIO_SIZE || base.as_usize().checked_add(size).is_none() {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "initialize AArch64 console",
                detail: alloc::format!(
                    "invalid 16550 MMIO range at {:#x} with size {size:#x}",
                    base.as_usize()
                ),
            });
        }
        Ok(Self {
            base,
            size,
            irq_id,
            state: StateLock::new(Uart16550State::new()),
        })
    }

    /// Enqueues host input and reports whether an RX interrupt is pending.
    pub fn push_input(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        let mut state = self.state.lock();
        for &byte in bytes {
            state.rx.push_drop_oldest(byte);
        }
        state.interrupt_identification() != IIR_NO_INT
    }

    /// Returns the guest interrupt line used by this UART.
    pub const fn irq_id(&self) -> usize {
        self.irq_id
    }

    /// Drains bytes transmitted by the guest.
    pub fn drain_output(&self, output: &mut [u8]) -> usize {
        self.state.lock().tx.drain_into(output)
    }

    fn read_byte(&self, offset: usize) -> u8 {
        let mut state = self.state.lock();
        let dlab = state.lcr & LCR_DLAB != 0;
        match offset {
            REG_RBR_THR_DLL if dlab => state.dll,
            REG_RBR_THR_DLL => state.rx.pop().unwrap_or(0),
            REG_IER_DLM if dlab => state.dlm,
            REG_IER_DLM => state.ier,
            REG_IIR_FCR => state.interrupt_identification(),
            REG_LCR => state.lcr,
            REG_MCR => state.mcr,
            REG_LSR => state.line_status(),
            REG_MSR => state.msr,
            REG_SCR => state.scr,
            _ => 0,
        }
    }

    fn write_byte(&self, offset: usize, value: u8) {
        let mut state = self.state.lock();
        let dlab = state.lcr & LCR_DLAB != 0;
        match offset {
            REG_RBR_THR_DLL if dlab => state.dll = value,
            REG_RBR_THR_DLL => state.tx.push_drop_oldest(value),
            REG_IER_DLM if dlab => state.dlm = value,
            REG_IER_DLM => state.ier = value,
            REG_IIR_FCR => {}
            REG_LCR => state.lcr = value,
            REG_MCR => state.mcr = value,
            REG_MSR => state.msr = value,
            REG_SCR => state.scr = value,
            _ => {}
        }
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for EmulatedUart16550 {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Console
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.base, self.size)
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        if width == AccessWidth::Qword {
            return Err(DeviceError::Unsupported {
                operation: "read 16550 register",
                detail: "16550 only supports 8/16/32-bit MMIO reads".into(),
            });
        }
        let offset = addr.as_usize() - self.base.as_usize();
        let mut value = 0;
        for index in 0..width.size() {
            value |= (self.read_byte(offset + index) as usize) << (index * 8);
        }
        Ok(value)
    }

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, value: usize) -> DeviceResult {
        if width == AccessWidth::Qword {
            return Err(DeviceError::Unsupported {
                operation: "write 16550 register",
                detail: "16550 only supports 8/16/32-bit MMIO writes".into(),
            });
        }
        let offset = addr.as_usize() - self.base.as_usize();
        for index in 0..width.size() {
            self.write_byte(offset + index, ((value >> (index * 8)) & 0xff) as u8);
        }
        Ok(())
    }
}

// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[cfg(test)]
mod tests {
    use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceError};

    use super::EmulatedUart16550;
    use crate::DeviceManagerError;

    const BASE: usize = 0x0900_0000;
    const FIFO_CAPACITY: usize = 4096;

    #[test]
    fn uart_16550_rx_interrupt_tracks_ier_and_fifo() {
        let uart = EmulatedUart16550::try_new(BASE.into(), 8, 33).unwrap();
        assert!(!uart.push_input(b"A"));
        assert_eq!(
            uart.handle_read((BASE + 2).into(), AccessWidth::Byte)
                .unwrap(),
            1
        );
        uart.handle_write((BASE + 1).into(), AccessWidth::Byte, 1)
            .unwrap();
        assert!(uart.push_input(b"B"));
        assert_eq!(
            uart.handle_read((BASE + 2).into(), AccessWidth::Byte)
                .unwrap(),
            4
        );
        assert_eq!(
            uart.handle_read(BASE.into(), AccessWidth::Byte).unwrap(),
            b"A"[0] as usize
        );
    }

    #[test]
    fn uart_16550_dlab_selects_divisor_registers() {
        let uart = EmulatedUart16550::try_new(BASE.into(), 8, 33).unwrap();
        uart.handle_write((BASE + 3).into(), AccessWidth::Byte, 0x83)
            .unwrap();
        uart.handle_write(BASE.into(), AccessWidth::Byte, 0x34)
            .unwrap();
        uart.handle_write((BASE + 1).into(), AccessWidth::Byte, 0x12)
            .unwrap();
        assert_eq!(
            uart.handle_read(BASE.into(), AccessWidth::Byte).unwrap(),
            0x34
        );
        assert_eq!(
            uart.handle_read((BASE + 1).into(), AccessWidth::Byte)
                .unwrap(),
            0x12
        );
    }

    #[test]
    fn uart_16550_rejects_qword_and_invalid_apertures() {
        let uart = EmulatedUart16550::try_new(BASE.into(), 8, 33).unwrap();
        assert!(matches!(
            uart.handle_read(BASE.into(), AccessWidth::Qword),
            Err(DeviceError::Unsupported { .. })
        ));
        assert!(matches!(
            EmulatedUart16550::try_new(BASE.into(), 7, 33),
            Err(DeviceManagerError::InvalidConfig { .. })
        ));
        assert!(matches!(
            EmulatedUart16550::try_new((usize::MAX - 3).into(), 8, 33),
            Err(DeviceManagerError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn uart_16550_rx_fifo_drops_the_oldest_byte() {
        let uart = EmulatedUart16550::try_new(BASE.into(), 8, 33).unwrap();
        let mut input = [b"A"[0]; FIFO_CAPACITY + 1];
        input[0] = b"0"[0];
        input[FIFO_CAPACITY] = b"Z"[0];
        assert!(!uart.push_input(&input));
        assert_eq!(
            uart.handle_read(BASE.into(), AccessWidth::Byte).unwrap(),
            b"A"[0] as usize
        );
        for _ in 1..FIFO_CAPACITY - 1 {
            uart.handle_read(BASE.into(), AccessWidth::Byte).unwrap();
        }
        assert_eq!(
            uart.handle_read(BASE.into(), AccessWidth::Byte).unwrap(),
            b"Z"[0] as usize
        );
    }

    #[test]
    fn uart_16550_tx_fifo_drops_the_oldest_byte() {
        let uart = EmulatedUart16550::try_new(BASE.into(), 8, 33).unwrap();
        uart.handle_write(BASE.into(), AccessWidth::Byte, b"0"[0] as usize)
            .unwrap();
        for _ in 1..FIFO_CAPACITY {
            uart.handle_write(BASE.into(), AccessWidth::Byte, b"A"[0] as usize)
                .unwrap();
        }
        uart.handle_write(BASE.into(), AccessWidth::Byte, b"Z"[0] as usize)
            .unwrap();

        let mut output = [0; FIFO_CAPACITY];
        assert_eq!(uart.drain_output(&mut output), FIFO_CAPACITY);
        assert_eq!(output[0], b"A"[0]);
        assert_eq!(output[FIFO_CAPACITY - 1], b"Z"[0]);
    }
}
