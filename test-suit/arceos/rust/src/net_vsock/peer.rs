//! Minimal split-queue peer; connection semantics stay in the production driver.
use std::sync::{Arc, atomic::Ordering};

use ax_sync::SpinLock;
use virtio_drivers::{
    Error, PhysAddr,
    transport::{DeviceStatus, DeviceType, InterruptStatus, Transport},
};
use zerocopy::{FromBytes, Immutable, IntoBytes};

use super::{GUEST_CID, HEADER_LEN, HOST_CID, HOST_PORT};

#[derive(Default)]
pub(super) struct Peer {
    status: DeviceStatus,
    queues: [DeviceQueue; 3],
    interrupt: bool,
    guest_port: u32,
    pub(super) received: Vec<u8>,
    pub(super) credit_requests: usize,
}
impl Peer {
    pub(super) fn inject(&mut self, operation: u16, allocation: u32) {
        let mut packet = [0u8; HEADER_LEN];
        packet[0..8].copy_from_slice(&HOST_CID.to_le_bytes());
        packet[8..16].copy_from_slice(&GUEST_CID.to_le_bytes());
        packet[16..20].copy_from_slice(&HOST_PORT.to_le_bytes());
        packet[20..24].copy_from_slice(&self.guest_port.to_le_bytes());
        packet[28..30].copy_from_slice(&1u16.to_le_bytes());
        packet[30..32].copy_from_slice(&operation.to_le_bytes());
        packet[36..40].copy_from_slice(&allocation.to_le_bytes());
        self.queues[0]
            .transfer(Some(&packet))
            .expect("posted RX buffer");
        self.interrupt = true;
    }
    fn transmit(&mut self) {
        while let Some(packet) = self.queues[1].transfer(None) {
            assert!(packet.len() >= HEADER_LEN);
            let operation = u16::from_le_bytes(packet[30..32].try_into().unwrap());
            match operation {
                1 => {
                    self.guest_port = u32::from_le_bytes(packet[16..20].try_into().unwrap());
                    self.inject(2, 0);
                }
                5 => self.received.extend_from_slice(&packet[HEADER_LEN..]),
                // Record requests without sending any automatic peer packet.
                7 => self.credit_requests += 1,
                // The guest answers our request with CREDIT_UPDATE.
                3 | 4 | 6 => {}
                _ => panic!("unexpected guest operation {operation}"),
            }
        }
    }
}

/// Queue addresses originate only from the real driver's `queue_set`. They
/// remain valid until `queue_unset`, serialized with peer access by its spinlock.
/// The driver publishes descriptors before avail.idx; the peer publishes used
/// elements before used.idx. No Rust references to device-owned memory escape.
#[derive(Default)]
struct DeviceQueue {
    size: u16,
    descriptors: usize,
    available: usize,
    used: usize,
    next: u16,
}
impl DeviceQueue {
    fn transfer(&mut self, input: Option<&[u8]>) -> Option<Vec<u8>> {
        if self.size == 0 {
            return None;
        }
        // SAFETY: queue_set provided a live, aligned split virtqueue. Only
        // the driver writes avail and descriptor entries; acquire publication
        // precedes reads, and only this spinlock-protected peer writes used.
        unsafe {
            let available = pointer(self.available).cast::<u16>();
            let published =
                (&*available.add(1).cast::<std::sync::atomic::AtomicU16>()).load(Ordering::Acquire);
            if published == self.next {
                return None;
            }
            assert!(published.wrapping_sub(self.next) <= self.size);
            let head = available
                .add(2 + usize::from(self.next % self.size))
                .read_volatile();
            let mut index = head;
            let mut output = Vec::new();
            let mut copied = 0;
            for _ in 0..self.size {
                assert!(index < self.size);
                let descriptor = pointer(self.descriptors).add(usize::from(index) * 16);
                let address = descriptor.cast::<u64>().read_volatile() as usize;
                let length = descriptor.add(8).cast::<u32>().read_volatile() as usize;
                let flags = descriptor.add(12).cast::<u16>().read_volatile();
                let next = descriptor.add(14).cast::<u16>().read_volatile();
                assert_eq!(flags & 4, 0, "indirect descriptors were not negotiated");
                let buffer = pointer(address);
                if let Some(input) = input {
                    assert_ne!(flags & 2, 0);
                    let count = length.min(input.len() - copied);
                    for (offset, byte) in input[copied..copied + count].iter().enumerate() {
                        buffer.add(offset).write_volatile(*byte);
                    }
                    copied += count;
                } else {
                    assert_eq!(flags & 2, 0);
                    for offset in 0..length {
                        output.push(buffer.add(offset).read_volatile());
                    }
                }
                if flags & 1 == 0 {
                    break;
                }
                index = next;
            }
            if let Some(input) = input {
                assert_eq!(copied, input.len());
            }
            let used = pointer(self.used);
            let element = used.add(4 + usize::from(self.next % self.size) * 8);
            element.cast::<u32>().write_volatile(u32::from(head));
            element.add(4).cast::<u32>().write_volatile(copied as u32);
            self.next = self.next.wrapping_add(1);
            (&*used.add(2).cast::<std::sync::atomic::AtomicU16>())
                .store(self.next, Ordering::Release);
            Some(output)
        }
    }
}

fn pointer(physical: usize) -> *mut u8 {
    ax_hal::mem::phys_to_virt(ax_memory_addr::PhysAddr::from(physical)).as_mut_ptr()
}

pub(super) struct MemoryTransport(pub(super) Arc<SpinLock<Peer>>);
impl Transport for MemoryTransport {
    fn device_type(&self) -> DeviceType {
        DeviceType::Socket
    }
    fn read_device_features(&mut self) -> u64 {
        0
    }
    fn write_driver_features(&mut self, features: u64) {
        assert_eq!(features, 0);
    }
    fn max_queue_size(&mut self, _: u16) -> u32 {
        8
    }
    fn notify(&mut self, queue: u16) {
        if queue == 1 {
            self.0.lock().transmit();
        }
    }
    fn get_status(&self) -> DeviceStatus {
        self.0.lock().status
    }
    fn set_status(&mut self, status: DeviceStatus) {
        self.0.lock().status = status;
    }
    fn set_guest_page_size(&mut self, _: u32) {}
    fn requires_legacy_layout(&self) -> bool {
        false
    }
    fn queue_set(
        &mut self,
        queue: u16,
        size: u32,
        descriptors: PhysAddr,
        available: PhysAddr,
        used: PhysAddr,
    ) {
        self.0.lock().queues[usize::from(queue)] = DeviceQueue {
            size: size.try_into().unwrap(),
            descriptors: descriptors.try_into().unwrap(),
            available: available.try_into().unwrap(),
            used: used.try_into().unwrap(),
            next: 0,
        };
    }
    fn queue_unset(&mut self, queue: u16) {
        self.0.lock().queues[usize::from(queue)] = DeviceQueue::default();
    }
    fn queue_used(&mut self, queue: u16) -> bool {
        self.0.lock().queues[usize::from(queue)].size != 0
    }
    fn ack_interrupt(&mut self) -> InterruptStatus {
        if std::mem::take(&mut self.0.lock().interrupt) {
            InterruptStatus::QUEUE_INTERRUPT
        } else {
            InterruptStatus::empty()
        }
    }
    fn read_config_generation(&self) -> u32 {
        0
    }
    fn read_config_space<T: FromBytes + IntoBytes>(&self, offset: usize) -> Result<T, Error> {
        let cid = GUEST_CID.to_le_bytes();
        let bytes = cid
            .get(offset..offset + size_of::<T>())
            .ok_or(Error::ConfigSpaceTooSmall)?;
        T::read_from_bytes(bytes).map_err(|_| Error::InvalidParam)
    }
    fn write_config_space<T: Immutable + IntoBytes>(
        &mut self,
        _: usize,
        _: T,
    ) -> Result<(), Error> {
        Err(Error::InvalidParam)
    }
}
