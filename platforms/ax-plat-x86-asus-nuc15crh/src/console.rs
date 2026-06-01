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

//! Uart 16550 serial port.

use ax_kspin::SpinNoIrq;
use ax_plat::console::ConsoleIf;
use uart_16550::SerialPort;

static COM1: SpinNoIrq<SerialPort> = unsafe { SpinNoIrq::new(SerialPort::new(0x3f8)) };
const COM1_PORT: u16 = 0x3f8;

/// Reads a byte from the console, or returns [`None`] if no input is available.
pub fn getchar() -> Option<u8> {
    COM1.lock().try_receive().ok()
}

pub fn init() {
    init_com1_115200();
}

fn init_com1_115200() {
    unsafe {
        x86::io::outb(COM1_PORT + 1, 0x00);
        x86::io::outb(COM1_PORT + 3, 0x80);
        x86::io::outb(COM1_PORT, 0x01);
        x86::io::outb(COM1_PORT + 1, 0x00);
        x86::io::outb(COM1_PORT + 3, 0x03);
        x86::io::outb(COM1_PORT + 2, 0xc7);
        x86::io::outb(COM1_PORT + 4, 0x0b);
        x86::io::outb(COM1_PORT + 1, 0x00);
    }
}

struct ConsoleIfImpl;

#[impl_plat_interface]
impl ConsoleIf for ConsoleIfImpl {
    /// Writes given bytes to the console.
    fn write_bytes(bytes: &[u8]) {
        // Hold COM1 for the entire write so that one logical output
        // (e.g. a full ANSI TUI frame) is never interleaved with output
        // from another task.  This also avoids paying lock-acquire cost
        // for every byte, which is visible on large frame dumps.
        let mut com = COM1.lock();
        for &c in bytes {
            com.send(c);
        }
    }

    /// Reads bytes from the console into the given mutable slice.
    ///
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize {
        let mut read_len = 0;
        while read_len < bytes.len() {
            if let Some(c) = getchar() {
                bytes[read_len] = c;
            } else {
                break;
            }
            read_len += 1;
        }
        read_len
    }

    /// Returns the IRQ number for the console input interrupt.
    ///
    /// Returns `None` if input interrupt is not supported.
    #[cfg(feature = "irq")]
    fn irq_num() -> Option<usize> {
        None
    }

    #[cfg(feature = "irq")]
    fn set_input_irq_enabled(_enabled: bool) {}

    #[cfg(feature = "irq")]
    fn handle_irq() -> ax_plat::console::ConsoleIrqEvent {
        ax_plat::console::ConsoleIrqEvent::empty()
    }
}
