//! Protocol error mapping and DMA-to-FIFO admission policy.

use crate::*;

pub(crate) fn map_protocol_error(err: Error) -> sdio_host2::Error {
    match err {
        Error::Timeout(_) => sdio_host2::Error::Timeout,
        Error::Crc(_) => sdio_host2::Error::Crc,
        Error::NoCard => sdio_host2::Error::NoCard,
        Error::Busy => sdio_host2::Error::Busy,
        Error::UnsupportedCommand => sdio_host2::Error::Unsupported,
        Error::Misaligned => sdio_host2::Error::Misaligned,
        Error::InvalidArgument => sdio_host2::Error::InvalidArgument,
        Error::BusError(_) => sdio_host2::Error::Bus,
        Error::ReadError(_) | Error::WriteError(_) | Error::BadResponse(_) => {
            sdio_host2::Error::Bus
        }
        Error::CardError(_) | Error::CardLocked => sdio_host2::Error::Controller,
        _ => sdio_host2::Error::Controller,
    }
}

pub(crate) fn sdhci_clock_divisor(base_clock_hz: u32, target_hz: u32) -> u16 {
    if target_hz == 0 || base_clock_hz <= target_hz {
        return 0;
    }
    for n in 1..=0x3FF {
        if base_clock_hz / (2 * n as u32) <= target_hz {
            return n;
        }
    }
    0x3FF
}

pub(crate) fn submit_read_with_dma_fifo_fallback(
    host: &mut Sdhci,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    if should_try_dma(cmd, block_size, block_count, len, DataDirection::Read)
        && let Some(dma) = host.dma.clone()
    {
        // SAFETY: the protocol-facing `DataRequest<'a>` retains the exclusive
        // buffer borrow until completion, including while this request is
        // moved between queue workers.
        match unsafe {
            host.submit_read_blocks(
                cmd.argument,
                buffer,
                NonZeroUsize::new(len).ok_or(Error::InvalidArgument)?,
                Some(&dma),
                BlockTransferMode::Dma,
                slot,
            )
        } {
            Ok(request) => {
                log_adma_path_once("read");
                return Ok(request);
            }
            Err(err) if can_fallback_to_fifo(err) => {
                log_adma_fallback_once("read", err);
            }
            Err(err) => return Err(err),
        }
    }

    // SAFETY: the protocol-facing `DataRequest<'a>` retains the exclusive
    // buffer borrow until completion.
    unsafe {
        host.submit_fifo_data_request(
            cmd,
            buffer,
            len,
            block_size,
            block_count,
            DataDirection::Read,
            slot,
        )
    }
}

pub(crate) fn submit_write_with_dma_fifo_fallback(
    host: &mut Sdhci,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    if should_try_dma(cmd, block_size, block_count, len, DataDirection::Write)
        && let Some(dma) = host.dma.clone()
    {
        // SAFETY: the protocol-facing `DataRequest<'a>` retains the shared
        // buffer borrow until completion, including while this request is
        // moved between queue workers.
        match unsafe {
            host.submit_write_blocks(
                cmd.argument,
                buffer,
                NonZeroUsize::new(len).ok_or(Error::InvalidArgument)?,
                Some(&dma),
                BlockTransferMode::Dma,
                slot,
            )
        } {
            Ok(request) => {
                log_adma_path_once("write");
                return Ok(request);
            }
            Err(err) if can_fallback_to_fifo(err) => {
                log_adma_fallback_once("write", err);
            }
            Err(err) => return Err(err),
        }
    }

    // SAFETY: the protocol-facing `DataRequest<'a>` retains the shared buffer
    // borrow until completion.
    unsafe {
        host.submit_fifo_data_request(
            cmd,
            buffer,
            len,
            block_size,
            block_count,
            DataDirection::Write,
            slot,
        )
    }
}

pub(crate) fn should_try_dma(
    cmd: &Command,
    block_size: u32,
    block_count: u32,
    len: usize,
    direction: DataDirection,
) -> bool {
    block_size == 512
        && len == block_count as usize * 512
        && matches!(
            (direction, cmd.index),
            (DataDirection::Read, 17 | 18) | (DataDirection::Write, 24 | 25)
        )
}

fn can_fallback_to_fifo(err: Error) -> bool {
    matches!(
        err,
        Error::UnsupportedCommand | Error::InvalidArgument | Error::Misaligned
    )
}

fn log_adma_path_once(direction: &str) {
    let logged = match direction {
        "read" => &ADMA_READ_PATH_LOGGED,
        "write" => &ADMA_WRITE_PATH_LOGGED,
        _ => return,
    };
    if !logged.swap(true, Ordering::Relaxed) {
        log::info!("sdhci: using ADMA2 {direction} data path");
    }
}

fn log_adma_fallback_once(direction: &str, err: Error) {
    let logged = match direction {
        "read" => &ADMA_READ_FALLBACK_LOGGED,
        "write" => &ADMA_WRITE_FALLBACK_LOGGED,
        _ => return,
    };
    if !logged.swap(true, Ordering::Relaxed) {
        log::warn!("sdhci: falling back to FIFO for {direction} data path: {err:?}");
    }
}
