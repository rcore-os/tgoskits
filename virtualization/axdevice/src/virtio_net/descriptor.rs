//! Bounded split-ring descriptor traversal and packet transfer.

use alloc::{format, vec::Vec};

use super::{
    memory::{
        GuestRead, GuestWrite, checked_guest_address, read_guest, read_u16, read_u32, read_u64,
        write_guest,
    },
    queue::QUEUE_NUM_MAX,
};
use crate::{DeviceManagerError, DeviceManagerResult};

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;
const VIRTQ_DESC_F_INDIRECT: u16 = 4;
const VIRTQ_DESC_SUPPORTED_FLAGS: u16 = VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE;
const VIRTQ_DESCRIPTOR_SIZE: u64 = 16;

#[derive(Clone, Copy)]
pub(super) enum DescriptorDirection {
    DeviceReadable,
    DeviceWritable,
}

pub(super) fn read_descriptor_chain(
    read: GuestRead<'_>,
    descriptor_table: u64,
    queue_size: u16,
    head: u16,
    direction: DescriptorDirection,
    max_length: Option<usize>,
) -> DeviceManagerResult<DescriptorChain> {
    let mut descriptors = Vec::new();
    descriptors
        .try_reserve_exact(queue_size as usize)
        .map_err(|_| DeviceManagerError::OutOfMemory {
            operation: "validate virtio-net descriptor chain",
        })?;
    let mut visited = [false; QUEUE_NUM_MAX as usize];
    let mut total_length = 0usize;
    let mut index = head;

    loop {
        validate_descriptor_index(index, queue_size, &visited, descriptors.len())?;
        visited[index as usize] = true;

        let descriptor = read_descriptor(read, descriptor_table, index)?;
        descriptor.validate(direction)?;
        total_length = total_length
            .checked_add(descriptor.length)
            .ok_or_else(|| invalid_chain("descriptor byte length overflow".into()))?;
        if let Some(limit) = max_length
            && total_length > limit
        {
            return Err(invalid_chain(format!(
                "descriptor chain length {total_length} exceeds limit {limit}"
            )));
        }
        descriptors.push(descriptor);

        if descriptor.flags & VIRTQ_DESC_F_NEXT == 0 {
            break;
        }
        index = descriptor.next;
    }

    Ok(DescriptorChain {
        descriptors,
        total_length,
    })
}

pub(super) struct DescriptorChain {
    descriptors: Vec<Descriptor>,
    total_length: usize,
}

impl DescriptorChain {
    pub(super) fn capacity(&self) -> usize {
        self.total_length
    }

    pub(super) fn read_packet(&self, read: GuestRead<'_>) -> DeviceManagerResult<Vec<u8>> {
        let mut packet = Vec::new();
        packet.try_reserve_exact(self.total_length).map_err(|_| {
            DeviceManagerError::OutOfMemory {
                operation: "allocate virtio-net TX packet",
            }
        })?;
        packet.resize(self.total_length, 0);
        let mut offset = 0usize;
        for descriptor in &self.descriptors {
            let end = offset + descriptor.length;
            read_guest(
                read,
                descriptor.address,
                &mut packet[offset..end],
                "read virtio-net TX packet",
            )?;
            offset = end;
        }
        Ok(packet)
    }

    pub(super) fn write_packet(&self, write: GuestWrite<'_>, packet: &[u8]) -> DeviceManagerResult {
        let mut packet_offset = 0usize;
        for descriptor in &self.descriptors {
            if packet_offset == packet.len() {
                break;
            }
            let write_len = descriptor.length.min(packet.len() - packet_offset);
            write_guest(
                write,
                descriptor.address,
                &packet[packet_offset..packet_offset + write_len],
                "write virtio-net RX packet",
            )?;
            packet_offset += write_len;
        }
        if packet_offset != packet.len() {
            return Err(invalid_chain(format!(
                "descriptor chain accepted {} of {} packet bytes",
                packet_offset,
                packet.len()
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Descriptor {
    address: u64,
    length: usize,
    flags: u16,
    next: u16,
}

impl Descriptor {
    fn validate(self, direction: DescriptorDirection) -> DeviceManagerResult {
        if self.length == 0 {
            return Err(invalid_chain("descriptor has zero length".into()));
        }
        if self.flags & VIRTQ_DESC_F_INDIRECT != 0 {
            return Err(invalid_chain(
                "descriptor uses an indirect table that was not negotiated".into(),
            ));
        }
        if self.flags & !VIRTQ_DESC_SUPPORTED_FLAGS != 0 {
            return Err(invalid_chain(format!(
                "descriptor has unsupported flags {:#x}",
                self.flags
            )));
        }
        let device_writable = self.flags & VIRTQ_DESC_F_WRITE != 0;
        let direction_matches = match direction {
            DescriptorDirection::DeviceReadable => !device_writable,
            DescriptorDirection::DeviceWritable => device_writable,
        };
        if !direction_matches {
            return Err(invalid_chain(
                "descriptor access direction does not match the queue operation".into(),
            ));
        }
        checked_guest_address(
            self.address,
            self.length,
            "validate virtio-net descriptor buffer",
        )?;
        Ok(())
    }
}

fn validate_descriptor_index(
    index: u16,
    queue_size: u16,
    visited: &[bool; QUEUE_NUM_MAX as usize],
    descriptor_count: usize,
) -> DeviceManagerResult {
    if index >= queue_size {
        return Err(invalid_chain(format!(
            "descriptor index {index} is outside queue size {queue_size}"
        )));
    }
    if visited[index as usize] {
        return Err(invalid_chain(format!(
            "descriptor chain contains a cycle at index {index}"
        )));
    }
    if descriptor_count >= queue_size as usize {
        return Err(invalid_chain(format!(
            "descriptor chain exceeds queue size {queue_size}"
        )));
    }
    Ok(())
}

fn read_descriptor(
    read: GuestRead<'_>,
    descriptor_table: u64,
    index: u16,
) -> DeviceManagerResult<Descriptor> {
    let offset = u64::from(index)
        .checked_mul(VIRTQ_DESCRIPTOR_SIZE)
        .ok_or_else(|| invalid_chain("descriptor-table offset overflow".into()))?;
    let address = read_u64(
        read,
        descriptor_table,
        offset,
        "read virtio-net descriptor address",
    )?;
    let length = read_u32(
        read,
        descriptor_table,
        offset + 8,
        "read virtio-net descriptor length",
    )? as usize;
    let flags = read_u16(
        read,
        descriptor_table,
        offset + 12,
        "read virtio-net descriptor flags",
    )?;
    let next = read_u16(
        read,
        descriptor_table,
        offset + 14,
        "read virtio-net next descriptor",
    )?;
    Ok(Descriptor {
        address,
        length,
        flags,
        next,
    })
}

fn invalid_chain(detail: alloc::string::String) -> DeviceManagerError {
    DeviceManagerError::InvalidInput {
        operation: "validate virtio-net descriptor chain",
        detail,
    }
}
