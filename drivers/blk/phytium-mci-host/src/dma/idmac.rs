//! Controller-lifetime Phytium IDMAC descriptor ring.

use dma_api::{CoherentArray, DeviceDma};
use sdmmc_protocol::error::Error;

use super::BLOCK_SIZE;

const DESC_LAST: u32 = 1 << 2;
const DESC_FIRST: u32 = 1 << 3;
const DESC_CHAIN: u32 = 1 << 4;
const DESC_END_RING: u32 = 1 << 5;
const DESC_OWN: u32 = 1 << 31;

pub const IDMAC_DESC_ALIGN: usize = 32;
pub const IDMAC_DESC_SIZE: usize = core::mem::size_of::<IdmacDesc>();
pub const IDMAC_DESC_MAX_BYTES: usize = 4096;
pub const IDMAC_RING_BYTES: usize = 4096;
pub const IDMAC_RING_DESC_COUNT: usize = IDMAC_RING_BYTES / IDMAC_DESC_SIZE;
pub const IDMAC_MAX_TRANSFER_SIZE: usize = IDMAC_RING_DESC_COUNT * IDMAC_DESC_MAX_BYTES;
pub const IDMAC_MAX_BLOCKS: u32 = (IDMAC_MAX_TRANSFER_SIZE / BLOCK_SIZE) as u32;
pub(crate) const IDMAC_BUFFER_ALIGN: u64 = 4;

#[repr(C, align(32))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdmacDesc {
    attribute: u32,
    reserved0: u32,
    len: u32,
    reserved1: u32,
    addr_lo: u32,
    addr_hi: u32,
    desc_lo: u32,
    desc_hi: u32,
}

pub(crate) struct IdmacRing {
    descriptors: CoherentArray<IdmacDesc>,
    used: usize,
}

pub(crate) struct IdmacDescriptorSnapshot {
    pub(crate) attribute: u32,
    pub(crate) len: u32,
    pub(crate) addr_lo: u32,
    pub(crate) addr_hi: u32,
    pub(crate) desc_lo: u32,
    pub(crate) desc_hi: u32,
}

impl IdmacRing {
    pub(crate) fn allocate(dma: &DeviceDma) -> Result<Self, Error> {
        let descriptors = dma
            .coherent_array_zero_with_align::<IdmacDesc>(IDMAC_RING_DESC_COUNT, IDMAC_RING_BYTES)
            .map_err(|_| Error::Misaligned)?;
        validate_dma_range(descriptors.dma_addr().as_u64(), descriptors.bytes_len())?;
        Ok(Self {
            descriptors,
            used: 0,
        })
    }

    pub(crate) fn prepare(&mut self, buffer_dma: u64, len: usize) -> Result<u64, Error> {
        if self.descriptors_are_owned() {
            return Err(Error::Busy);
        }
        validate_dma_range(buffer_dma, len)?;

        let table_dma = self.descriptors.dma_addr().as_u64();
        let count = self
            .descriptors
            .write_with_cpu(IDMAC_RING_DESC_COUNT, |descriptors| {
                prepare_idmac_descriptors(descriptors, table_dma, buffer_dma, len)
            })?;
        self.used = count;
        Ok(table_dma)
    }

    pub(crate) fn release_after_quiesce(&mut self) {
        self.clear();
    }

    pub(crate) fn clear_after_reset(&mut self) {
        self.clear();
    }

    fn clear(&mut self) {
        self.descriptors
            .write_with_cpu(IDMAC_RING_DESC_COUNT, |descriptors| {
                descriptors.fill(IdmacDesc::default());
            });
        self.used = 0;
    }

    fn descriptors_are_owned(&self) -> bool {
        (0..self.used).any(|index| {
            self.descriptors
                .read_cpu(index)
                .is_some_and(|descriptor| descriptor.attribute & DESC_OWN != 0)
        })
    }

    pub(crate) fn diagnostic_first_descriptor(&self) -> Option<IdmacDescriptorSnapshot> {
        let descriptor = self.descriptors.read_cpu(0)?;
        Some(IdmacDescriptorSnapshot {
            attribute: descriptor.attribute,
            len: descriptor.len,
            addr_lo: descriptor.addr_lo,
            addr_hi: descriptor.addr_hi,
            desc_lo: descriptor.desc_lo,
            desc_hi: descriptor.desc_hi,
        })
    }
}

fn validate_dma_range(start: u64, len: usize) -> Result<(), Error> {
    if len == 0 {
        return Err(Error::InvalidArgument);
    }
    let end = start
        .checked_add(len as u64)
        .ok_or(Error::InvalidArgument)?;
    if end > u32::MAX as u64 + 1 {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

fn prepare_idmac_descriptors(
    descriptors: &mut [IdmacDesc],
    table_dma: u64,
    buffer_dma: u64,
    len: usize,
) -> Result<usize, Error> {
    if len == 0 || !buffer_dma.is_multiple_of(IDMAC_BUFFER_ALIGN) {
        return Err(Error::Misaligned);
    }
    let count = len.div_ceil(IDMAC_DESC_MAX_BYTES);
    if count > descriptors.len() {
        return Err(Error::InvalidArgument);
    }
    validate_dma_range(buffer_dma, len)?;
    validate_dma_range(
        table_dma,
        count
            .checked_mul(IDMAC_DESC_SIZE)
            .ok_or(Error::InvalidArgument)?,
    )?;

    for (index, descriptor) in descriptors[..count].iter_mut().enumerate() {
        let offset = index * IDMAC_DESC_MAX_BYTES;
        let chunk_len = (len - offset).min(IDMAC_DESC_MAX_BYTES);
        let first = index == 0;
        let last = index + 1 == count;
        let next = if last {
            0
        } else {
            table_dma + ((index + 1) * IDMAC_DESC_SIZE) as u64
        };
        let mut attribute = DESC_OWN | DESC_CHAIN;
        if first {
            attribute |= DESC_FIRST;
        }
        if last {
            attribute |= DESC_LAST | DESC_END_RING;
        }
        let buffer_addr = buffer_dma + offset as u64;
        *descriptor = IdmacDesc {
            attribute,
            reserved0: 0,
            len: u32::try_from(chunk_len).map_err(|_| Error::InvalidArgument)?,
            reserved1: 0,
            addr_lo: buffer_addr as u32,
            addr_hi: (buffer_addr >> 32) as u32,
            desc_lo: next as u32,
            desc_hi: (next >> 32) as u32,
        };
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_builder_splits_4608_bytes_into_4096_and_512() {
        let mut descriptors = [IdmacDesc::default(); 2];
        let count =
            prepare_idmac_descriptors(&mut descriptors, 0x1000_0000, 0x2000_0000, 4608).unwrap();

        assert_eq!(count, 2);
        assert_eq!(descriptors[0].len, 4096);
        assert_eq!(descriptors[1].len, 512);
        assert_eq!(descriptors[0].addr_lo, 0x2000_0000);
        assert_eq!(descriptors[1].addr_lo, 0x2000_1000);
        assert_eq!(descriptors[0].attribute, DESC_OWN | DESC_CHAIN | DESC_FIRST);
        assert_eq!(
            descriptors[1].attribute,
            DESC_OWN | DESC_CHAIN | DESC_LAST | DESC_END_RING
        );
    }

    #[test]
    fn descriptor_builder_accepts_word_aligned_protocol_buffer() {
        let mut descriptors = [IdmacDesc::default(); 1];
        let count =
            prepare_idmac_descriptors(&mut descriptors, 0x1000_0000, 0x2000_0004, 64).unwrap();

        assert_eq!(count, 1);
        assert_eq!(descriptors[0].len, 64);
        assert_eq!(descriptors[0].addr_lo, 0x2000_0004);
    }

    #[test]
    fn descriptor_builder_does_not_rewrite_entries_after_the_terminal_descriptor() {
        let sentinel = IdmacDesc {
            attribute: 0x11,
            reserved0: 0x22,
            len: 0x33,
            reserved1: 0x44,
            addr_lo: 0x55,
            addr_hi: 0x66,
            desc_lo: 0x77,
            desc_hi: 0x88,
        };
        let mut descriptors = [IdmacDesc::default(); 4];
        descriptors[3] = sentinel;

        let count =
            prepare_idmac_descriptors(&mut descriptors, 0x1000_0000, 0x2000_0000, 512).unwrap();

        assert_eq!(count, 1);
        assert_eq!(descriptors[3], sentinel);
    }

    #[test]
    fn descriptor_builder_rejects_ring_overflow() {
        let mut descriptor = [IdmacDesc::default(); 1];
        assert_eq!(
            prepare_idmac_descriptors(
                &mut descriptor,
                0x1000_0000,
                0x2000_0000,
                IDMAC_DESC_MAX_BYTES + 1,
            ),
            Err(Error::InvalidArgument)
        );
    }

    #[test]
    fn descriptor_builder_rejects_32_bit_dma_boundary_crossing() {
        let mut descriptor = [IdmacDesc::default(); 1];
        assert_eq!(
            prepare_idmac_descriptors(&mut descriptor, 0x1000_0000, u32::MAX as u64 - 511, 1024,),
            Err(Error::InvalidArgument)
        );
    }
}
