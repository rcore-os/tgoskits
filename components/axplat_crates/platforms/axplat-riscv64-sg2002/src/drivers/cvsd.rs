use alloc::{boxed::Box, format, sync::Arc};

use ax_driver::{PlatformDevice, block::PlatformDeviceBlock, probe::OnProbeError};
use rd_block::{BlkError, BuffConfig, DriverGeneric, Event, IQueue, Interface, Request, RequestId};
use sg200x_bsp::sdmmc::Sdmmc;
use spin::Mutex;

use crate::config::devices;

const BLOCK_SIZE: usize = 512;
const SDMMC_SIZE: usize = 0x1000;
const SYSCON_SIZE: usize = 0x8000;
pub const DEVICE_NAME: &str = "cvsd";

ax_driver::model_register!(
    name: "Static CVSD",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe,
    }],
);

fn probe(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    register_mmio(
        plat_dev,
        devices::CVSD_PADDR,
        SDMMC_SIZE,
        devices::SYSCON_PADDR,
        SYSCON_SIZE,
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

    let sdmmc = map_region(sdmmc_paddr, sdmmc_size, "CVSD")?;
    let syscon = map_region(syscon_paddr, syscon_size, "SYSCON")?;
    let driver =
        CvsdDriver::new(sdmmc, syscon).map_err(|_| OnProbeError::other("CVSD init failed"))?;
    plat_dev.register_block(CvsdBlock::new(driver));
    Ok(())
}

fn map_region(address: usize, size: usize, name: &str) -> Result<usize, OnProbeError> {
    let mmio = axklib::mmio::ioremap_raw(address.into(), size)
        .map_err(|err| OnProbeError::other(format!("failed to map {name}: {err:?}")))?;
    Ok(mmio.as_ptr() as usize)
}

struct CvsdDriver(Sdmmc);

// The SG2002 SD/MMC core stores MMIO registers as `UnsafeCell`-backed
// references, so the raw register block is intentionally not `Sync`.
// `CvsdDriver` is owned by `CvsdBlock`, which serializes all access through a
// mutex and never clones the driver, so moving that owner between execution
// contexts is sound.
unsafe impl Send for CvsdDriver {}

impl CvsdDriver {
    fn new(sdmmc: usize, syscon: usize) -> Result<Self, ()> {
        let sdmmc = unsafe { Sdmmc::new(sdmmc, syscon) };
        sdmmc.init().map_err(|_| ())?;
        sdmmc.clk_en(true);
        Ok(Self(sdmmc))
    }

    fn num_blocks(&self) -> u64 {
        self.0.card_capacity_blocks()
    }

    fn checked_lba(block_id: u64, offset: usize) -> Result<u32, BlkError> {
        let lba = block_id
            .checked_add(offset as u64)
            .ok_or(BlkError::InvalidBlockIndex(block_id as usize))?;
        u32::try_from(lba).map_err(|_| BlkError::InvalidBlockIndex(block_id as usize))
    }

    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> Result<(), BlkError> {
        if !buf.len().is_multiple_of(BLOCK_SIZE) {
            return Err(BlkError::NotSupported);
        }

        for (i, block) in buf.chunks_exact_mut(BLOCK_SIZE).enumerate() {
            self.0
                .read_block(Self::checked_lba(block_id, i)?, block)
                .map_err(|_| BlkError::Other("CVSD read failed".into()))?;
        }
        Ok(())
    }

    fn write_blocks(&mut self, block_id: u64, buf: &[u8]) -> Result<(), BlkError> {
        if !buf.len().is_multiple_of(BLOCK_SIZE) {
            return Err(BlkError::NotSupported);
        }

        for (i, block) in buf.chunks_exact(BLOCK_SIZE).enumerate() {
            self.0
                .write_block(Self::checked_lba(block_id, i)?, block)
                .map_err(|_| BlkError::Other("CVSD write failed".into()))?;
        }
        Ok(())
    }
}

struct CvsdBlock {
    inner: Arc<Mutex<CvsdDriver>>,
    queue_created: bool,
    irq_enabled: bool,
}

impl CvsdBlock {
    fn new(driver: CvsdDriver) -> Self {
        Self {
            inner: Arc::new(Mutex::new(driver)),
            queue_created: false,
            irq_enabled: false,
        }
    }
}

impl DriverGeneric for CvsdBlock {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

impl Interface for CvsdBlock {
    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        if self.queue_created {
            return None;
        }
        self.queue_created = true;
        Some(Box::new(CvsdQueue {
            id: 0,
            inner: Arc::clone(&self.inner),
        }))
    }

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        Event::none()
    }
}

struct CvsdQueue {
    id: usize,
    inner: Arc<Mutex<CvsdDriver>>,
}

impl IQueue for CvsdQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.inner.lock().num_blocks() as usize
    }

    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    fn buff_config(&self) -> BuffConfig {
        BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: self.block_size(),
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        match request.kind {
            rd_block::RequestKind::Read(mut buffer) => self
                .inner
                .lock()
                .read_blocks(request.block_id as u64, &mut buffer)?,
            rd_block::RequestKind::Write(items) => self
                .inner
                .lock()
                .write_blocks(request.block_id as u64, items)?,
        }
        Ok(RequestId::new(0))
    }

    fn poll_request(&mut self, _request: RequestId) -> Result<(), BlkError> {
        Ok(())
    }
}
