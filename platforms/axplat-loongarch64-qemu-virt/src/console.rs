use crate::mem::phys_to_virt;
use kspin::SpinNoIrq;
use memory_addr::PhysAddr;
use ns16550a::Uart;

const UART_BASE: PhysAddr = pa!(crate::config::devices::UART_PADDR);

static UART: SpinNoIrq<Uart> = SpinNoIrq::new(Uart::new(phys_to_virt(UART_BASE).as_usize()));

use axplat::console::ConsoleIf;

struct ConsoleIfImpl;

#[impl_plat_interface]
impl ConsoleIf for ConsoleIfImpl {
    /// Writes bytes to the console from input u8 slice.
    fn write_bytes(bytes: &[u8]) {
        for &c in bytes {
            let uart = UART.lock();
            match c {
                b'\n' => {
                    let _ = uart.put(b'\r');
                    let _ = uart.put(b'\n');
                }
                c => {
                    let _ = uart.put(c);
                }
            }
        }
    }

    /// Reads bytes from the console into the given mutable slice.
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize {
        for (i, byte) in bytes.iter_mut().enumerate() {
            match UART.lock().get() {
                Some(c) => *byte = c,
                None => return i,
            }
        }
        bytes.len()
    }
}
