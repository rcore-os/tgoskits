pub struct Console;

const QEMU_VIRT_UART_PADDR: usize = 0x1fe0_01e0;
const QEMU_VIRT_UART_CLOCK_HZ: u32 = 100_000_000;
const QEMU_VIRT_UART_REG_WIDTH: usize = 1;

impl crate::console::ArchConsoleOps for Console {
    fn init() -> bool {
        use core::ptr::NonNull;

        use some_serial::InterfaceRaw;

        let Some(base) = NonNull::new(crate::mem::_fixmap_io(QEMU_VIRT_UART_PADDR)) else {
            return false;
        };
        let mut uart = some_serial::ns16550::Ns16550::new_mmio(
            base,
            QEMU_VIRT_UART_CLOCK_HZ,
            QEMU_VIRT_UART_REG_WIDTH,
        );
        uart.open();

        let Some(tx) = uart.take_tx() else {
            return false;
        };
        let Some(rx) = uart.take_rx() else {
            return false;
        };

        crate::console::set_earlycon_sender(tx);
        crate::console::set_earlycon_receiver(rx);
        unsafe {
            crate::console::DEBUG_BASE = QEMU_VIRT_UART_PADDR;
            crate::console::DEBUG_IS_MMIO = true;
        }
        true
    }
}
