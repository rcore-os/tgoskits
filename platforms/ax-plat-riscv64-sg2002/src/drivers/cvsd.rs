use alloc::{boxed::Box, format, sync::Arc};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_driver::{PlatformDevice, block::PlatformDeviceBlock, probe::OnProbeError};
use rdif_block::{
    BlkError, DeviceInfo, DriverGeneric, IQueue, Interface, QueueInfo, QueueLimits, Request,
    RequestFlags, RequestId, RequestOp, RequestStatus, validate_request,
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
            .ok_or(BlkError::InvalidBlockIndex(block_id))?;
        u32::try_from(lba).map_err(|_| BlkError::InvalidBlockIndex(block_id))
    }

    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> Result<(), BlkError> {
        if !buf.len().is_multiple_of(BLOCK_SIZE) {
            return Err(BlkError::NotSupported);
        }

        for (i, block) in buf.chunks_exact_mut(BLOCK_SIZE).enumerate() {
            self.0
                .read_block(Self::checked_lba(block_id, i)?, block)
                .map_err(|_| BlkError::Other("CVSD read failed"))?;
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
                .map_err(|_| BlkError::Other("CVSD write failed"))?;
        }
        Ok(())
    }
}

struct CvsdBlock {
    inner: SharedCvsdDriver,
    queue_created: bool,
}

impl CvsdBlock {
    fn new(driver: CvsdDriver) -> Self {
        Self {
            inner: SharedCvsdDriver::new(driver),
            queue_created: false,
        }
    }
}

impl DriverGeneric for CvsdBlock {
    fn name(&self) -> &str {
        DEVICE_NAME
    }
}

impl Interface for CvsdBlock {
    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            name: Some(DEVICE_NAME),
            ..DeviceInfo::new(
                self.inner.with_mut(|driver| driver.num_blocks()),
                BLOCK_SIZE,
            )
        }
    }

    fn queue_limits(&self) -> QueueLimits {
        QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 0x1000,
            max_blocks_per_request: 1,
            max_segments: 1,
            max_segment_size: BLOCK_SIZE,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        }
    }

    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        if self.queue_created {
            return None;
        }
        self.queue_created = true;
        Some(Box::new(CvsdQueue {
            id: 0,
            inner: self.inner.clone(),
        }))
    }
}

struct CvsdQueue {
    id: usize,
    inner: SharedCvsdDriver,
}

// SAFETY: CVSD operations complete synchronously inside `submit_request`; the
// queue never retains segment pointers beyond the call.
unsafe impl IQueue for CvsdQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        QueueInfo {
            id: self.id,
            device: DeviceInfo {
                name: Some(DEVICE_NAME),
                ..DeviceInfo::new(
                    self.inner.with_mut(|driver| driver.num_blocks()),
                    BLOCK_SIZE,
                )
            },
            limits: QueueLimits {
                dma_mask: u64::MAX,
                dma_alignment: 0x1000,
                max_blocks_per_request: 1,
                max_segments: 1,
                max_segment_size: BLOCK_SIZE,
                supported_flags: RequestFlags::NONE,
                supports_flush: false,
                supports_discard: false,
                supports_write_zeroes: false,
            },
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        validate_request(self.info(), &request)?;
        match request.op {
            RequestOp::Read => {
                let segment = request
                    .segments
                    .first_mut()
                    .ok_or(BlkError::InvalidRequest)?;
                self.inner
                    .with_mut(|driver| driver.read_blocks(request.lba, segment))?;
            }
            RequestOp::Write => {
                let segment = request.segments.first().ok_or(BlkError::InvalidRequest)?;
                self.inner
                    .with_mut(|driver| driver.write_blocks(request.lba, segment))?;
            }
            RequestOp::Flush | RequestOp::Discard | RequestOp::WriteZeroes => {
                return Err(BlkError::NotSupported);
            }
        }
        Ok(RequestId::new(0))
    }

    fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
        Ok(RequestStatus::Complete)
    }
}
