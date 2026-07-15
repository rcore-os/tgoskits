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

#[cfg(not(test))]
use ax_kspin::SpinNoIrq as StateLock;
#[cfg(test)]
use ax_kspin::SpinRaw as StateLock;
use axdevice_base::{
    AccessWidth, BaseDeviceOps, DeviceError, DeviceResult, EmuDeviceType, GuestPhysAddr,
    GuestPhysAddrRange,
};

use crate::{DeviceManagerError, DeviceManagerResult};

const MIN_MMIO_SIZE: usize = 0x1000;
const FIFO_CAPACITY: usize = 4096;

const REG_DR: usize = 0x000;
const REG_FR: usize = 0x018;
const REG_IBRD: usize = 0x024;
const REG_FBRD: usize = 0x028;
const REG_LCR_H: usize = 0x02c;
const REG_CR: usize = 0x030;
const REG_IFLS: usize = 0x034;
const REG_IMSC: usize = 0x038;
const REG_RIS: usize = 0x03c;
const REG_MIS: usize = 0x040;
const REG_ICR: usize = 0x044;

const FR_RXFE: u32 = 1 << 4;
const FR_RXFF: u32 = 1 << 6;
const FR_TXFE: u32 = 1 << 7;
const INT_RX: u32 = 1 << 4;
const INT_TX: u32 = 1 << 5;
const INT_RT: u32 = 1 << 6;
const SUPPORTED_INTERRUPTS: u32 = INT_RX | INT_TX | INT_RT;

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
struct Pl011State {
    rx: ByteFifo,
    tx: ByteFifo,
    ibrd: u32,
    fbrd: u32,
    lcr_h: u32,
    cr: u32,
    ifls: u32,
    imsc: u32,
    ris: u32,
}

impl Pl011State {
    const fn new() -> Self {
        Self {
            rx: ByteFifo::new(),
            tx: ByteFifo::new(),
            ibrd: 0,
            fbrd: 0,
            lcr_h: 0,
            cr: 0x300,
            ifls: 0,
            imsc: 0,
            ris: INT_TX,
        }
    }

    fn update_rx_interrupt(&mut self) {
        if self.rx.len == 0 {
            self.ris &= !(INT_RX | INT_RT);
        } else {
            self.ris |= INT_RX;
        }
    }

    fn flags(&self) -> u32 {
        let mut flags = FR_TXFE;
        if self.rx.len == 0 {
            flags |= FR_RXFE;
        }
        if self.rx.len == FIFO_CAPACITY {
            flags |= FR_RXFF;
        }
        flags
    }

    fn masked_interrupts(&self) -> u32 {
        self.ris & self.imsc & SUPPORTED_INTERRUPTS
    }
}

/// Minimal PL011-compatible MMIO UART for AArch64 guests.
pub struct EmulatedPl011 {
    base: GuestPhysAddr,
    size: usize,
    irq_id: usize,
    state: StateLock<Pl011State>,
}

impl EmulatedPl011 {
    /// Creates an emulated PL011 after validating its MMIO aperture.
    pub fn try_new(base: GuestPhysAddr, size: usize, irq_id: usize) -> DeviceManagerResult<Self> {
        if size < MIN_MMIO_SIZE || base.as_usize().checked_add(size).is_none() {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "initialize AArch64 console",
                detail: alloc::format!(
                    "invalid PL011 MMIO range at {:#x} with size {size:#x}",
                    base.as_usize()
                ),
            });
        }
        Ok(Self {
            base,
            size,
            irq_id,
            state: StateLock::new(Pl011State::new()),
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
        state.update_rx_interrupt();
        state.masked_interrupts() != 0
    }

    /// Returns the guest interrupt line used by this UART.
    pub const fn irq_id(&self) -> usize {
        self.irq_id
    }

    /// Drains bytes transmitted by the guest.
    pub fn drain_output(&self, output: &mut [u8]) -> usize {
        self.state.lock().tx.drain_into(output)
    }

    fn read_register(&self, offset: usize) -> u32 {
        let mut state = self.state.lock();
        match offset {
            REG_DR => {
                let byte = state.rx.pop().unwrap_or(0);
                state.update_rx_interrupt();
                byte as u32
            }
            REG_FR => state.flags(),
            REG_IBRD => state.ibrd,
            REG_FBRD => state.fbrd,
            REG_LCR_H => state.lcr_h,
            REG_CR => state.cr,
            REG_IFLS => state.ifls,
            REG_IMSC => state.imsc,
            REG_RIS => state.ris,
            REG_MIS => state.masked_interrupts(),
            0xfe0 => 0x11,
            0xfe4 => 0x10,
            0xfe8 => 0x14,
            0xfec => 0,
            0xff0 => 0x0d,
            0xff4 => 0xf0,
            0xff8 => 0x05,
            0xffc => 0xb1,
            _ => 0,
        }
    }

    fn write_register(&self, offset: usize, value: u32) {
        let mut state = self.state.lock();
        match offset {
            REG_DR => {
                state.tx.push_drop_oldest(value as u8);
                state.ris |= INT_TX;
            }
            REG_IBRD => state.ibrd = value,
            REG_FBRD => state.fbrd = value,
            REG_LCR_H => state.lcr_h = value,
            REG_CR => state.cr = value,
            REG_IFLS => state.ifls = value,
            REG_IMSC => state.imsc = value & SUPPORTED_INTERRUPTS,
            REG_ICR => {
                state.ris &= !(value & SUPPORTED_INTERRUPTS);
                state.update_rx_interrupt();
            }
            _ => {}
        }
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for EmulatedPl011 {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::Console
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.base, self.size)
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        if width == AccessWidth::Qword {
            return Err(DeviceError::Unsupported {
                operation: "read PL011 register",
                detail: "PL011 only supports 8/16/32-bit MMIO reads".into(),
            });
        }
        let offset = addr.as_usize() - self.base.as_usize();
        let shift = (offset & 3) * 8;
        Ok(((self.read_register(offset & !3) >> shift) as usize) & width_mask(width))
    }

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, value: usize) -> DeviceResult {
        if width == AccessWidth::Qword {
            return Err(DeviceError::Unsupported {
                operation: "write PL011 register",
                detail: "PL011 only supports 8/16/32-bit MMIO writes".into(),
            });
        }
        let offset = addr.as_usize() - self.base.as_usize();
        let register = offset & !3;
        if register == REG_DR {
            self.write_register(register, value as u32);
            return Ok(());
        }
        let shift = (offset & 3) * 8;
        let mask = width_mask(width) << shift;
        let current = self.read_register(register) as usize;
        self.write_register(
            register,
            ((current & !mask) | ((value << shift) & mask)) as u32,
        );
        Ok(())
    }
}

fn width_mask(width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => 0xff,
        AccessWidth::Word => 0xffff,
        AccessWidth::Dword => 0xffff_ffff,
        AccessWidth::Qword => usize::MAX,
    }
}

#[cfg(test)]
mod tests {
    use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceError};

    use super::EmulatedPl011;
    use crate::DeviceManagerError;

    const BASE: usize = 0x0900_0000;
    const FIFO_CAPACITY: usize = 4096;

    #[test]
    fn pl011_rx_interrupt_tracks_fifo_and_mask() {
        let uart = EmulatedPl011::try_new(BASE.into(), 0x1000, 33).unwrap();
        uart.handle_write((BASE + 0x038).into(), AccessWidth::Dword, 1 << 4)
            .unwrap();
        assert!(uart.push_input(b"A"));
        assert_eq!(
            uart.handle_read((BASE + 0x040).into(), AccessWidth::Dword)
                .unwrap(),
            1 << 4
        );
        assert_eq!(
            uart.handle_read(BASE.into(), AccessWidth::Byte).unwrap(),
            b"A"[0] as usize
        );
        assert_eq!(
            uart.handle_read((BASE + 0x040).into(), AccessWidth::Dword)
                .unwrap(),
            0
        );
    }

    #[test]
    fn pl011_reports_fifo_status_and_peripheral_ids() {
        let uart = EmulatedPl011::try_new(BASE.into(), 0x1000, 33).unwrap();
        assert_eq!(
            uart.handle_read((BASE + 0x018).into(), AccessWidth::Dword)
                .unwrap(),
            (1 << 4) | (1 << 7)
        );
        assert_eq!(
            uart.handle_read((BASE + 0xfe0).into(), AccessWidth::Dword)
                .unwrap(),
            0x11
        );
        assert_eq!(
            uart.handle_read((BASE + 0xffc).into(), AccessWidth::Dword)
                .unwrap(),
            0xb1
        );
    }

    #[test]
    fn pl011_rejects_qword_accesses() {
        let uart = EmulatedPl011::try_new(BASE.into(), 0x1000, 33).unwrap();
        assert!(matches!(
            uart.handle_read(BASE.into(), AccessWidth::Qword),
            Err(DeviceError::Unsupported { .. })
        ));
        assert!(matches!(
            uart.handle_write(BASE.into(), AccessWidth::Qword, 0),
            Err(DeviceError::Unsupported { .. })
        ));
    }

    #[test]
    fn pl011_rejects_invalid_mmio_apertures() {
        assert!(matches!(
            EmulatedPl011::try_new(BASE.into(), 0xfff, 33),
            Err(DeviceManagerError::InvalidConfig { .. })
        ));
        assert!(matches!(
            EmulatedPl011::try_new((usize::MAX - 0x7ff).into(), 0x1000, 33),
            Err(DeviceManagerError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn pl011_rx_fifo_drops_the_oldest_byte_and_reports_full() {
        let uart = EmulatedPl011::try_new(BASE.into(), 0x1000, 33).unwrap();
        let mut input = [b"A"[0]; FIFO_CAPACITY + 1];
        input[0] = b"0"[0];
        input[FIFO_CAPACITY] = b"Z"[0];

        assert!(!uart.push_input(&input));
        assert_eq!(
            uart.handle_read((BASE + 0x018).into(), AccessWidth::Dword)
                .unwrap()
                & (1 << 6),
            1 << 6
        );
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
    fn pl011_tx_fifo_drops_the_oldest_byte() {
        let uart = EmulatedPl011::try_new(BASE.into(), 0x1000, 33).unwrap();
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
