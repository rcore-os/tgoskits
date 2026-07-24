use alloc::vec::Vec;

use axtest::prelude::*;
use irq_framework::{CpuId, IrqDomainId, IrqError, IrqId};

use crate::{
    DriverGeneric, Interface, IrqAffinity, Msi, MsiAllocation, MsiDeviceId, MsiEventId, MsiMessage,
    MsiProviderId, MsiRequest, MsiVector, MsiVectorIndex,
};

struct MockMsiProvider {
    count_delta: i16,
    enabled: bool,
    affinity: IrqAffinity,
    freed: bool,
}

impl MockMsiProvider {
    const fn new() -> Self {
        Self {
            count_delta: 0,
            enabled: false,
            affinity: IrqAffinity::Any,
            freed: false,
        }
    }
}

impl DriverGeneric for MockMsiProvider {
    fn name(&self) -> &str {
        "mock-msi"
    }
}

impl Interface for MockMsiProvider {
    fn allocate_vectors(&mut self, request: &MsiRequest) -> Result<Vec<MsiVector>, IrqError> {
        let count = usize::from(request.vector_count.saturating_add_signed(self.count_delta));
        let mut vectors = Vec::new();
        for index in 0..count {
            vectors.push(MsiVector::with_parent(
                MsiVectorIndex(index as u16),
                MsiEventId(100 + index as u32),
                IrqId::new(IrqDomainId(1), irq_framework::HwIrq(index as u32)),
                IrqId::new(IrqDomainId(9), irq_framework::HwIrq(index as u32)),
            ));
        }
        Ok(vectors)
    }

    fn compose_message(&self, vector: &MsiVector) -> Result<MsiMessage, IrqError> {
        Ok(MsiMessage::new(0xfee0_0000, vector.event.0))
    }

    fn set_vector_enabled(&mut self, _vector: &MsiVector, enabled: bool) -> Result<(), IrqError> {
        self.enabled = enabled;
        Ok(())
    }

    fn set_vector_affinity(
        &mut self,
        _vector: &MsiVector,
        affinity: IrqAffinity,
    ) -> Result<(), IrqError> {
        self.affinity = affinity;
        Ok(())
    }

    fn free_vectors(&mut self, _allocation: MsiAllocation) -> Result<(), IrqError> {
        self.freed = true;
        Ok(())
    }
}

#[axtest]
fn rdif_msi_plain_ids_vectors_and_requests_keep_fields() {
    let vector = MsiVector::with_parent(
        MsiVectorIndex(3),
        MsiEventId(7),
        IrqId::new(IrqDomainId(1), irq_framework::HwIrq(11)),
        IrqId::new(IrqDomainId(2), irq_framework::HwIrq(12)),
    );
    ax_assert_eq!(vector.index, MsiVectorIndex(3));
    ax_assert_eq!(vector.event, MsiEventId(7));
    ax_assert_eq!(vector.parent_irq.domain, IrqDomainId(2));
    ax_assert_eq!(
        MsiVector::new(MsiVectorIndex(4), MsiEventId(8), vector.irq).parent_irq,
        vector.irq
    );

    let request = MsiRequest::new(MsiDeviceId(9), 2).affinity(IrqAffinity::Fixed(CpuId(1)));
    ax_assert_eq!(request.device, MsiDeviceId(9));
    ax_assert_eq!(request.vector_count, 2);
    ax_assert_eq!(request.affinity, IrqAffinity::Fixed(CpuId(1)));

    let message = MsiMessage::new(0xfee0_0000, 0x45);
    ax_assert_eq!(message.address, 0xfee0_0000);
    ax_assert_eq!(message.data, 0x45);
}

#[axtest]
fn rdif_msi_allocation_preserves_provider_device_and_vectors() {
    let vectors = alloc::vec![
        MsiVector::new(
            MsiVectorIndex(0),
            MsiEventId(10),
            IrqId::new(IrqDomainId(1), irq_framework::HwIrq(0)),
        ),
        MsiVector::new(
            MsiVectorIndex(1),
            MsiEventId(11),
            IrqId::new(IrqDomainId(1), irq_framework::HwIrq(1)),
        ),
    ];
    let allocation =
        MsiAllocation::new(MsiProviderId(5), MsiDeviceId(6), vectors.into_boxed_slice());
    ax_assert_eq!(allocation.provider(), MsiProviderId(5));
    ax_assert_eq!(allocation.device(), MsiDeviceId(6));
    ax_assert_eq!(allocation.vectors().len(), 2);
    ax_assert_eq!(allocation.into_vectors().len(), 2);
}

#[axtest]
fn rdif_msi_wrapper_validates_counts_delegates_and_frees_allocations() {
    let mut msi = Msi::new(MsiProviderId(77), MockMsiProvider::new());
    ax_assert_eq!(msi.name(), "mock-msi");
    ax_assert_eq!(msi.provider(), MsiProviderId(77));
    ax_assert_eq!(
        msi.allocate(MsiRequest::new(MsiDeviceId(3), 0)),
        Err(IrqError::InvalidIrq)
    );

    let allocation = msi
        .allocate(MsiRequest::new(MsiDeviceId(3), 2).affinity(IrqAffinity::Fixed(CpuId(0))))
        .unwrap();
    ax_assert_eq!(allocation.vectors().len(), 2);
    let vector = allocation.vectors()[0];
    ax_assert_eq!(msi.compose_message(&vector).unwrap().data, 100);
    msi.set_vector_enabled(&vector, true).unwrap();
    msi.set_vector_affinity(&vector, IrqAffinity::Fixed(CpuId(2)))
        .unwrap();
    ax_assert!(msi.typed_ref::<MockMsiProvider>().unwrap().enabled);
    ax_assert_eq!(
        msi.typed_ref::<MockMsiProvider>().unwrap().affinity,
        IrqAffinity::Fixed(CpuId(2))
    );

    let wrong_provider = MsiAllocation::new(
        MsiProviderId(78),
        MsiDeviceId(3),
        alloc::vec![vector].into_boxed_slice(),
    );
    ax_assert_eq!(msi.free(wrong_provider), Err(IrqError::InvalidIrq));
    msi.free(allocation).unwrap();
    ax_assert!(msi.typed_ref::<MockMsiProvider>().unwrap().freed);
}

#[axtest]
fn rdif_msi_wrapper_rejects_driver_returning_wrong_vector_count() {
    let mut msi = Msi::new(
        MsiProviderId(88),
        MockMsiProvider {
            count_delta: -1,
            enabled: false,
            affinity: IrqAffinity::Any,
            freed: false,
        },
    );
    ax_assert_eq!(
        msi.allocate(MsiRequest::new(MsiDeviceId(9), 2)),
        Err(IrqError::InvalidIrq)
    );
}

#[axtest]
fn rdif_msi_type_constants_hold() {
    // MsiMessage::new
    let msg = MsiMessage::new(0xFEE00000, 0x1234);
    ax_assert_eq!(msg.address, 0xFEE00000);
    ax_assert_eq!(msg.data, 0x1234);
    
    // MsiVectorIndex
    let idx = MsiVectorIndex(42);
    ax_assert_eq!(idx.0, 42);
    
    // MsiEventId
    let evt = MsiEventId(10);
    ax_assert_eq!(evt.0, 10);
    
    // MsiDeviceId
    let dev = MsiDeviceId(5);
    ax_assert_eq!(dev.0, 5);
    
    // MsiProviderId
    let prov = MsiProviderId(99);
    ax_assert_eq!(prov.0, 99);
}

#[axtest]
fn rdif_msi_request_and_interface_default_hold() {
    use crate::{Interface, IrqError, MsiRequest};

    // Test MsiRequest::new with default affinity
    let request = MsiRequest::new(MsiDeviceId(1), 4);
    ax_assert_eq!(request.device, MsiDeviceId(1));
    ax_assert_eq!(request.vector_count, 4);
    // Default affinity is Any
    match request.affinity {
        IrqAffinity::Any => {}
        _ => panic!("Expected Any affinity"),
    }

    // Test MsiRequest::new with custom affinity
    let custom = MsiRequest::new(MsiDeviceId(2), 8).affinity(IrqAffinity::Fixed(CpuId(3)));
    ax_assert_eq!(custom.device, MsiDeviceId(2));
    ax_assert_eq!(custom.vector_count, 8);
    match custom.affinity {
        IrqAffinity::Fixed(cpu) => ax_assert_eq!(cpu, CpuId(3)),
        _ => panic!("Expected Fixed affinity"),
    }

    // Test Interface trait default implementations return Unsupported
    struct MinimalMsi;
    impl DriverGeneric for MinimalMsi {
        fn name(&self) -> &str { "minimal-msi" }
    }
    impl Interface for MinimalMsi {
        fn allocate_vectors(&mut self, _request: &MsiRequest) -> Result<Vec<MsiVector>, IrqError> {
            Ok(Vec::new())
        }
        fn compose_message(&self, _vector: &MsiVector) -> Result<MsiMessage, IrqError> {
            Ok(MsiMessage::new(0, 0))
        }
        fn free_vectors(&mut self, _allocation: MsiAllocation) -> Result<(), IrqError> {
            Ok(())
        }
    }

    let mut minimal = MinimalMsi;
    let vector = MsiVector::new(MsiVectorIndex(0), MsiEventId(0), IrqId::new(IrqDomainId(0), irq_framework::HwIrq(0)));
    ax_assert!(minimal.set_vector_enabled(&vector, true) == Err(IrqError::Unsupported));
    ax_assert!(minimal.set_vector_affinity(&vector, IrqAffinity::Any) == Err(IrqError::Unsupported));
}

#[axtest]
fn rdif_msi_message_and_allocation_hold() {
    use crate::{MsiAllocation, MsiDeviceId, MsiMessage, MsiProviderId, MsiVector, MsiVectorIndex, MsiEventId};

    // Test MsiMessage fields
    let msg = MsiMessage::new(0xfee0_0000, 0x1234);
    ax_assert_eq!(msg.address, 0xfee0_0000);
    ax_assert_eq!(msg.data, 0x1234);

    // Test MsiVector fields
    let vector = MsiVector::new(
        MsiVectorIndex(5),
        MsiEventId(42),
        IrqId::new(IrqDomainId(1), irq_framework::HwIrq(7)),
    );
    ax_assert_eq!(vector.index.0, 5);
    ax_assert_eq!(vector.event.0, 42);

    // Test MsiAllocation with actual constructor
    let alloc = MsiAllocation::new(
        MsiProviderId(2),
        MsiDeviceId(1),
        alloc::vec![MsiVector::new(MsiVectorIndex(10), MsiEventId(0), IrqId::new(IrqDomainId(0), irq_framework::HwIrq(0)))].into_boxed_slice(),
    );
    ax_assert_eq!(alloc.provider().0, 2);
    ax_assert_eq!(alloc.device.0, 1);
}
