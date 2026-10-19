//! Shared access to the opaque raw driver's transport for confirmed reset.
use alloc::sync::Arc;

use ax_sync::RawSpinLock;
use virtio_drivers::{
    PhysAddr, Result,
    transport::{DeviceStatus, DeviceType, InterruptStatus, Transport},
};
use zerocopy::{FromBytes, Immutable, IntoBytes};

/// All accesses remain inside the enclosing NetInner task/IRQ gate. The lock
/// also serializes the reset handle with the transport owned by VirtIONetRaw.
pub(super) struct ResettableTransport<T>(Arc<RawSpinLock<T>>);

impl<T: Transport> ResettableTransport<T> {
    pub(super) fn new(transport: T) -> Self {
        Self(Arc::new(RawSpinLock::new(transport)))
    }

    pub(super) fn reset(&self) -> bool {
        let mut transport = self.0.lock_irqsave();
        transport.set_status(DeviceStatus::empty());
        // VirtIO 1.2 sections 2.4 and 4.1.4.3: status zero acknowledges reset;
        // the device no longer accesses queues until initialization restarts.
        transport.get_status().is_empty()
    }
}

impl<T> Clone for ResettableTransport<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: Transport> Transport for ResettableTransport<T> {
    fn device_type(&self) -> DeviceType {
        self.0.lock_irqsave().device_type()
    }
    fn read_device_features(&mut self) -> u64 {
        self.0.lock_irqsave().read_device_features()
    }
    fn write_driver_features(&mut self, features: u64) {
        self.0.lock_irqsave().write_driver_features(features);
    }
    fn max_queue_size(&mut self, queue: u16) -> u32 {
        self.0.lock_irqsave().max_queue_size(queue)
    }
    fn notify(&mut self, queue: u16) {
        self.0.lock_irqsave().notify(queue);
    }
    fn get_status(&self) -> DeviceStatus {
        self.0.lock_irqsave().get_status()
    }
    fn set_status(&mut self, status: DeviceStatus) {
        self.0.lock_irqsave().set_status(status);
    }
    fn set_guest_page_size(&mut self, size: u32) {
        self.0.lock_irqsave().set_guest_page_size(size);
    }
    fn requires_legacy_layout(&self) -> bool {
        self.0.lock_irqsave().requires_legacy_layout()
    }
    fn queue_set(
        &mut self,
        queue: u16,
        size: u32,
        descriptors: PhysAddr,
        driver_area: PhysAddr,
        device_area: PhysAddr,
    ) {
        self.0
            .lock_irqsave()
            .queue_set(queue, size, descriptors, driver_area, device_area);
    }
    fn queue_unset(&mut self, queue: u16) {
        self.0.lock_irqsave().queue_unset(queue);
    }
    fn queue_used(&mut self, queue: u16) -> bool {
        self.0.lock_irqsave().queue_used(queue)
    }
    fn ack_interrupt(&mut self) -> InterruptStatus {
        self.0.lock_irqsave().ack_interrupt()
    }
    fn read_config_generation(&self) -> u32 {
        self.0.lock_irqsave().read_config_generation()
    }
    fn read_config_space<U: FromBytes + IntoBytes>(&self, offset: usize) -> Result<U> {
        self.0.lock_irqsave().read_config_space(offset)
    }
    fn write_config_space<U: IntoBytes + Immutable>(
        &mut self,
        offset: usize,
        value: U,
    ) -> Result<()> {
        self.0.lock_irqsave().write_config_space(offset, value)
    }
}
