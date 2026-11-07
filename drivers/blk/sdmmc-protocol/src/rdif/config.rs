use rdif_block::{BlkError, DeviceInfo, QueueLimits};
pub use sdmmc_host::block_queue::*;

use crate::Error;
pub const fn queue_limits(config: &BlockConfig) -> QueueLimits {
    config.limits
}

pub const fn device_info(config: &BlockConfig) -> DeviceInfo {
    config.device
}

pub fn block_addr_for_card(block_id: u64, high_capacity: bool) -> Result<u32, BlkError> {
    let block_id = u32::try_from(block_id).map_err(|_| BlkError::InvalidBlockIndex(block_id))?;
    if high_capacity {
        Ok(block_id)
    } else {
        block_id
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(BlkError::InvalidBlockIndex(block_id as u64))
    }
}

pub fn map_dev_err_to_blk_err(err: Error) -> BlkError {
    match err {
        Error::Busy => BlkError::Retry,
        Error::NoCard | Error::UnsupportedCommand | Error::CardLocked => BlkError::NotSupported,
        Error::Misaligned | Error::InvalidArgument => {
            BlkError::Other("SD/MMC request is not block aligned")
        }
        _ => BlkError::Io,
    }
}
