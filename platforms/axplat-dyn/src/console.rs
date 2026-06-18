#[cfg(feature = "irq")]
use ax_plat::console::ConsoleIrqEvent;
use ax_plat::console::{ConsoleDeviceId, ConsoleIf};

struct ConsoleIfImpl;

#[impl_plat_interface]
impl ConsoleIf for ConsoleIfImpl {
    /// Writes given bytes to the console.
    fn write_bytes(bytes: &[u8]) {
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let written = somehal::console::_write_bytes(remaining);
            if written == 0 {
                core::hint::spin_loop();
                continue;
            }
            remaining = &remaining[written..];
        }
    }

    /// Reads bytes from the console into the given mutable slice.
    ///
    /// Returns the number of bytes read.
    fn read_bytes(bytes: &mut [u8]) -> usize {
        let mut read_len = 0;
        while read_len < bytes.len() {
            if let Some(c) = somehal::console::read_byte() {
                bytes[read_len] = c;
            } else {
                break;
            }
            read_len += 1;
        }
        read_len
    }

    fn device_id() -> Option<ConsoleDeviceId> {
        somehal::console_device_id()
    }

    /// Returns the IRQ number for the console input interrupt.
    ///
    /// Returns `None` if input interrupt is not supported.
    #[cfg(feature = "irq")]
    fn irq_num() -> Option<usize> {
        somehal::console::irq_num()
    }

    #[cfg(feature = "irq")]
    fn set_input_irq_enabled(enabled: bool) {
        somehal::console::set_input_irq_enabled(enabled);
    }

    #[cfg(feature = "irq")]
    fn handle_irq() -> ConsoleIrqEvent {
        let raw = somehal::console::handle_irq();
        let mut event = ConsoleIrqEvent::empty();
        if raw & somehal::console::CONSOLE_IRQ_RX_READY != 0 {
            event |= ConsoleIrqEvent::RX_READY;
        }
        if raw & somehal::console::CONSOLE_IRQ_RX_ERROR != 0 {
            event |= ConsoleIrqEvent::RX_ERROR;
        }
        if raw & somehal::console::CONSOLE_IRQ_OVERRUN != 0 {
            event |= ConsoleIrqEvent::OVERRUN;
        }
        event
    }
}
