//! Virtio block request descriptor validation and data transfer.

use alloc::{format, vec, vec::Vec};

use crate::{
    DeviceManagerError, DeviceManagerResult,
    virtio::{
        memory::{
            GuestRead, GuestWrite, checked_guest_address, read_guest, read_u16, read_u32, read_u64,
            write_guest,
        },
        queue::QUEUE_NUM_MAX,
    },
};

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;
const VIRTQ_DESC_F_INDIRECT: u16 = 4;
const VIRTQ_DESC_SUPPORTED_FLAGS: u16 = VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE;
const VIRTQ_DESCRIPTOR_SIZE: u64 = 16;
const REQUEST_HEADER_SIZE: usize = 16;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RequestType {
    Read,
    Write,
    Flush,
    Unsupported,
}

pub(super) struct BlockRequest {
    request_type: RequestType,
    sector: u64,
    data: Vec<Descriptor>,
    status: Descriptor,
    data_length: usize,
}

impl BlockRequest {
    pub(super) fn read(
        read: GuestRead<'_>,
        descriptor_table: u64,
        queue_size: u16,
        head: u16,
        max_data_length: usize,
    ) -> DeviceManagerResult<Self> {
        let descriptors = read_descriptor_chain(read, descriptor_table, queue_size, head)?;
        if descriptors.len() < 2 {
            return Err(invalid_chain(
                "block request requires a header and status descriptor".into(),
            ));
        }

        let header = descriptors[0];
        if header.device_writable() || header.length != REQUEST_HEADER_SIZE {
            return Err(invalid_chain(format!(
                "block request header must be a {REQUEST_HEADER_SIZE}-byte device-readable \
                 descriptor"
            )));
        }
        let status = *descriptors
            .last()
            .ok_or_else(|| invalid_chain("block request has no status descriptor".into()))?;
        if !status.device_writable() || status.length < 1 {
            return Err(invalid_chain(
                "block request status must be device-writable and non-empty".into(),
            ));
        }

        let mut header_bytes = [0_u8; REQUEST_HEADER_SIZE];
        read_guest(
            read,
            header.address,
            &mut header_bytes,
            "read virtio block request header",
        )?;
        let raw_type = u32::from_le_bytes(
            header_bytes[0..4]
                .try_into()
                .map_err(|_| invalid_chain("block request type field is incomplete".into()))?,
        );
        let sector = u64::from_le_bytes(
            header_bytes[8..16]
                .try_into()
                .map_err(|_| invalid_chain("block request sector field is incomplete".into()))?,
        );
        let request_type = match raw_type {
            VIRTIO_BLK_T_IN => RequestType::Read,
            VIRTIO_BLK_T_OUT => RequestType::Write,
            VIRTIO_BLK_T_FLUSH => RequestType::Flush,
            _ => RequestType::Unsupported,
        };
        let data = descriptors[1..descriptors.len() - 1].to_vec();
        let data_length = data.iter().try_fold(0usize, |total, descriptor| {
            total
                .checked_add(descriptor.length)
                .ok_or_else(|| invalid_chain("block request data length overflows".into()))
        })?;
        if data_length > max_data_length {
            return Err(invalid_chain(format!(
                "block request data length {data_length} exceeds limit {max_data_length}"
            )));
        }
        validate_data_direction(&data, request_type)?;

        Ok(Self {
            request_type,
            sector,
            data,
            status,
            data_length,
        })
    }

    pub(super) fn request_type(&self) -> RequestType {
        self.request_type
    }

    pub(super) fn sector(&self) -> u64 {
        self.sector
    }

    pub(super) fn allocate_data_buffer(&self, writable: bool) -> DeviceManagerResult<Vec<u8>> {
        self.require_data_direction(writable)?;
        let mut data = Vec::new();
        data.try_reserve_exact(self.data_length)
            .map_err(|_| DeviceManagerError::OutOfMemory {
                operation: "allocate virtio block request buffer",
            })?;
        data.resize(self.data_length, 0);
        Ok(data)
    }

    pub(super) fn read_data(
        &self,
        read: GuestRead<'_>,
        writable: bool,
    ) -> DeviceManagerResult<Vec<u8>> {
        let mut data = self.allocate_data_buffer(writable)?;
        let mut offset = 0usize;
        for descriptor in &self.data {
            let end = offset + descriptor.length;
            read_guest(
                read,
                descriptor.address,
                &mut data[offset..end],
                "read virtio block request data",
            )?;
            offset = end;
        }
        Ok(data)
    }

    pub(super) fn write_data(&self, write: GuestWrite<'_>, data: &[u8]) -> DeviceManagerResult {
        self.require_data_direction(true)?;
        if data.len() != self.data_length {
            return Err(invalid_chain(format!(
                "block response length {} does not match descriptor capacity {}",
                data.len(),
                self.data_length
            )));
        }
        let mut offset = 0usize;
        for descriptor in &self.data {
            let end = offset + descriptor.length;
            write_guest(
                write,
                descriptor.address,
                &data[offset..end],
                "write virtio block response data",
            )?;
            offset = end;
        }
        Ok(())
    }

    pub(super) fn require_empty_data(&self) -> DeviceManagerResult {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(invalid_chain(
                "flush request must not contain data descriptors".into(),
            ))
        }
    }

    pub(super) fn write_status(&self, write: GuestWrite<'_>, status: u8) -> DeviceManagerResult {
        write_guest(
            write,
            self.status.address,
            &[status],
            "write virtio block request status",
        )
    }

    fn require_data_direction(&self, writable: bool) -> DeviceManagerResult {
        if self
            .data
            .iter()
            .all(|descriptor| descriptor.device_writable() == writable)
        {
            Ok(())
        } else {
            Err(invalid_chain(
                "block request data descriptor direction does not match request type".into(),
            ))
        }
    }
}

fn validate_data_direction(data: &[Descriptor], request_type: RequestType) -> DeviceManagerResult {
    let expected_writable = match request_type {
        RequestType::Read => Some(true),
        RequestType::Write => Some(false),
        RequestType::Flush => None,
        RequestType::Unsupported => return Ok(()),
    };
    if let Some(expected_writable) = expected_writable
        && data
            .iter()
            .any(|descriptor| descriptor.device_writable() != expected_writable)
    {
        return Err(invalid_chain(
            "block request data descriptor direction does not match request type".into(),
        ));
    }
    if matches!(request_type, RequestType::Flush) && !data.is_empty() {
        return Err(invalid_chain(
            "flush request must not contain data descriptors".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Descriptor {
    address: u64,
    length: usize,
    flags: u16,
}

impl Descriptor {
    fn device_writable(self) -> bool {
        self.flags & VIRTQ_DESC_F_WRITE != 0
    }
}

fn read_descriptor_chain(
    read: GuestRead<'_>,
    descriptor_table: u64,
    queue_size: u16,
    head: u16,
) -> DeviceManagerResult<Vec<Descriptor>> {
    let mut descriptors = Vec::new();
    descriptors
        .try_reserve_exact(queue_size as usize)
        .map_err(|_| DeviceManagerError::OutOfMemory {
            operation: "validate virtio block descriptor chain",
        })?;
    let mut visited = vec![false; queue_size as usize];
    let mut index = head;

    loop {
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
        if descriptors.len() >= usize::from(QUEUE_NUM_MAX) {
            return Err(invalid_chain(format!(
                "descriptor chain exceeds maximum queue size {QUEUE_NUM_MAX}"
            )));
        }
        visited[index as usize] = true;

        let descriptor = read_descriptor(read, descriptor_table, index)?;
        descriptors.push(descriptor);
        if descriptor.flags & VIRTQ_DESC_F_NEXT == 0 {
            break;
        }
        index = read_u16(
            read,
            descriptor_table,
            u64::from(index) * VIRTQ_DESCRIPTOR_SIZE + 14,
            "read virtio block next descriptor",
        )?;
    }
    Ok(descriptors)
}

fn read_descriptor(
    read: GuestRead<'_>,
    descriptor_table: u64,
    index: u16,
) -> DeviceManagerResult<Descriptor> {
    let offset = u64::from(index)
        .checked_mul(VIRTQ_DESCRIPTOR_SIZE)
        .ok_or_else(|| invalid_chain("descriptor-table offset overflows".into()))?;
    let address = read_u64(
        read,
        descriptor_table,
        offset,
        "read virtio block descriptor address",
    )?;
    let length = read_u32(
        read,
        descriptor_table,
        offset + 8,
        "read virtio block descriptor length",
    )? as usize;
    let flags = read_u16(
        read,
        descriptor_table,
        offset + 12,
        "read virtio block descriptor flags",
    )?;
    if length == 0 {
        return Err(invalid_chain("descriptor has zero length".into()));
    }
    if flags & VIRTQ_DESC_F_INDIRECT != 0 {
        return Err(invalid_chain(
            "descriptor uses an indirect table that was not negotiated".into(),
        ));
    }
    if flags & !VIRTQ_DESC_SUPPORTED_FLAGS != 0 {
        return Err(invalid_chain(format!(
            "descriptor has unsupported flags {flags:#x}"
        )));
    }
    checked_guest_address(address, length, "validate virtio block descriptor buffer")?;
    Ok(Descriptor {
        address,
        length,
        flags,
    })
}

fn invalid_chain(detail: alloc::string::String) -> DeviceManagerError {
    DeviceManagerError::InvalidInput {
        operation: "validate virtio block descriptor chain",
        detail,
    }
}
