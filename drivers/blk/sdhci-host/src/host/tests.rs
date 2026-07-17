use core::ptr::NonNull;

use super::*;

#[test]
fn constructs_from_mapped_mmio_pointer() {
    let base = NonNull::new(0x1000_0000 as *mut u8).unwrap();
    let host = unsafe { Sdhci::new(base) };

    assert_eq!(host.base_addr, 0x1000_0000);
}

#[test]
fn legacy_addr_constructor_keeps_raw_mmio_boundary_explicit() {
    let host = unsafe { Sdhci::new_from_addr(0x1000_0000) };

    assert_eq!(host.base_addr, 0x1000_0000);
}

#[test]
fn external_clock_can_be_scoped_and_cleared() {
    struct Clock;

    impl HostClock for Clock {
        fn set_clock(&self, _target_hz: u32) -> Result<(), Error> {
            Ok(())
        }
    }

    let mut mmio = [0u8; 256];
    let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };

    host.set_external_clock(Clock);
    assert!(host.ext_clock.is_some());

    host.clear_external_clock();
    assert!(host.ext_clock.is_none());
}

#[test]
fn initialization_status_mode_keeps_signal_irq_masked() {
    let mut mmio = [0u8; 256];
    let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.write_u16(REG_NORMAL_INT_STATUS_ENABLE, 0);
    host.write_u16(REG_ERROR_INT_STATUS_ENABLE, 0);
    host.write_u16(REG_NORMAL_INT_SIGNAL_ENABLE, NORMAL_INT_CLEAR_ALL);
    host.write_u16(REG_ERROR_INT_SIGNAL_ENABLE, ERROR_INT_CLEAR_ALL);

    host.enable_initialization_status().unwrap();
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
    assert!(host.initialization_status_owned());
}

#[test]
fn initialization_status_cannot_reclaim_runtime_irq_ownership() {
    let mut mmio = [0u8; 256];
    let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };
    host.enable_completion_irq();
    host.disable_completion_irq();

    assert_eq!(host.enable_initialization_status(), Err(Error::Busy));
    assert!(host.runtime_irq_status_owned());
}

#[test]
fn masking_runtime_delivery_does_not_transfer_status_ownership() {
    let mut mmio = [0u8; 256];
    let base = NonNull::new(mmio.as_mut_ptr()).unwrap();
    let mut host = unsafe { Sdhci::new(base) };

    host.enable_completion_irq();
    host.disable_completion_irq();

    assert!(!host.completion_irq_enabled());
    assert!(host.runtime_irq_status_owned());
}
