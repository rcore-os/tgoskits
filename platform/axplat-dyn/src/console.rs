use ax_plat::console::ConsoleIf;

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
