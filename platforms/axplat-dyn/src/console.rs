#[cfg(feature = "irq")]
use ax_plat::console::ConsoleIrqEvent;
use ax_plat::console::{ConsoleDeviceIdError, ConsoleDeviceIdResult, ConsoleIf};

#[cfg(all(feature = "irq", target_arch = "x86_64"))]
fn console_irq(raw: usize) -> Option<ax_plat::irq::IrqId> {
    if let Some(gsi) = raw.checked_sub(rdrive::probe::acpi::PCI_INTX_VECTOR_BASE) {
        ax_plat::irq::resolve_irq_source(ax_plat::irq::IrqSource::AcpiGsi(gsi as u32)).ok()
    } else {
        Some(ax_plat::irq::IrqNumber(raw).expect("console IRQ exceeds legacy IRQ width"))
    }
}

#[cfg(all(feature = "irq", not(target_arch = "x86_64")))]
fn console_irq(raw: usize) -> Option<ax_plat::irq::IrqId> {
    Some(ax_plat::irq::IrqNumber(raw).expect("console IRQ exceeds legacy IRQ width"))
}

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

    fn device_id() -> ConsoleDeviceIdResult {
        somehal::console_device_id().map_err(|err| match err {
            somehal::ConsoleDeviceIdError::NotSpecified => ConsoleDeviceIdError::NotSpecified,
            somehal::ConsoleDeviceIdError::NoHardwareDevice => {
                ConsoleDeviceIdError::NoHardwareDevice
            }
            somehal::ConsoleDeviceIdError::DeviceNotFound => ConsoleDeviceIdError::DeviceNotFound,
        })
    }

    fn claim_runtime_output() {
        somehal::console::claim_runtime_output();
    }

    /// Returns the IRQ number for the console input interrupt.
    ///
    /// Returns `None` if input interrupt is not supported.
    #[cfg(feature = "irq")]
    fn irq_num() -> Option<ax_plat::irq::IrqId> {
        somehal::console::irq_num().and_then(console_irq)
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

#[cfg(all(test, feature = "irq", target_arch = "x86_64"))]
mod tests {
    #[test]
    fn x86_console_irq_without_acpi_route_falls_back_to_polling() {
        let raw = rdrive::probe::acpi::PCI_INTX_VECTOR_BASE + 4;

        assert!(super::console_irq(raw).is_none());
    }
}
