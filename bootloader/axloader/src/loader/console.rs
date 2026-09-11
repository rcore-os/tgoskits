use core::fmt;

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        $crate::console::print(format_args!($($arg)*));
    }};
}

#[macro_export]
macro_rules! logln {
    ($($arg:tt)*) => {{
        $crate::console::println(format_args!($($arg)*));
    }};
}

/// Writes diagnostics through the firmware-selected console output.
///
/// The loader deliberately does not open `SerialIo`: the firmware decides
/// where `ConOut` is rendered, and the target kernel owns the serial device
/// after handoff.
pub fn print(args: fmt::Arguments<'_>) {
    let _ = uefi::system::with_stdout(|stdout| fmt::write(stdout, args));
}

pub fn println(args: fmt::Arguments<'_>) {
    print(args);
    print(format_args!("\n"));
}
