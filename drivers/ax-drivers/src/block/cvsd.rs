use rdrive::{
    PlatformDevice,
    probe::{OnProbeError, static_::StaticInfo},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
use {alloc::format, sg200x_bsp::sdmmc::Sdmmc};

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
use super::{SyncBlockOps, register_sync_block};

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
const BLOCK_SIZE: usize = 512;
pub const DEVICE_NAME: &str = "cvsd";

pub const REGISTER: DriverRegister = DriverRegister {
    name: "Static CV SD/MMC",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_cvsd,
    }],
};

fn probe_cvsd(info: StaticInfo, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if info.name() != DEVICE_NAME {
        return Err(OnProbeError::NotMatch);
    }
    probe_cvsd_target(plat_dev)
}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
fn probe_cvsd_target(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if ax_config::devices::CVSD_PADDR == 0 {
        return Err(OnProbeError::NotMatch);
    }

    let sdmmc = map_region(ax_config::devices::CVSD_PADDR, 0x1000, "CVSD")?;
    let syscon = map_region(ax_config::devices::SYSCON_PADDR, 0x8000, "SYSCON")?;
    let driver =
        CvsdDriver::new(sdmmc, syscon).map_err(|_| OnProbeError::other("CVSD init failed"))?;
    register_sync_block(plat_dev, driver);
    Ok(())
}

#[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
fn probe_cvsd_target(_plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    Err(OnProbeError::NotMatch)
}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
fn map_region(address: usize, size: usize, name: &str) -> Result<usize, OnProbeError> {
    let mmio = axklib::mmio::ioremap_raw(address.into(), size)
        .map_err(|err| OnProbeError::other(format!("failed to map {name}: {err:?}")))?;
    Ok(mmio.as_ptr() as usize)
}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
struct CvsdDriver(Sdmmc);

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
impl CvsdDriver {
    fn new(sdmmc: usize, syscon: usize) -> Result<Self, ()> {
        let sdmmc = unsafe { Sdmmc::from_base_addresses(sdmmc, syscon) };
        sdmmc.init().map_err(|_| ())?;
        sdmmc.clk_en(true);
        Ok(Self(sdmmc))
    }

    fn checked_lba(block_id: u64, offset: usize) -> Result<u32, rd_block::BlkError> {
        let lba = block_id
            .checked_add(offset as u64)
            .ok_or(rd_block::BlkError::InvalidBlockIndex(block_id as usize))?;
        u32::try_from(lba).map_err(|_| rd_block::BlkError::InvalidBlockIndex(block_id as usize))
    }
}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
impl SyncBlockOps for CvsdDriver {
    fn name(&self) -> &'static str {
        DEVICE_NAME
    }

    fn num_blocks(&self) -> u64 {
        self.0.card_capacity_blocks()
    }

    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> Result<(), rd_block::BlkError> {
        if !buf.len().is_multiple_of(BLOCK_SIZE) {
            return Err(rd_block::BlkError::NotSupported);
        }

        for (i, block) in buf.chunks_exact_mut(BLOCK_SIZE).enumerate() {
            self.0
                .read_block(Self::checked_lba(block_id, i)?, block)
                .map_err(|_| rd_block::BlkError::Other("CVSD read failed".into()))?;
        }
        Ok(())
    }

    fn write_blocks(&mut self, block_id: u64, buf: &[u8]) -> Result<(), rd_block::BlkError> {
        if !buf.len().is_multiple_of(BLOCK_SIZE) {
            return Err(rd_block::BlkError::NotSupported);
        }

        for (i, block) in buf.chunks_exact(BLOCK_SIZE).enumerate() {
            self.0
                .write_block(Self::checked_lba(block_id, i)?, block)
                .map_err(|_| rd_block::BlkError::Other("CVSD write failed".into()))?;
        }
        Ok(())
    }
}
