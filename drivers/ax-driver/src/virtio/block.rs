extern crate alloc;

use alloc::format;

use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(any(probe = "fdt", probe = "pci"))]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    Error as VirtIoError,
    device::blk::{SECTOR_SIZE, VirtIOBlk},
    transport::Transport,
};

use crate::{block::PlatformDeviceBlock, virtio::VirtIoHalImpl};

const VIRTIO_BLK_DMA_BUFFER_SIZE: usize = 32 * SECTOR_SIZE;

#[cfg(probe = "pci")]
crate::model_register!(
    name: "VirtIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(probe = "pci")]
fn probe_pci(
    endpoint: &mut rdrive::probe::pci::EndpointRc,
    plat_dev: PlatformDevice,
) -> Result<(), OnProbeError> {
    let transport = crate::pci::take_virtio_transport(endpoint, DeviceType::Block)?;
    register_transport(plat_dev, transport)
}

#[cfg(probe = "fdt")]
crate::model_register!(
    name: "VirtIO MMIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["virtio,mmio"],
        on_probe: probe_fdt,
    }],
);

#[cfg(probe = "fdt")]
fn probe_fdt(
    info: rdrive::register::FdtInfo<'_>,
    plat_dev: PlatformDevice,
) -> Result<(), OnProbeError> {
    let (ty, transport) = crate::virtio::probe_fdt_mmio_device(&info)?;
    if ty != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }
    register_transport(plat_dev, transport)
}

pub fn register_transport<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    let dev = VirtIoBlkDevice::new(transport)
        .map_err(|err| OnProbeError::other(format!("failed to initialize virtio-blk: {err:?}")))?;
    plat_dev.register_block(BlockDevice {
        dev: Some(dev),
        irq_enabled: false,
    });
    log::info!("registered virtio block device");
    Ok(())
}

struct VirtIoBlkDevice<T: Transport + 'static> {
    raw: VirtIOBlk<VirtIoHalImpl, T>,
}

unsafe impl<T: Transport + 'static> Send for VirtIoBlkDevice<T> {}

impl<T: Transport + 'static> VirtIoBlkDevice<T> {
    fn new(transport: T) -> Result<Self, VirtIoError> {
        let mut raw = VirtIOBlk::new(transport)?;
        raw.disable_interrupts();
        Ok(Self { raw })
    }
}

struct BlockDevice<T: Transport + 'static> {
    dev: Option<VirtIoBlkDevice<T>>,
    irq_enabled: bool,
}

impl<T: Transport + 'static> DriverGeneric for BlockDevice<T> {
    fn name(&self) -> &str {
        "virtio-blk"
    }
}

impl<T: Transport + 'static> rd_block::Interface for BlockDevice<T> {
    fn create_queue(&mut self) -> Option<alloc::boxed::Box<dyn rd_block::IQueue>> {
        self.dev
            .take()
            .map(|dev| alloc::boxed::Box::new(BlockQueue { raw: dev }) as _)
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

struct BlockQueue<T: Transport + 'static> {
    raw: VirtIoBlkDevice<T>,
}

impl<T: Transport + 'static> rd_block::IQueue for BlockQueue<T> {
    fn id(&self) -> usize {
        0
    }

    fn num_blocks(&self) -> usize {
        self.raw.raw.capacity() as _
    }

    fn block_size(&self) -> usize {
        SECTOR_SIZE
    }

    fn buff_config(&self) -> rd_block::BuffConfig {
        rd_block::BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: VIRTIO_BLK_DMA_BUFFER_SIZE,
        }
    }

    fn submit_request(
        &mut self,
        request: rd_block::Request<'_>,
    ) -> Result<rd_block::RequestId, rd_block::BlkError> {
        match request.kind {
            rd_block::RequestKind::Read(mut buffer) => {
                self.raw
                    .raw
                    .read_blocks(request.block_id, &mut buffer)
                    .map_err(map_virtio_err_to_blk_err)?;
            }
            rd_block::RequestKind::Write(items) => {
                self.raw
                    .raw
                    .write_blocks(request.block_id, items)
                    .map_err(map_virtio_err_to_blk_err)?;
            }
        }
        Ok(rd_block::RequestId::new(0))
    }

    fn poll_request(&mut self, _request: rd_block::RequestId) -> Result<(), rd_block::BlkError> {
        Ok(())
    }
}

fn map_virtio_err_to_blk_err(err: VirtIoError) -> rd_block::BlkError {
    match err {
        VirtIoError::QueueFull | VirtIoError::NotReady => rd_block::BlkError::Retry,
        VirtIoError::WrongToken
        | VirtIoError::ConfigSpaceTooSmall
        | VirtIoError::ConfigSpaceMissing => rd_block::BlkError::Other("bad internal state".into()),
        VirtIoError::AlreadyUsed => rd_block::BlkError::Other("already exists".into()),
        VirtIoError::InvalidParam => rd_block::BlkError::Other("invalid parameter".into()),
        VirtIoError::DmaError => rd_block::BlkError::NoMemory,
        VirtIoError::IoError => rd_block::BlkError::Other("I/O error".into()),
        VirtIoError::Unsupported => rd_block::BlkError::NotSupported,
        VirtIoError::SocketDeviceError(_) => rd_block::BlkError::Other("socket error".into()),
    }
}
