#![allow(unused)]

use alloc::vec::Vec;

use log::debug;

use crate::queue::CommandSet;

#[repr(transparent)]
pub struct Opcode(u8);

impl Opcode {
    const fn new(generic: u8, function: u8, data_transfer: u8) -> Self {
        Opcode(generic << 7 | function << 2 | data_transfer)
    }

    pub fn as_u32(&self) -> u32 {
        self.0 as _
    }

    pub const DELETE_IO_SQ: Self = Self::new(0b0, 0b0, 0b0);
    pub const CREATE_IO_SQ: Self = Self::new(0b0, 0b0, 0b1);
    pub const GET_LOG_PAGE: Self = Self::new(0b0, 0b0, 0b10);
    pub const DELETE_IO_CQ: Self = Self::new(0b0, 0b1, 0b0);
    pub const CREATE_IO_CQ: Self = Self::new(0b0, 0b1, 0b1);
    pub const IDENTIFY: Self = Self::new(0b0, 0b1, 0b10);
    pub const ABORT: Self = Self::new(0b0, 0b10, 0b0);
    pub const SET_FEATURES: Self = Self::new(0b0, 0b10, 0b1);
    pub const GET_FEATURES: Self = Self::new(0b0, 0b10, 0b10);
    pub const ASYNCHRONOUS_EVENT_REQUEST: Self = Self::new(0b0, 0b11, 0b0);
    pub const NAMESPACE_MANAGEMENT: Self = Self::new(0b0, 0b11, 0b1);
    pub const FIRMWARE_COMMIT: Self = Self::new(0b1, 0b100, 0b0);
    pub const FIRMWARE_IMAGE_DOWNLOAD: Self = Self::new(0b1, 0b100, 0b1);
    pub const DEVICE_SELF_TEST: Self = Self::new(0b1, 0b101, 0b0);
    pub const NAMESPACE_ATTACHMENT: Self = Self::new(0b1, 0b101, 0b1);
    pub const KEEP_ALIVE: Self = Self::new(0b1, 0b110, 0b0);
    pub const DIRECTIVE_SEND: Self = Self::new(0b1, 0b110, 0b1);
    pub const DIRECTIVE_RECEIVE: Self = Self::new(0b1, 0b110, 0b10);
    pub const VIRTUALIZATION_MANAGEMENT: Self = Self::new(0b1, 0b111, 0b0);
    pub const NVME_MI_SEND: Self = Self::new(0b1, 0b111, 0b1);
    pub const NVME_MI_RECEIVE: Self = Self::new(0b1, 0b111, 0b10);
    pub const DOORBELL_BUFFER_CONFIG: Self = Self::new(0b111, 0b11111, 0b0);

    pub const NVM_FLUSH: Self = Self::new(0b0, 0b000, 0b00);
    pub const NVM_WRITE: Self = Self::new(0b0, 0b000, 0b01);
    pub const NVM_READ: Self = Self::new(0b0, 0b000, 0b10);
}

pub enum Feature {
    NumberOfQueues { nsq: u32, ncq: u32 },
    InterruptVectorConfiguration {},
}

impl Feature {
    pub fn to_cdw10(&self) -> u32 {
        match self {
            Feature::NumberOfQueues { .. } => 0x7,
            Feature::InterruptVectorConfiguration { .. } => 0x9,
        }
    }
}

pub trait Identify {
    const CNS: u32;
    type Output;

    fn command_set_mut(&mut self) -> &mut CommandSet;
    fn parse(&self, data: &[u8]) -> Self::Output;
}

pub struct IdentifyNamespaceDataStructure {
    command_set: CommandSet,
}

impl IdentifyNamespaceDataStructure {
    pub fn new(nsid: u32) -> Self {
        let mut command_set = CommandSet {
            nsid,
            ..Default::default()
        };
        Self { command_set }
    }
}

impl Identify for IdentifyNamespaceDataStructure {
    const CNS: u32 = 0x0;

    type Output = Option<NamespaceDataStructure>;

    fn parse(&self, data: &[u8]) -> Self::Output {
        let namespace_size = read_le_u64(data, 0)?;
        if namespace_size == 0 {
            return None;
        }
        let namespace_capacity = read_le_u64(data, 8)?;
        let namespace_used = read_le_u64(data, 16)?;
        let format_count = usize::from(*data.get(25)?) + 1;
        let formatted_lba_size = *data.get(26)?;
        let format_index = usize::from(formatted_lba_size & 0x0f);
        if format_index >= format_count {
            return None;
        }
        let format_offset = 128_usize.checked_add(format_index.checked_mul(4)?)?;
        let metadata_size = read_le_u16(data, format_offset)?;
        let lba_data_size = u32::from(*data.get(format_offset + 2)?);
        let lba_size = 1_u32.checked_shl(lba_data_size)?;

        Some(NamespaceDataStructure {
            namespace_size,
            namespace_capacity,
            namespace_used,
            lba_size,
            metadata_size: u32::from(metadata_size),
        })
    }

    fn command_set_mut(&mut self) -> &mut CommandSet {
        &mut self.command_set
    }
}

pub struct IdentifyActiveNamespaceList {
    command_set: CommandSet,
}

impl IdentifyActiveNamespaceList {
    pub fn new() -> Self {
        let mut command_set = CommandSet::default();
        Self { command_set }
    }
}

impl Identify for IdentifyActiveNamespaceList {
    const CNS: u32 = 0x02;

    type Output = Vec<u32>;

    fn parse(&self, data: &[u8]) -> Self::Output {
        let mut id_list = Vec::new();
        for bytes in data.chunks_exact(4) {
            let id = u32::from_le_bytes(
                bytes
                    .try_into()
                    .expect("chunks_exact yields four-byte namespace IDs"),
            );
            if id == 0 {
                break;
            }
            id_list.push(id);
        }

        id_list
    }

    fn command_set_mut(&mut self) -> &mut CommandSet {
        &mut self.command_set
    }
}

#[derive(Debug, Clone)]
pub struct NamespaceDataStructure {
    pub namespace_size: u64,
    pub namespace_capacity: u64,
    pub namespace_used: u64,
    pub lba_size: u32,
    pub metadata_size: u32,
}

fn read_le_u16(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

fn read_le_u64(data: &[u8], offset: usize) -> Option<u64> {
    let bytes = data.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

pub struct IdentifyController {
    command_set: CommandSet,
}

impl IdentifyController {
    pub fn new() -> Self {
        let mut command_set = CommandSet::default();
        Self { command_set }
    }
}

impl Identify for IdentifyController {
    const CNS: u32 = 0x01;

    type Output = ControllerInfo;

    fn parse(&self, data: &[u8]) -> Self::Output {
        let raw = unsafe {
            let ptr = data.as_ptr();
            (ptr as *const ControllerData).read_volatile()
        };

        ControllerInfo {
            vendor_id: raw.vendor_id,
            product_id: raw.product_id,
            mdts: raw.mdts,
            sqes_max: raw.sqes >> 4,
            sqes_min: raw.sqes & 0b1111,
            cqes_max: raw.cqes >> 4,
            cqes_min: raw.cqes & 0b1111,
            max_cmd: raw.max_cmd,
            number_of_namespaces: raw.number_of_namespaces,
        }
    }

    fn command_set_mut(&mut self) -> &mut CommandSet {
        &mut self.command_set
    }
}

#[repr(C)]
pub struct ControllerData {
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: [u8; 20],
    pub model_number: [u8; 40],
    pub firmware_revision: [u8; 8],
    pub rsv_before_mdts: [u8; 5],
    pub mdts: u8,
    pub rsv: [u8; 512 - 8 - 40 - 20 - 2 - 2 - 5 - 1],
    pub sqes: u8,
    pub cqes: u8,
    pub max_cmd: u16,
    pub number_of_namespaces: u32,
}

#[derive(Debug)]
pub struct ControllerInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub mdts: u8,
    pub sqes_max: u8,
    pub sqes_min: u8,
    pub cqes_max: u8,
    pub cqes_min: u8,
    pub max_cmd: u16,
    pub number_of_namespaces: u32,
}

#[cfg(test)]
mod tests {
    use super::{Identify, IdentifyController, IdentifyNamespaceDataStructure};

    #[test]
    fn identify_controller_reads_mdts_from_spec_offset() {
        let mut data = [0_u8; 4096];
        data[77] = 7;
        data[512] = 0x66;
        data[513] = 0x44;
        data[516..520].copy_from_slice(&3_u32.to_le_bytes());

        let info = IdentifyController::new().parse(&data);

        assert_eq!(info.mdts, 7);
        assert_eq!(info.sqes_min, 6);
        assert_eq!(info.sqes_max, 6);
        assert_eq!(info.cqes_min, 4);
        assert_eq!(info.cqes_max, 4);
        assert_eq!(info.number_of_namespaces, 3);
    }

    #[test]
    fn identify_namespace_preserves_64_bit_capacity_and_selected_lba_format() {
        let mut data = [0_u8; 4096];
        data[0..8].copy_from_slice(&0x1_0000_0001_u64.to_le_bytes());
        data[8..16].copy_from_slice(&0x1_0000_0000_u64.to_le_bytes());
        data[16..24].copy_from_slice(&7_u64.to_le_bytes());
        data[25] = 1;
        data[26] = 1;
        data[132..134].copy_from_slice(&8_u16.to_le_bytes());
        data[134] = 12;

        let namespace = IdentifyNamespaceDataStructure::new(1)
            .parse(&data)
            .expect("nonzero namespace must parse");

        assert_eq!(namespace.namespace_size, 0x1_0000_0001);
        assert_eq!(namespace.namespace_capacity, 0x1_0000_0000);
        assert_eq!(namespace.namespace_used, 7);
        assert_eq!(namespace.lba_size, 4096);
        assert_eq!(namespace.metadata_size, 8);
    }
}
