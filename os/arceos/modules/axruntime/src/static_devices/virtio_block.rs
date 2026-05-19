extern crate alloc;

use alloc::{boxed::Box, format};

use ax_hal::mem::phys_to_virt;
use rd_block::{BlkError, BuffConfig, IQueue, Request, RequestId, RequestKind};
use rdrive::{
    DriverGeneric, PlatformDevice,
    probe::{OnProbeError, static_::StaticInfo},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};
use virtio_drivers::{
    Error as VirtIoError,
    device::blk::{SECTOR_SIZE, VirtIOBlk},
    transport::{DeviceType, Transport},
};

use crate::static_devices::{
    dma::IDENTITY_DMA,
    virtio::{self, VirtIoHalImpl, VirtIoTransport},
};

pub(super) const REGISTER: DriverRegister = DriverRegister {
    name: "Static VirtIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Static {
            on_probe: probe_mmio,
        },
        ProbeKind::Pci {
            on_probe: probe_pci,
        },
    ],
};

struct VirtIoBlkDevice<T: VirtIoTransport> {
    raw: VirtIOBlk<VirtIoHalImpl, T>,
}

// SAFETY: the adapter owns one transport and moves it into one rd-block queue;
// no shared access to the virtio queue or transport is introduced.
unsafe impl<T: VirtIoTransport> Send for VirtIoBlkDevice<T> {}

impl<T: VirtIoTransport> VirtIoBlkDevice<T> {
    fn new(transport: T) -> Result<Self, VirtIoError> {
        let mut raw = VirtIOBlk::new(transport)?;
        raw.disable_interrupts();
        Ok(Self { raw })
    }
}

struct BlockDevice<T: VirtIoTransport> {
    raw: Option<VirtIoBlkDevice<T>>,
    irq_enabled: bool,
}

struct BlockQueue<T: VirtIoTransport> {
    raw: VirtIoBlkDevice<T>,
}

impl<T: VirtIoTransport> DriverGeneric for BlockDevice<T> {
    fn name(&self) -> &str {
        "virtio-blk"
    }
}

impl<T: VirtIoTransport> rd_block::Interface for BlockDevice<T> {
    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        self.raw
            .take()
            .map(|raw| Box::new(BlockQueue { raw }) as Box<dyn IQueue>)
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

    fn handle_irq(&mut self) -> rd_block::Event {
        rd_block::Event::none()
    }
}

impl<T: VirtIoTransport> IQueue for BlockQueue<T> {
    fn id(&self) -> usize {
        0
    }

    fn num_blocks(&self) -> usize {
        self.raw.raw.capacity() as usize
    }

    fn block_size(&self) -> usize {
        SECTOR_SIZE
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
            RequestKind::Read(mut buffer) => self
                .raw
                .raw
                .read_blocks(request.block_id, &mut buffer)
                .map_err(map_block_error)?,
            RequestKind::Write(items) => self
                .raw
                .raw
                .write_blocks(request.block_id, items)
                .map_err(map_block_error)?,
        }
        Ok(RequestId::new(0))
    }

    fn poll_request(&mut self, _request: RequestId) -> Result<(), BlkError> {
        Ok(())
    }
}

fn probe_mmio(info: StaticInfo, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if info.name() != virtio::MMIO_DEVICE_NAME {
        return Err(OnProbeError::NotMatch);
    }

    for (base, size) in ax_config::devices::VIRTIO_MMIO_RANGES {
        let base_vaddr = phys_to_virt((*base).into()).as_mut_ptr();
        let Some((ty, transport)) = virtio::probe_mmio_device(base_vaddr, *size) else {
            continue;
        };
        if ty == DeviceType::Block {
            return register_block(plat_dev, transport);
        }
    }

    Err(OnProbeError::NotMatch)
}

fn probe_pci(
    endpoint: &mut rdrive::probe::pci::EndpointRc,
    plat_dev: PlatformDevice,
) -> Result<(), OnProbeError> {
    let transport = crate::static_devices::pci::take_virtio_transport(endpoint, DeviceType::Block)?;
    register_block(plat_dev, transport)
}

fn register_block<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    let raw = VirtIoBlkDevice::new(transport).map_err(|err| {
        OnProbeError::other(format!(
            "failed to initialize static VirtIO block device: {err:?}"
        ))
    })?;
    let block = rd_block::Block::new(
        BlockDevice {
            raw: Some(raw),
            irq_enabled: false,
        },
        &IDENTITY_DMA,
    );
    plat_dev.register(block);
    info!("registered static virtio block device");
    Ok(())
}

fn map_block_error(err: VirtIoError) -> BlkError {
    match err {
        VirtIoError::QueueFull | VirtIoError::NotReady => BlkError::Retry,
        VirtIoError::DmaError => BlkError::NoMemory,
        VirtIoError::Unsupported => BlkError::NotSupported,
        VirtIoError::InvalidParam => BlkError::InvalidBlockIndex(0),
        other => BlkError::Other(virtio::map_virtio_error(other).into()),
    }
}
