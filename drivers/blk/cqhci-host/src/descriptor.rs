//! CQHCI task-list and transfer descriptor ownership.

use dma_api::{CoherentArray, DeviceDma, DmaSegment};
use rdif_block::{BlkError, RequestOp};

use crate::regs::*;

const DESCRIPTOR_TABLE_ALIGNMENT: usize = 1024;

pub(crate) struct DescriptorTables {
    task_list: CoherentArray<u64>,
    transfers: CoherentArray<u64>,
}

impl DescriptorTables {
    pub(crate) fn allocate(dma: &DeviceDma) -> Result<Self, BlkError> {
        let task_list =
            dma.coherent_array_zero_with_align(TOTAL_SLOTS * 2, DESCRIPTOR_TABLE_ALIGNMENT)?;
        let transfers = dma.coherent_array_zero_with_align(
            TOTAL_SLOTS * TRANSFER_DESCRIPTORS_PER_SLOT,
            DESCRIPTOR_TABLE_ALIGNMENT,
        )?;
        let mut tables = Self {
            task_list,
            transfers,
        };
        tables.initialize_links()?;
        Ok(tables)
    }

    pub(crate) fn task_list_dma(&self) -> u64 {
        self.task_list.dma_addr().as_u64()
    }

    pub(crate) fn prepare_data(
        &mut self,
        tag: usize,
        op: RequestOp,
        lba: u64,
        block_count: u32,
        segment: DmaSegment,
    ) -> Result<(), BlkError> {
        if tag >= DATA_SLOTS
            || lba > u32::MAX.into()
            || !matches!(op, RequestOp::Read | RequestOp::Write)
            || lba
                .checked_add(u64::from(block_count).saturating_sub(1))
                .is_none_or(|end| end > u64::from(u32::MAX))
            || block_count == 0
            || block_count > u16::MAX.into()
            || segment.len.get() != block_count as usize * 512
        {
            return Err(BlkError::InvalidRequest);
        }
        let task = data_task_descriptor(op, lba as u32, block_count as u16);
        self.prepare_transfer_chain(tag, segment)?;
        self.task_list.set_cpu(tag * 2, task.to_le());
        Ok(())
    }

    pub(crate) fn prepare_direct_command(
        &mut self,
        command: u8,
        argument: u32,
        busy_response: bool,
    ) {
        self.task_list.set_cpu(
            DIRECT_COMMAND_SLOT * 2,
            direct_command_descriptor(command, argument, busy_response).to_le(),
        );
    }

    pub(crate) fn clear_task(&mut self, tag: usize) {
        self.task_list.set_cpu(tag * 2, 0);
    }

    fn initialize_links(&mut self) -> Result<(), BlkError> {
        let task_base = self.task_list.dma_addr().as_u64();
        let transfer_base = self.transfers.dma_addr().as_u64();
        for (address, bytes) in [
            (task_base, TOTAL_SLOTS * 16),
            (
                transfer_base,
                TOTAL_SLOTS * TRANSFER_DESCRIPTORS_PER_SLOT * 8,
            ),
        ] {
            if address & (DESCRIPTOR_TABLE_ALIGNMENT as u64 - 1) != 0
                || address
                    .checked_add(bytes as u64 - 1)
                    .is_none_or(|end| end > u64::from(u32::MAX))
            {
                return Err(BlkError::InvalidRequest);
            }
        }
        for tag in 0..DATA_SLOTS {
            let byte_offset = tag
                .checked_mul(TRANSFER_DESCRIPTORS_PER_SLOT)
                .and_then(|entries| entries.checked_mul(core::mem::size_of::<u64>()))
                .ok_or(BlkError::InvalidRequest)?;
            let address = transfer_base
                .checked_add(u64::try_from(byte_offset).map_err(|_| BlkError::InvalidRequest)?)
                .ok_or(BlkError::InvalidRequest)?;
            if address > u32::MAX.into() {
                return Err(BlkError::InvalidRequest);
            }
            let link = ATTR_VALID | ATTR_ACT_LINK | address << DESCRIPTOR_ADDRESS_SHIFT;
            self.task_list.set_cpu(tag * 2 + 1, link.to_le());
        }
        Ok(())
    }

    fn prepare_transfer_chain(&mut self, tag: usize, segment: DmaSegment) -> Result<(), BlkError> {
        let mut address = segment.addr.as_u64();
        let mut remaining = segment.len.get();
        if address & 3 != 0
            || address
                .checked_add(remaining as u64 - 1)
                .is_none_or(|end| end > u64::from(u32::MAX))
        {
            return Err(BlkError::InvalidRequest);
        }
        let first = tag * TRANSFER_DESCRIPTORS_PER_SLOT;
        let mut count = 0;
        while remaining != 0 {
            if count == TRANSFER_DESCRIPTORS_PER_SLOT {
                return Err(BlkError::InvalidRequest);
            }
            // DWCMSHC cannot cross a 128 MiB boundary in one descriptor.
            let boundary_bytes = (1 << 27) - (address as usize & ((1 << 27) - 1));
            let length = remaining
                .min(MAX_TRANSFER_DESCRIPTOR_BYTES)
                .min(boundary_bytes);
            let encoded_length = if length == MAX_TRANSFER_DESCRIPTOR_BYTES {
                0
            } else {
                length as u16
            };
            let descriptor =
                transfer_descriptor(address as u32, encoded_length, length == remaining);
            self.transfers.set_cpu(first + count, descriptor.to_le());
            address += length as u64;
            remaining -= length;
            count += 1;
        }
        for index in count..TRANSFER_DESCRIPTORS_PER_SLOT {
            self.transfers.set_cpu(first + index, 0);
        }
        Ok(())
    }
}

fn data_task_descriptor(op: RequestOp, lba: u32, block_count: u16) -> u64 {
    ATTR_VALID
        | ATTR_END
        | ATTR_INTERRUPT
        | ATTR_ACT_TASK
        | if op == RequestOp::Read {
            TASK_DATA_READ
        } else {
            0
        }
        | u64::from(block_count) << TASK_BLOCK_COUNT_SHIFT
        | u64::from(lba) << TASK_BLOCK_ADDRESS_SHIFT
}

fn transfer_descriptor(address: u32, encoded_length: u16, end: bool) -> u64 {
    ATTR_VALID
        | if end { ATTR_END } else { 0 }
        | ATTR_ACT_TRANSFER
        | u64::from(encoded_length) << TRANSFER_LENGTH_SHIFT
        | u64::from(address) << DESCRIPTOR_ADDRESS_SHIFT
}

fn direct_command_descriptor(command: u8, argument: u32, busy_response: bool) -> u64 {
    let timing: u64 = if busy_response { 0 } else { 1 };
    let response: u64 = if busy_response { 3 } else { 2 };
    ATTR_VALID
        | ATTR_END
        | ATTR_INTERRUPT
        | TASK_QUEUE_BARRIER
        | ATTR_ACT_TASK
        | u64::from(command) << DCMD_COMMAND_SHIFT
        | timing << DCMD_TIMING_SHIFT
        | response << DCMD_RESPONSE_SHIFT
        | u64::from(argument) << DESCRIPTOR_ADDRESS_SHIFT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_task_descriptor_matches_cqhci_wire_layout() {
        let descriptor = data_task_descriptor(RequestOp::Read, 0x1234_5678, 0x40);

        assert_eq!(descriptor & TASK_DATA_READ, TASK_DATA_READ);
        assert_eq!(descriptor >> TASK_BLOCK_COUNT_SHIFT & 0xffff, 0x40);
        assert_eq!(descriptor >> TASK_BLOCK_ADDRESS_SHIFT, 0x1234_5678);
        assert_eq!(
            descriptor & 0x3f,
            ATTR_VALID | ATTR_END | ATTR_INTERRUPT | ATTR_ACT_TASK
        );
    }

    #[test]
    fn transfer_descriptor_encodes_64k_as_zero_length() {
        let descriptor = transfer_descriptor(0x1234_0000, 0, true);

        assert_eq!(descriptor >> DESCRIPTOR_ADDRESS_SHIFT, 0x1234_0000);
        assert_eq!(descriptor >> TRANSFER_LENGTH_SHIFT & 0xffff, 0);
        assert_eq!(descriptor & ATTR_END, ATTR_END);
        assert_eq!(descriptor & (0x7 << 3), ATTR_ACT_TRANSFER);
    }

    #[test]
    fn cache_flush_descriptor_is_a_busy_barrier_command() {
        let argument = MMC_SWITCH_WRITE_BYTE_TEST << 24
            | MMC_FLUSH_CACHE_INDEX_TEST << 16
            | MMC_FLUSH_CACHE_TRIGGER_TEST << 8;
        let descriptor = direct_command_descriptor(6, argument, true);

        assert_eq!(descriptor & TASK_QUEUE_BARRIER, TASK_QUEUE_BARRIER);
        assert_eq!(descriptor >> DCMD_COMMAND_SHIFT & 0x3f, 6);
        assert_eq!(descriptor >> DCMD_TIMING_SHIFT & 1, 0);
        assert_eq!(descriptor >> DCMD_RESPONSE_SHIFT & 0x3, 3);
        assert_eq!(descriptor >> DESCRIPTOR_ADDRESS_SHIFT, u64::from(argument));
    }

    const MMC_SWITCH_WRITE_BYTE_TEST: u32 = 0b11;
    const MMC_FLUSH_CACHE_INDEX_TEST: u32 = 32;
    const MMC_FLUSH_CACHE_TRIGGER_TEST: u32 = 1;
}
