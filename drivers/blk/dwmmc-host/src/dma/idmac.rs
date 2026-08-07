//! Persistent IDMAC descriptor ring and hardware interrupt definitions.

use dma_api::{CoherentArray, DeviceDma};
use sdmmc_protocol::error::{Error, Phase};

use super::{BLOCK_SIZE, map_dma_error};

pub(super) const DESC_OWN: u32 = 1 << 31;
pub(super) const DESC_CH: u32 = 1 << 4;
pub(super) const DESC_FS: u32 = 1 << 3;
pub(super) const DESC_LD: u32 = 1 << 2;
pub(super) const DESC_DIC: u32 = 1 << 1;

pub(crate) const IDMAC_INT_TI: u32 = 1 << 0;
pub(crate) const IDMAC_INT_RI: u32 = 1 << 1;
pub(crate) const IDMAC_INT_FBE: u32 = 1 << 2;
pub(crate) const IDMAC_INT_DU: u32 = 1 << 4;
pub(crate) const IDMAC_INT_CES: u32 = 1 << 5;
pub(crate) const IDMAC_INT_NI: u32 = 1 << 8;
pub(crate) const IDMAC_INT_AI: u32 = 1 << 9;
pub(crate) const IDMAC_INT_ERROR: u32 = IDMAC_INT_FBE | IDMAC_INT_DU | IDMAC_INT_CES | IDMAC_INT_AI;
pub(crate) const IDMAC_INT_CLR: u32 = IDMAC_INT_AI
    | IDMAC_INT_NI
    | IDMAC_INT_CES
    | IDMAC_INT_DU
    | IDMAC_INT_FBE
    | IDMAC_INT_RI
    | IDMAC_INT_TI;
pub(super) const IDMAC_INT_ENABLE: u32 =
    IDMAC_INT_NI | IDMAC_INT_RI | IDMAC_INT_TI | IDMAC_INT_ERROR;

pub const IDMAC_DESC_ALIGN: usize = 16;
pub const IDMAC_DESC_SIZE: usize = core::mem::size_of::<IdmacDesc>();

/// Maximum payload addressable by one DW IDMAC descriptor.
///
/// The hardware descriptor length field is 13 bits, but Linux's DW MMC
/// implementation programs descriptors in 4 KiB chunks to avoid controller
/// length quirks.
pub const IDMAC_DESC_MAX_BYTES: usize = 4096;
const IDMAC_DESC_BOUNDARY_BYTES: u64 = 4096;
pub const IDMAC_RING_BYTES: usize = 4096;
pub const IDMAC_RING_DESC_COUNT: usize = IDMAC_RING_BYTES / IDMAC_DESC_SIZE;
/// Maximum transfer size guaranteed for every DMA base address.
///
/// One descriptor may be consumed by the prefix before the first 4 KiB
/// boundary. Aligned buffers can still use the final descriptor, but the
/// advertised queue limit must be valid for every accepted DMA address.
pub const IDMAC_MAX_TRANSFER_SIZE: usize = (IDMAC_RING_DESC_COUNT - 1) * IDMAC_DESC_MAX_BYTES;
pub const IDMAC_MAX_BLOCKS: u32 = (IDMAC_MAX_TRANSFER_SIZE / BLOCK_SIZE) as u32;

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdmacDesc {
    pub(super) des0: u32,
    pub(super) des1: u32,
    pub(super) des2: u32,
    pub(super) des3: u32,
}

pub(crate) struct IdmacRing {
    descriptors: CoherentArray<IdmacDesc>,
    used: usize,
}

impl IdmacRing {
    pub(crate) fn allocate(dma: &DeviceDma) -> Result<Self, Error> {
        let descriptors = dma
            .coherent_array_zero_with_align::<IdmacDesc>(IDMAC_RING_DESC_COUNT, IDMAC_RING_BYTES)
            .map_err(|err| map_dma_error(err, Phase::Init))?;
        let base = descriptors.dma_addr().as_u64();
        let end = base
            .checked_add(descriptors.bytes_len() as u64)
            .ok_or(Error::InvalidArgument)?;
        if end > u32::MAX as u64 + 1 {
            return Err(Error::InvalidArgument);
        }
        Ok(Self {
            descriptors,
            used: 0,
        })
    }

    pub(crate) fn prepare(&mut self, buffer_dma: u64, len: usize) -> Result<u32, Error> {
        if len == 0 {
            return Err(Error::InvalidArgument);
        }
        for index in 0..self.used {
            if self
                .descriptors
                .read_cpu(index)
                .is_some_and(|descriptor| descriptor.des0 & DESC_OWN != 0)
            {
                return Err(Error::Busy);
            }
        }

        let table_dma = self.descriptors.dma_addr().as_u64();
        let count = self
            .descriptors
            .write_with_cpu(IDMAC_RING_DESC_COUNT, |descriptors| {
                prepare_idmac_descriptors(descriptors, table_dma, buffer_dma, len)
            })?;
        self.used = count;
        Ok(table_dma as u32)
    }

    pub(crate) fn clear_after_reset(&mut self) {
        self.descriptors
            .write_with_cpu(IDMAC_RING_DESC_COUNT, |descriptors| {
                descriptors.fill(IdmacDesc::default());
            });
        self.used = 0;
    }
}

pub(super) fn prepare_idmac_descriptors(
    descriptors: &mut [IdmacDesc],
    table_dma: u64,
    buffer_dma: u64,
    len: usize,
) -> Result<usize, Error> {
    if len == 0 {
        return Err(Error::InvalidArgument);
    }
    let buffer_end = buffer_dma
        .checked_add(len as u64)
        .ok_or(Error::InvalidArgument)?;
    let count = idmac_descriptor_count(buffer_dma, len)?;
    if count > descriptors.len() {
        return Err(Error::InvalidArgument);
    }
    let descriptor_bytes = count
        .checked_mul(IDMAC_DESC_SIZE)
        .ok_or(Error::InvalidArgument)?;
    let descriptor_end = table_dma
        .checked_add(descriptor_bytes as u64)
        .ok_or(Error::InvalidArgument)?;
    if buffer_end > u32::MAX as u64 + 1 || descriptor_end > u32::MAX as u64 + 1 {
        return Err(Error::InvalidArgument);
    }

    let mut remaining = len;
    let mut offset = 0_u64;
    for (index, descriptor) in descriptors[..count].iter_mut().enumerate() {
        let dma_addr = buffer_dma + offset;
        let boundary_remaining =
            (IDMAC_DESC_BOUNDARY_BYTES - dma_addr % IDMAC_DESC_BOUNDARY_BYTES) as usize;
        let chunk = remaining.min(IDMAC_DESC_MAX_BYTES).min(boundary_remaining);
        let last = index + 1 == count;
        let next = if last {
            0
        } else {
            (table_dma + (index as u64 + 1) * IDMAC_DESC_SIZE as u64) as u32
        };
        *descriptor = IdmacDesc::chained(dma_addr as u32, chunk as u32, next, index == 0, last);
        remaining -= chunk;
        offset += chunk as u64;
    }
    Ok(count)
}

fn idmac_descriptor_count(buffer_dma: u64, len: usize) -> Result<usize, Error> {
    let mut count = 0_usize;
    let mut dma_addr = buffer_dma;
    let mut remaining = len;
    while remaining != 0 {
        let boundary_remaining =
            (IDMAC_DESC_BOUNDARY_BYTES - dma_addr % IDMAC_DESC_BOUNDARY_BYTES) as usize;
        let chunk = remaining.min(IDMAC_DESC_MAX_BYTES).min(boundary_remaining);
        count = count.checked_add(1).ok_or(Error::InvalidArgument)?;
        dma_addr = dma_addr
            .checked_add(chunk as u64)
            .ok_or(Error::InvalidArgument)?;
        remaining -= chunk;
    }
    Ok(count)
}

impl IdmacDesc {
    pub fn chained(buffer_dma: u32, len: u32, next_desc_dma: u32, first: bool, last: bool) -> Self {
        debug_assert!(len as usize <= IDMAC_DESC_MAX_BYTES);
        let mut des0 = DESC_OWN;
        if !last {
            des0 |= DESC_CH | DESC_DIC;
        }
        if first {
            des0 |= DESC_FS;
        }
        if last {
            des0 |= DESC_LD;
        }
        Self {
            des0,
            des1: len,
            des2: buffer_dma,
            des3: next_desc_dma,
        }
    }
}
