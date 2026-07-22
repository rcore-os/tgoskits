#![no_std]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::ops::{Deref, DerefMut};

pub use irq_framework::{IrqAffinity, IrqError, IrqId};
pub use rdif_base::DriverGeneric;

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MsiProviderId(pub u64);

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsiDeviceId(pub u32);

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct MsiEventId(pub u32);

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct MsiVectorIndex(pub u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsiMessage {
    pub address: u64,
    pub data: u32,
}

impl MsiMessage {
    pub const fn new(address: u64, data: u32) -> Self {
        Self { address, data }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsiVector {
    pub index: MsiVectorIndex,
    pub event: MsiEventId,
    /// IRQ registered by the device driver.
    pub irq: IrqId,
    /// IRQ owned by the parent MSI interrupt controller.
    pub parent_irq: IrqId,
}

impl MsiVector {
    pub const fn new(index: MsiVectorIndex, event: MsiEventId, irq: IrqId) -> Self {
        Self {
            index,
            event,
            irq,
            parent_irq: irq,
        }
    }

    pub const fn with_parent(
        index: MsiVectorIndex,
        event: MsiEventId,
        irq: IrqId,
        parent_irq: IrqId,
    ) -> Self {
        Self {
            index,
            event,
            irq,
            parent_irq,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MsiAllocation {
    provider: MsiProviderId,
    device: MsiDeviceId,
    vectors: Box<[MsiVector]>,
}

impl MsiAllocation {
    pub fn new(provider: MsiProviderId, device: MsiDeviceId, vectors: Box<[MsiVector]>) -> Self {
        Self {
            provider,
            device,
            vectors,
        }
    }

    pub const fn provider(&self) -> MsiProviderId {
        self.provider
    }

    pub const fn device(&self) -> MsiDeviceId {
        self.device
    }

    pub fn vectors(&self) -> &[MsiVector] {
        &self.vectors
    }

    pub fn into_vectors(self) -> Box<[MsiVector]> {
        self.vectors
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsiRequest {
    pub device: MsiDeviceId,
    pub vector_count: u16,
    pub affinity: IrqAffinity,
}

impl MsiRequest {
    pub const fn new(device: MsiDeviceId, vector_count: u16) -> Self {
        Self {
            device,
            vector_count,
            affinity: IrqAffinity::Any,
        }
    }

    pub const fn affinity(mut self, affinity: IrqAffinity) -> Self {
        self.affinity = affinity;
        self
    }
}

/// Requests one exact MSI translation from a parent interrupt controller.
///
/// Unlike [`MsiRequest`], this request does not permit the provider to choose
/// the event or parent interrupt. It is intended for resource-assignment
/// boundaries such as a VMM that has already partitioned ITS/LPI ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsiReservationRequest {
    device: MsiDeviceId,
    index: MsiVectorIndex,
    event: MsiEventId,
    parent_irq: IrqId,
    affinity: IrqAffinity,
}

impl MsiReservationRequest {
    /// Creates an exact reservation request with provider-selected affinity.
    pub const fn new(
        device: MsiDeviceId,
        index: MsiVectorIndex,
        event: MsiEventId,
        parent_irq: IrqId,
    ) -> Self {
        Self {
            device,
            index,
            event,
            parent_irq,
            affinity: IrqAffinity::Any,
        }
    }

    /// Requests delivery to a particular CPU.
    pub const fn affinity(mut self, affinity: IrqAffinity) -> Self {
        self.affinity = affinity;
        self
    }

    /// Returns the MSI-producing device identifier.
    pub const fn device(self) -> MsiDeviceId {
        self.device
    }

    /// Returns the device-local vector index.
    pub const fn index(self) -> MsiVectorIndex {
        self.index
    }

    /// Returns the exact device event identifier.
    pub const fn event(self) -> MsiEventId {
        self.event
    }

    /// Returns the exact parent-controller interrupt.
    pub const fn parent_irq(self) -> IrqId {
        self.parent_irq
    }

    /// Returns the requested delivery affinity.
    pub const fn requested_affinity(self) -> IrqAffinity {
        self.affinity
    }
}

pub trait Interface: DriverGeneric {
    fn allocate_vectors(&mut self, request: &MsiRequest) -> Result<Vec<MsiVector>, IrqError>;

    /// Reserves one caller-selected event and parent interrupt.
    fn reserve_vector(&mut self, _request: &MsiReservationRequest) -> Result<MsiVector, IrqError> {
        Err(IrqError::Unsupported)
    }

    fn compose_message(&self, vector: &MsiVector) -> Result<MsiMessage, IrqError>;

    fn set_vector_enabled(&mut self, _vector: &MsiVector, _enabled: bool) -> Result<(), IrqError> {
        Err(IrqError::Unsupported)
    }

    fn set_vector_affinity(
        &mut self,
        _vector: &MsiVector,
        _affinity: IrqAffinity,
    ) -> Result<(), IrqError> {
        Err(IrqError::Unsupported)
    }

    fn free_vectors(&mut self, allocation: MsiAllocation) -> Result<(), IrqError>;
}

pub struct Msi {
    provider: MsiProviderId,
    inner: Box<dyn Interface>,
}

impl Msi {
    pub fn new<T: Interface>(provider: MsiProviderId, driver: T) -> Self {
        Self {
            provider,
            inner: Box::new(driver),
        }
    }

    pub const fn provider(&self) -> MsiProviderId {
        self.provider
    }

    pub fn allocate(&mut self, request: MsiRequest) -> Result<MsiAllocation, IrqError> {
        if request.vector_count == 0 {
            return Err(IrqError::InvalidIrq);
        }
        let vectors = self.inner.allocate_vectors(&request)?;
        if vectors.len() != usize::from(request.vector_count) {
            return Err(IrqError::InvalidIrq);
        }
        Ok(MsiAllocation::new(
            self.provider,
            request.device,
            vectors.into_boxed_slice(),
        ))
    }

    /// Reserves one exact device event and parent-controller interrupt.
    ///
    /// # Errors
    ///
    /// Returns [`IrqError::Unsupported`] when the provider cannot reserve
    /// explicit resources, or [`IrqError::InvalidIrq`] when the provider
    /// returns a different event, vector index, or parent interrupt.
    pub fn reserve(&mut self, request: MsiReservationRequest) -> Result<MsiAllocation, IrqError> {
        let vector = self.inner.reserve_vector(&request)?;
        if vector.index != request.index()
            || vector.event != request.event()
            || vector.parent_irq != request.parent_irq()
        {
            return Err(IrqError::InvalidIrq);
        }
        Ok(MsiAllocation::new(
            self.provider,
            request.device(),
            Box::new([vector]),
        ))
    }

    pub fn compose_message(&self, vector: &MsiVector) -> Result<MsiMessage, IrqError> {
        self.inner.compose_message(vector)
    }

    pub fn set_vector_enabled(
        &mut self,
        vector: &MsiVector,
        enabled: bool,
    ) -> Result<(), IrqError> {
        self.inner.set_vector_enabled(vector, enabled)
    }

    pub fn set_vector_affinity(
        &mut self,
        vector: &MsiVector,
        affinity: IrqAffinity,
    ) -> Result<(), IrqError> {
        self.inner.set_vector_affinity(vector, affinity)
    }

    pub fn free(&mut self, allocation: MsiAllocation) -> Result<(), IrqError> {
        if allocation.provider != self.provider {
            return Err(IrqError::InvalidIrq);
        }
        self.inner.free_vectors(allocation)
    }

    pub fn typed_ref<T: Interface>(&self) -> Option<&T> {
        self.raw_any()?.downcast_ref()
    }

    pub fn typed_mut<T: Interface>(&mut self) -> Option<&mut T> {
        self.raw_any_mut()?.downcast_mut()
    }
}

impl DriverGeneric for Msi {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(self.inner.as_ref() as &dyn core::any::Any)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self.inner.as_mut() as &mut dyn core::any::Any)
    }
}

impl Deref for Msi {
    type Target = dyn Interface;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref()
    }
}

impl DerefMut for Msi {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, vec::Vec};
    use core::cell::RefCell;

    extern crate alloc;

    use irq_framework::{HwIrq, IrqDomainId, IrqError, IrqId};
    use rdif_base::DriverGeneric;

    use crate::{
        Interface, Msi, MsiAllocation, MsiDeviceId, MsiEventId, MsiMessage, MsiProviderId,
        MsiRequest, MsiReservationRequest, MsiVector, MsiVectorIndex,
    };

    struct MockProvider {
        freed: RefCell<Vec<MsiAllocation>>,
    }

    impl DriverGeneric for MockProvider {
        fn name(&self) -> &str {
            "mock-msi"
        }
    }

    impl Interface for MockProvider {
        fn allocate_vectors(&mut self, request: &MsiRequest) -> Result<Vec<MsiVector>, IrqError> {
            Ok((0..request.vector_count)
                .map(|index| {
                    MsiVector::new(
                        MsiVectorIndex(index),
                        MsiEventId(32 + u32::from(index)),
                        IrqId::new(IrqDomainId(7), HwIrq(8192 + u32::from(index))),
                    )
                })
                .collect())
        }

        fn compose_message(&self, vector: &MsiVector) -> Result<MsiMessage, IrqError> {
            Ok(MsiMessage::new(0x0808_0000, vector.event.0))
        }

        fn reserve_vector(
            &mut self,
            request: &MsiReservationRequest,
        ) -> Result<MsiVector, IrqError> {
            Ok(MsiVector::new(
                request.index(),
                request.event(),
                request.parent_irq(),
            ))
        }

        fn free_vectors(&mut self, allocation: MsiAllocation) -> Result<(), IrqError> {
            self.freed.borrow_mut().push(allocation);
            Ok(())
        }
    }

    #[test]
    fn allocation_records_provider_device_and_vectors() {
        let provider = MsiProviderId(3);
        let mut msi = Msi::new(
            provider,
            MockProvider {
                freed: RefCell::new(Vec::new()),
            },
        );

        let allocation = msi
            .allocate(MsiRequest::new(MsiDeviceId(0x1234), 2))
            .unwrap();

        assert_eq!(allocation.provider(), provider);
        assert_eq!(allocation.device(), MsiDeviceId(0x1234));
        assert_eq!(allocation.vectors().len(), 2);
        assert_eq!(allocation.vectors()[1].index, MsiVectorIndex(1));
        assert_eq!(
            allocation.vectors()[1].parent_irq,
            allocation.vectors()[1].irq
        );
        assert_eq!(
            msi.compose_message(&allocation.vectors()[1]).unwrap(),
            MsiMessage::new(0x0808_0000, 33)
        );
    }

    #[test]
    fn vector_can_expose_leaf_irq_while_remembering_parent_irq() {
        let parent_irq = IrqId::new(IrqDomainId(7), HwIrq(8192));
        let leaf_irq = IrqId::new(IrqDomainId(8), HwIrq(0));

        let vector = MsiVector::with_parent(MsiVectorIndex(0), MsiEventId(4), leaf_irq, parent_irq);

        assert_eq!(vector.irq, leaf_irq);
        assert_eq!(vector.parent_irq, parent_irq);
    }

    #[test]
    fn explicit_reservation_preserves_requested_identity() {
        let mut msi = Msi::new(
            MsiProviderId(3),
            MockProvider {
                freed: RefCell::new(Vec::new()),
            },
        );
        let parent_irq = IrqId::new(IrqDomainId(7), HwIrq(8201));
        let request = MsiReservationRequest::new(
            MsiDeviceId(0x1234),
            MsiVectorIndex(5),
            MsiEventId(17),
            parent_irq,
        )
        .affinity(irq_framework::IrqAffinity::Fixed(irq_framework::CpuId(2)));

        let allocation = msi.reserve(request).unwrap();

        assert_eq!(allocation.device(), MsiDeviceId(0x1234));
        assert_eq!(allocation.vectors().len(), 1);
        assert_eq!(allocation.vectors()[0].index, MsiVectorIndex(5));
        assert_eq!(allocation.vectors()[0].event, MsiEventId(17));
        assert_eq!(allocation.vectors()[0].parent_irq, parent_irq);
    }

    #[test]
    fn freeing_allocation_rejects_wrong_provider() {
        let mut msi = Msi::new(
            MsiProviderId(7),
            MockProvider {
                freed: RefCell::new(Vec::new()),
            },
        );
        let wrong = MsiAllocation::new(
            MsiProviderId(8),
            MsiDeviceId(1),
            Box::new([MsiVector::new(
                MsiVectorIndex(0),
                MsiEventId(4),
                IrqId::new(IrqDomainId(7), HwIrq(8192)),
            )]),
        );

        assert_eq!(msi.free(wrong), Err(IrqError::InvalidIrq));
    }
}
