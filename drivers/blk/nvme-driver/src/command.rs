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
        let namespace_size = u64::from_le_bytes(data.get(0..8)?.try_into().ok()?);
        if namespace_size == 0 {
            return None;
        }

        let namespace_capacity = u64::from_le_bytes(data.get(8..16)?.try_into().ok()?);
        let namespace_nused = u64::from_le_bytes(data.get(16..24)?.try_into().ok()?);
        let max_lba_format_index = *data.get(25)?;
        if usize::from(max_lba_format_index) >= 64 {
            return None;
        }

        let formatted_lba_size_field = *data.get(26)?;
        // FLBAS bits 6:5 hold the upper two bits of the 6-bit LBAF index.
        let lba_size_idx = usize::from(formatted_lba_size_field & 0x0f)
            | usize::from((formatted_lba_size_field & 0x60) >> 1);
        if lba_size_idx > usize::from(max_lba_format_index) {
            return None;
        }

        let lba_format_offset = 128 + lba_size_idx * 4;
        let lba_format = data.get(lba_format_offset..lba_format_offset + 4)?;
        let metadata_size = u16::from_le_bytes(lba_format[0..2].try_into().ok()?);
        let lba_data_size = lba_format[2];
        if lba_data_size < 9 {
            return None;
        }
        let lba_size = 1usize.checked_shl(u32::from(lba_data_size))?;

        Some(NamespaceDataStructure {
            namespace_size,
            namespcae_capacity: namespace_capacity,
            namespace_nused,
            lba_size,
            metadata_size,
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

        for bytes in data.as_chunks::<4>().0 {
            let id = u32::from_le_bytes(*bytes);
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
    pub namespcae_capacity: u64,
    pub namespace_nused: u64,
    pub lba_size: usize,
    pub metadata_size: u16,
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

    type Output = Option<ControllerInfo>;

    fn parse(&self, data: &[u8]) -> Self::Output {
        let sqes = *data.get(512)?;
        let cqes = *data.get(513)?;
        Some(ControllerInfo {
            vendor_id: u16::from_le_bytes(data.get(0..2)?.try_into().ok()?),
            product_id: u16::from_le_bytes(data.get(2..4)?.try_into().ok()?),
            mdts: *data.get(77)?,
            sqes_max: sqes >> 4,
            sqes_min: sqes & 0x0f,
            cqes_max: cqes >> 4,
            cqes_min: cqes & 0x0f,
            max_cmd: u16::from_le_bytes(data.get(514..516)?.try_into().ok()?),
            number_of_namespaces: u32::from_le_bytes(data.get(516..520)?.try_into().ok()?),
        })
    }

    fn command_set_mut(&mut self) -> &mut CommandSet {
        &mut self.command_set
    }
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
    use super::{
        Identify, IdentifyActiveNamespaceList, IdentifyController, IdentifyNamespaceDataStructure,
    };

    #[repr(align(8))]
    struct IdentifyData([u8; 4096]);

    impl IdentifyData {
        fn namespace(nsze: u64, nlbaf: u8, flbas: u8, metadata_capabilities: u8) -> Self {
            let mut data = Self([0; 4096]);
            data.0[0..8].copy_from_slice(&nsze.to_le_bytes());
            data.0[8..16].copy_from_slice(&nsze.to_le_bytes());
            data.0[16..24].copy_from_slice(&nsze.to_le_bytes());
            data.0[25] = nlbaf;
            data.0[26] = flbas;
            data.0[27] = metadata_capabilities;
            data
        }

        fn set_lba_format(&mut self, index: usize, metadata_size: u16, data_size: u8) {
            let offset = 128 + index * 4;
            self.0[offset..offset + 2].copy_from_slice(&metadata_size.to_le_bytes());
            self.0[offset + 2] = data_size;
        }

        fn parse(&self) -> Option<super::NamespaceDataStructure> {
            IdentifyNamespaceDataStructure::new(1).parse(&self.0)
        }
    }

    #[test]
    fn identify_namespace_uses_selected_lba_format_zero() {
        let mut data = IdentifyData::namespace(1, 0, 0x10, 1);
        data.set_lba_format(0, 16, 12);

        let namespace = data.parse().unwrap();

        assert_eq!(namespace.lba_size, 4096);
        assert_eq!(namespace.metadata_size, 16);
    }

    #[test]
    fn identify_namespace_preserves_64_bit_capacity() {
        let mut data = IdentifyData::namespace(1_u64 << 32, 1, 1, 0);
        data.set_lba_format(1, 0, 9);

        let namespace = data.parse().unwrap();

        assert_eq!(namespace.namespace_size, 1_u64 << 32);
        assert_eq!(namespace.namespcae_capacity, 1_u64 << 32);
        assert_eq!(namespace.namespace_nused, 1_u64 << 32);
    }

    #[test]
    fn identify_namespace_uses_extended_lba_format_index() {
        let mut data = IdentifyData::namespace(1, 63, 0x7f, 1);
        data.set_lba_format(0, 0, 9);
        data.set_lba_format(63, 8, 12);

        let namespace = data.parse().unwrap();

        assert_eq!(namespace.lba_size, 4096);
        assert_eq!(namespace.metadata_size, 8);
    }

    #[test]
    fn identify_namespace_rejects_lba_format_outside_supported_range() {
        let mut data = IdentifyData::namespace(1, 0, 0x20, 0);
        data.set_lba_format(16, 0, 9);

        assert!(data.parse().is_none());
    }

    #[test]
    fn identify_namespace_rejects_unsupported_or_unrepresentable_lba_data_sizes() {
        for lba_data_size in [0, 8, usize::BITS as u8] {
            let mut data = IdentifyData::namespace(1, 0, 0, 0);
            data.set_lba_format(0, 0, lba_data_size);

            assert!(data.parse().is_none());
        }
    }

    #[test]
    fn identify_controller_reads_mdts_from_spec_offset() {
        #[repr(align(4))]
        struct AlignedBuffer([u8; 4097]);
        let mut storage = AlignedBuffer([0; 4097]);
        let data = &mut storage.0[1..];
        data[77] = 7;
        data[512] = 0x66;
        data[513] = 0x44;
        data[516..520].copy_from_slice(&3_u32.to_le_bytes());

        let info = IdentifyController::new().parse(data).unwrap();

        assert_eq!(info.mdts, 7);
        assert_eq!(info.sqes_min, 6);
        assert_eq!(info.sqes_max, 6);
        assert_eq!(info.cqes_min, 4);
        assert_eq!(info.cqes_max, 4);
        assert_eq!(info.number_of_namespaces, 3);
        assert!(IdentifyController::new().parse(&data[..519]).is_none());
        assert!(IdentifyController::new().parse(&[]).is_none());
    }

    #[test]
    fn active_namespace_list_decodes_unaligned_bytes_and_stops_at_zero() {
        #[repr(align(4))]
        struct AlignedBuffer([u8; 17]);
        let mut storage = AlignedBuffer([0; 17]);
        let data = &mut storage.0[1..];
        data.copy_from_slice(&[1, 0, 0, 0, 0x34, 0x12, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(IdentifyActiveNamespaceList::new().parse(data), [1, 0x1234]);
    }
}
