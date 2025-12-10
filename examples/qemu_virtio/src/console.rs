//! 控制台输出

use core::fmt::{self, Write};
use spin::Mutex;

/// UART 基地址 (QEMU virt 机器)
const UART_BASE: usize = 0x1000_0000;

struct Uart {
    base: usize,
}

impl Uart {
    fn putchar(&mut self, c: u8) {
        unsafe {
            let ptr = self.base as *mut u8;
            ptr.write_volatile(c);
        }
    }
}

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.bytes() {
            self.putchar(c);
        }
        Ok(())
    }
}

static UART: Mutex<Uart> = Mutex::new(Uart { base: UART_BASE });

/// 输出单个字符
pub fn console_putchar(c: u8) {
    UART.lock().putchar(c);
}

/// 格式化输出
pub fn _print(args: fmt::Arguments) {
    UART.lock().write_fmt(args).unwrap();
}

struct SimpleLogger;

impl log::Log for SimpleLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let color = match record.level() {
                log::Level::Error => "\x1b[31m", // 红色
                log::Level::Warn => "\x1b[33m",  // 黄色
                log::Level::Info => "\x1b[32m",  // 绿色
                log::Level::Debug => "\x1b[36m", // 青色
                log::Level::Trace => "\x1b[90m", // 灰色
            };
            crate::println!(
                "{}[{}]\x1b[0m {}",
                color,
                record.level(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

static LOGGER: SimpleLogger = SimpleLogger;

/// 初始化控制台和日志
pub fn init() {
    log::set_logger(&LOGGER).unwrap();
    //log::set_max_level(log::LevelFilter::Error);
}
