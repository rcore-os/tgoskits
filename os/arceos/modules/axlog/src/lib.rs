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

use core::{fmt, str::FromStr};

#[cfg(not(feature = "std"))]
use ax_crate_interface::call_interface;
use log::{Level, LevelFilter, Log, Metadata, Record};
pub use log::{debug, error, info, trace, warn};

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

/// Kind of one complete console record submitted to the runtime.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    /// Direct output from `ax_print!` and `ax_println!`.
    Print,
    /// One record emitted through the `log` facade.
    Log,
}

/// Metadata which does not require runtime callbacks to collect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordMeta {
    kind: RecordKind,
}

impl RecordMeta {
    /// Metadata for direct console output.
    pub const fn print() -> Self {
        Self {
            kind: RecordKind::Print,
        }
    }

    /// Metadata for a structured log record.
    pub const fn log() -> Self {
        Self {
            kind: RecordKind::Log,
        }
    }

    /// Returns the record kind.
    pub const fn kind(self) -> RecordKind {
        self.kind
    }
}

/// Result of a non-blocking runtime publication attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishStatus {
    /// The complete formatted record was published.
    Published,
    /// A bounded, UTF-8-safe prefix plus truncation marker was published.
    Truncated,
    /// No slot could be reserved and the record was dropped.
    Dropped,
}

impl PublishStatus {
    /// Returns whether the runtime accepted a record for deferred output.
    pub const fn is_published(self) -> bool {
        matches!(self, Self::Published | Self::Truncated)
    }
}

/// Extern interfaces that must be implemented in other crates.
#[ax_crate_interface::def_interface]
pub trait LogIf {
    /// Consumes and publishes one complete ordinary record without blocking.
    fn try_publish(meta: RecordMeta, args: fmt::Arguments<'_>) -> PublishStatus;

    /// Performs one bounded emergency output attempt without using the mailbox.
    fn emergency_write(args: fmt::Arguments<'_>) -> usize;
}

struct Logger;

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
                publish_fmt(
                    RecordMeta::log(),
                    format_args!(
                        "{}\n",
                        with_color!(
                            ColorCode::White,
                            "[{time} {path}:{line}] {args}",
                            time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.6f"),
                            path = path,
                            line = line,
                            args = with_color!(args_color, "{}", record.args()),
                        )
                    ),
                );
            } else {
                publish_fmt(
                    RecordMeta::log(),
                    format_args!(
                        "{}\n",
                        with_color!(
                            ColorCode::White,
                            "{path}:{line}] {args}",
                            path = path,
                            line = line,
                            args = with_color!(args_color, "{}", record.args()),
                        )
                    ),
                );
            }
        }
    }

    fn flush(&self) {}
}

fn publish_fmt(meta: RecordMeta, args: fmt::Arguments<'_>) -> PublishStatus {
    cfg_if::cfg_if! {
        if #[cfg(feature = "std")] {
            let _ = meta;
            std::print!("{args}");
            PublishStatus::Published
        } else if #[cfg(not(feature = "std"))] {
            if axpanic::oops_in_progress() {
                if call_interface!(LogIf::emergency_write, args) == 0 {
                    PublishStatus::Dropped
                } else {
                    PublishStatus::Published
                }
            } else {
                call_interface!(LogIf::try_publish, meta, args)
            }
        }
    }
}

/// Prints the formatted string to the console.
pub fn print_fmt(args: fmt::Arguments) -> fmt::Result {
    let _ = publish_fmt(RecordMeta::print(), args);
    Ok(())
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
    extern crate std;

    use std::format;

    use super::*;

    #[test]
    fn structured_log_resets_color_before_the_terminal_newline() {
        let rendered = format!(
            "{}",
            format_args!(
                "{}\n",
                with_color!(
                    ColorCode::White,
                    "module:7] {}",
                    with_color!(ColorCode::Green, "message")
                )
            )
        );

        assert!(rendered.ends_with("\u{1b}[m\n"));
        assert!(!rendered.contains("\n\u{1b}[m"));
    }

    #[test]
    fn metadata_distinguishes_prints_from_structured_logs() {
        assert_eq!(RecordMeta::print().kind(), RecordKind::Print);
        assert_eq!(RecordMeta::log().kind(), RecordKind::Log);
    }

    #[test]
    fn truncated_publication_is_still_accepted() {
        assert!(PublishStatus::Published.is_published());
        assert!(PublishStatus::Truncated.is_published());
        assert!(!PublishStatus::Dropped.is_published());
    }
}
