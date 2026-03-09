#![allow(unused)]

use core::ptr::{slice_from_raw_parts, slice_from_raw_parts_mut};

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
        let raw = unsafe { &*slice_from_raw_parts(data.as_ptr() as *const u32, data.len() / 4) };
        unsafe {
            if raw[0] == 0 {
                return None;
            }
            let number_of_lba_formats = data.as_ptr().add(25).read_volatile();
            let formatted_lba_size_field = data.as_ptr().add(26).read_volatile();
            let has_metadata = (formatted_lba_size_field >> 4) & 1 == 1;

            let lba_fmt_list = data.as_ptr().add(128) as *const LBAFormatDataStructure;

            let lba_size_idx = (formatted_lba_size_field & 0b1111) as usize;

            let lba_fmt = if lba_size_idx > 0 {
                lba_fmt_list.add(lba_size_idx).read_volatile()
            } else {
                LBAFormatDataStructure {
                    metadata_size: 0,
                    lba_data_size: 9,
                    other: 0,
                }
            };

            Some(NamespaceDataStructure {
                namespace_size: raw[0],
                namespcae_capacity: raw[1],
                namespace_nused: raw[2],
                lba_size: 2u32.pow(lba_fmt.lba_data_size as u32),
                metadata_size: if has_metadata {
                    data[27] as _
                } else {
                    lba_fmt.metadata_size as _
                },
            })
        }
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

        let raw = unsafe { &*slice_from_raw_parts(data.as_ptr() as *const u32, data.len() / 4) };

        for id in raw {
            if *id == 0 {
                break;
            }
            id_list.push(*id);
        }

        id_list
    }

    fn command_set_mut(&mut self) -> &mut CommandSet {
        &mut self.command_set
    }
}

#[derive(Debug, Clone)]
pub struct NamespaceDataStructure {
    pub namespace_size: u32,
    pub namespcae_capacity: u32,
    pub namespace_nused: u32,
    pub lba_size: u32,
    pub metadata_size: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct LBAFormatDataStructure {
    metadata_size: u16,
    lba_data_size: u8,
    other: u8,
}

impl LBAFormatDataStructure {
    fn relative_performance(&self) -> bool {
        self.other & 1 > 0
    }
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
    pub rsv: [u8; 512 - 8 - 40 - 20 - 2 - 2],
    pub sqes: u8,
    pub cqes: u8,
    pub max_cmd: u16,
    pub number_of_namespaces: u32,
}

#[derive(Debug)]
pub struct ControllerInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub sqes_max: u8,
    pub sqes_min: u8,
    pub cqes_max: u8,
    pub cqes_min: u8,
    pub max_cmd: u16,
    pub number_of_namespaces: u32,
}
