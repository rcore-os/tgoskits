use dma_api::{CoherentArray, DeviceDma, DmaAddr};
use rdif_block::RequestOp;

use crate::{
    AhciError,
    registers::{PX_CLB, PX_CLBU, PX_FB, PX_FBU, RegisterIo, write_port},
};

const COMMAND_LIST_BYTES: usize = 1024;
const RECEIVED_FIS_BYTES: usize = 256;
const COMMAND_TABLE_BYTES: usize = 256;
const COMMAND_LIST_ALIGN: usize = 1024;
const RECEIVED_FIS_ALIGN: usize = 256;
const COMMAND_TABLE_ALIGN: usize = 128;
const COMMAND_FIS_DWORDS: u32 = 5;
const COMMAND_HEADER_WRITE: u32 = 1 << 6;
const COMMAND_HEADER_PRDT_SHIFT: u32 = 16;
const PRDT_OFFSET: usize = 128;
const PRDT_INTERRUPT_ON_COMPLETION: u32 = 1 << 31;
const MAX_PRDT_BYTES: usize = 1 << 22;

const FIS_TYPE_REGISTER_H2D: u8 = 0x27;
const FIS_COMMAND: u8 = 0x80;
const ATA_IDENTIFY_DEVICE: u8 = 0xec;
const ATA_READ_DMA: u8 = 0xc8;
const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA: u8 = 0xca;
const ATA_WRITE_DMA_EXT: u8 = 0x35;
const ATA_FLUSH_CACHE: u8 = 0xe7;
const ATA_FLUSH_CACHE_EXT: u8 = 0xea;

pub(crate) struct PortCommandMemory {
    command_list: CoherentArray<u8>,
    received_fis: CoherentArray<u8>,
    command_table: CoherentArray<u8>,
}

impl PortCommandMemory {
    pub(crate) fn allocate(dma: &DeviceDma) -> Result<Self, AhciError> {
        Ok(Self {
            command_list: dma
                .coherent_array_zero_with_align(COMMAND_LIST_BYTES, COMMAND_LIST_ALIGN)?,
            received_fis: dma
                .coherent_array_zero_with_align(RECEIVED_FIS_BYTES, RECEIVED_FIS_ALIGN)?,
            command_table: dma
                .coherent_array_zero_with_align(COMMAND_TABLE_BYTES, COMMAND_TABLE_ALIGN)?,
        })
    }

    pub(crate) fn command_list_dma(&self) -> u64 {
        self.command_list.dma_addr().as_u64()
    }

    pub(crate) fn received_fis_dma(&self) -> u64 {
        self.received_fis.dma_addr().as_u64()
    }

    pub(crate) fn program_bases(&self, registers: &dyn RegisterIo, port: usize) {
        write_u64_pair(
            registers,
            port,
            PX_CLB,
            PX_CLBU,
            self.command_list.dma_addr(),
        );
        write_u64_pair(registers, port, PX_FB, PX_FBU, self.received_fis.dma_addr());
    }

    pub(crate) fn build_identify(&mut self, data: DmaAddr) {
        self.build_command(Command::Identify { data });
    }

    pub(crate) fn build_io(
        &mut self,
        op: RequestOp,
        lba: u64,
        block_count: u32,
        data: Option<(DmaAddr, usize)>,
        lba48: bool,
    ) -> Result<(), rdif_block::BlkError> {
        let command = match op {
            RequestOp::Read => Command::Read {
                lba,
                block_count,
                data: data.ok_or(rdif_block::BlkError::InvalidRequest)?,
                lba48,
            },
            RequestOp::Write => Command::Write {
                lba,
                block_count,
                data: data.ok_or(rdif_block::BlkError::InvalidRequest)?,
                lba48,
            },
            RequestOp::Flush => Command::Flush { lba48 },
            RequestOp::Discard | RequestOp::WriteZeroes => {
                return Err(rdif_block::BlkError::NotSupported);
            }
        };
        self.build_command(command);
        Ok(())
    }

    fn build_command(&mut self, command: Command) {
        let (ata_command, write, lba, count, data) = command.fields();
        self.command_table
            .write_with_cpu(COMMAND_TABLE_BYTES, |table| {
                table.fill(0);
                table[0] = FIS_TYPE_REGISTER_H2D;
                table[1] = FIS_COMMAND;
                table[2] = ata_command;
                encode_lba_count(table, lba, count, command.uses_lba48());
                if let Some((address, bytes)) = data {
                    write_u32(table, PRDT_OFFSET, address.as_u64() as u32);
                    write_u32(table, PRDT_OFFSET + 4, (address.as_u64() >> 32) as u32);
                    write_u32(table, PRDT_OFFSET + 8, 0);
                    write_u32(
                        table,
                        PRDT_OFFSET + 12,
                        ((bytes - 1) as u32 & 0x3f_ffff) | PRDT_INTERRUPT_ON_COMPLETION,
                    );
                }
            });

        let options = COMMAND_FIS_DWORDS
            | if write { COMMAND_HEADER_WRITE } else { 0 }
            | if data.is_some() {
                1 << COMMAND_HEADER_PRDT_SHIFT
            } else {
                0
            };
        let command_table_dma = self.command_table.dma_addr().as_u64();
        self.command_list.write_with_cpu(32, |header| {
            header.fill(0);
            write_u32(header, 0, options);
            write_u32(header, 8, command_table_dma as u32);
            write_u32(header, 12, (command_table_dma >> 32) as u32);
        });
    }
}

enum Command {
    Identify {
        data: DmaAddr,
    },
    Read {
        lba: u64,
        block_count: u32,
        data: (DmaAddr, usize),
        lba48: bool,
    },
    Write {
        lba: u64,
        block_count: u32,
        data: (DmaAddr, usize),
        lba48: bool,
    },
    Flush {
        lba48: bool,
    },
}

impl Command {
    fn fields(&self) -> (u8, bool, u64, u32, Option<(DmaAddr, usize)>) {
        match *self {
            Self::Identify { data } => (ATA_IDENTIFY_DEVICE, false, 0, 0, Some((data, 512))),
            Self::Read {
                lba,
                block_count,
                data,
                lba48,
            } => (
                if lba48 {
                    ATA_READ_DMA_EXT
                } else {
                    ATA_READ_DMA
                },
                false,
                lba,
                block_count,
                Some(data),
            ),
            Self::Write {
                lba,
                block_count,
                data,
                lba48,
            } => (
                if lba48 {
                    ATA_WRITE_DMA_EXT
                } else {
                    ATA_WRITE_DMA
                },
                true,
                lba,
                block_count,
                Some(data),
            ),
            Self::Flush { lba48 } => (
                if lba48 {
                    ATA_FLUSH_CACHE_EXT
                } else {
                    ATA_FLUSH_CACHE
                },
                false,
                0,
                0,
                None,
            ),
        }
    }

    fn uses_lba48(&self) -> bool {
        matches!(
            self,
            Self::Read { lba48: true, .. }
                | Self::Write { lba48: true, .. }
                | Self::Flush { lba48: true }
        )
    }
}

fn encode_lba_count(table: &mut [u8], lba: u64, count: u32, lba48: bool) {
    table[4] = lba as u8;
    table[5] = (lba >> 8) as u8;
    table[6] = (lba >> 16) as u8;
    table[7] = 0x40;
    table[12] = count as u8;
    if lba48 {
        table[8] = (lba >> 24) as u8;
        table[9] = (lba >> 32) as u8;
        table[10] = (lba >> 40) as u8;
        table[13] = (count >> 8) as u8;
    } else {
        table[7] |= ((lba >> 24) as u8) & 0x0f;
    }
}

fn write_u64_pair(
    registers: &dyn RegisterIo,
    port: usize,
    low_offset: usize,
    high_offset: usize,
    address: DmaAddr,
) {
    let address = address.as_u64();
    write_port(registers, port, low_offset, address as u32);
    write_port(registers, port, high_offset, (address >> 32) as u32);
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

pub(crate) const fn max_prdt_bytes() -> usize {
    MAX_PRDT_BYTES
}
