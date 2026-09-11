use core::cell::Cell;

use axdevice_base::{AccessWidth, DeviceError};

use super::{super::*, fixtures::*};

#[test]
fn rejects_accesses_that_cross_transport_regions() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };

    assert!(matches!(
        transport.read_mmio(COMMON_CONFIG_SIZE - 1, AccessWidth::Word),
        Err(DeviceError::OutOfRange { .. })
    ));
    assert!(matches!(
        transport.read_mmio(DEVICE_CONFIG_OFFSET + 3, AccessWidth::Dword),
        Err(DeviceError::OutOfRange { .. })
    ));
    assert!(matches!(
        transport.write_mmio_with_dma(
            NOTIFY_CONFIG_OFFSET + 1,
            AccessWidth::Word,
            0,
            true,
            &mut memory,
        ),
        Err(DeviceError::OutOfRange { .. })
    ));
    assert!(matches!(
        transport.write_mmio_with_dma(
            NOTIFY_CONFIG_OFFSET,
            AccessWidth::Dword,
            0,
            true,
            &mut memory,
        ),
        Err(DeviceError::InvalidWidth { .. })
    ));
}
