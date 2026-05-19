extern crate alloc;

use alloc::format;

use ax_plat::mem::PhysAddr;
use rdrive::{
    DriverGeneric, PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo,
};
use virtio_drivers::{
    Error as VirtIoError,
    device::blk::{SECTOR_SIZE, VirtIOBlk},
    transport::{DeviceType, Transport},
};

use super::PlatformDeviceBlock;
use crate::drivers::{iomap, virtio::VirtIoHalImpl};

pub(super) struct VirtIoBlkDevice<T: Transport + 'static> {
    raw: VirtIOBlk<VirtIoHalImpl, T>,
}

// SAFETY: the platform adapter owns the transport exclusively after probe and
// moves it into one rd-block queue, so no shared transport access is introduced.
unsafe impl<T: Transport + 'static> Send for VirtIoBlkDevice<T> {}

impl<T: Transport + 'static> VirtIoBlkDevice<T> {
    pub(super) fn new(transport: T) -> Result<Self, VirtIoError> {
        let mut raw = VirtIOBlk::new(transport)?;
        raw.disable_interrupts();
        Ok(Self { raw })
    }

    fn capacity(&self) -> u64 {
        self.raw.capacity()
    }

    fn read_blocks(&mut self, block_id: usize, buf: &mut [u8]) -> Result<(), VirtIoError> {
        self.raw.read_blocks(block_id, buf)
    }

    fn write_blocks(&mut self, block_id: usize, buf: &[u8]) -> Result<(), VirtIoError> {
        self.raw.write_blocks(block_id, buf)
    }
}

module_driver!(
    name: "Virtio Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["virtio,mmio"],
            on_probe: probe
        }
    ],
);

fn probe(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let base_reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )))?;

    let mmio_size = base_reg.size.unwrap_or(0x1000) as usize;
    let mmio_base = PhysAddr::from_usize(base_reg.address as usize);

    let mmio_base = iomap(mmio_base, mmio_size)?.as_ptr();

    let (ty, transport) = probe_mmio_device(mmio_base, mmio_size).ok_or(OnProbeError::NotMatch)?;

    if ty != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }

    let dev = VirtIoBlkDevice::new(transport).map_err(|e| {
        OnProbeError::other(format!(
            "failed to initialize Virtio Block device at [PA:{mmio_base:?},): {e:?}"
        ))
    })?;

    register_virtio_block(plat_dev, dev);
    debug!("virtio block device registered successfully");
    Ok(())
}

pub(super) fn register_virtio_block<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    dev: VirtIoBlkDevice<T>,
) {
    plat_dev.register_block(BlockDevice {
        dev: Some(dev),
        irq_enabled: false,
    });
}

struct BlockDevice<T: Transport + 'static> {
    dev: Option<VirtIoBlkDevice<T>>,
    irq_enabled: bool,
}

struct BlockQueue<T: Transport + 'static> {
    raw: VirtIoBlkDevice<T>,
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

impl<T: Transport + 'static> rd_block::IQueue for BlockQueue<T> {
    fn num_blocks(&self) -> usize {
        self.raw.capacity() as _
    }

    fn block_size(&self) -> usize {
        SECTOR_SIZE
    }

    fn id(&self) -> usize {
        0
    }

    fn buff_config(&self) -> rd_block::BuffConfig {
        rd_block::BuffConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            size: self.block_size(),
        }
    }

    fn submit_request(
        &mut self,
        request: rd_block::Request<'_>,
    ) -> Result<rd_block::RequestId, rd_block::BlkError> {
        let id = request.block_id;
        match request.kind {
            rd_block::RequestKind::Read(mut buffer) => {
                self.raw
                    .read_blocks(id as _, &mut buffer)
                    .map_err(map_virtio_err_to_blk_err)?;
                Ok(rd_block::RequestId::new(0))
            }
            rd_block::RequestKind::Write(items) => {
                self.raw
                    .write_blocks(id as _, items)
                    .map_err(map_virtio_err_to_blk_err)?;
                Ok(rd_block::RequestId::new(0))
            }
        }
    }

    fn poll_request(&mut self, _request: rd_block::RequestId) -> Result<(), rd_block::BlkError> {
        Ok(())
    }
}

pub(super) use ax_drivers::virtio::probe_mmio_device;

fn map_virtio_err_to_blk_err(err: VirtIoError) -> rd_block::BlkError {
    match err {
        VirtIoError::QueueFull | VirtIoError::NotReady => rd_block::BlkError::Retry,
        VirtIoError::WrongToken
        | VirtIoError::ConfigSpaceTooSmall
        | VirtIoError::ConfigSpaceMissing => rd_block::BlkError::Other("Bad internal state".into()),
        VirtIoError::AlreadyUsed => rd_block::BlkError::Other("Already exists".into()),
        VirtIoError::InvalidParam => rd_block::BlkError::Other("Invalid parameter".into()),
        VirtIoError::DmaError => rd_block::BlkError::NoMemory,
        VirtIoError::IoError => rd_block::BlkError::Other("I/O error".into()),
        VirtIoError::Unsupported => rd_block::BlkError::NotSupported,
        VirtIoError::SocketDeviceError(_) => rd_block::BlkError::Other("Socket error".into()),
    }
}
