use bcm2835_sdhci::{
    Bcm2835SDhci::{BLOCK_SIZE, EmmcCtl},
    SDHCIError,
};
use rdrive::{PlatformDevice, probe::OnProbeError};

use super::{SyncBlockOps, register_sync_block};

pub const DEVICE_NAME: &str = "bcm2835_sdhci";

pub fn register(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let driver = Bcm2835Sdhci::try_new()
        .map_err(|err| OnProbeError::other(alloc::format!("BCM2835 SDHCI init failed: {err:?}")))?;
    register_sync_block(plat_dev, driver);
    Ok(())
}

struct Bcm2835Sdhci(EmmcCtl);

impl Bcm2835Sdhci {
    fn try_new() -> Result<Self, rd_block::BlkError> {
        let mut ctrl = EmmcCtl::new();
        if ctrl.init() == 0 {
            Ok(Self(ctrl))
        } else {
            Err(rd_block::BlkError::Other(
                "BCM2835 SDHCI init failed".into(),
            ))
        }
    }
}

impl SyncBlockOps for Bcm2835Sdhci {
    fn name(&self) -> &'static str {
        DEVICE_NAME
    }

    fn num_blocks(&self) -> u64 {
        self.0.get_block_num()
    }

    fn block_size(&self) -> usize {
        self.0.get_block_size()
    }

    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> Result<(), rd_block::BlkError> {
        let block_count = buf.len() / BLOCK_SIZE;
        if block_count == 0 || !buf.len().is_multiple_of(BLOCK_SIZE) {
            return Err(rd_block::BlkError::NotSupported);
        }
        let (prefix, aligned, suffix) = unsafe { buf.align_to_mut::<u32>() };
        if !prefix.is_empty() || !suffix.is_empty() {
            return Err(rd_block::BlkError::NotSupported);
        }
        self.0
            .read_block(block_id as u32, block_count, aligned)
            .map_err(map_sdhci_err)
    }

    fn write_blocks(&mut self, block_id: u64, buf: &[u8]) -> Result<(), rd_block::BlkError> {
        let block_count = buf.len() / BLOCK_SIZE;
        if block_count == 0 || !buf.len().is_multiple_of(BLOCK_SIZE) {
            return Err(rd_block::BlkError::NotSupported);
        }
        let (prefix, aligned, suffix) = unsafe { buf.align_to::<u32>() };
        if !prefix.is_empty() || !suffix.is_empty() {
            return Err(rd_block::BlkError::NotSupported);
        }
        self.0
            .write_block(block_id as u32, block_count, aligned)
            .map_err(map_sdhci_err)
    }
}

fn map_sdhci_err(err: SDHCIError) -> rd_block::BlkError {
    match err {
        SDHCIError::Again => rd_block::BlkError::Retry,
        SDHCIError::NoMemory => rd_block::BlkError::NoMemory,
        SDHCIError::Unsupported => rd_block::BlkError::NotSupported,
        _ => rd_block::BlkError::Other("BCM2835 SDHCI I/O error".into()),
    }
}
