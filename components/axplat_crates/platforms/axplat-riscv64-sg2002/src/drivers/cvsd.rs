use alloc::{boxed::Box, format, sync::Arc};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_driver::{PlatformDevice, block::PlatformDeviceBlock, probe::OnProbeError};
use rdif_block::{
    BlkError, BuffConfig, DriverGeneric, IReadQueue, IWriteQueue, Interface, QueueInfo, RequestId,
    RequestRead, RequestStatus, RequestWrite,
};
use sg200x_bsp::sdmmc::Sdmmc;

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

struct SharedCvsdDriver {
    inner: Arc<SharedCvsdDriverInner>,
}

struct SharedCvsdDriverInner {
    driver: UnsafeCell<CvsdDriver>,
    borrowed: AtomicBool,
}

struct SharedCvsdDriverGuard<'a> {
    inner: &'a SharedCvsdDriverInner,
}

// SAFETY: Access to the `UnsafeCell` is serialized by `borrowed` and scoped to
// `with_mut`. CVSD has no exported hard-IRQ handler, so callers only use this
// from task-side queue methods.
unsafe impl Send for SharedCvsdDriverInner {}

// SAFETY: See the `Send` impl.
unsafe impl Sync for SharedCvsdDriverInner {}

impl SharedCvsdDriver {
    fn new(driver: CvsdDriver) -> Self {
        Self {
            inner: Arc::new(SharedCvsdDriverInner {
                driver: UnsafeCell::new(driver),
                borrowed: AtomicBool::new(false),
            }),
        }
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut CvsdDriver) -> R) -> R {
        let mut guard = self.inner.enter();
        f(guard.get_mut())
    }
}

impl Clone for SharedCvsdDriver {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl SharedCvsdDriverInner {
    fn enter(&self) -> SharedCvsdDriverGuard<'_> {
        while self
            .borrowed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SharedCvsdDriverGuard { inner: self }
    }
}

impl SharedCvsdDriverGuard<'_> {
    fn get_mut(&mut self) -> &mut CvsdDriver {
        unsafe { &mut *self.inner.driver.get() }
    }
}

impl Drop for SharedCvsdDriverGuard<'_> {
    fn drop(&mut self) {
        self.inner.borrowed.store(false, Ordering::Release);
    }
}

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
    inner: SharedCvsdDriver,
    read_queue_created: bool,
    write_queue_created: bool,
}

impl CvsdBlock {
    fn new(driver: CvsdDriver) -> Self {
        Self {
            inner: SharedCvsdDriver::new(driver),
            read_queue_created: false,
            write_queue_created: false,
        }
    }
}

impl DriverGeneric for CvsdBlock {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

impl Interface for CvsdBlock {
    fn create_read_queue(&mut self) -> Option<Box<dyn IReadQueue>> {
        if self.read_queue_created {
            return None;
        }
        self.read_queue_created = true;
        Some(Box::new(CvsdReadQueue {
            id: 0,
            inner: self.inner.clone(),
        }))
    }

    fn create_write_queue(&mut self) -> Option<Box<dyn IWriteQueue>> {
        if self.write_queue_created {
            return None;
        }
        self.write_queue_created = true;
        Some(Box::new(CvsdWriteQueue {
            id: 0,
            inner: self.inner.clone(),
        }))
    }
}

struct CvsdReadQueue {
    id: usize,
    inner: SharedCvsdDriver,
}

struct CvsdWriteQueue {
    id: usize,
    inner: SharedCvsdDriver,
}

impl QueueInfo for CvsdReadQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.inner.with_mut(|driver| driver.num_blocks() as usize)
    }

    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    fn buffer_config(&self) -> BuffConfig {
        BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: self.block_size(),
        }
    }
}

impl IReadQueue for CvsdReadQueue {
    fn submit_read(&mut self, mut request: RequestRead<'_>) -> Result<RequestId, BlkError> {
        self.inner
            .with_mut(|driver| driver.read_blocks(request.block_id as u64, &mut request.buffer))?;
        Ok(RequestId::new(0))
    }

    fn poll_read(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
        Ok(RequestStatus::Complete)
    }
}

impl QueueInfo for CvsdWriteQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.inner.with_mut(|driver| driver.num_blocks() as usize)
    }

    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    fn buffer_config(&self) -> BuffConfig {
        BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: self.block_size(),
        }
    }
}

impl IWriteQueue for CvsdWriteQueue {
    fn submit_write(&mut self, request: RequestWrite<'_>) -> Result<RequestId, BlkError> {
        self.inner
            .with_mut(|driver| driver.write_blocks(request.block_id as u64, &request.buffer))?;
        Ok(RequestId::new(0))
    }

    fn poll_write(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
        Ok(RequestStatus::Complete)
    }
}
