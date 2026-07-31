use rdif_block::{BlkError, RequestFlags, RequestOp};

pub(super) const LOGICAL_BLOCK_SIZE: usize = 512;
pub(super) const MAX_PRD_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_PRDS: usize = 56;
pub(super) const MAX_LBA48_BLOCKS: u32 = u16::MAX as u32;
pub(super) const MAX_LBA28_BLOCKS: u32 = 256;

const ATA_CMD_IDENTIFY: u8 = 0xec;
const ATA_CMD_READ_DMA: u8 = 0xc8;
const ATA_CMD_WRITE_DMA: u8 = 0xca;
const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
const ATA_CMD_WRITE_DMA_FUA_EXT: u8 = 0x3d;
const ATA_CMD_READ_FPDMA_QUEUED: u8 = 0x60;
const ATA_CMD_WRITE_FPDMA_QUEUED: u8 = 0x61;
const ATA_CMD_FLUSH: u8 = 0xe7;
const ATA_CMD_FLUSH_EXT: u8 = 0xea;

const FIS_TYPE_REGISTER_H2D: u8 = 0x27;
const FIS_COMMAND: u8 = 1 << 7;
const DEVICE_LBA: u8 = 1 << 6;
const DEVICE_FUA: u8 = 1 << 7;
const PRD_INTERRUPT_ON_COMPLETION: u32 = 1 << 31;

const IDENTIFY_CAPABILITIES: usize = 49;
const IDENTIFY_QUEUE_DEPTH: usize = 75;
const IDENTIFY_SATA_CAPABILITIES: usize = 76;
const IDENTIFY_COMMAND_SET_2: usize = 83;
const IDENTIFY_COMMAND_SET_EXTENSION: usize = 84;
const IDENTIFY_LBA28_CAPACITY: usize = 60;
const IDENTIFY_LBA48_CAPACITY: usize = 100;
const IDENTIFY_LBA_SUPPORTED: u16 = 1 << 9;
const IDENTIFY_NCQ_SUPPORTED: u16 = 1 << 8;
const IDENTIFY_QUEUE_DEPTH_MASK: u16 = 0x1f;
const IDENTIFY_LBA48_SUPPORTED: u16 = 1 << 10;
const IDENTIFY_FLUSH_SUPPORTED: u16 = 1 << 12;
const IDENTIFY_FLUSH_EXT_SUPPORTED: u16 = 1 << 13;
const IDENTIFY_FUA_SUPPORTED: u16 = 1 << 6;
const IDENTIFY_VALID_COMMAND_SET: u16 = 0x4000;

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(super) struct CommandHeader {
    options: u32,
    transferred_bytes: u32,
    table_address_low: u32,
    table_address_high: u32,
    reserved: [u32; 4],
}

pub(super) type CommandList = [CommandHeader; 32];
pub(super) type ReceivedFis = [u8; 256];

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct PhysicalRegionDescriptor {
    address_low: u32,
    address_high: u32,
    reserved: u32,
    byte_count_and_flags: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct CommandTable {
    command_fis: [u8; 64],
    atapi_command: [u8; 16],
    reserved: [u8; 48],
    descriptors: [PhysicalRegionDescriptor; MAX_PRDS],
}

impl Default for CommandTable {
    fn default() -> Self {
        Self {
            command_fis: [0; 64],
            atapi_command: [0; 16],
            reserved: [0; 48],
            descriptors: [PhysicalRegionDescriptor::default(); MAX_PRDS],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct IdentifyGeometry {
    pub(super) blocks: u64,
    pub(super) lba48: bool,
    pub(super) flush: bool,
    pub(super) flush_ext: bool,
    pub(super) fua: bool,
    pub(super) ncq_depth: Option<u8>,
}

pub(super) struct IoCommandSpec {
    pub(super) command_table_dma: u64,
    pub(super) op: RequestOp,
    pub(super) lba: u64,
    pub(super) blocks: u32,
    pub(super) data_dma: Option<u64>,
    pub(super) data_len: usize,
    pub(super) flags: RequestFlags,
    pub(super) lba48: bool,
    pub(super) flush_ext: bool,
    pub(super) ncq: bool,
    pub(super) tag: u8,
}

pub(super) fn identify_command(
    command_table_dma: u64,
    identify_dma: u64,
) -> Result<(CommandHeader, CommandTable), BlkError> {
    let mut fis = RegisterHostToDeviceFis::command(ATA_CMD_IDENTIFY);
    fis.device = 0;
    command_with_data(
        command_table_dma,
        fis,
        identify_dma,
        LOGICAL_BLOCK_SIZE,
        false,
    )
}

pub(super) fn io_command(
    request: IoCommandSpec,
) -> Result<(CommandHeader, CommandTable), BlkError> {
    match request.op {
        RequestOp::Flush => {
            if request.ncq {
                return Err(BlkError::InvalidRequest);
            }
            let fis = RegisterHostToDeviceFis::command(if request.flush_ext {
                ATA_CMD_FLUSH_EXT
            } else {
                ATA_CMD_FLUSH
            });
            Ok(command_without_data(request.command_table_dma, fis))
        }
        RequestOp::Read | RequestOp::Write => data_command(request),
    }
}

pub(super) fn parse_identify(bytes: &[u8]) -> Result<IdentifyGeometry, BlkError> {
    if bytes.len() < LOGICAL_BLOCK_SIZE {
        return Err(BlkError::Io);
    }
    let word = |index: usize| {
        let offset = index * 2;
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    };
    if word(IDENTIFY_CAPABILITIES) & IDENTIFY_LBA_SUPPORTED == 0 {
        return Err(BlkError::NotSupported);
    }

    let command_set_2 = word(IDENTIFY_COMMAND_SET_2);
    let command_set_2_valid = command_set_2 & 0xc000 == IDENTIFY_VALID_COMMAND_SET;
    let lba48 = command_set_2_valid && command_set_2 & IDENTIFY_LBA48_SUPPORTED != 0;
    let flush = command_set_2_valid && command_set_2 & IDENTIFY_FLUSH_SUPPORTED != 0;
    let flush_ext =
        lba48 && command_set_2_valid && command_set_2 & IDENTIFY_FLUSH_EXT_SUPPORTED != 0;
    let command_set_extension = word(IDENTIFY_COMMAND_SET_EXTENSION);
    let fua = lba48
        && command_set_extension & 0xc000 == IDENTIFY_VALID_COMMAND_SET
        && command_set_extension & IDENTIFY_FUA_SUPPORTED != 0;
    let ncq_depth = (lba48 && word(IDENTIFY_SATA_CAPABILITIES) & IDENTIFY_NCQ_SUPPORTED != 0)
        .then(|| ((word(IDENTIFY_QUEUE_DEPTH) & IDENTIFY_QUEUE_DEPTH_MASK) + 1) as u8);
    let blocks = if lba48 {
        u64::from(word(IDENTIFY_LBA48_CAPACITY))
            | (u64::from(word(IDENTIFY_LBA48_CAPACITY + 1)) << 16)
            | (u64::from(word(IDENTIFY_LBA48_CAPACITY + 2)) << 32)
            | (u64::from(word(IDENTIFY_LBA48_CAPACITY + 3)) << 48)
    } else {
        u64::from(word(IDENTIFY_LBA28_CAPACITY))
            | (u64::from(word(IDENTIFY_LBA28_CAPACITY + 1)) << 16)
    };
    if blocks == 0 {
        return Err(BlkError::Io);
    }
    Ok(IdentifyGeometry {
        blocks,
        lba48,
        flush: flush || flush_ext,
        flush_ext,
        fua,
        ncq_depth,
    })
}

fn data_command(request: IoCommandSpec) -> Result<(CommandHeader, CommandTable), BlkError> {
    let expected = usize::try_from(request.blocks)
        .ok()
        .and_then(|blocks| blocks.checked_mul(LOGICAL_BLOCK_SIZE))
        .ok_or(BlkError::InvalidRequest)?;
    let max_data_len = MAX_PRDS
        .checked_mul(MAX_PRD_BYTES)
        .ok_or(BlkError::InvalidRequest)?;
    if request.blocks == 0
        || request.data_len != expected
        || request.data_len > max_data_len
        || !request.data_len.is_multiple_of(2)
        || request.data_dma.is_none()
    {
        return Err(BlkError::InvalidRequest);
    }

    let is_write = request.op == RequestOp::Write;
    let fis = if request.ncq {
        RegisterHostToDeviceFis::ncq(
            if is_write {
                ATA_CMD_WRITE_FPDMA_QUEUED
            } else {
                ATA_CMD_READ_FPDMA_QUEUED
            },
            request.lba,
            request.blocks,
            request.tag,
            is_write && request.flags.contains(RequestFlags::FUA),
        )?
    } else {
        let command = if request.lba48 {
            if is_write && request.flags.contains(RequestFlags::FUA) {
                ATA_CMD_WRITE_DMA_FUA_EXT
            } else if is_write {
                ATA_CMD_WRITE_DMA_EXT
            } else {
                ATA_CMD_READ_DMA_EXT
            }
        } else if is_write {
            ATA_CMD_WRITE_DMA
        } else {
            ATA_CMD_READ_DMA
        };
        RegisterHostToDeviceFis::rw(command, request.lba, request.blocks, request.lba48)?
    };
    command_with_data(
        request.command_table_dma,
        fis,
        request.data_dma.ok_or(BlkError::InvalidRequest)?,
        request.data_len,
        is_write,
    )
}

fn command_with_data(
    command_table_dma: u64,
    fis: RegisterHostToDeviceFis,
    data_dma: u64,
    data_len: usize,
    is_write: bool,
) -> Result<(CommandHeader, CommandTable), BlkError> {
    let mut table = CommandTable::default();
    table.command_fis[..size_of::<RegisterHostToDeviceFis>()].copy_from_slice(fis.as_bytes());
    let prd_count = data_len.div_ceil(MAX_PRD_BYTES);
    if prd_count == 0 || prd_count > MAX_PRDS {
        return Err(BlkError::InvalidRequest);
    }
    let mut address = data_dma;
    let mut remaining = data_len;
    for (index, descriptor) in table.descriptors[..prd_count].iter_mut().enumerate() {
        let bytes = remaining.min(MAX_PRD_BYTES);
        let end = address
            .checked_add(bytes as u64)
            .ok_or(BlkError::InvalidRequest)?;
        descriptor.address_low = address as u32;
        descriptor.address_high = (address >> 32) as u32;
        descriptor.byte_count_and_flags = (bytes - 1) as u32;
        if index + 1 == prd_count {
            descriptor.byte_count_and_flags |= PRD_INTERRUPT_ON_COMPLETION;
        }
        address = end;
        remaining -= bytes;
    }
    Ok((
        command_header(command_table_dma, prd_count as u16, is_write),
        table,
    ))
}

fn command_without_data(
    command_table_dma: u64,
    fis: RegisterHostToDeviceFis,
) -> (CommandHeader, CommandTable) {
    let mut table = CommandTable::default();
    table.command_fis[..size_of::<RegisterHostToDeviceFis>()].copy_from_slice(fis.as_bytes());
    (command_header(command_table_dma, 0, false), table)
}

fn command_header(command_table_dma: u64, prd_count: u16, is_write: bool) -> CommandHeader {
    let command_fis_dwords = (size_of::<RegisterHostToDeviceFis>() / size_of::<u32>()) as u32;
    CommandHeader {
        options: command_fis_dwords | (u32::from(is_write) << 6) | (u32::from(prd_count) << 16),
        transferred_bytes: 0,
        table_address_low: command_table_dma as u32,
        table_address_high: (command_table_dma >> 32) as u32,
        reserved: [0; 4],
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct RegisterHostToDeviceFis {
    fis_type: u8,
    flags: u8,
    command: u8,
    feature_low: u8,
    lba_low: u8,
    lba_mid: u8,
    lba_high: u8,
    device: u8,
    lba_low_exp: u8,
    lba_mid_exp: u8,
    lba_high_exp: u8,
    feature_high: u8,
    count_low: u8,
    count_high: u8,
    icc: u8,
    control: u8,
    reserved: [u8; 4],
}

impl RegisterHostToDeviceFis {
    fn command(command: u8) -> Self {
        Self {
            fis_type: FIS_TYPE_REGISTER_H2D,
            flags: FIS_COMMAND,
            command,
            ..Self::default()
        }
    }

    fn rw(command: u8, lba: u64, blocks: u32, lba48: bool) -> Result<Self, BlkError> {
        if lba48 {
            if lba >= 1_u64 << 48 || blocks == 0 || blocks > MAX_LBA48_BLOCKS {
                return Err(BlkError::InvalidRequest);
            }
        } else if lba >= 1_u64 << 28 || blocks == 0 || blocks > MAX_LBA28_BLOCKS {
            return Err(BlkError::InvalidRequest);
        }

        let mut fis = Self::command(command);
        fis.set_lba(lba, lba48);
        fis.count_low = blocks as u8;
        if lba48 {
            fis.count_high = (blocks >> 8) as u8;
        }
        Ok(fis)
    }

    fn ncq(command: u8, lba: u64, blocks: u32, tag: u8, fua: bool) -> Result<Self, BlkError> {
        if lba >= 1_u64 << 48 || blocks == 0 || blocks > MAX_LBA48_BLOCKS || tag >= 32 {
            return Err(BlkError::InvalidRequest);
        }
        let mut fis = Self::command(command);
        fis.set_lba(lba, true);
        if fua {
            fis.device |= DEVICE_FUA;
        }
        fis.feature_low = blocks as u8;
        fis.feature_high = (blocks >> 8) as u8;
        fis.count_low = tag << 3;
        Ok(fis)
    }

    fn set_lba(&mut self, lba: u64, lba48: bool) {
        self.lba_low = lba as u8;
        self.lba_mid = (lba >> 8) as u8;
        self.lba_high = (lba >> 16) as u8;
        self.device = DEVICE_LBA;
        if lba48 {
            self.lba_low_exp = (lba >> 24) as u8;
            self.lba_mid_exp = (lba >> 32) as u8;
            self.lba_high_exp = (lba >> 40) as u8;
        } else {
            self.device |= ((lba >> 24) as u8) & 0x0f;
        }
    }

    fn as_bytes(&self) -> &[u8] {
        // SAFETY: The FIS is a repr(C) collection of integer bytes without
        // references. Its size is asserted below.
        unsafe {
            core::slice::from_raw_parts(core::ptr::from_ref(self).cast::<u8>(), size_of::<Self>())
        }
    }
}

const _: () = assert!(size_of::<CommandHeader>() == 32);
const _: () = assert!(size_of::<CommandList>() == 1024);
const _: () = assert!(size_of::<CommandTable>() == 1024);
const _: () = assert!(size_of::<RegisterHostToDeviceFis>() == 20);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_parser_negotiates_ncq_depth() {
        let mut identify = identify_with_lba48_capacity(0x1234_5678);
        set_word(
            &mut identify,
            IDENTIFY_SATA_CAPABILITIES,
            IDENTIFY_NCQ_SUPPORTED,
        );
        set_word(&mut identify, IDENTIFY_QUEUE_DEPTH, 15);

        assert_eq!(
            parse_identify(&identify),
            Ok(IdentifyGeometry {
                blocks: 0x1234_5678,
                lba48: true,
                flush: true,
                flush_ext: true,
                fua: true,
                ncq_depth: Some(16),
            })
        );
    }

    #[test]
    fn ncq_command_encodes_tag_count_and_fua() {
        let (_, table) = io_command(IoCommandSpec {
            command_table_dma: 0x2000,
            op: RequestOp::Write,
            lba: 0x1234,
            blocks: 8,
            data_dma: Some(0x4000),
            data_len: 4096,
            flags: RequestFlags::FUA,
            lba48: true,
            flush_ext: true,
            ncq: true,
            tag: 7,
        })
        .expect("valid NCQ command");

        assert_eq!(table.command_fis[2], ATA_CMD_WRITE_FPDMA_QUEUED);
        assert_eq!(table.command_fis[3], 8);
        assert_eq!(table.command_fis[12], 7 << 3);
        assert_ne!(table.command_fis[7] & DEVICE_FUA, 0);
    }

    #[test]
    fn contiguous_transfer_splits_at_four_mibibyte_prd_boundary() {
        let blocks = (MAX_PRD_BYTES / LOGICAL_BLOCK_SIZE + 1) as u32;
        let (header, table) = io_command(IoCommandSpec {
            command_table_dma: 0x2000,
            op: RequestOp::Read,
            lba: 0,
            blocks,
            data_dma: Some(0x4000),
            data_len: blocks as usize * LOGICAL_BLOCK_SIZE,
            flags: RequestFlags::NONE,
            lba48: true,
            flush_ext: true,
            ncq: true,
            tag: 0,
        })
        .expect("valid multi-PRD command");

        assert_eq!((header.options >> 16) & 0xffff, 2);
        assert_eq!(
            table.descriptors[0].byte_count_and_flags,
            (MAX_PRD_BYTES - 1) as u32
        );
        assert_eq!(
            table.descriptors[1].address_low,
            (0x4000 + MAX_PRD_BYTES) as u32
        );
        assert_ne!(
            table.descriptors[1].byte_count_and_flags & PRD_INTERRUPT_ON_COMPLETION,
            0
        );
    }

    #[test]
    fn lba28_rejects_requests_that_need_more_than_one_sector_count_byte() {
        assert_eq!(
            io_command(IoCommandSpec {
                command_table_dma: 0x2000,
                op: RequestOp::Read,
                lba: 0,
                blocks: MAX_LBA28_BLOCKS + 1,
                data_dma: Some(0x4000),
                data_len: (MAX_LBA28_BLOCKS as usize + 1) * LOGICAL_BLOCK_SIZE,
                flags: RequestFlags::NONE,
                lba48: false,
                flush_ext: false,
                ncq: false,
                tag: 0,
            })
            .err(),
            Some(BlkError::InvalidRequest)
        );
    }

    fn identify_with_lba48_capacity(blocks: u64) -> [u8; LOGICAL_BLOCK_SIZE] {
        let mut identify = [0_u8; LOGICAL_BLOCK_SIZE];
        set_word(&mut identify, IDENTIFY_CAPABILITIES, IDENTIFY_LBA_SUPPORTED);
        set_word(
            &mut identify,
            IDENTIFY_COMMAND_SET_2,
            IDENTIFY_VALID_COMMAND_SET
                | IDENTIFY_LBA48_SUPPORTED
                | IDENTIFY_FLUSH_SUPPORTED
                | IDENTIFY_FLUSH_EXT_SUPPORTED,
        );
        set_word(
            &mut identify,
            IDENTIFY_COMMAND_SET_EXTENSION,
            IDENTIFY_VALID_COMMAND_SET | IDENTIFY_FUA_SUPPORTED,
        );
        for word_index in 0..4 {
            set_word(
                &mut identify,
                IDENTIFY_LBA48_CAPACITY + word_index,
                (blocks >> (word_index * 16)) as u16,
            );
        }
        identify
    }

    fn set_word(bytes: &mut [u8], index: usize, value: u16) {
        bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
    }
}
