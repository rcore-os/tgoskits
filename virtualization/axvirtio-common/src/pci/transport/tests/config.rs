use core::cell::Cell;

use axdevice_base::{AccessWidth, DeviceError};

use super::{super::*, fixtures::*};
use crate::constants::{
    VIRTIO_F_VERSION_1, VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DEVICE_NEEDS_RESET,
    VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};

#[test]
fn msix_vector_registers_remain_unmapped_without_msix_capability() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };

    for offset in [MSIX_CONFIG, QUEUE_MSIX_VECTOR] {
        write(&transport, offset, AccessWidth::Word, 7, &mut memory);
        assert_eq!(
            transport
                .read_mmio(offset, AccessWidth::Word)
                .expect("MSI-X vector register should be readable"),
            u16::MAX as u64
        );
    }
}

#[test]
fn device_status_accepts_cumulative_phases_but_rejects_clearing_bits() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };

    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK) as u64,
        &mut memory,
    );
    assert_eq!(transport.status(), 0x0b);

    assert!(matches!(
        transport.write_mmio_with_dma(DEVICE_STATUS, AccessWidth::Byte, 3, true, &mut memory,),
        Err(DeviceError::InvalidState { .. })
    ));
    assert_eq!(transport.status(), 0x0b);
}

#[test]
fn driver_features_are_frozen_after_features_ok() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };
    for status in [1, 3] {
        write(
            &transport,
            DEVICE_STATUS,
            AccessWidth::Byte,
            status,
            &mut memory,
        );
    }
    write(
        &transport,
        DRIVER_FEATURE_SELECT,
        AccessWidth::Dword,
        1,
        &mut memory,
    );
    write(
        &transport,
        DRIVER_FEATURE,
        AccessWidth::Dword,
        VIRTIO_F_VERSION_1 >> 32,
        &mut memory,
    );
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0b,
        &mut memory,
    );

    assert!(matches!(
        transport.write_mmio_with_dma(
            DRIVER_FEATURE_SELECT,
            AccessWidth::Dword,
            0,
            true,
            &mut memory,
        ),
        Err(DeviceError::InvalidState { .. })
    ));
    assert!(matches!(
        transport.write_mmio_with_dma(DRIVER_FEATURE, AccessWidth::Dword, 0, true, &mut memory,),
        Err(DeviceError::InvalidState { .. })
    ));
    assert_eq!(transport.driver_features(), VIRTIO_F_VERSION_1);
}

#[test]
fn unsupported_driver_features_fail_negotiation_without_driver_ok() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };
    acknowledge_driver(&transport, &mut memory);
    write(
        &transport,
        DRIVER_FEATURE_SELECT,
        AccessWidth::Dword,
        0,
        &mut memory,
    );
    write(
        &transport,
        DRIVER_FEATURE,
        AccessWidth::Dword,
        1,
        &mut memory,
    );
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK) as u64,
        &mut memory,
    );

    let status = transport.status();
    assert_eq!(
        status,
        (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER) as u8
    );
    assert_eq!(
        status & (VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK) as u8,
        0
    );
}

#[test]
fn queue_programming_does_not_probe_guest_memory() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };

    acknowledge_driver(&transport, &mut memory);
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0b,
        &mut memory,
    );
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0f,
        &mut memory,
    );
    write(
        &transport,
        QUEUE_DESC,
        AccessWidth::Qword,
        0x1000,
        &mut memory,
    );
    write(
        &transport,
        QUEUE_DRIVER,
        AccessWidth::Qword,
        0x2000,
        &mut memory,
    );
    write(
        &transport,
        QUEUE_DEVICE,
        AccessWidth::Qword,
        0x3000,
        &mut memory,
    );
    write(&transport, QUEUE_ENABLE, AccessWidth::Word, 1, &mut memory);

    assert_eq!(memory.reads.get(), 0);
    assert_eq!(transport.status(), 0x0f);
}

#[test]
fn queue_addresses_accept_linux_split_dword_accesses() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };

    for (offset, address) in [
        (QUEUE_DESC, 0x0000_0001_0000_1000_u64),
        (QUEUE_DRIVER, 0x0000_0002_0000_2000_u64),
        (QUEUE_DEVICE, 0x0000_0003_0000_3000_u64),
    ] {
        write(
            &transport,
            offset,
            AccessWidth::Dword,
            address & u32::MAX as u64,
            &mut memory,
        );
        write(
            &transport,
            offset + 4,
            AccessWidth::Dword,
            address >> 32,
            &mut memory,
        );

        assert_eq!(
            transport
                .read_mmio(offset, AccessWidth::Dword)
                .expect("queue address low dword should be readable"),
            address & u32::MAX as u64
        );
        assert_eq!(
            transport
                .read_mmio(offset + 4, AccessWidth::Dword)
                .expect("queue address high dword should be readable"),
            address >> 32
        );
        assert_eq!(
            transport
                .read_mmio(offset, AccessWidth::Qword)
                .expect("queue address should retain qword compatibility"),
            address
        );
    }

    assert_eq!(memory.reads.get(), 0);
}

#[test]
fn queue_enable_rejects_an_unconfigured_layout() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };

    assert!(matches!(
        transport.write_mmio_with_dma(QUEUE_ENABLE, AccessWidth::Word, 1, false, &mut memory,),
        Err(DeviceError::InvalidData { .. })
    ));
    assert_eq!(
        transport
            .read_mmio(QUEUE_ENABLE, AccessWidth::Word)
            .expect("queue enable read should succeed"),
        0
    );
    assert_eq!(memory.reads.get(), 0);
}

#[test]
fn disabled_dma_stops_notify_before_guest_memory_access() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };
    acknowledge_driver(&transport, &mut memory);
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0b,
        &mut memory,
    );
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0f,
        &mut memory,
    );
    write(
        &transport,
        QUEUE_DESC,
        AccessWidth::Qword,
        0x1000,
        &mut memory,
    );
    write(
        &transport,
        QUEUE_DRIVER,
        AccessWidth::Qword,
        0x2000,
        &mut memory,
    );
    write(
        &transport,
        QUEUE_DEVICE,
        AccessWidth::Qword,
        0x3000,
        &mut memory,
    );
    write(&transport, QUEUE_ENABLE, AccessWidth::Word, 1, &mut memory);

    let outcome = transport
        .write_mmio_with_dma(
            NOTIFY_CONFIG_OFFSET,
            AccessWidth::Word,
            0,
            false,
            &mut memory,
        )
        .expect("disabled-DMA notify should be accepted as a stopped queue");
    let VirtioPciWriteOutcome::QueueNotified(notification) = outcome else {
        panic!("expected queue notification");
    };
    assert_eq!(notification.outcome(), QueueNotifyOutcome::Idle);
    notification.complete();
    assert_eq!(memory.reads.get(), 0);
}

#[test]
fn try_new_rejects_invalid_core_configuration_without_panicking() {
    assert!(matches!(
        VirtioPciTransport::try_new(InvalidCore {
            queue_num_max: 0,
            queue_size_max: 8,
            deferred: false,
        }),
        Err(DeviceError::InvalidInput { .. })
    ));
    assert!(matches!(
        VirtioPciTransport::try_new(InvalidCore {
            queue_num_max: 1,
            queue_size_max: 3,
            deferred: false,
        }),
        Err(DeviceError::InvalidInput { .. })
    ));
    assert!(matches!(
        VirtioPciTransport::try_new(InvalidCore {
            queue_num_max: 1,
            queue_size_max: 8,
            deferred: true,
        }),
        Err(DeviceError::Unsupported { .. })
    ));
    assert!(matches!(
        VirtioPciTransport::try_new(InvalidCore {
            queue_num_max: 2,
            queue_size_max: 8,
            deferred: false,
        }),
        Err(DeviceError::InvalidInput { .. })
    ));
}

#[test]
fn queue_fault_status_cannot_be_cleared_by_nonzero_status_write() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut configuration_memory = TestMemory {
        reads: Cell::new(0),
    };
    acknowledge_driver(&transport, &mut configuration_memory);
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0b,
        &mut configuration_memory,
    );
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0f,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DESC,
        AccessWidth::Qword,
        0x1000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DRIVER,
        AccessWidth::Qword,
        0x2000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DEVICE,
        AccessWidth::Qword,
        0x3000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_ENABLE,
        AccessWidth::Word,
        1,
        &mut configuration_memory,
    );

    let mut failing_memory = FailingMemory;
    let outcome = transport
        .write_mmio_with_dma(
            NOTIFY_CONFIG_OFFSET,
            AccessWidth::Word,
            0,
            true,
            &mut failing_memory,
        )
        .expect("queue faults are returned as an outcome");
    let VirtioPciWriteOutcome::Fault { .. } = outcome else {
        panic!("expected a queue fault");
    };

    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0f,
        &mut configuration_memory,
    );
    assert_ne!(
        transport.status() & VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8,
        0
    );
}
