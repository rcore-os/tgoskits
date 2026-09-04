use core::cell::Cell;
use std::sync::{
    Arc as StdArc, Barrier,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use axvm_types::GuestPhysAddr;

use super::*;
use crate::{VirtioResult, constants::VIRTIO_F_VERSION_1};

pub(crate) struct TestCore;

impl VirtioDeviceCore for TestCore {
    fn device_type(&self) -> VirtioDeviceID {
        VirtioDeviceID::Block
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1
    }

    fn queue_size_max(&self) -> u16 {
        8
    }

    fn device_config_size(&self) -> u32 {
        4
    }

    fn read_device_config(&self, offset: u64, width: AccessWidth) -> DeviceResult<u64> {
        require_width(width, AccessWidth::Dword)?;
        (offset == 0)
            .then_some(0x1234_5678)
            .ok_or(DeviceError::OutOfRange { addr: offset })
    }

    fn write_device_config(&self, offset: u64, width: AccessWidth, _value: u64) -> DeviceResult {
        require_width(width, AccessWidth::Dword)?;
        (offset == 0)
            .then_some(())
            .ok_or(DeviceError::OutOfRange { addr: offset })
    }

    fn notify_queue(
        &self,
        _queue: &mut VirtioQueue<NoGuestMemoryAccessor>,
        _memory: &mut dyn GuestMemory,
    ) -> DeviceResult<QueueNotifyOutcome> {
        Ok(QueueNotifyOutcome::Idle)
    }
}

pub(crate) struct BlockingNotifyCore {
    pub(crate) entered: StdArc<Barrier>,
    pub(crate) release: StdArc<Barrier>,
}

pub(crate) struct CountingNotifyCore {
    pub(crate) notify_calls: StdArc<AtomicUsize>,
}

impl VirtioDeviceCore for CountingNotifyCore {
    fn device_type(&self) -> VirtioDeviceID {
        TestCore.device_type()
    }

    fn device_features(&self) -> u64 {
        TestCore.device_features()
    }

    fn queue_size_max(&self) -> u16 {
        TestCore.queue_size_max()
    }

    fn device_config_size(&self) -> u32 {
        TestCore.device_config_size()
    }

    fn read_device_config(&self, offset: u64, width: AccessWidth) -> DeviceResult<u64> {
        TestCore.read_device_config(offset, width)
    }

    fn write_device_config(&self, offset: u64, width: AccessWidth, value: u64) -> DeviceResult {
        TestCore.write_device_config(offset, width, value)
    }

    fn notify_queue(
        &self,
        _queue: &mut VirtioQueue<NoGuestMemoryAccessor>,
        _memory: &mut dyn GuestMemory,
    ) -> DeviceResult<QueueNotifyOutcome> {
        self.notify_calls.fetch_add(1, Ordering::AcqRel);
        Ok(QueueNotifyOutcome::Idle)
    }
}

impl VirtioDeviceCore for BlockingNotifyCore {
    fn device_type(&self) -> VirtioDeviceID {
        VirtioDeviceID::Block
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1
    }

    fn queue_size_max(&self) -> u16 {
        8
    }

    fn device_config_size(&self) -> u32 {
        4
    }

    fn read_device_config(&self, _offset: u64, _width: AccessWidth) -> DeviceResult<u64> {
        Ok(0)
    }

    fn write_device_config(&self, _offset: u64, _width: AccessWidth, _value: u64) -> DeviceResult {
        Ok(())
    }

    fn notify_queue(
        &self,
        _queue: &mut VirtioQueue<NoGuestMemoryAccessor>,
        _memory: &mut dyn GuestMemory,
    ) -> DeviceResult<QueueNotifyOutcome> {
        self.entered.wait();
        self.release.wait();
        Ok(QueueNotifyOutcome::Idle)
    }
}

pub(crate) struct BlockingResetCore {
    pub(crate) entered: StdArc<Barrier>,
    pub(crate) release: StdArc<Barrier>,
    pub(crate) reset_calls: StdArc<AtomicUsize>,
    pub(crate) allow_reset: Option<StdArc<AtomicBool>>,
}

impl VirtioDeviceCore for BlockingResetCore {
    fn device_type(&self) -> VirtioDeviceID {
        VirtioDeviceID::Block
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1
    }

    fn queue_size_max(&self) -> u16 {
        8
    }

    fn device_config_size(&self) -> u32 {
        4
    }

    fn read_device_config(&self, _offset: u64, _width: AccessWidth) -> DeviceResult<u64> {
        Ok(0)
    }

    fn write_device_config(&self, _offset: u64, _width: AccessWidth, _value: u64) -> DeviceResult {
        Ok(())
    }

    fn notify_queue(
        &self,
        _queue: &mut VirtioQueue<NoGuestMemoryAccessor>,
        _memory: &mut dyn GuestMemory,
    ) -> DeviceResult<QueueNotifyOutcome> {
        Ok(QueueNotifyOutcome::Idle)
    }

    fn reset(&self) -> DeviceResult {
        if let Some(allow_reset) = &self.allow_reset {
            assert!(
                allow_reset.load(Ordering::Acquire),
                "reset entered the device core before the IRQ transition completed"
            );
            return Ok(());
        }
        if self.reset_calls.fetch_add(1, Ordering::AcqRel) == 0 {
            self.entered.wait();
            self.release.wait();
        }
        Ok(())
    }
}

pub(crate) struct InvalidCore {
    pub(crate) queue_num_max: u16,
    pub(crate) queue_size_max: u16,
    pub(crate) deferred: bool,
}

impl VirtioDeviceCore for InvalidCore {
    fn device_type(&self) -> VirtioDeviceID {
        VirtioDeviceID::Block
    }

    fn device_features(&self) -> u64 {
        0
    }

    fn queue_num_max(&self) -> u16 {
        self.queue_num_max
    }

    fn queue_size_max(&self) -> u16 {
        self.queue_size_max
    }

    fn device_config_size(&self) -> u32 {
        0
    }

    fn read_device_config(&self, offset: u64, _width: AccessWidth) -> DeviceResult<u64> {
        Err(DeviceError::OutOfRange { addr: offset })
    }

    fn write_device_config(&self, offset: u64, _width: AccessWidth, _value: u64) -> DeviceResult {
        Err(DeviceError::OutOfRange { addr: offset })
    }

    fn notify_queue(
        &self,
        _queue: &mut VirtioQueue<NoGuestMemoryAccessor>,
        _memory: &mut dyn GuestMemory,
    ) -> DeviceResult<QueueNotifyOutcome> {
        Ok(QueueNotifyOutcome::Idle)
    }

    fn requires_deferred_processing(&self) -> bool {
        self.deferred
    }
}

pub(crate) struct TestMemory {
    pub(crate) reads: Cell<usize>,
}

pub(crate) struct FailingMemory;

impl GuestMemory for FailingMemory {
    fn read(&mut self, _guest_addr: GuestPhysAddr, _data: &mut [u8]) -> VirtioResult<()> {
        Err(crate::VirtioError::MemoryError)
    }

    fn write(&mut self, _guest_addr: GuestPhysAddr, _data: &[u8]) -> VirtioResult<()> {
        Ok(())
    }
}

impl GuestMemory for TestMemory {
    fn read(&mut self, _guest_addr: GuestPhysAddr, _data: &mut [u8]) -> VirtioResult<()> {
        self.reads.set(self.reads.get() + 1);
        Ok(())
    }

    fn write(&mut self, _guest_addr: GuestPhysAddr, _data: &[u8]) -> VirtioResult<()> {
        Ok(())
    }
}

pub(crate) fn write<D: VirtioDeviceCore>(
    transport: &VirtioPciTransport<D>,
    offset: u64,
    width: AccessWidth,
    value: u64,
    memory: &mut TestMemory,
) {
    transport
        .write_mmio_with_dma(offset, width, value, true, memory)
        .expect("test transport write should succeed");
}

pub(crate) fn acknowledge_driver<D: VirtioDeviceCore>(
    transport: &VirtioPciTransport<D>,
    memory: &mut TestMemory,
) {
    for status in [1, 3] {
        write(transport, DEVICE_STATUS, AccessWidth::Byte, status, memory);
    }
}
