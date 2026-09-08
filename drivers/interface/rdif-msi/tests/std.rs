extern crate alloc;

use alloc::vec::Vec;

use irq_framework::{CpuId, IrqDomainId, IrqError, IrqId};
use rdif_msi::{
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

#[test]
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
    assert_eq!(allocation.provider(), MsiProviderId(5));
    assert_eq!(allocation.device(), MsiDeviceId(6));
    assert_eq!(allocation.vectors().len(), 2);
    assert_eq!(allocation.into_vectors().len(), 2);
}

#[test]
fn rdif_msi_wrapper_validates_counts_delegates_and_frees_allocations() {
    let mut msi = Msi::new(MsiProviderId(77), MockMsiProvider::new());
    assert_eq!(msi.name(), "mock-msi");
    assert_eq!(msi.provider(), MsiProviderId(77));
    assert_eq!(
        msi.allocate(MsiRequest::new(MsiDeviceId(3), 0)),
        Err(IrqError::InvalidIrq)
    );

    let allocation = msi
        .allocate(MsiRequest::new(MsiDeviceId(3), 2).affinity(IrqAffinity::Fixed(CpuId(0))))
        .unwrap();
    assert_eq!(allocation.vectors().len(), 2);
    let vector = allocation.vectors()[0];
    assert_eq!(msi.compose_message(&vector).unwrap().data, 100);
    msi.set_vector_enabled(&vector, true).unwrap();
    msi.set_vector_affinity(&vector, IrqAffinity::Fixed(CpuId(2)))
        .unwrap();
    assert!(msi.typed_ref::<MockMsiProvider>().unwrap().enabled);
    assert_eq!(
        msi.typed_ref::<MockMsiProvider>().unwrap().affinity,
        IrqAffinity::Fixed(CpuId(2))
    );

    let wrong_provider = MsiAllocation::new(
        MsiProviderId(78),
        MsiDeviceId(3),
        alloc::vec![vector].into_boxed_slice(),
    );
    assert_eq!(msi.free(wrong_provider), Err(IrqError::InvalidIrq));
    msi.free(allocation).unwrap();
    assert!(msi.typed_ref::<MockMsiProvider>().unwrap().freed);
}

#[test]
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
    assert_eq!(
        msi.allocate(MsiRequest::new(MsiDeviceId(9), 2)),
        Err(IrqError::InvalidIrq)
    );
}

#[test]
fn rdif_msi_unsupported_operations_report_errors() {
    use rdif_msi::{Interface, IrqError, MsiRequest};

    // Test Interface trait default implementations return Unsupported
    struct MinimalMsi;
    impl DriverGeneric for MinimalMsi {
        fn name(&self) -> &str {
            "minimal-msi"
        }
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
    let vector = MsiVector::new(
        MsiVectorIndex(0),
        MsiEventId(0),
        IrqId::new(IrqDomainId(0), irq_framework::HwIrq(0)),
    );
    assert!(minimal.set_vector_enabled(&vector, true) == Err(IrqError::Unsupported));
    assert!(minimal.set_vector_affinity(&vector, IrqAffinity::Any) == Err(IrqError::Unsupported));
}
