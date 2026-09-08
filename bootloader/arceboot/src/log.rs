//! Logger implementation for ArceBoot, publishing directly to the SBI
//! console. The runtime has no scheduling or IRQ infrastructure, so the
//! records are formatted and written inline.

struct LogIfImpl;

/// Formats `fmt::Arguments` to the SBI console without allocation.
struct ConsoleWriter;

impl core::fmt::Write for ConsoleWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        // The SBI legacy console is used instead of the platform UART: some
        // SBI firmware (e.g. RustSBI Prototyper) denies S-mode access to the
        // UART MMIO region in its PMP configuration.
        #[allow(deprecated)]
        for &byte in s.as_bytes() {
            sbi_rt::legacy::console_putchar(byte as usize);
        }
        Ok(())
    }
}

#[ax_crate_interface::impl_interface]
impl ax_log::LogIf for LogIfImpl {
    fn try_publish(
        _meta: ax_log::RecordMeta,
        args: core::fmt::Arguments<'_>,
    ) -> ax_log::PublishStatus {
        let mut writer = ConsoleWriter;
        if core::fmt::write(&mut writer, args).is_ok() {
            ax_log::PublishStatus::Published
        } else {
            ax_log::PublishStatus::Dropped
        }
    }

    fn emergency_write(args: core::fmt::Arguments<'_>) -> usize {
        let mut writer = ConsoleWriter;
        core::fmt::write(&mut writer, args).map_or(0, |_| 1)
    }
}
