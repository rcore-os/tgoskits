use core::sync::atomic::{AtomicBool, Ordering};

use super::*;

static ADMA_READ_PATH_LOGGED: AtomicBool = AtomicBool::new(false);
static ADMA_WRITE_PATH_LOGGED: AtomicBool = AtomicBool::new(false);
static ADMA_READ_FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);
static ADMA_WRITE_FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);

pub(super) fn submit_read_with_dma_fifo_fallback(
    host: &mut Sdhci,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    match select_block_data_path(
        host.block_transfer_policy,
        host.dma.is_some(),
        cmd,
        block_size,
        block_count,
        len,
        DataDirection::Read,
    )? {
        SelectedDataPath::Adma2 => {
            let dma = host.dma.clone().ok_or(Error::UnsupportedCommand)?;
            match host.submit_adma2_data_request(
                cmd,
                buffer,
                len,
                block_size,
                block_count,
                DataDirection::Read,
                &dma,
                slot,
            ) {
                Ok(request) => {
                    log_adma_path_once("read");
                    return Ok(request);
                }
                Err(err)
                    if host.block_transfer_policy == BlockTransferPolicy::PreferAdma2
                        && can_fallback_to_fifo(err) =>
                {
                    log_adma_fallback_once("read", err);
                }
                Err(err) => return Err(err),
            }
        }
        SelectedDataPath::Fifo => {}
    }

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

pub(super) fn submit_write_with_dma_fifo_fallback(
    host: &mut Sdhci,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    match select_block_data_path(
        host.block_transfer_policy,
        host.dma.is_some(),
        cmd,
        block_size,
        block_count,
        len,
        DataDirection::Write,
    )? {
        SelectedDataPath::Adma2 => {
            let dma = host.dma.clone().ok_or(Error::UnsupportedCommand)?;
            match host.submit_adma2_data_request(
                cmd,
                buffer,
                len,
                block_size,
                block_count,
                DataDirection::Write,
                &dma,
                slot,
            ) {
                Ok(request) => {
                    log_adma_path_once("write");
                    return Ok(request);
                }
                Err(err)
                    if host.block_transfer_policy == BlockTransferPolicy::PreferAdma2
                        && can_fallback_to_fifo(err) =>
                {
                    log_adma_fallback_once("write", err);
                }
                Err(err) => return Err(err),
            }
        }
        SelectedDataPath::Fifo => {}
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SelectedDataPath {
    Adma2,
    Fifo,
}

pub(super) fn select_block_data_path(
    policy: BlockTransferPolicy,
    dma_available: bool,
    cmd: &Command,
    block_size: u32,
    block_count: u32,
    len: usize,
    direction: DataDirection,
) -> Result<SelectedDataPath, Error> {
    let dma_compatible = should_try_dma(cmd, block_size, block_count, len, direction);
    match (policy, dma_compatible, dma_available) {
        (_, true, true) => Ok(SelectedDataPath::Adma2),
        (BlockTransferPolicy::PreferAdma2, ..) => Ok(SelectedDataPath::Fifo),
        (BlockTransferPolicy::RequireAdma2, false, _) => Err(Error::InvalidArgument),
        (BlockTransferPolicy::RequireAdma2, true, false) => Err(Error::UnsupportedCommand),
    }
}

pub(super) fn should_try_dma(
    _cmd: &Command,
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
