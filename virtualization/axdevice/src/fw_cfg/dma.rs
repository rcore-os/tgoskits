use super::*;

pub(super) fn validate_dma_buffer(
    buffer_addr: GuestPhysAddr,
    length: usize,
) -> DeviceManagerResult {
    buffer_addr
        .as_usize()
        .checked_add(length)
        .ok_or_else(|| DeviceManagerError::InvalidInput {
            operation: "validate fw_cfg DMA buffer",
            detail: format!(
                "buffer at {:#x} with length {length:#x} overflows the guest address space",
                buffer_addr.as_usize()
            ),
        })?;
    Ok(())
}

pub(super) fn dma_read_entry<W>(
    data: &[u8],
    start: usize,
    length: usize,
    buffer_addr: GuestPhysAddr,
    write_guest: &mut W,
) -> DeviceManagerResult
where
    W: FnMut(GuestPhysAddr, &[u8]) -> DeviceManagerResult,
{
    let mut remaining = length;
    let mut guest_offset = 0usize;
    let mut data_offset = start.min(data.len());
    let zeroes = [0u8; FW_CFG_DMA_SCRATCH_SIZE];

    while remaining != 0 {
        let chunk_len = remaining.min(FW_CFG_DMA_SCRATCH_SIZE);
        let guest_addr = add_guest_offset(buffer_addr, guest_offset)?;
        let available = data.len().saturating_sub(data_offset).min(chunk_len);
        if available == chunk_len {
            write_guest(guest_addr, &data[data_offset..data_offset + chunk_len])?;
        } else {
            if available != 0 {
                write_guest(guest_addr, &data[data_offset..data_offset + available])?;
            }
            let zero_addr = add_guest_offset(buffer_addr, guest_offset + available)?;
            write_guest(zero_addr, &zeroes[..chunk_len - available])?;
        }

        remaining -= chunk_len;
        guest_offset += chunk_len;
        data_offset = data_offset.saturating_add(chunk_len);
    }

    Ok(())
}

pub(super) fn dma_discard_guest_write<R>(
    length: usize,
    buffer_addr: GuestPhysAddr,
    read_guest: &mut R,
) -> DeviceManagerResult
where
    R: FnMut(GuestPhysAddr, &mut [u8]) -> DeviceManagerResult,
{
    let mut scratch = [0u8; FW_CFG_DMA_SCRATCH_SIZE];
    let mut remaining = length;
    let mut guest_offset = 0usize;
    while remaining != 0 {
        let chunk_len = remaining.min(scratch.len());
        let guest_addr = add_guest_offset(buffer_addr, guest_offset)?;
        read_guest(guest_addr, &mut scratch[..chunk_len])?;
        remaining -= chunk_len;
        guest_offset += chunk_len;
    }
    Ok(())
}

fn add_guest_offset(base: GuestPhysAddr, offset: usize) -> DeviceManagerResult<GuestPhysAddr> {
    base.as_usize()
        .checked_add(offset)
        .map(GuestPhysAddr::from_usize)
        .ok_or_else(|| DeviceManagerError::InvalidInput {
            operation: "advance fw_cfg DMA buffer",
            detail: format!(
                "buffer at {:#x} with offset {offset:#x} overflows the guest address space",
                base.as_usize()
            ),
        })
}
