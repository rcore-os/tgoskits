use core::{
    fmt::Arguments,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Function pointer type used by axtest for formatted output.
pub type AxTestPrintFn = for<'a> fn(Arguments<'a>);

static PRINTER: AtomicUsize = AtomicUsize::new(0);

/// Set the formatted output function used by axtest.
///
/// When no printer is configured, test output is discarded.
pub fn set_printer(printer: AxTestPrintFn) {
    PRINTER.store(printer as usize, Ordering::Release);
}

#[doc(hidden)]
pub fn _print(args: Arguments<'_>) {
    let printer = PRINTER.load(Ordering::Acquire);
    if printer == 0 {
        return;
    }

    // SAFETY: `printer` was loaded from `PRINTER`, which is only written by
    // `set_printer` with the address of a valid `AxTestPrintFn` function
    // pointer. The zero sentinel is handled above, so any non-zero value
    // here is a valid function pointer of the expected type.
    let printer: AxTestPrintFn = unsafe { core::mem::transmute(printer) };
    printer(args);
}

#[doc(hidden)]
pub fn _println(args: Arguments<'_>) {
    _print(args);
    _print(format_args!("\n"));
}

#[macro_export]
macro_rules! axtest_println {
    () => {
        $crate::print::_println(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::print::_println(format_args!($($arg)*))
    };
}
