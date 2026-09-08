use core::{fmt::Write as _, panic::PanicInfo};

/// Formats `fmt::Arguments` to the SBI console without allocation.
pub(crate) struct SbiConsoleWriter;

impl core::fmt::Write for SbiConsoleWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        #[allow(deprecated)]
        for &byte in s.as_bytes() {
            sbi_rt::legacy::console_putchar(byte as usize);
        }
        Ok(())
    }
}

// Host-side unit tests link against std, which provides its own panic
// handler.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Print through the SBI console directly: the panic may fire before
    // `ax_log` is initialized, in which case the `log` facade would swallow
    // the message silently.
    let mut writer = SbiConsoleWriter;
    let _ = writeln!(writer, "ArceBoot panicked: {}", info);
    ax_hal::power::system_off()
}
