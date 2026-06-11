pub struct Console;

const COM1_PORT: u16 = 0x3f8;
const COM1_CLOCK_HZ: u32 = 1_843_200;

impl crate::console::ArchConsoleOps for Console {
    fn init() -> bool {
        use some_serial::InterfaceRaw;

        let mut uart = some_serial::ns16550::Ns16550::new_port(COM1_PORT, COM1_CLOCK_HZ);
        uart.open();

        let Some(tx) = uart.take_tx() else {
            return false;
        };
        let Some(rx) = uart.take_rx() else {
            return false;
        };

        crate::console::set_earlycon_sender(tx);
        crate::console::set_earlycon_receiver(rx);
        true
    }

    fn read_byte() -> Option<u8> {
        unsafe {
            let status = x86::io::inb(COM1_PORT + 5);
            if status & 1 == 0 {
                None
            } else {
                Some(x86::io::inb(COM1_PORT))
            }
        }
    }
}
