extern crate alloc;

use alloc::format;
use core::sync::atomic::{AtomicBool, Ordering};

use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(any(probe = "fdt", probe = "pci"))]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    Error as VirtIoError,
    device::blk::{SECTOR_SIZE, VirtIOBlk},
    transport::Transport,
};

use crate::{
    block::{PlatformDeviceBlock, SharedDriver},
    virtio::VirtIoHalImpl,
};

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
        dev: Some(SharedDriver::new(dev)),
        irq_enabled: AtomicBool::new(false),
        read_queue_created: false,
        write_queue_created: false,
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
    dev: Option<SharedDriver<VirtIoBlkDevice<T>>>,
    irq_enabled: AtomicBool,
    read_queue_created: bool,
    write_queue_created: bool,
}

impl<T: Transport + 'static> DriverGeneric for BlockDevice<T> {
    fn name(&self) -> &str {
        "virtio-blk"
    }
}

impl<T: Transport + 'static> rdif_block::Interface for BlockDevice<T> {
    fn create_read_queue(&mut self) -> Option<alloc::boxed::Box<dyn rdif_block::IReadQueue>> {
        if self.read_queue_created {
            return None;
        }
        self.dev.as_ref().map(|dev| {
            self.read_queue_created = true;
            alloc::boxed::Box::new(BlockReadQueue { raw: dev.clone() }) as _
        })
    }

    fn create_write_queue(&mut self) -> Option<alloc::boxed::Box<dyn rdif_block::IWriteQueue>> {
        if self.write_queue_created {
            return None;
        }
        self.dev.as_ref().map(|dev| {
            self.write_queue_created = true;
            alloc::boxed::Box::new(BlockWriteQueue { raw: dev.clone() }) as _
        })
    }

    fn enable_irq(&self) {
        if let Some(dev) = &self.dev {
            dev.with_mut(|dev| dev.raw.enable_interrupts());
        }
        self.irq_enabled.store(true, Ordering::Release);
    }

    fn disable_irq(&self) {
        if let Some(dev) = &self.dev {
            dev.with_mut(|dev| dev.raw.disable_interrupts());
        }
        self.irq_enabled.store(false, Ordering::Release);
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled.load(Ordering::Acquire)
    }
}

struct BlockReadQueue<T: Transport + 'static> {
    raw: SharedDriver<VirtIoBlkDevice<T>>,
}

struct BlockWriteQueue<T: Transport + 'static> {
    raw: SharedDriver<VirtIoBlkDevice<T>>,
}

impl<T: Transport + 'static> rdif_block::QueueInfo for BlockReadQueue<T> {
    fn id(&self) -> usize {
        0
    }

    fn num_blocks(&self) -> usize {
        self.raw.with_mut(|raw| raw.raw.capacity() as _)
    }

    fn block_size(&self) -> usize {
        SECTOR_SIZE
    }

    fn buffer_config(&self) -> rdif_block::BuffConfig {
        rdif_block::BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: VIRTIO_BLK_DMA_BUFFER_SIZE,
        }
    }
}

impl<T: Transport + 'static> rdif_block::IReadQueue for BlockReadQueue<T> {
    fn submit_read(
        &mut self,
        mut request: rdif_block::RequestRead<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        self.raw
            .with_mut(|raw| raw.raw.read_blocks(request.block_id, &mut request.buffer))
            .map_err(map_virtio_err_to_blk_err)?;
        Ok(rdif_block::RequestId::new(0))
    }

    fn poll_read(
        &mut self,
        _request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        Ok(rdif_block::RequestStatus::Complete)
    }
}

impl<T: Transport + 'static> rdif_block::QueueInfo for BlockWriteQueue<T> {
    fn id(&self) -> usize {
        0
    }

    fn num_blocks(&self) -> usize {
        self.raw.with_mut(|raw| raw.raw.capacity() as _)
    }

    fn block_size(&self) -> usize {
        SECTOR_SIZE
    }

    fn buffer_config(&self) -> rdif_block::BuffConfig {
        rdif_block::BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: VIRTIO_BLK_DMA_BUFFER_SIZE,
        }
    }
}

impl<T: Transport + 'static> rdif_block::IWriteQueue for BlockWriteQueue<T> {
    fn submit_write(
        &mut self,
        request: rdif_block::RequestWrite<'_>,
    ) -> Result<rdif_block::RequestId, rdif_block::BlkError> {
        self.raw
            .with_mut(|raw| raw.raw.write_blocks(request.block_id, &request.buffer))
            .map_err(map_virtio_err_to_blk_err)?;
        Ok(rdif_block::RequestId::new(0))
    }

    fn poll_write(
        &mut self,
        _request: rdif_block::RequestId,
    ) -> Result<rdif_block::RequestStatus, rdif_block::BlkError> {
        Ok(rdif_block::RequestStatus::Complete)
    }
}

fn map_virtio_err_to_blk_err(err: VirtIoError) -> rdif_block::BlkError {
    match err {
        VirtIoError::QueueFull | VirtIoError::NotReady => rdif_block::BlkError::Retry,
        VirtIoError::WrongToken
        | VirtIoError::ConfigSpaceTooSmall
        | VirtIoError::ConfigSpaceMissing => {
            rdif_block::BlkError::Other("bad internal state".into())
        }
        VirtIoError::AlreadyUsed => rdif_block::BlkError::Other("already exists".into()),
        VirtIoError::InvalidParam => rdif_block::BlkError::Other("invalid parameter".into()),
        VirtIoError::DmaError => rdif_block::BlkError::NoMemory,
        VirtIoError::IoError => rdif_block::BlkError::Other("I/O error".into()),
        VirtIoError::Unsupported => rdif_block::BlkError::NotSupported,
        VirtIoError::SocketDeviceError(_) => rdif_block::BlkError::Other("socket error".into()),
    }
}
