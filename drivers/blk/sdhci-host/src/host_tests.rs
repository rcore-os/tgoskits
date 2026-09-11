//! Tests for host construction and platform capability hooks.

use core::{
    ptr::NonNull,
    sync::atomic::{AtomicU8, Ordering},
};

use super::*;

#[test]
fn reset_all_calls_owned_platform_before_hook_before_software_reset() {
    struct Hook;
    static OBSERVED_RESET: AtomicU8 = AtomicU8::new(u8::MAX);

    impl HostResetHook for Hook {
        fn before_reset_all(&self, host: &mut Sdhci) -> Result<(), Error> {
            OBSERVED_RESET.store(host.read_u8(REG_SOFTWARE_RESET), Ordering::Release);
            Ok(())
        }

        fn after_reset(&self, _host: &mut Sdhci) -> Result<(), Error> {
            Ok(())
        }
    }

    let mut mmio = [0u8; 256];
    let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.set_reset_hook(Hook);

    assert!(host.reset_all().is_err());

    assert_eq!(OBSERVED_RESET.load(Ordering::Acquire), 0);
}

#[test]
fn interrupt_status_capture_keeps_signal_irq_masked() {
    let mut mmio = [0u8; 256];
    let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.write_u16(REG_NORMAL_INT_STATUS_ENABLE, 0);
    host.write_u16(REG_ERROR_INT_STATUS_ENABLE, 0);
    host.write_u16(REG_NORMAL_INT_SIGNAL_ENABLE, NORMAL_INT_CLEAR_ALL);
    host.write_u16(REG_ERROR_INT_SIGNAL_ENABLE, ERROR_INT_CLEAR_ALL);

    host.enable_interrupt_status_capture();
    assert_eq!(
        host.read_u16(REG_NORMAL_INT_STATUS_ENABLE),
        NORMAL_INT_CLEAR_ALL
    );
    assert_eq!(
        host.read_u16(REG_ERROR_INT_STATUS_ENABLE),
        ERROR_INT_CLEAR_ALL
    );
    assert_eq!(host.read_u16(REG_NORMAL_INT_SIGNAL_ENABLE), 0);
    assert_eq!(host.read_u16(REG_ERROR_INT_SIGNAL_ENABLE), 0);
}
