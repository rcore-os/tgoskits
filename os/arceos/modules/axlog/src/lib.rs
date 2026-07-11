//! Macros for multi-level formatted logging used by
//! [ArceOS](https://github.com/arceos-org/arceos).
//!
//! The log macros, in descending order of level, are: [`error!`], [`warn!`],
//! [`info!`], [`debug!`], and [`trace!`].
//!
//! If it is used in `no_std` environment, the users need to implement the
//! [`LogIf`] to provide external functions such as console output.
//!
//! To use in the `std` environment, please enable the `std` feature:
//!
//! ```toml
//! [dependencies]
//! ax-log = { version = "0.1", features = ["std"] }
//! ```
//!
//! # Cargo features:
//!
//! - `std`: Use in the `std` environment. If it is enabled, you can use console
//!   output without implementing the [`LogIf`] trait. This is disabled by default.
//!
//! # Examples
//!
//! ```
//! # #[cfg(feature = "std")]
//! # {
//! use ax_log::{debug, error, info, trace, warn};
//!
//! // Initialize the logger.
//! ax_log::init();
//! // Set the maximum log level to `info`.
//! ax_log::set_max_level("info");
//!
//! // The following logs will be printed.
//! error!("error");
//! warn!("warn");
//! info!("info");
//!
//! // The following logs will not be printed.
//! debug!("debug");
//! trace!("trace");
//! # }
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

extern crate log;

use core::{
    fmt::{self, Write},
    str::FromStr,
};

#[cfg(not(feature = "std"))]
use ax_crate_interface::call_interface;
use log::{Level, LevelFilter, Log, Metadata, Record};
pub use log::{debug, error, info, trace, warn};

#[cfg(all(feature = "kmsg", not(feature = "std")))]
pub mod ring;

/// Prints to the console.
///
/// Equivalent to the [`ax_println!`] macro except that a newline is not printed at
/// the end of the message.
#[macro_export]
macro_rules! ax_print {
    ($($arg:tt)*) => {
        $crate::__print_impl(format_args!($($arg)*));
    }
}

/// Prints to the console, with a newline.
#[macro_export]
macro_rules! ax_println {
    () => { $crate::ax_print!("\n") };
    ($($arg:tt)*) => {
        $crate::__print_impl(format_args!("{}\n", format_args!($($arg)*)));
    }
}

macro_rules! with_color {
    ($color_code:expr, $($arg:tt)*) => {
        format_args!("\u{1B}[{}m{}\u{1B}[m", $color_code as u8, format_args!($($arg)*))
    };
}

#[repr(u8)]
#[allow(dead_code)]
enum ColorCode {
    Black         = 30,
    Red           = 31,
    Green         = 32,
    Yellow        = 33,
    Blue          = 34,
    Magenta       = 35,
    Cyan          = 36,
    White         = 37,
    BrightBlack   = 90,
    BrightRed     = 91,
    BrightGreen   = 92,
    BrightYellow  = 93,
    BrightBlue    = 94,
    BrightMagenta = 95,
    BrightCyan    = 96,
    BrightWhite   = 97,
}

/// Extern interfaces that must be implemented in other crates.
#[ax_crate_interface::def_interface]
pub trait LogIf {
    /// Writes a string to the console.
    fn console_write_str(s: &str);

    /// Gets current clock time.
    fn current_time() -> core::time::Duration;

    /// Gets current CPU ID.
    ///
    /// Returns [`None`] if you don't want to show the CPU ID in the log.
    fn current_cpu_id() -> Option<usize>;

    /// Gets current task ID.
    ///
    /// Returns [`None`] if you don't want to show the task ID in the log.
    fn current_task_id() -> Option<u64>;
}

struct Logger;

impl Write for Logger {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        cfg_if::cfg_if! {
            if #[cfg(feature = "std")] {
                std::print!("{s}");
            } else {
                call_interface!(LogIf::console_write_str, s);
            }
        }
        Ok(())
    }
}

const LOG_BUFFER_SIZE: usize = 2048;
const LOG_TRUNCATED_WITH_NEWLINE: &str = "\u{1B}[m<log truncated>\n";

struct LogBuffer<const N: usize> {
    buf: [u8; N],
    len: usize,
    truncated: bool,
}

impl<const N: usize> LogBuffer<N> {
    const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            truncated: false,
        }
    }

    fn as_str(&self) -> &str {
        // SAFETY: LogBuffer only appends complete UTF-8 strings or prefixes
        // ending at UTF-8 character boundaries.
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }

    fn append_truncation_marker(&mut self) {
        if !self.truncated {
            return;
        }

        let marker = LOG_TRUNCATED_WITH_NEWLINE.as_bytes();
        if N < marker.len() {
            self.len = 0;
            return;
        }

        if self.len + marker.len() > N {
            let mut keep = self.len.min(N - marker.len());
            while !self.is_char_boundary(keep) {
                keep -= 1;
            }
            self.len = keep;
        }

        self.buf[self.len..self.len + marker.len()].copy_from_slice(marker);
        self.len += marker.len();
    }

    fn is_char_boundary(&self, index: usize) -> bool {
        index == 0 || index == self.len || (self.buf[index] & 0b1100_0000) != 0b1000_0000
    }
}

impl<const N: usize> Write for LogBuffer<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let available = N.saturating_sub(self.len);
        if s.len() <= available {
            self.buf[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
            self.len += s.len();
            return Ok(());
        }

        self.truncated = true;
        let end = s
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|&index| index <= available)
            .last()
            .unwrap_or(0);
        self.buf[self.len..self.len + end].copy_from_slice(&s.as_bytes()[..end]);
        self.len += end;
        Ok(())
    }
}

/// Map a log [`Level`] to a Linux syslog priority (kernel facility 0, so the
/// priority byte equals the severity): `ERR`=3, `WARNING`=4, `INFO`=6,
/// `DEBUG`/`TRACE`=7.
#[cfg(all(feature = "kmsg", not(feature = "std")))]
fn level_to_priority(level: Level) -> u8 {
    match level {
        Level::Error => 3,
        Level::Warn => 4,
        Level::Info => 6,
        Level::Debug | Level::Trace => 7,
    }
}

/// Store a record into the kernel log ring with the caller-supplied `priority`
/// byte and echo it to the console, mirroring Linux's `devkmsg_emit` (store +
/// console). Used by the userspace `/dev/kmsg` write path, which needs the exact
/// priority preserved rather than collapsed through the 5-level log macros.
#[cfg(all(feature = "kmsg", not(feature = "std")))]
pub fn kmsg_emit(priority: u8, msg: &str) {
    let ts_nsec = call_interface!(LogIf::current_time).as_nanos() as u64;
    ring::push(priority, ts_nsec, msg);
    call_interface!(LogIf::console_write_str, msg);
    call_interface!(LogIf::console_write_str, "\n");
}

impl Log for Logger {
    #[inline]
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let level = record.level();
        let line = record.line().unwrap_or(0);
        let path = record.target();
        let args_color = match level {
            Level::Error => ColorCode::Red,
            Level::Warn => ColorCode::Yellow,
            Level::Info => ColorCode::Green,
            Level::Debug => ColorCode::Cyan,
            Level::Trace => ColorCode::BrightBlack,
        };

        cfg_if::cfg_if! {
            if #[cfg(feature = "std")] {
                print_log_fmt(with_color!(
                    ColorCode::White,
                    "[{time} {path}:{line}] {args}\n",
                    time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.6f"),
                    path = path,
                    line = line,
                    args = with_color!(args_color, "{}", record.args()),
                ));
            } else {
                let cpu_id = call_interface!(LogIf::current_cpu_id);
                let tid = call_interface!(LogIf::current_task_id);
                let now = call_interface!(LogIf::current_time);
                #[cfg(feature = "kmsg")]
                ring::push_fmt(
                    level_to_priority(level),
                    now.as_nanos() as u64,
                    *record.args(),
                );
                if let Some(cpu_id) = cpu_id {
                    if let Some(tid) = tid {
                        // show CPU ID and task ID
                        print_log_fmt(with_color!(
                            ColorCode::White,
                            "[{:>3}.{:06} {cpu_id}:{tid} {path}:{line}] {args}\n",
                            now.as_secs(),
                            now.subsec_micros(),
                            cpu_id = cpu_id,
                            tid = tid,
                            path = path,
                            line = line,
                            args = with_color!(args_color, "{}", record.args()),
                        ));
                    } else {
                        // show CPU ID only
                        print_log_fmt(with_color!(
                            ColorCode::White,
                            "[{:>3}.{:06} {cpu_id} {path}:{line}] {args}\n",
                            now.as_secs(),
                            now.subsec_micros(),
                            cpu_id = cpu_id,
                            path = path,
                            line = line,
                            args = with_color!(args_color, "{}", record.args()),
                        ));
                    }
                } else {
                    // neither CPU ID nor task ID is shown
                    print_log_fmt(with_color!(
                        ColorCode::White,
                        "[{:>3}.{:06} {path}:{line}] {args}\n",
                        now.as_secs(),
                        now.subsec_micros(),
                        path = path,
                        line = line,
                        args = with_color!(args_color, "{}", record.args()),
                    ));
                }
            }
        }
    }

    fn flush(&self) {}
}

fn write_fmt_locked(args: fmt::Arguments) -> fmt::Result {
    use ax_kspin::SpinNoIrq; // TODO: more efficient
    static LOCK: SpinNoIrq<()> = SpinNoIrq::new(());

    // Panic and oops paths must not re-enter the normal print lock because its
    // unlock path may restore preemption/IRQs and trigger more complex control
    // flow while the kernel is already failing.
    if axpanic::oops_in_progress() {
        return Logger.write_fmt(args);
    }

    let _guard = LOCK.lock();
    Logger.write_fmt(args)
}

fn print_log_fmt(args: fmt::Arguments) {
    if axpanic::oops_in_progress() {
        Logger.write_fmt(args).unwrap();
        return;
    }

    // Log records are internal tracing messages with a clear record boundary, so
    // they may be truncated to keep Display formatting outside the print lock.
    // They always include a trailing newline, even when ANSI reset bytes are
    // formatted after it. Keep that newline after replacing truncated content.
    let mut buf = LogBuffer::<LOG_BUFFER_SIZE>::new();
    buf.write_fmt(args).unwrap();
    buf.append_truncation_marker();
    write_fmt_locked(format_args!("{}", buf.as_str())).unwrap();
}

/// Prints the formatted string to the console.
pub fn print_fmt(args: fmt::Arguments) -> fmt::Result {
    // Direct console output preserves the caller's original bytes and length.
    // This keeps ax_print!/ax_println! and axstd println! behavior unchanged;
    // callers that format under this lock must still avoid recursive printing.
    write_fmt_locked(args)
}

#[doc(hidden)]
pub fn __print_impl(args: fmt::Arguments) {
    print_fmt(args).unwrap();
}

/// Initializes the logger.
///
/// This function should be called before any log macros are used, otherwise
/// nothing will be printed.
pub fn init() {
    log::set_logger(&Logger).unwrap();
    log::set_max_level(LevelFilter::Warn);
}

/// Set the maximum log level.
///
/// Unlike the features such as `log-level-error`, setting the logging level in
/// this way incurs runtime overhead. In addition, this function is no effect
/// when those features are enabled.
///
/// `level` should be one of `off`, `error`, `warn`, `info`, `debug`, `trace`.
pub fn set_max_level(level: &str) {
    let lf = LevelFilter::from_str(level)
        .ok()
        .unwrap_or(LevelFilter::Off);
    log::set_max_level(lf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_buffer_truncates_at_utf8_boundary() {
        let mut buf = LogBuffer::<22>::new();
        write!(buf, "ab你好xxxxxxxxxxxxxxxx\n").unwrap();
        buf.append_truncation_marker();

        assert_eq!(buf.as_str(), "ab\u{1B}[m<log truncated>\n");
    }

    #[test]
    fn log_buffer_keeps_untruncated_message() {
        let mut buf = LogBuffer::<32>::new();
        write!(buf, "short").unwrap();
        buf.append_truncation_marker();

        assert_eq!(buf.as_str(), "short");
    }
}
