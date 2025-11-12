use ansi_rgb::{Foreground, red, yellow};
use log::{Level, Log};
use rgb::{RGB8, Rgb};

pub fn init() {
    log::set_logger(&KLogger).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
}

fn level_to_rgb(level: Level) -> RGB8 {
    match level {
        Level::Error => red(),
        Level::Warn => yellow(),
        Level::Info => Rgb::new(0x00, 0xBC, 0x12),
        Level::Debug => Rgb::new(0x16, 0x85, 0xA9),
        Level::Trace => Rgb::new(128, 128, 128),
    }
}

fn level_icon(level: Level) -> &'static str {
    match level {
        Level::Error => "💥",
        Level::Warn => "⚠️",
        Level::Info => "💡",
        Level::Debug => "🐛",
        Level::Trace => "🔍",
    }
}

pub struct KLogger;

impl Log for KLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let level = record.level();
            let line = record.line().unwrap_or(0);
            let path = record.target();
            let args = record.args();

            let duration = super::time::since_boot();
            crate::__export::_write_fmt(format_args!(
                "{}",
                format_args!(
                    "{} {duration:<10.3?} [{path}:{line}] {args}\r\n",
                    level_icon(level),
                )
                .fg(level_to_rgb(level))
            ));
        }
    }
    fn flush(&self) {}
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::__export::_write_fmt(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {
        $crate::print!("{}\r\n", format_args!($($arg)*))
    };
}
