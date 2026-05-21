use alloc::format;

use rdrive::{PlatformDevice, probe::OnProbeError};
use simple_sdmmc::SdMmc;

use super::{SyncBlockOps, register_sync_block};

pub const DEVICE_NAME: &str = "sdmmc";

#[cfg(probe = "static")]
crate::register_driver!(
    name: "Static SD/MMC",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_static,
    }],
);

#[cfg(probe = "static")]
fn probe_static(
    info: rdrive::probe::static_::StaticInfo,
    plat_dev: PlatformDevice,
) -> Result<(), OnProbeError> {
    if info.name() != DEVICE_NAME {
        return Err(OnProbeError::NotMatch);
    }
    let Some((address, size)) = info.regs().first().copied() else {
        return Err(OnProbeError::NotMatch);
    };
    register_mmio(plat_dev, address, size)
}

pub fn register_mmio(
    plat_dev: PlatformDevice,
    base_paddr: usize,
    size: usize,
) -> Result<(), OnProbeError> {
    if base_paddr == 0 || size == 0 {
        return Err(OnProbeError::NotMatch);
    }

    let mmio = axklib::mmio::ioremap_raw(base_paddr.into(), size)
        .map_err(|err| OnProbeError::other(format!("failed to map SD/MMC: {err:?}")))?;
    let driver = unsafe { SdMmcDriver::new(mmio.as_ptr() as usize) };
    register_sync_block(plat_dev, driver);
    Ok(())
}

struct SdMmcDriver(SdMmc);

impl SdMmcDriver {
    unsafe fn new(base: usize) -> Self {
        Self(unsafe { SdMmc::new(base) })
    }
}

impl SyncBlockOps for SdMmcDriver {
    fn name(&self) -> &'static str {
        DEVICE_NAME
    }

    fn num_blocks(&self) -> u64 {
        self.0.num_blocks()
    }

    fn block_size(&self) -> usize {
        SdMmc::BLOCK_SIZE
    }

    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> Result<(), rd_block::BlkError> {
        if !buf.len().is_multiple_of(SdMmc::BLOCK_SIZE) {
            return Err(rd_block::BlkError::NotSupported);
        }

        for (i, block) in buf.chunks_exact_mut(SdMmc::BLOCK_SIZE).enumerate() {
            let block: &mut [u8; SdMmc::BLOCK_SIZE] = block.try_into().expect("fixed chunk size");
            self.0.read_block(block_id as u32 + i as u32, block);
        }
        Ok(())
    }

    fn write_blocks(&mut self, block_id: u64, buf: &[u8]) -> Result<(), rd_block::BlkError> {
        if !buf.len().is_multiple_of(SdMmc::BLOCK_SIZE) {
            return Err(rd_block::BlkError::NotSupported);
        }

        for (i, block) in buf.chunks_exact(SdMmc::BLOCK_SIZE).enumerate() {
            let block: &[u8; SdMmc::BLOCK_SIZE] = block.try_into().expect("fixed chunk size");
            self.0.write_block(block_id as u32 + i as u32, block);
        }
        Ok(())
    }
}
