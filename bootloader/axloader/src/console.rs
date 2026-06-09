use core::fmt::{self, Write};

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        $crate::console::serial_print(format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => {{
        $crate::console::serial_println(format_args!($($arg)*));
    }};
}

pub fn serial_print(args: fmt::Arguments<'_>) {
    let mut serial = SerialWriter;
    let _ = serial.write_fmt(args);
}

pub fn serial_println(args: fmt::Arguments<'_>) {
    let mut serial = SerialWriter;
    let _ = serial.write_fmt(args);
    serial.write_str("\n").ok();
}

struct SerialWriter;

impl Write for SerialWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        write_serial(text.as_bytes());
        Ok(())
    }
}

#[cfg(target_arch = "x86_64")]
mod imp {
    use core::sync::atomic::{AtomicBool, Ordering};

    const COM1_PORT: u16 = 0x3f8;
    const UART_RBR_THR: u16 = 0;
    const UART_IER: u16 = 1;
    const UART_FCR: u16 = 2;
    const UART_LCR: u16 = 3;
    const UART_MCR: u16 = 4;
    const UART_LSR: u16 = 5;
    const UART_DLL: u16 = 0;
    const UART_DLM: u16 = 1;
    const UART_LSR_THRE: u8 = 1 << 5;
    const UART_LSR_TEMT: u8 = 1 << 6;
    const UART_LCR_DLAB: u8 = 1 << 7;
    static COM1_INITIALIZED: AtomicBool = AtomicBool::new(false);

    pub fn write_serial(bytes: &[u8]) {
        init_com1();
        for byte in bytes {
            if *byte == b'\n' {
                serial_putc(b'\r');
            }
            serial_putc(*byte);
        }
    }

    fn init_com1() {
        if COM1_INITIALIZED.swap(true, Ordering::AcqRel) {
            return;
        }
        unsafe {
            outb(COM1_PORT + UART_IER, 0x00);
            outb(COM1_PORT + UART_LCR, UART_LCR_DLAB);
            outb(COM1_PORT + UART_DLL, 0x01);
            outb(COM1_PORT + UART_DLM, 0x00);
            outb(COM1_PORT + UART_LCR, 0x03);
            outb(COM1_PORT + UART_FCR, 0xc7);
            outb(COM1_PORT + UART_MCR, 0x0b);
        }
    }

    fn serial_putc(byte: u8) {
        for _ in 0..100_000 {
            if unsafe { inb(COM1_PORT + UART_LSR) } & UART_LSR_THRE != 0 {
                unsafe { outb(COM1_PORT + UART_RBR_THR, byte) };
                wait_serial_empty();
                return;
            }
        }
    }

    fn wait_serial_empty() {
        for _ in 0..100_000 {
            if unsafe { inb(COM1_PORT + UART_LSR) } & UART_LSR_TEMT != 0 {
                return;
            }
        }
    }

    unsafe fn outb(port: u16, value: u8) {
        unsafe {
            core::arch::asm!(
                "out dx, al",
                in("dx") port,
                in("al") value,
                options(nomem, nostack, preserves_flags)
            );
        }
    }

    unsafe fn inb(port: u16) -> u8 {
        let value: u8;
        unsafe {
            core::arch::asm!(
                "in al, dx",
                in("dx") port,
                out("al") value,
                options(nomem, nostack, preserves_flags)
            );
        }
        value
    }
}

#[cfg(not(target_arch = "x86_64"))]
mod imp {
    pub fn write_serial(_bytes: &[u8]) {}
}

fn write_serial(bytes: &[u8]) {
    imp::write_serial(bytes);
}
