//! Filesystem adapter for runtime-owned inline and IRQ-only block controllers.

use alloc::{sync::Arc, vec::Vec};

use ax_alloc::UsageKind;
use ax_errno::{AxError, AxResult};
use ax_fs_ng::{BlockDevice, BlockDeviceMetadata, os::BlockTimeProvider};

use crate::block::{
    BlockDeviceView, BlockServiceError, HardwareQueueError, activate_discovered_controllers,
};

struct RuntimeTimeProvider;

impl BlockTimeProvider for RuntimeTimeProvider {
    fn wall_time(&self) -> core::time::Duration {
        ax_hal::time::wall_time()
    }
}

struct RuntimePageProvider;

impl ax_fs_ng::os::FsPageProvider for RuntimePageProvider {
    fn alloc_page(&self) -> AxResult<ax_fs_ng::os::FsPage> {
        let addr = ax_alloc::global_allocator()
            .alloc_pages(1, ax_fs_ng::os::memory::PAGE_SIZE, UsageKind::PageCache)
            .map_err(|_| AxError::NoMemory)?;
        Ok(unsafe {
            // SAFETY: the page-cache allocator returned one live, page-aligned
            // page whose ownership is transferred into FsPage.
            ax_fs_ng::os::FsPage::from_raw(addr)
        })
    }

    fn dealloc_page(&self, page: ax_fs_ng::os::FsPage) {
        ax_alloc::global_allocator().dealloc_pages(page.addr(), 1, UsageKind::PageCache);
    }

    fn virt_to_phys(&self, vaddr: usize) -> Option<usize> {
        Some(ax_hal::mem::virt_to_phys(ax_hal::mem::VirtAddr::from(vaddr)).as_usize())
    }
}

struct RuntimeBlockDevice {
    device: BlockDeviceView,
    metadata: BlockDeviceMetadata,
}

impl RuntimeBlockDevice {
    fn new(device: BlockDeviceView) -> AxResult<Self> {
        let info = device.device_info();
        let metadata = BlockDeviceMetadata::new(info.num_blocks, info.logical_block_size)?;
        Ok(Self { device, metadata })
    }
}

impl BlockDevice for RuntimeBlockDevice {
    fn name(&self) -> &str {
        self.device.name()
    }

    fn metadata(&self) -> BlockDeviceMetadata {
        self.metadata
    }

    fn read_blocks(&self, start_block: u64, buffer: &mut [u8]) -> AxResult {
        self.device
            .read_blocks(start_block, buffer)
            .map_err(map_block_service_error)
    }

    fn write_blocks(&self, start_block: u64, buffer: &[u8]) -> AxResult {
        self.device
            .write_blocks(start_block, buffer)
            .map_err(map_block_service_error)
    }

    fn flush(&self) -> AxResult {
        self.device.flush().map_err(map_block_service_error)
    }
}

static TIME_PROVIDER: RuntimeTimeProvider = RuntimeTimeProvider;
static PAGE_PROVIDER: RuntimePageProvider = RuntimePageProvider;

pub(super) fn init(bootargs: Option<&str>) {
    ax_fs_ng::os::install(&TIME_PROVIDER, &PAGE_PROVIDER);
    let devices = activate_discovered_controllers()
        .into_iter()
        .flat_map(|controller| controller.logical_devices())
        .filter_map(|device| match RuntimeBlockDevice::new(device) {
            Ok(device) => Some(Arc::new(device) as Arc<dyn BlockDevice>),
            Err(error) => {
                error!("logical block device published invalid filesystem geometry: {error}");
                None
            }
        })
        .collect::<Vec<_>>();
    ax_fs_ng::root::init_root(devices, bootargs);
}

fn map_block_service_error(error: BlockServiceError) -> AxError {
    match error {
        BlockServiceError::InvalidTransfer => AxError::InvalidInput,
        BlockServiceError::Dma(dma_api::DmaError::NoMemory) => AxError::NoMemory,
        BlockServiceError::Dma(_) => AxError::Io,
        BlockServiceError::Driver(error) => map_driver_error(error),
        BlockServiceError::HardwareQueue(error) => map_hctx_error(error),
        BlockServiceError::DriverInvariant => AxError::BadState,
        BlockServiceError::ControllerUnavailable => AxError::NoSuchDevice,
        BlockServiceError::AmbiguousLogicalDevice { .. } => AxError::BadState,
    }
}

fn map_driver_error(error: rdif_block::BlkError) -> AxError {
    match error {
        rdif_block::BlkError::NotSupported => AxError::OperationNotSupported,
        rdif_block::BlkError::Retry | rdif_block::BlkError::Busy => AxError::ResourceBusy,
        rdif_block::BlkError::TimedOut => AxError::TimedOut,
        rdif_block::BlkError::Cancelled => AxError::Interrupted,
        rdif_block::BlkError::Offline | rdif_block::BlkError::Quarantined => AxError::NoSuchDevice,
        rdif_block::BlkError::InvalidDmaProof => AxError::BadState,
        rdif_block::BlkError::NoMemory => AxError::NoMemory,
        rdif_block::BlkError::InvalidBlockIndex(_) | rdif_block::BlkError::InvalidRequest => {
            AxError::InvalidInput
        }
        rdif_block::BlkError::Io | rdif_block::BlkError::Other(_) => AxError::Io,
    }
}

fn map_hctx_error(error: HardwareQueueError) -> AxError {
    match error {
        HardwareQueueError::InvalidCpu(_)
        | HardwareQueueError::RequestState
        | HardwareQueueError::StaleCompletion
        | HardwareQueueError::StaleIrqEvent
        | HardwareQueueError::SynchronousCompletion
        | HardwareQueueError::UnsafeContext
        | HardwareQueueError::WrongOwner
        | HardwareQueueError::Lifecycle(_) => AxError::BadState,
        HardwareQueueError::NotInterruptQueue { .. }
        | HardwareQueueError::MissingInterruptSource { .. } => AxError::OperationNotSupported,
        HardwareQueueError::Maintenance(_) | HardwareQueueError::Task(_) => AxError::BadState,
        HardwareQueueError::Driver(error) => map_driver_error(error),
        HardwareQueueError::EventOverflow { .. } => AxError::Io,
        HardwareQueueError::Capacity => AxError::NoMemory,
        HardwareQueueError::Offline => AxError::NoSuchDevice,
    }
}
