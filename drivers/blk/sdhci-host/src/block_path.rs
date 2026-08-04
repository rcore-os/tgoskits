use super::*;

pub(super) fn adma2_shape_supported(
    block_size: u32,
    block_count: u32,
    len: usize,
    direction: DataDirection,
) -> bool {
    block_size != 0
        && block_size <= 0x0fff
        && block_count != 0
        && block_count <= u16::MAX.into()
        && usize::try_from(block_size).ok().and_then(|size| {
            usize::try_from(block_count)
                .ok()
                .and_then(|count| size.checked_mul(count))
        }) == Some(len)
        && matches!(direction, DataDirection::Read | DataDirection::Write)
}

pub(super) fn submit_read_adma2(
    host: &mut Sdhci,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    let dma = host.dma.take().ok_or(Error::UnsupportedCommand)?;
    let result = host.submit_adma2_data_request(
        cmd,
        buffer,
        len,
        block_size,
        block_count,
        DataDirection::Read,
        &dma,
        slot,
    );
    host.dma = Some(dma);
    result
}

pub(super) fn submit_write_adma2(
    host: &mut Sdhci,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    let dma = host.dma.take().ok_or(Error::UnsupportedCommand)?;
    let result = host.submit_adma2_data_request(
        cmd,
        buffer,
        len,
        block_size,
        block_count,
        DataDirection::Write,
        &dma,
        slot,
    );
    host.dma = Some(dma);
    result
}
