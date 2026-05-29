#[cfg(plat_dyn)]
use rdrive::register::FdtInfo;
use rdrive::{PlatformDevice, probe::OnProbeError};
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
use {alloc::format, sg200x_bsp::sdmmc::Sdmmc};

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
use super::{SyncBlockOps, register_sync_block};

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
const BLOCK_SIZE: usize = 512;
pub const DEVICE_NAME: &str = "cvsd";

#[cfg(plat_dyn)]
crate::model_register!(
    name: "FDT CVSD",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["cvitek,cv181x-sd"],
        on_probe: probe_fdt,
    }],
);

#[cfg(plat_dyn)]
fn probe_fdt(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let sdmmc =
        info.node.regs().into_iter().next().ok_or_else(|| {
            OnProbeError::other(alloc::format!("[{}] has no reg", info.node.name()))
        })?;
    let syscon = info
        .find_compatible(&["syscon"])
        .into_iter()
        .find_map(|node| node.regs().into_iter().next())
        .ok_or_else(|| OnProbeError::other("CVSD syscon node not found in FDT"))?;

    register_mmio(
        plat_dev,
        sdmmc.address as usize,
        sdmmc.size.unwrap_or(0x1000) as usize,
        syscon.address as usize,
        syscon.size.unwrap_or(0x1000) as usize,
    )
}

fn register_mmio(
    plat_dev: PlatformDevice,
    sdmmc_paddr: usize,
    sdmmc_size: usize,
    syscon_paddr: usize,
    syscon_size: usize,
) -> Result<(), OnProbeError> {
    if sdmmc_paddr == 0 || sdmmc_size == 0 || syscon_paddr == 0 || syscon_size == 0 {
        return Err(OnProbeError::NotMatch);
    }
    register_mmio_target(plat_dev, sdmmc_paddr, sdmmc_size, syscon_paddr, syscon_size)
}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
fn register_mmio_target(
    plat_dev: PlatformDevice,
    sdmmc_paddr: usize,
    sdmmc_size: usize,
    syscon_paddr: usize,
    syscon_size: usize,
) -> Result<(), OnProbeError> {
    let sdmmc = map_region(sdmmc_paddr, sdmmc_size, "CVSD")?;
    let syscon = map_region(syscon_paddr, syscon_size, "SYSCON")?;
    let driver =
        CvsdDriver::new(sdmmc, syscon).map_err(|_| OnProbeError::other("CVSD init failed"))?;
    register_sync_block(plat_dev, driver);
    Ok(())
}

#[cfg(not(any(target_arch = "riscv32", target_arch = "riscv64")))]
fn register_mmio_target(
    _plat_dev: PlatformDevice,
    _sdmmc_paddr: usize,
    _sdmmc_size: usize,
    _syscon_paddr: usize,
    _syscon_size: usize,
) -> Result<(), OnProbeError> {
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

// The SG2002 SD/MMC core stores MMIO registers as `UnsafeCell`-backed
// references, so the raw register block is intentionally not `Sync`.
// `CvsdDriver` is owned by `SyncBlockDevice`, which serializes all access
// through a mutex and never clones the driver, so moving that owner between
// execution contexts is sound.
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
unsafe impl Send for CvsdDriver {}

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
impl CvsdDriver {
    fn new(sdmmc: usize, syscon: usize) -> Result<Self, ()> {
        let sdmmc = unsafe { Sdmmc::new(sdmmc, syscon) };
        sdmmc.init().map_err(|_| ())?;
        sdmmc.clk_en(true);
        Ok(Self(sdmmc))
    }

    fn checked_lba(block_id: u64, offset: usize) -> Result<u32, rdif_block::BlkError> {
        let lba = block_id
            .checked_add(offset as u64)
            .ok_or(rdif_block::BlkError::InvalidBlockIndex(block_id))?;
        u32::try_from(lba).map_err(|_| rdif_block::BlkError::InvalidBlockIndex(block_id))
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

    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> Result<(), rdif_block::BlkError> {
        if !buf.len().is_multiple_of(BLOCK_SIZE) {
            return Err(rdif_block::BlkError::NotSupported);
        }

        for (i, block) in buf.chunks_exact_mut(BLOCK_SIZE).enumerate() {
            self.0
                .read_block(Self::checked_lba(block_id, i)?, block)
                .map_err(|_| rdif_block::BlkError::Other("CVSD read failed"))?;
        }
        Ok(())
    }

    fn write_blocks(&mut self, block_id: u64, buf: &[u8]) -> Result<(), rdif_block::BlkError> {
        if !buf.len().is_multiple_of(BLOCK_SIZE) {
            return Err(rdif_block::BlkError::NotSupported);
        }

        for (i, block) in buf.chunks_exact(BLOCK_SIZE).enumerate() {
            self.0
                .write_block(Self::checked_lba(block_id, i)?, block)
                .map_err(|_| rdif_block::BlkError::Other("CVSD write failed"))?;
        }
        Ok(())
    }
}
