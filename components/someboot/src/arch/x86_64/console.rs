pub struct Console;

const COM1_PORT: u16 = 0x3f8;
const COM1_CLOCK_HZ: u32 = 1_843_200;
const COM1_IRQ_VECTOR: usize = 0x30 + 4;

const UART_IER: u16 = 1;
const UART_IIR: u16 = 2;
const UART_LCR: u16 = 3;
const UART_MCR: u16 = 4;
const UART_LSR: u16 = 5;

const IER_RECEIVED_DATA_AVAILABLE: u8 = 1 << 0;
const IER_RECEIVER_LINE_STATUS: u8 = 1 << 2;

const LCR_DIVISOR_LATCH_ACCESS: u8 = 1 << 7;
const MCR_INTERRUPT_OUTPUT_ENABLE: u8 = 1 << 3;

const IIR_NO_INTERRUPT_PENDING: u8 = 1 << 0;
const IIR_INTERRUPT_ID_MASK: u8 = 0x0e;
const IIR_RECEIVER_LINE_STATUS: u8 = 0x06;
const IIR_RECEIVED_DATA_AVAILABLE: u8 = 0x04;
const IIR_CHARACTER_TIMEOUT: u8 = 0x0c;

const LSR_DATA_READY: u8 = 1 << 0;
const LSR_OVERRUN_ERROR: u8 = 1 << 1;
const LSR_RX_ERROR_MASK: u8 = 0x1e;

impl crate::console::ArchConsoleOps for Console {
    fn init() -> bool {
        let mut uart = some_serial::ns16550::Ns16550::new_port(COM1_PORT, COM1_CLOCK_HZ);
        uart.open();
        crate::console::set_earlycon_serial(crate::console::EarlySerial::Ns16550Port(uart));
        true
    }

    fn read_byte() -> Option<u8> {
        unsafe {
            let status = x86::io::inb(COM1_PORT + UART_LSR);
            if status & LSR_DATA_READY == 0 {
                None
            } else {
                Some(x86::io::inb(COM1_PORT))
            }
        }
    }

    fn irq_num() -> Option<usize> {
        Some(COM1_IRQ_VECTOR)
    }

    fn set_input_irq_enabled(enabled: bool) {
        unsafe {
            let lcr = x86::io::inb(COM1_PORT + UART_LCR);
            x86::io::outb(COM1_PORT + UART_LCR, lcr & !LCR_DIVISOR_LATCH_ACCESS);

            let mut mcr = x86::io::inb(COM1_PORT + UART_MCR);
            if enabled {
                mcr |= MCR_INTERRUPT_OUTPUT_ENABLE;
            } else {
                mcr &= !MCR_INTERRUPT_OUTPUT_ENABLE;
            }
            x86::io::outb(COM1_PORT + UART_MCR, mcr);

            x86::io::outb(
                COM1_PORT + UART_IER,
                if enabled {
                    IER_RECEIVED_DATA_AVAILABLE | IER_RECEIVER_LINE_STATUS
                } else {
                    0
                },
            );
        }
    }

    fn handle_irq() -> u32 {
        let iir = unsafe { x86::io::inb(COM1_PORT + UART_IIR) };
        let lsr = unsafe { x86::io::inb(COM1_PORT + UART_LSR) };
        let mut events = 0;

        if lsr & LSR_DATA_READY != 0 {
            events |= crate::console::CONSOLE_IRQ_RX_READY;
        }
        if lsr & LSR_OVERRUN_ERROR != 0 {
            events |= crate::console::CONSOLE_IRQ_OVERRUN;
        }
        if lsr & LSR_RX_ERROR_MASK != 0 {
            events |= crate::console::CONSOLE_IRQ_RX_ERROR;
        }

        if iir & IIR_NO_INTERRUPT_PENDING == 0 {
            match iir & IIR_INTERRUPT_ID_MASK {
                IIR_RECEIVED_DATA_AVAILABLE | IIR_CHARACTER_TIMEOUT => {
                    events |= crate::console::CONSOLE_IRQ_RX_READY;
                }
                IIR_RECEIVER_LINE_STATUS => {
                    events |= crate::console::CONSOLE_IRQ_RX_ERROR;
                }
                _ => {}
            }
        }

        events
    }
}
